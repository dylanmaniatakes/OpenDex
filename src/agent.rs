//! Workspace tools and the permission checks behind them.
use crate::{
    images::raster_mime,
    mcp::process_path,
    memory,
    providers::Tool,
    state::{Attachment, Mode, Permissions, Profile, Rule},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{encoded_len, engine::general_purpose::STANDARD, write::EncoderWriter};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Seek, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

const MAX_FILE: u64 = 1024 * 1024;
const MAX_ATTACHMENT_BYTES: u64 = 200 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 8;
const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Read,
    Write,
    Command,
    Mcp,
}

impl Capability {
    pub fn rule(self, policy: &Permissions) -> Rule {
        match self {
            Self::Read => policy.read,
            Self::Write => policy.write,
            Self::Command => policy.command,
            Self::Mcp => policy.mcp,
        }
    }
}

fn definition(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    Tool {
        name: name.into(),
        description: description.into(),
        schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}

pub fn tools(mode: Mode, root: Option<&Path>, policy: &Permissions) -> Vec<Tool> {
    if mode != Mode::Agent || root.is_none() {
        return vec![];
    }
    let mut tools = Vec::new();
    if policy.read != Rule::Deny {
        tools.extend([
            definition("list_files", "List workspace files respecting ignore rules.", json!({
                "path":{"type":"string","description":"Workspace-relative directory; defaults to the workspace root."}
            }), &[]),
            definition("read_file", "Read UTF-8 text with line numbers from a workspace file.", json!({
                "path":{"type":"string"}, "start":{"type":"integer","description":"First line, starting at 1."},
                "end":{"type":"integer","description":"Last line, inclusive."}
            }), &["path"]),
            definition("search_files", "Search small workspace text files for an exact substring, respecting ignore rules.", json!({
                "text":{"type":"string"}, "path":{"type":"string","description":"Workspace-relative directory; defaults to the workspace root."}
            }), &["text"]),
            definition("workspace_diff", "Show tracked changes against HEAD plus status for untracked paths.", json!({}), &[]),
        ]);
    }
    if policy.write != Rule::Deny {
        tools.extend([
            definition("create_file", "Create a new UTF-8 file. The parent directory must already exist.", json!({
                "path":{"type":"string"}, "content":{"type":"string"}
            }), &["path","content"]),
            definition("edit_file", "Replace exactly one matching string in a workspace file. Read the file before editing.", json!({
                "path":{"type":"string"}, "old_text":{"type":"string"}, "new_text":{"type":"string"}
            }), &["path","old_text","new_text"]),
            definition("remember", "Save a durable note about this project for future turns. Use it for conventions, decisions, and gotchas the user would want carried forward; keep each note short and factual.", json!({
                "note":{"type":"string","description":"One fact or decision, up to 2,000 bytes."}
            }), &["note"]),
        ]);
    }
    if policy.command != Rule::Deny {
        tools.push(definition("run_command", "Run a program from the workspace with a literal argument list, for example cargo with [\"test\"]. Output is bounded.", json!({
            "program":{"type":"string"}, "args":{"type":"array","items":{"type":"string"}}
        }), &["program","args"]));
    }
    tools
}

pub fn capability(name: &str) -> Option<Capability> {
    match name {
        "list_files" | "read_file" | "search_files" | "workspace_diff" => Some(Capability::Read),
        "create_file" | "edit_file" | "remember" => Some(Capability::Write),
        "run_command" => Some(Capability::Command),
        _ => None,
    }
}

/// Permanently allow one tool category by switching to the custom profile.
pub fn always_allow(policy: &mut Permissions, capability: Capability) {
    policy.profile = Profile::Custom;
    match capability {
        Capability::Read => policy.read = Rule::Allow,
        Capability::Write => policy.write = Rule::Allow,
        Capability::Command => policy.command = Rule::Allow,
        Capability::Mcp => policy.mcp = Rule::Allow,
    }
}

pub fn checked_root(root: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .context("The selected workspace is unavailable.")?;
    ensure!(root.is_dir(), "Choose a directory as your workspace.");
    ensure!(
        root.parent().is_some(),
        "Choose a project directory inside the filesystem root."
    );
    Ok(root)
}

/// Reject links and traversal at every component. These checks are repeated on
/// each filesystem operation, independent of anything supplied by the model.
pub fn workspace_path(root: &Path, requested: &str, new_leaf: bool) -> Result<PathBuf> {
    let relative = Path::new(requested);
    ensure!(!relative.is_absolute(), "Give a workspace-relative path.");
    let mut path = root.to_owned();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        match part {
            Component::CurDir => continue,
            Component::Normal(name) => {
                ensure!(
                    !name.to_string_lossy().eq_ignore_ascii_case(".git"),
                    "Use workspace_diff for Git metadata."
                );
                path.push(name);
            }
            _ => bail!("Paths cannot leave the selected workspace."),
        }
        match path.symlink_metadata() {
            Ok(metadata) => ensure!(
                !metadata.is_symlink(),
                "Symbolic links are outside the workspace tool scope."
            ),
            Err(error)
                if new_leaf
                    && parts.peek().is_none()
                    && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Workspace path is unavailable."),
        }
    }
    Ok(path)
}

