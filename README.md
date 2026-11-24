# OpenDex

I built OpenDex to use the models I choose for everyday chat and coding. It runs on my Mac without an OpenDex account. I can connect local servers and cloud APIs at the same time, keep a short model list for each connection, and switch providers from the composer.

## Try it on macOS

1. Open `dist/OpenDex.app` if you have the local preview bundle. To build it yourself, follow the instructions below.
2. In **Model Chat**, choose **Find local models** if you already run Ollama. Pick an installed model to add it to the composer.
3. To connect another service, open **Settings → Providers → Add provider**. Choose an adapter, enter the base URL and key if the endpoint requires one, and save.
4. Use **Discover models** to browse an endpoint’s inventory, or enter a model ID manually. Each model I add appears in both modes. Ollama Cloud has its own searchable catalog and uses an Ollama API key.
5. Use the sidebar dropdown to switch between **Model Chat** and **Code Agent**. Model Chat shows its conversations. In Code Agent, use **Add project** to choose a folder, then use the plus button beside a project to create another thread in it.

I can rename or export each conversation and set instructions globally or per thread. Both modes stream replies, and the stop button interrupts the active turn.

In either composer, I can attach several files at once or drop them onto the message box. I can remove files individually before sending. Each message can include up to eight files totaling 12 MiB, with up to 1 MiB per UTF-8 text file and 5 MiB per image or PDF. Images and PDFs require a model and endpoint that accept them.

In **Model Chat**, I can edit a user message or retry a reply. Both actions create a branch using the history through the original prompt, with its saved attachments. I can also use **Branch from here** in either mode to continue through a selected message. I can return to the source under **Conversation settings → Open original conversation**. Code Agent branches use the selected project folder, and I can create a separate worktree for isolated changes.

## Connections

| Adapter | Default base URL | Authentication |
| --- | --- | --- |
| OpenAI API | `https://api.openai.com/v1` | OpenAI API key. Uses the Responses API. |
| ChatGPT Account | Installed `codex` executable | Browser sign-in through Codex app-server. Uses eligible Codex models. |
| Anthropic API | `https://api.anthropic.com/v1` | Anthropic Console key or Claude account sign-in. Uses the Messages API. |
| Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | Google AI Studio API key. Uses native Gemini streaming and function calling. |
| Ollama Local | `http://127.0.0.1:11434/api` | Local Ollama normally accepts unauthenticated requests. |
| Ollama Cloud | `https://ollama.com/api` | Ollama API key. |
| OpenAI-compatible | A service’s Chat Completions base URL | Optional bearer key and headers populated from environment variables. |

I can point an API adapter at a compatible gateway by editing its base URL. For example, LM Studio and many self-hosted inference servers expose Chat Completions. An endpoint with its own wire format needs a compatible gateway or a new adapter. Model discovery is optional, so I can still enter IDs when an endpoint provides inference only. Under **Advanced connection settings**, I can set the header that receives my saved API key. This lets me connect to gateways that expect a header such as `api-key`.

