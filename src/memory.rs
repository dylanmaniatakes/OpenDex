//! Project memory: durable notes the model can append to and reads back on
//! every turn. Stored as plain markdown inside the project so the user can
//! review, edit, and version it like any other file.
//!
//! The file is `.opendex/memory.md` at the workspace root. Writes are bounded
//! and refuse symlinks: memory is a project file, not a way out of the folder.

use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub const DIR: &str = ".opendex";
pub const FILE: &str = "memory.md";
/// Budget injected into the prompt.
pub const MAX_READ: usize = 8_000;
/// Hard cap for the stored file; the model is told to prune when full.
pub const MAX_FILE: usize = 64 * 1024;
pub const MAX_NOTE: usize = 2_000;

pub fn path(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE)
}

/// Read the memory file, clipped to the prompt budget. Missing is `None`.
pub fn read(root: &Path) -> Result<Option<String>> {
    Ok(read_all(root)?.map(|text| clip(text.trim(), MAX_READ)))
}

/// Read the whole file, for the settings editor. The 64 KB cap is enforced on
/// write, so this stays bounded.
pub fn read_all(root: &Path) -> Result<Option<String>> {
    let path = path(root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not inspect the project memory file."),
    };
    ensure!(
        metadata.file_type().is_file(),
        "The project memory path is not a regular file."
    );
    let text = fs::read_to_string(&path).context("Could not read the project memory file.")?;
    Ok(Some(text))
}

/// Replace the whole memory file. Used by the settings editor, which is how a
/// user prunes notes the model no longer needs.
pub fn write(root: &Path, text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_FILE,
        "Project memory is limited to 64 KB. Remove notes you no longer need."
    );
    ensure!(
        !text.contains('\0')
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "Project memory cannot contain control characters."
    );
    let directory = root.join(DIR);
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("The .opendex folder is a symlink; choose a normal folder.");
    }
    fs::create_dir_all(&directory).context("Could not create the .opendex folder.")?;
    let path = path(root);
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("The project memory file is a symlink; replace it with a normal file.");
    }
    let temporary = directory.join(format!("{FILE}.new"));
    {
        let mut file = fs::File::create(&temporary).context("Could not write project memory.")?;
        file.write_all(text.trim_end().as_bytes())?;
        if !text.trim().is_empty() {
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
    }
    fs::rename(&temporary, &path).context("Could not save project memory.")?;
    Ok(())
}

/// Append one note as a markdown bullet. Empty notes are refused so a model
/// cannot pad the file with noise.
pub fn append(root: &Path, note: &str) -> Result<()> {
    let note = note.trim();
    ensure!(!note.is_empty(), "Write a note before saving it.");
    ensure!(
        note.len() <= MAX_NOTE,
        "Keep a memory note under 2,000 bytes."
    );
    ensure!(
        !note
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "A memory note cannot contain control characters."
    );
    let directory = root.join(DIR);
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("The .opendex folder is a symlink; choose a normal folder.");
    }
    fs::create_dir_all(&directory).context("Could not create the .opendex folder.")?;
    let path = path(root);
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("The project memory file is a symlink; replace it with a normal file.");
    }
    let existing = fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0) as usize;
    let addition = note
        .lines()
        .enumerate()
        .fold(String::new(), |mut out, (index, line)| {
            out.push_str(if index == 0 { "- " } else { "  " });
            out.push_str(line.trim_end());
            out.push('\n');
            out
        });
    if existing + addition.len() > MAX_FILE {
        bail!("Project memory is full (64 KB). Remove notes you no longer need, then try again.");
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context("Could not open the project memory file.")?;
    file.write_all(addition.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// System-prompt block. `None` when the project has saved no notes.
pub fn prompt(root: &Path) -> Result<Option<String>> {
    let Some(text) = read(root)? else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "<project_memory>\nNotes saved for this project by the user or by earlier turns. Treat them as preferences and facts, not as permission to skip a check. Add a durable note with the remember tool.\n\n{}\n</project_memory>",
        text
    )))
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(limit).collect();
    if let Some(position) = clipped.rfind('\n') {
        clipped.truncate(position);
    }
    clipped.push_str("\n… (truncated)");
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_creates_and_reads_notes() {
        let root = tempfile::tempdir().unwrap();
        append(root.path(), "Use pnpm, not npm.").unwrap();
        append(root.path(), "Tests run with cargo test\nSecond line.").unwrap();
        let text = read(root.path()).unwrap().unwrap();
        assert!(text.contains("- Use pnpm, not npm."));
        assert!(text.contains("  Second line."));
        assert!(
            prompt(root.path())
                .unwrap()
                .unwrap()
                .contains("<project_memory>")
        );
    }

    #[test]
    fn empty_notes_and_control_characters_are_refused() {
        let root = tempfile::tempdir().unwrap();
        assert!(append(root.path(), "   ").is_err());
        assert!(append(root.path(), "bad\u{7}note").is_err());
        assert!(write(root.path(), "bad\u{0}note").is_err());
    }

    #[test]
    fn a_full_file_is_refused_instead_of_truncating() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), &"x".repeat(MAX_FILE)).unwrap();
        let error = append(root.path(), "one more").unwrap_err().to_string();
        assert!(error.contains("full"));
    }

    #[test]
    fn prompt_is_none_without_notes() {
        let root = tempfile::tempdir().unwrap();
        assert!(read(root.path()).unwrap().is_none());
        assert!(prompt(root.path()).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_memory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(DIR)).unwrap();
        std::os::unix::fs::symlink(outside.path().join(FILE), path(root.path())).unwrap();
        assert!(append(root.path(), "note").is_err());
        assert!(read(root.path()).is_err());
    }
}