fn arg<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments[key]
        .as_str()
        .with_context(|| format!("Expected a string argument named {key}."))
}

fn text_file(path: &Path) -> Result<String> {
    ensure!(path.symlink_metadata()?.is_file(), "Choose a regular file.");
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .context("The file must be UTF-8 text.")?;
    ensure!(
        text.len() as u64 <= MAX_FILE,
        "The file exceeds the workspace tool size limit."
    );
    Ok(text)
}

struct AttachmentFile {
    path: PathBuf,
    name: String,
    file: fs::File,
    size: u64,
}

fn attachment_files(paths: Vec<PathBuf>) -> Result<Vec<AttachmentFile>> {
    ensure!(
        paths.len() <= MAX_ATTACHMENTS,
        "Attach up to eight files per message."
    );
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut files = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .context("The selected file has no name.")?
            .to_string_lossy()
            .into_owned();
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Attachment {name} is unavailable."))?;
        if !seen.insert(canonical.clone()) {
            continue;
        }
        ensure!(
            canonical.symlink_metadata()?.is_file(),
            "Attachment {name} must be a regular file."
        );
        let file = fs::File::open(&canonical)
            .with_context(|| format!("Could not open attachment {name}."))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "Attachment {name} must be a regular file."
        );
        let size = metadata.len();
        ensure!(
            size <= MAX_ATTACHMENT_BYTES - total,
            "Attachments can total up to 200 MiB per message."
        );
        total += size;
        files.push(AttachmentFile {
            path,
            name,
            file,
            size,
        });
    }
    Ok(files)
}

pub fn inspect_attachments(paths: Vec<PathBuf>) -> Result<Vec<(PathBuf, String)>> {
    Ok(attachment_files(paths)?
        .into_iter()
        .map(|file| (file.path, file.name))
        .collect())
}

pub fn load_attachments(paths: Vec<PathBuf>) -> Result<Vec<(PathBuf, Attachment)>> {
    let mut remaining = MAX_ATTACHMENT_BYTES;
    let mut attachments = Vec::new();
    for AttachmentFile {
        path,
        name,
        mut file,
        size,
    } in attachment_files(paths)?
    {
        let (attachment, bytes) = read_attachment(&name, &mut file, size, remaining)
            .with_context(|| format!("Could not attach {name}."))?;
        remaining -= bytes;
        attachments.push((path, attachment));
    }
    Ok(attachments)
}

fn read_attachment(
    name: &str,
    file: &mut fs::File,
    size: u64,
    remaining: u64,
) -> Result<(Attachment, u64)> {
    let mut header = Vec::with_capacity(12);
    Read::by_ref(file).take(12).read_to_end(&mut header)?;
    let mime = if header.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        raster_mime(&header)
    };
    file.rewind()?;
    let mut input = file.take(remaining + 1);
    let capacity = size.min(remaining) as usize;
    let (text, bytes) = if mime.is_some() {
        let mut encoded =
            Vec::with_capacity(encoded_len(capacity, true).expect("bounded file size"));
        let mut encoder = EncoderWriter::new(&mut encoded, &STANDARD);
        let bytes = io::copy(&mut input, &mut encoder)?;
        encoder.finish()?;
        drop(encoder);
        (String::from_utf8(encoded).expect("Base64 is UTF-8"), bytes)
    } else {
        let mut text = String::with_capacity(capacity);
        let bytes = input
            .read_to_string(&mut text)
            .context("Choose UTF-8 text, a PDF, a PNG, JPEG, GIF, or WebP image.")?;
        ensure!(
            !text.contains('\0'),
            "Binary attachments must be an image or PDF."
        );
        (text, bytes as u64)
    };
    ensure!(
        bytes <= remaining,
        "Attachments can total up to 200 MiB per message."
    );
    Ok((
        Attachment {
            name: name.into(),
            text: text.into(),
            media_type: mime.map(str::to_owned),
        },
        bytes,
    ))
}