For Gemini, I can discover models that support content generation, add model IDs manually, and use them in either mode. Text replies, image and PDF inputs, and Code Agent tool calls use Gemini’s own API. OpenDex keeps signed tool-call parts in memory while a turn is running. [Gemini API reference](https://aistudio.google.com/api-reference/generate-content)

For a ChatGPT account, I install the [Codex CLI](https://learn.chatgpt.com/docs/cli), add the **ChatGPT Account** adapter, and click **Connect account**. The Codex executable opens the official browser flow and manages its own credentials. OpenDex uses a dedicated Codex home so the connection is independent of my terminal setup. The browser flow and subscribed inference are still part of my live verification checklist. [Codex app-server authentication](https://learn.chatgpt.com/docs/app-server#auth-endpoints)

I can use Anthropic two ways. A Console API key works with the standard Messages API. I can also select **Claude account** in the provider settings, save the connection, then use **Connect account** to open Anthropic’s sign-in page in my browser. That request asks only for Claude inference and profile access. OpenDex keeps the session token in the system credential store and refreshes it when it expires. To switch back to an API key, I first sign out of the account, then save my Console key. [Anthropic authentication policy](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)

## Code Agent and permissions

I choose a project folder for Code Agent to read, search, and edit. A Git worktree gives a thread a separate checkout and branch while keeping it under the original project in the sidebar. **Review changes** opens the worktree’s Git status and diff. I can run multiple conversations and use separate worktrees for concurrent agent turns on the same repository.

I start in **Workspace** mode. It allows project reads and asks before writes, commands, MCP calls, and web requests. **Read only** gives the agent project inspection. **Custom** lets me set each action to **Deny**, **Ask every time**, or **Allow**, choose network access for scoped commands, and set a tool-call limit. **Full access** runs commands with my user permissions and network access. MCP calls and web requests still require approval in that profile.

On macOS I run scoped commands through Seatbelt. They can read approved project content and system or installed toolchain resources. Project writes and command networking follow the selected profile. A command approval covers the command and its child processes. Project builds that need caches or credentials outside the workspace may need a different permission profile. I run trusted local MCP executables separately from the command sandbox, so enabling one lets it start with my app’s user privileges.

## MCP servers

In **Settings → MCP Servers**, I can add a local stdio command or a Streamable HTTP URL. Local servers take a JSON array of arguments and a list of environment variable names. HTTP servers can use a bearer key. I can set tool approvals for a whole server, then use **Test & list tools** to set rules for individual tools. The strictest rule among the permission profile, server, and tool applies to model calls. A denied tool is hidden from the model.

In **Conversation settings → MCP servers**, I can use all enabled servers or select specific servers for that conversation. An empty selection keeps MCP servers out of that conversation. Branches inherit the selection.

I use the **Add MCP context** button in either composer to browse a server enabled for the conversation. I can search for a resource, preview the text, and attach it alongside a local file. For a resource template I enter the filled-in URI. For a prompt I fill in its arguments, preview the result, then insert the text into my draft. When a prompt includes assistant examples, I see them labeled in the draft. The model receives the draft and any selected resources when I send the message.

I currently support initialized MCP sessions and the July 2026 protocol revision for tools, resources, and prompts. The MCP protocol selector is on each server. [MCP transport specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)

## Web search

In **Settings → Web search**, I can point OpenDex at a SearXNG search URL with JSON output enabled or choose a saved Ollama Cloud connection. Ollama Cloud also lets the model request a page’s main text. The search service is independent of the model endpoint, so I can use web tools with a local model that supports tool calling. [SearXNG search API](https://docs.searxng.org/dev/search_api.html), [Ollama web API](https://docs.ollama.com/capabilities/web-search)

For my self-hosted SearXNG instance, I enter its search URL, such as `https://search.example.org/search`, including any path used by my reverse proxy. **Test search** sends the query “OpenDex” and reports whether the server returned results. If the server returns HTTP 403, I check that `json` is enabled in `search.formats` and that the server allows access from this computer. [SearXNG search settings](https://docs.searxng.org/admin/settings/settings_search.html#formats)

I turn on **Web search** in the composer for each conversation where I want it. The approval prompt shows the query or page URL before OpenDex sends it. **Custom → Search and fetch web pages** controls when I’m asked. The conversation keeps the titles, URLs, and excerpts returned by the service, and I can open those sources in my browser.

## Image generation

I can click **Generate image** in either composer and choose a saved OpenAI API or OpenAI-compatible connection with an Images API. I enter the image model ID, prompt, and any size or quality settings my endpoint accepts. I can keep a different provider and model for the rest of the conversation. [OpenAI Images API](https://learn.chatgpt.com/api/reference/resources/images/methods/generate)

I see the generated image in the conversation and use **Save image** to export the original bytes. I can reopen the image prompt and generate another variation. Ordinary chat turns keep using the chat model I selected. I can attach a saved image if I want that model to analyze it.

OpenDex can read inline Base64 images and image URLs on the same origin as the configured API. For a compatible gateway that requires an explicit Base64 response setting, I use **Request Base64 output** in the image form.

## Appearance and data

I default to dark mode. Under **Appearance**, I can change the theme, accent, typeface, text scale, spacing, widths, corner radius, and animation preference.

The app saves project links, settings, conversations, and attachments in a local workspace file. On macOS, it lives under `~/Library/Application Support/org.opendex.desktop`. OpenDex uses the operating system credential store for API and MCP keys. I can move the data folder with `--data-dir` or `OPENDEX_DATA_DIR`. OpenDex allows one running copy per desktop user, including when I choose a different data folder. Messages, attachments, and approved tool output go to the currently selected model endpoint.

### Encrypted settings backup

In **Settings → Backups**, I can export my provider connections and pinned models, MCP and web search configuration, image preferences, permission profile, appearance, global instructions, and project links. The archive includes API keys and MCP bearer keys I saved in OpenDex. I protect it with a passphrase and keep that passphrase so I can restore the file later. The archive uses the [age file format](https://age-encryption.org/v1).

To restore, I open the backup and enter its passphrase. I review the connections and permission profile and can choose a new folder for each project before applying the settings. Restoration replaces the saved settings and puts the keys in the destination computer’s credential store. My existing conversations and project links stay in place. The backup carries project links without copying their files or conversation history.

My ChatGPT Account connection is included as a setting. I use **Connect account** on a new computer because Codex manages that session in its own credential store. I also set up any environment variables referenced by provider headers or local MCP servers on the new computer. [Codex credential storage](https://learn.chatgpt.com/docs/auth?surface=cli#credential-storage)

My Anthropic account session is included too. It restores with the saved Anthropic endpoint and account mode, so I can reconnect with my current access token. [Anthropic authentication policy](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)

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

I verified conversation-level MCP selection with two loopback servers in the macOS app. I declined and approved individual tool calls under a globally permissive profile, checked that a denied tool stayed out of the model request, and ran a Code Agent conversation with an empty selection.

I exported and restored an encrypted settings backup between two local data folders in the macOS app. I kept the destination conversation, remapped a project folder, and confirmed that the restored Keychain entries authenticated requests to a model fixture and an MCP fixture. Automated tests cover wrong passphrases, tampering, and changes to the file after preview.

I tested selecting several attachments in Model Chat, removing one, and verifying that the text and image reached a local model endpoint fixture. I edited that message after moving the source files and verified that the branch sent the saved copies. I also dragged a file from Finder into a Code Agent thread and confirmed that the endpoint received it.

I tested SearXNG searches and Ollama page retrieval in the macOS app with local fixtures. I verified that declining a search kept it from reaching the service, that the source link opened in my browser, and that the Read only profile kept web tools out of the model request.

I tested image generation in both modes with a local Images API fixture. I verified inline Base64 and same-origin URL results, exported a byte-for-byte copy, and confirmed that a regular follow-up still used the original text model. I also stopped a pending generation and reopened its prompt.

### Source layout

- `src/providers` contains inventory discovery, API payloads, and streaming decoders.
- `src/anthropic.rs` handles Anthropic account sign-in and session refresh.
- `src/state.rs` owns the conversation and settings schema, atomic persistence, and credential access.
- `src/backup.rs` owns encrypted settings export, restore preview, and credential restoration.
- `src/agent.rs` contains project file tools, command scoping, and Git operations.
- `src/mcp.rs` contains the JSON-RPC connection and tool operations.
- `src/web.rs` sends search and page requests to the configured service and normalizes the sources.
- `src/images.rs` handles image requests, raster input detection, and file export.
- `src/codex.rs` connects to the optional Codex app server.
- `src/desktop.rs` coordinates turns, approval prompts, and desktop commands.
- `ui` contains the desktop screens, appearance controls, and Markdown display.

## Preview scope

This is my first local preview. My priorities are provider freedom, usable MCP settings, project-based agent work, and transparent permissions. I still have work to do on voice, image editing, computer-use tools, automations, shared/cloud history, MCP subscriptions and media-rich prompts, and platform packaging. I plan to share the source after I’ve verified the workflows I use most often.

I’m making OpenDex available under the [MIT License](LICENSE).
