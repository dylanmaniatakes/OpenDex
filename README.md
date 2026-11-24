# OpenDex

I built OpenDex to use the models I choose for everyday chat and coding. It runs on my Mac without an OpenDex account. I can connect local servers and cloud APIs at the same time, keep a short model list for each connection, and switch providers from the composer.

## Try it on macOS

1. Open `dist/OpenDex.app` if you have the local preview bundle. To build it yourself, follow the instructions below.
2. In **Model Chat**, choose **Find local models** if you already run Ollama. Pick an installed model to add it to the composer.
3. To connect another service, open **Settings → Providers → Add provider**. Choose an adapter, enter the base URL and key if the endpoint requires one, and save.
4. Use **Discover models** to browse an endpoint’s inventory, or enter a model ID manually. Each model I add appears in both modes. Ollama Cloud has its own searchable catalog and uses an Ollama API key.
5. Use the sidebar dropdown to switch between **Model Chat** and **Code Agent**. Model Chat shows its conversations. In Code Agent, use **Add project** to choose a folder, then use the plus button beside a project to create another thread in it.

I can rename or export each conversation, attach a file, and set instructions globally or per thread. Both modes stream replies, and the stop button interrupts the active turn. Images and PDFs require a model and endpoint that accept them.

In **Model Chat**, I can edit a user message or retry a reply. Both actions create a branch using the history through the original prompt, with its saved attachments. I can also use **Branch from here** in either mode to continue through a selected message. I can return to the source under **Conversation settings → Open original conversation**. Code Agent branches use the selected project folder, and I can create a separate worktree for isolated changes.

## Connections

| Adapter | Default base URL | Authentication |
| --- | --- | --- |
| OpenAI API | `https://api.openai.com/v1` | OpenAI API key. Uses the Responses API. |
| ChatGPT Account | Installed `codex` executable | Browser sign-in through Codex app-server. Uses eligible Codex models. |
| Anthropic API | `https://api.anthropic.com/v1` | Anthropic Console key. Uses the Messages API. |
| Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | Google AI Studio API key. Uses native Gemini streaming and function calling. |
| Ollama Local | `http://127.0.0.1:11434/api` | Local Ollama normally accepts unauthenticated requests. |
| Ollama Cloud | `https://ollama.com/api` | Ollama API key. |
| OpenAI-compatible | A service’s Chat Completions base URL | Optional bearer key and headers populated from environment variables. |

I can point an API adapter at a compatible gateway by editing its base URL. For example, LM Studio and many self-hosted inference servers expose Chat Completions. An endpoint with its own wire format needs a compatible gateway or a new adapter. Model discovery is optional, so I can still enter IDs when an endpoint provides inference only. Under **Advanced connection settings**, I can set the header that receives my saved API key. This lets me connect to gateways that expect a header such as `api-key`.