fn trimmed(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let boundary = text.floor_char_boundary(limit);
    format!("{}\n[Output truncated]", &text[..boundary])
}

fn walk(root: &Path, arguments: &Value, search: bool) -> Result<String> {
    let path = workspace_path(root, arguments["path"].as_str().unwrap_or("."), false)?;
    ensure!(path.is_dir(), "Choose a workspace directory.");
    let needle = if search {
        let needle = arg(arguments, "text")?;
        ensure!(
            !needle.is_empty() && needle.len() <= 400,
            "Use a nonempty search string of at most 400 bytes."
        );
        needle
    } else {
        ""
    };

    let mut hits = Vec::new();
    let mut total = 0;
    let walker = WalkBuilder::new(&path)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();
    for entry in walker.filter_map(std::result::Result::ok).take(6000) {
        if entry.path() == path || !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        if !search {
            hits.push(relative.display().to_string());
        } else if let Ok(metadata) = entry.metadata()
            && metadata.len() <= 250_000
        {
            total += metadata.len();
            if total > 30_000_000 {
                break;
            }
            if let Ok(text) = text_file(entry.path()) {
                for (index, line) in text.lines().enumerate() {
                    if line.contains(needle) {
                        hits.push(format!(
                            "{}:{}: {}",
                            relative.display(),
                            index + 1,
                            trimmed(line, 1000)
                        ));
                        if hits.len() >= 200 {
                            break;
                        }
                    }
                }
            }
        }
        if hits.len() >= 200 {
            break;
        }
    }
    if hits.is_empty() {
        return Ok(if search {
            "No matching text found."
        } else {
            "No files found."
        }
        .into());
    }
    Ok(trimmed(&hits.join("\n"), MAX_OUTPUT))
}

fn file_tool(root: &Path, name: &str, arguments: &Value) -> Result<String> {
    match name {
        "list_files" => walk(root, arguments, false),
        "search_files" => walk(root, arguments, true),
        "read_file" => {
            let path = workspace_path(root, arg(arguments, "path")?, false)?;
            let start = arguments["start"].as_u64().unwrap_or(1).max(1);
            let end = arguments["end"]
                .as_u64()
                .unwrap_or(start.saturating_add(299));
            ensure!(
                end >= start && end - start < 800,
                "A line range may contain at most 800 lines."
            );
            let text = text_file(&path)?;
            let mut output = String::new();
            for (index, line) in text.lines().enumerate() {
                let number = index as u64 + 1;
                if number > end {
                    break;
                }
                if number >= start {
                    output.push_str(&format!("{number:>4} | {line}\n"));
                }
                if output.len() > MAX_OUTPUT {
                    break;
                }
            }
            Ok(trimmed(&output, MAX_OUTPUT))
        }
        "create_file" => {
            let path = workspace_path(root, arg(arguments, "path")?, true)?;
            let content = arg(arguments, "content")?;
            ensure!(
                content.len() as u64 <= MAX_FILE,
                "The new file exceeds the size limit."
            );
            let parent = path.parent().context("A file needs a parent directory.")?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(content.as_bytes())?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&path)
                .context("Could not create a new file at that path.")?;
            Ok(format!("Created {}", path.strip_prefix(root)?.display()))
        }
        "edit_file" => {
            let path = workspace_path(root, arg(arguments, "path")?, false)?;
            let old = arg(arguments, "old_text")?;
            let new = arg(arguments, "new_text")?;
            ensure!(!old.is_empty(), "Give a nonempty old_text value.");
            let text = text_file(&path)?;
            ensure!(
                text.match_indices(old).count() == 1,
                "old_text must match exactly once. Re-read the file before retrying."
            );
            let content = text.replacen(old, new, 1);
            ensure!(
                content.len() as u64 <= MAX_FILE,
                "The edited file exceeds the size limit."
            );
            let mut file = tempfile::NamedTempFile::new_in(
                path.parent().context("Missing parent directory.")?,
            )?;
            file.as_file()
                .set_permissions(path.metadata()?.permissions())?;
            file.write_all(content.as_bytes())?;
            file.as_file().sync_all()?;
            ensure!(
                text_file(&path)? == text,
                "The file changed while preparing the edit. Re-read it before retrying."
            );
            file.persist(&path)?;
            Ok(format!("Updated {}", path.strip_prefix(root)?.display()))
        }
        _ => bail!("Unknown workspace tool."),
    }
}

pub async fn execute(
    root: &Path,
    policy: &Permissions,
    name: &str,
    arguments: &Value,
) -> Result<String> {
    ensure!(
        arguments.is_object() && arguments.get("_invalid_arguments").is_none(),
        "The tool needs a valid JSON object."
    );
    let capability = capability(name).context("Unknown workspace tool.")?;
    ensure!(
        capability.rule(policy) != Rule::Deny,
        "That tool is disabled by the permission profile."
    );
    match name {
        "workspace_diff" => diff(root).await,
        "remember" => {
            memory::append(root, arg(arguments, "note")?)?;
            Ok("Saved to project memory.".into())
        }
        "run_command" => {
            let program = arg(arguments, "program")?;
            let args = arguments["args"]
                .as_array()
                .context("Give an array of argument strings.")?;
            ensure!(args.len() <= 100, "Too many command arguments.");
            let args = args
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .context("Every command argument must be a string.")
                })
                .collect::<Result<Vec<_>>>()?;
            let command = scoped_command(root, policy, program, &args)?;
            capture(command, Duration::from_secs(90)).await
        }
        _ => file_tool(root, name, arguments),
    }
}

pub fn workspace_git(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", process_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(root)
        .args(["--no-pager", "-c", "core.fsmonitor=false"])
        .args(args);
    command
}

async fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = capture(workspace_git(root, args), Duration::from_secs(12)).await?;
    output
        .strip_prefix("Exit 0\n")
        .map(str::to_owned)
        .with_context(|| format!("Git could not read changes in this project.\n{output}"))
}

pub async fn diff(root: &Path) -> Result<String> {
    let status = git_output(
        root,
        &["status", "--short", "--untracked-files=normal", "--", "."],
    )
    .await?;
    let has_head = git_output(root, &["rev-parse", "--verify", "HEAD"])
        .await
        .is_ok();
    let tracked = if has_head {
        git_output(
            root,
            &["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--", "."],
        )
        .await?
    } else {
        let staged = git_output(
            root,
            &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                ".",
            ],
        )
        .await?;
        let unstaged =
            git_output(root, &["diff", "--no-ext-diff", "--no-textconv", "--", "."]).await?;
        format!("{staged}{unstaged}")
    };
    let untracked = git_output(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ],
    )
    .await?;
    let mut changes = format!(
        "Status\n{}\nChanges\n{tracked}",
        if status.is_empty() {
            "Clean\n"
        } else {
            &status
        }
    );
    for name in untracked.split_terminator('\0').take(10) {
        if changes.len() > MAX_OUTPUT {
            break;
        }
        if let Ok(path) = workspace_path(root, name, false)
            && let Ok(content) = text_file(&path)
            && !content.contains('\0')
        {
            changes.push_str(&format!("\n--- /dev/null\n+++ {name:?}\n"));
            for line in content.lines() {
                changes.push_str(&format!("+{}\n", trimmed(line, 2000)));
                if changes.len() > MAX_OUTPUT {
                    break;
                }
            }
        }
    }
    Ok(trimmed(&changes, MAX_OUTPUT))
}

pub async fn create_worktree(root: &Path, target: &Path, branch: &str) -> Result<String> {
    ensure!(
        target.is_absolute() && !target.exists() && !branch.is_empty() && !branch.starts_with('-'),
        "Choose a new worktree folder and enter a branch name."
    );
    let target = target.to_str().context("Use a UTF-8 worktree path.")?;
    let mut command = workspace_git(
        root,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "worktree",
            "add",
            "-b",
            branch,
            target,
            "HEAD",
        ],
    );
    command.env_remove("GIT_OPTIONAL_LOCKS");
    capture(command, Duration::from_secs(60)).await
}