For Gemini, I can discover models that support content generation, add model IDs manually, and use them in either mode. Text replies, image and PDF inputs, and Code Agent tool calls use Gemini’s own API. OpenDex keeps signed tool-call parts in memory while a turn is running. [Gemini API reference](https://aistudio.google.com/api-reference/generate-content)

For a ChatGPT account, I install the [Codex CLI](https://learn.chatgpt.com/docs/cli), add the **ChatGPT Account** adapter, and click **Connect account**. The Codex executable opens the official browser flow and manages its own credentials. OpenDex uses a dedicated Codex home so the connection is independent of my terminal setup. The browser flow and subscribed inference are still part of my live verification checklist. [Codex app-server authentication](https://learn.chatgpt.com/docs/app-server#auth-endpoints)

I use an Anthropic Console API key for Claude in OpenDex. Claude.ai subscription sign-in is available through Anthropic’s own flow under its current authentication policy. [Anthropic authentication policy](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)

## Code Agent and permissions

I choose a project folder for Code Agent to read, search, and edit. A Git worktree gives a thread a separate checkout and branch while keeping it under the original project in the sidebar. **Review changes** opens the worktree’s Git status and diff. I can run multiple conversations and use separate worktrees for concurrent agent turns on the same repository.

I start in **Workspace** mode. It allows project reads and asks before writes, commands, and MCP calls. **Read only** gives the agent project inspection. **Custom** lets me set each action to **Deny**, **Ask every time**, or **Allow**, choose network access for scoped commands, and set a tool-call limit. **Full access** runs commands with my user permissions and network access. MCP calls still require approval in that profile.

On macOS I run scoped commands through Seatbelt. They can read approved project content and system or installed toolchain resources. Project writes and command networking follow the selected profile. A command approval covers the command and its child processes. Project builds that need caches or credentials outside the workspace may need a different permission profile. I run trusted local MCP executables separately from the command sandbox, so enabling one lets it start with my app’s user privileges.

## MCP servers

In **Settings → MCP Servers**, I can add a local stdio command or a Streamable HTTP URL. Local servers take a JSON array of arguments and a list of environment variable names. HTTP servers can use a bearer key. **Test & list tools** checks the connection and exposes individual tool switches. I can enable the server for turns, then apply the MCP permission rule to its calls.

I use the **Add MCP context** button in either composer to browse an enabled server. I can search for a resource, preview the text, and attach it alongside a local file. For a resource template I enter the filled-in URI. For a prompt I fill in its arguments, preview the result, then insert the text into my draft. When a prompt includes assistant examples, I see them labeled in the draft. The model receives the draft and any selected resources when I send the message.

I currently support initialized MCP sessions and the July 2026 protocol revision for tools, resources, and prompts. The MCP protocol selector is on each server. [MCP transport specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)

## Appearance and data

I default to dark mode. Under **Appearance**, I can change the theme, accent, typeface, text scale, spacing, widths, corner radius, and animation preference.

The app saves project links, settings, conversations, and attachments in a local workspace file. On macOS, it lives under `~/Library/Application Support/org.opendex.desktop`. OpenDex uses the operating system credential store for API and MCP keys. I can move the data folder with `--data-dir` or `OPENDEX_DATA_DIR`. OpenDex allows one running copy per desktop user, including when I choose a different data folder. Messages, attachments, and approved tool output go to the currently selected model endpoint.

## Build from source

I use Rust for the provider adapters, state, MCP, and permission checks. Tauri provides the desktop window and native dialogs, with a small HTML/CSS/JavaScript frontend and no Node build step. The workspace supports **Rust 1.95** and uses the checked-in dependency lockfile.

On macOS I use the Xcode command-line tools and Python 3 for bundle packaging.

```sh
cargo run --locked
bash scripts/bundle-macos.sh
```

The bundling script writes `dist/OpenDex.app` and applies a local ad-hoc signature. Finder can launch the preview on the Mac that built it. Developer ID signing and notarization are release tasks. The Rust and Tauri code leave room for Windows and Linux builds. I have tested the desktop on Apple Silicon macOS and scoped command isolation is implemented for macOS.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --test provider_transports installed_codex_server -- --ignored
```

I use loopback fixtures to test each wire format and MCP transport without account credentials. I verified Gemini model discovery, streaming chat, and a Code Agent file read through the desktop app against a native-protocol fixture. The optional Codex check exercises the installed CLI with an isolated data directory. I tested live local Ollama replies in Model Chat, including edited and retried branches, a live Code Agent file read, and the public Ollama Cloud catalog from the macOS GUI. I also switched between modes, created a thread under one of two test projects, approved a file edit and an MCP call in a disposable project, and sent a rendered MCP prompt with a text resource to a local Ollama model.

### Source layout

- `src/providers` contains inventory discovery, API payloads, and streaming decoders.
- `src/state.rs` owns the conversation and settings schema, atomic persistence, and credential access.
- `src/agent.rs` contains project file tools, command scoping, and Git operations.
- `src/mcp.rs` contains the JSON-RPC connection and tool operations.
- `src/codex.rs` connects to the optional Codex app server.
- `src/desktop.rs` coordinates turns, approval prompts, and desktop commands.
- `ui` contains the desktop screens, appearance controls, and Markdown display.

## Preview scope

This is my first local preview. My priorities are provider freedom, usable MCP settings, project-based agent work, and transparent permissions. I still have work to do on voice, generated media, web and computer-use tools, automations, shared/cloud history, MCP subscriptions and media-rich prompts, and platform packaging. I plan to share the source after I’ve verified the workflows I use most often.

### Planned encrypted backup and restore

I want to be able to save a password-protected backup of my complete settings for a reinstall or a move to another computer. That includes provider connections and model lists, MCP settings, permission profiles, appearance, instructions, and project links. I plan to include OpenDex-managed API keys and MCP credentials in the encrypted archive, along with provider account connection details.

On restore, I want OpenDex to authenticate and decrypt the file before applying any changes, let me remap project paths, and put restored secrets back in the operating system credential store. For provider-managed account logins, I plan to use supported migration methods where available and guide users through sign-in again when a session cannot be transferred.

I’m making OpenDex available under the [MIT License](LICENSE).