fn scoped_command(
    root: &Path,
    policy: &Permissions,
    program: &str,
    args: &[&str],
) -> Result<Command> {
    ensure!(
        !program.trim().is_empty() && !program.starts_with('-'),
        "Enter a program name."
    );
    let mut command = if policy.profile == Profile::FullAccess {
        Command::new(program)
    } else {
        #[cfg(not(target_os = "macos"))]
        bail!(
            "Workspace command isolation is currently available on macOS. Choose another permission profile or use the file tools."
        );
        #[cfg(target_os = "macos")]
        {
            let escaped_root = seatbelt_string(root)?;
            let file_rules = if policy.read == Rule::Deny {
                String::new()
            } else {
                format!("(allow file-read-data (subpath {escaped_root}))")
            };
            let write_rules = if policy.write == Rule::Deny {
                String::new()
            } else {
                format!("(allow file-write* (subpath {escaped_root}))")
            };
            let network = if policy.network {
                "(allow network*)"
            } else {
                ""
            };
            let profile = format!(
                r#"(version 1)
(allow default)
(deny file-write*)
(deny file-read-data)
(deny network*)
(deny appleevent-send)
(deny mach-lookup (global-name "com.apple.securityd") (global-name "com.apple.secd"))
(allow file-read-data
  (literal "/") (subpath "/usr") (subpath "/System") (subpath "/Library/Developer") (subpath "/Applications/Xcode.app")
  (subpath "/bin") (subpath "/sbin") (subpath "/opt/homebrew") (subpath "/opt/local") (subpath "/private/etc")
  (subpath "/dev") (subpath "/private/var/db/dyld") (subpath "/private/var/db/timezone")
)
(allow file-write-data (literal "/dev/null") (literal "/dev/tty"))
{file_rules}
{write_rules}
{network}"#
            );
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.args(["-p", &profile, "--", program]);
            command
        }
    };
    command
        .env_clear()
        .env("PATH", process_path())
        .env("LANG", "en_US.UTF-8")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .current_dir(root)
        .args(args);
    Ok(command)
}

#[cfg(target_os = "macos")]
fn seatbelt_string(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .context("The selected workspace path is not UTF-8.")?;
    ensure!(
        !path.chars().any(char::is_control),
        "The selected workspace path contains control characters."
    );
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

async fn collect(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let copy = read.min(MAX_OUTPUT.saturating_sub(kept.len()));
        kept.extend_from_slice(&buffer[..copy]);
    }
    Ok(kept)
}

pub async fn capture(mut command: Command, limit: Duration) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().context("Could not start the command.")?;
    let _group = ProcessGroup(child.id());
    let stdout = child.stdout.take().context("Missing command stdout.")?;
    let stderr = child.stderr.take().context("Missing command stderr.")?;
    let (status, stdout, stderr) = tokio::time::timeout(limit, async {
        tokio::try_join!(
            async { Ok(child.wait().await?) },
            collect(stdout),
            collect(stderr)
        )
    })
    .await
    .context("The command timed out.")??;
    Ok(trimmed(
        &format!(
            "Exit {}\n{}{}",
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        ),
        MAX_OUTPUT,
    ))
}

struct ProcessGroup(Option<u32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // A dedicated process group limits cancellation to this command tree.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Profile;

    #[test]
    fn always_allow_upgrades_only_the_requested_category() {
        let mut policy = Permissions::default();
        assert_eq!(policy.profile, Profile::Workspace);
        assert_eq!(policy.command, Rule::Ask);

        always_allow(&mut policy, Capability::Command);
        assert_eq!(policy.profile, Profile::Custom);
        assert_eq!(policy.command, Rule::Allow);
        // Unrelated categories keep the workspace defaults.
        assert_eq!(policy.write, Rule::Ask);
        assert_eq!(policy.mcp, Rule::Ask);
        assert_eq!(policy.effective().command, Rule::Allow);

        always_allow(&mut policy, Capability::Read);
        assert_eq!(policy.read, Rule::Allow);
        assert_eq!(policy.write, Rule::Ask);
    }

    #[test]
    fn only_tools_with_rules_can_be_always_allowed() {
        // Web and MCP calls have no approval rule in this list, so the
        // approval card must not offer the button for them.
        assert!(capability("web_search").is_none());
        assert!(capability("mcp__example__tool").is_none());
        assert_eq!(capability("run_command"), Some(Capability::Command));
    }

    #[test]
    fn remember_is_gated_by_write_permission() {
        let root = tempfile::tempdir().unwrap();
        let mut policy = Permissions::default();
        policy.write = Rule::Allow;
        let offered = tools(Mode::Agent, Some(root.path()), &policy);
        assert!(offered.iter().any(|tool| tool.name == "remember"));

        policy.write = Rule::Deny;
        let offered = tools(Mode::Agent, Some(root.path()), &policy);
        assert!(!offered.iter().any(|tool| tool.name == "remember"));
    }

    #[tokio::test]
    async fn remember_writes_a_project_note() {
        let root = tempfile::tempdir().unwrap();
        let mut policy = Permissions::default();
        policy.write = Rule::Allow;
        let output = execute(
            root.path(),
            &policy,
            "remember",
            &json!({"note": "Deploy uses fly.io."}),
        )
        .await
        .unwrap();
        assert!(output.contains("project memory"));
        let text = memory::read(root.path()).unwrap().unwrap();
        assert!(text.contains("fly.io"));
    }
}
