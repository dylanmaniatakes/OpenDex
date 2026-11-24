# OpenDex

I built OpenDex to use the models I choose for everyday chat and coding. It runs on my Mac without an OpenDex account. I can connect local servers and cloud APIs at the same time, keep a short model list for each connection, and switch providers from the composer.

## Try it on macOS

1. Open `dist/OpenDex.app` if you have the local preview bundle. To build it yourself, follow the instructions below.
2. In **Model Chat**, choose **Find local models** if you already run Ollama. Pick an installed model to add it to the composer.
3. To connect another service, open **Settings → Providers → Add provider**. Choose an adapter, enter the base URL and key if the endpoint requires one, and save.
4. Use **Discover models** to browse an endpoint’s inventory, or paste model IDs one per line and choose **Add models**. Duplicate IDs are removed. Each model I add appears in both modes. For Ollama Cloud I use the Ollama API key and can submit a query to the public model database, then add only the model IDs I want. I can also enter IDs manually.
5. Use the sidebar dropdown to switch between **Model Chat** and **Code Agent**. Model Chat shows its conversations. In Code Agent, use **Add project** to choose a folder, then use the plus button beside a project to create another thread in it.

I can search saved conversation titles, message text, project names, attachment names, MCP resource labels, selected providers and models, and tool names from the sidebar. In the composer, I can filter provider and model lists by name so I can find the connection I want among several. Search stays on my computer and filters the selected mode. In Code Agent, matching project groups expand while I search. Escape clears the query.

I can rename or export each conversation and set instructions globally or per thread. Export includes provider and model, thread instructions, tool outputs, saved attachment metadata, and MCP context labels while keeping attachment contents private. Both modes stream replies, and the stop button interrupts the active turn.

In either composer, I can attach several files at once or drop them onto the message box. I can remove files individually before sending. Each message can include up to eight files totaling 12 MiB, with up to 1 MiB per UTF-8 text file and 5 MiB per image or PDF. Images and PDFs require a model and endpoint that accept them.

In **Model Chat**, I can edit a user message or retry a reply. Both actions create a branch using the history through the original prompt, with its saved attachments. I can also use **Branch from here** in either mode to continue through a selected message. I can return to the source under **Conversation settings → Open original conversation**. Code Agent branches use the selected project folder, and I can create a separate worktree for isolated changes.

I run one OpenDex at a time. The app locks its data folder, so a second launch exits quietly on other systems, and on macOS it brings the running copy to the front instead of opening more windows.

## Connections

| Adapter | Default base URL | Authentication |
| --- | --- | --- |
| OpenAI API | `https://api.openai.com/v1` | OpenAI API key. Uses the Responses API. |
| ChatGPT Account | Installed `codex` executable | Browser sign-in through Codex app-server. Uses eligible Codex models. |
| Anthropic API | `https://api.anthropic.com/v1` | Console key through the Messages API, or user sign-in through installed Claude Code. Live account verification is pending. |
| Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | Google AI Studio API key. Uses native Gemini streaming and function calling. |
| Ollama Local | `http://127.0.0.1:11434/api` | Local Ollama normally accepts unauthenticated requests. |
| Ollama Cloud | `https://ollama.com/api` | Ollama API key. |
| OpenAI-compatible | A service’s Chat Completions base URL | Optional bearer key and headers populated from environment variables. |
| OpenAI-compatible model search | Composer filter | Filters the provider and model lists by name. |
| Ollama Cloud model search | Public Ollama search | Searches only official library result cards; Ollama API key is optional for search. |

For an Ollama Cloud connection, **Search Ollama** queries the public model database and shows its result IDs. Search responses are capped, and only official `library/{model}` result links are accepted. I click the ID to pin it, then use manual entry when Ollama search misses a private or custom model. The Cloud tag fetch remains available when I prefer to inspect that provider list. For example, LM Studio and many self-hosted inference servers expose Chat Completions. An endpoint with its own wire format needs a compatible gateway or a new adapter. Model discovery is optional, so I can still enter IDs when an endpoint provides inference only. Under **Advanced connection settings**, I can set the header that receives my saved API key. This lets me connect to gateways that expect a header such as `api-key`.

For Gemini, I can discover models that support content generation, add model IDs manually, and use them in either mode. Text replies, image and PDF inputs, and Code Agent tool calls use Gemini’s own API. OpenDex keeps signed tool-call parts in memory while a turn is running. [Gemini API reference](https://aistudio.google.com/api-reference/generate-content)

For a ChatGPT account, I install the [Codex CLI](https://learn.chatgpt.com/docs/cli), add the **ChatGPT Account** adapter, and click **Connect account**. The Codex executable opens the official browser flow and manages its own credentials. OpenDex uses a dedicated Codex home so the connection is independent of my terminal setup. The browser flow and subscribed inference are still part of my live verification checklist. [Codex app-server authentication](https://learn.chatgpt.com/docs/app-server#auth-endpoints)

I use an Anthropic Console API key for Claude through the Messages API. For an account connection, I choose **Claude account** under **Anthropic access**, set the installed Claude Code executable path if needed, save, and click **Connect account**. The unmodified CLI opens its own browser sign-in and manages its credentials in a dedicated configuration for the connection. Model discovery and both conversation modes run through that executable. OpenDex's virtual MCP server routes tools through the same permission and persistence layer used by other providers. Native CLI tools, hooks, project settings, and external MCP discovery are disabled. [Anthropic authentication policy](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)

I removed the direct OAuth prototype. If I saved credentials with that prototype, I use **Sign out** to delete them and reconnect through Claude Code. Live browser sign-in and subscribed inference still need verification. Claude Code account sessions stay under the CLI's control and require sign-in again after moving to another computer.

## Code Agent and permissions

I choose a project folder for Code Agent to read, search, and edit. A Git worktree gives a thread a separate checkout and branch while keeping it under the original project in the sidebar. **Review changes** opens the worktree’s Git status and diff. I can run multiple conversations and use separate worktrees for concurrent agent turns on the same repository.

I start in **Workspace** mode. It allows project reads and asks before writes, commands, MCP calls, and web requests. **Read only** gives the agent project inspection. **Custom** lets me set each action to **Deny**, **Ask every time**, or **Allow**, choose network access for scoped commands, and set a tool-call limit. **Full access** runs commands with my user permissions and network access. MCP calls and web requests still require approval in that profile. When a CLI-backed account turn reaches its limit before a later tool request, OpenDex stops that turn because it has already exposed the tool set. I tested this with a one-call limit: the project read completed, the queued write was rejected, and no approval prompt appeared.

On macOS I run scoped commands through Seatbelt. They can read approved project content and system or installed toolchain resources. Project writes and command networking follow the selected profile. A command approval covers the command and its child processes. Project builds that need caches or credentials outside the workspace may need a different permission profile. I run trusted local MCP executables separately from the command sandbox, so enabling one lets it start with my app’s user privileges.

## MCP servers

In **Settings → MCP Servers**, I can add a local stdio command or a Streamable HTTP URL. Local servers take a JSON array of arguments and a list of environment entries. I can enter `NAME` to pass that variable from the OpenDex process environment, or `NAME=VALUE` to give the server a fixed value, which is how I bring servers over from a Codex `config.toml` with their custom environment intact. HTTP servers can use a bearer key and headers read from environment variables. I set each custom HTTP header as `HEADER_NAME=ENVIRONMENT_VARIABLE`, so I can add a gateway organization or routing header without storing another secret in OpenDex. I can set tool approvals for a whole server, then use **Test & list tools** to set rules for individual tools. The strictest rule among the permission profile, server, and tool applies to model calls. A denied tool is hidden from the model.

In **Conversation settings → MCP servers**, I can use all enabled servers or select specific servers for that conversation. An empty selection keeps MCP servers out of that conversation. Branches inherit the selection.

I use the **Add MCP context** button in either composer to browse a server enabled for the conversation. I can search for a resource, preview the text, and attach it alongside a local file. For a resource template I enter the filled-in URI. For a prompt I fill in its arguments, preview the result, then insert the text into my draft. When a prompt includes assistant examples, I see them labeled in the draft. The model receives the draft and any selected resources when I send the message.

I currently support initialized MCP sessions and the July 2026 protocol revision for tools, resources, and prompts. The MCP protocol selector is on each server. [MCP transport specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)

## Web search

In **Settings → Web search**, I can point OpenDex at a SearXNG search URL with JSON output enabled or choose a saved Ollama Cloud connection. Ollama Cloud also lets the model request a page’s main text. The search service is independent of the model endpoint, so I can use web tools with a local model that supports tool calling. [SearXNG search API](https://docs.searxng.org/dev/search_api.html), [Ollama web API](https://docs.ollama.com/capabilities/web-search)

For my self-hosted SearXNG instance, I enter its search URL, such as `https://search.example.org/search`, including any path used by my reverse proxy. **Test search** sends the query “OpenDex” and reports whether the server returned results. If the server returns HTTP 403, I check that `json` is enabled in `search.formats` and that the server allows access from this computer. [SearXNG search settings](https://docs.searxng.org/admin/settings/settings_search.html#formats)

If my instance returns HTTP 401, I check the reverse proxy’s authentication. For HTTP 429, I wait before retrying or check the instance’s limiter settings. A redirect means I need to enter the final search URL, and OpenDex shows the server’s `Location` header so I can copy the exact path. If a proxy returns an HTML login page, OpenDex reports that the response was not JSON.

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

My ChatGPT Account connection and its direct-Keychain session are included when OpenDex finds that session in its dedicated Codex home. The restore preview tells me whether the archive contains a session. Restore writes it to the destination home’s credential entry and rolls back to the previous session if saving settings fails. An expired or revoked session may still require **Connect account**. This currently covers Codex’s direct keyring backend on macOS and Linux. When I restore an archive on Windows, the other settings and keys restore and the preview tells me to sign in to ChatGPT again. Codex’s encrypted secrets-file backend also requires signing in again. I also preserve provider and MCP header names with their environment-variable names, so I only need to recreate the referenced environment variables on the new computer. [Codex credential storage implementation](https://github.com/openai/codex/blob/rust-v0.151.0/codex-rs/login/src/auth/storage.rs), [Codex storage backend defaults](https://github.com/openai/codex/blob/rust-v0.151.0/codex-rs/config/src/types.rs)

The archive preserves any legacy Anthropic session bytes from the removed prototype so I can restore old archives and then remove those credentials using **Sign out**. They are never used for API requests. New Claude Code account credentials stay with the CLI and are not exported. The executable path and selected models are included in provider settings.

## Build from source

I use Rust for the provider adapters, state, MCP, and permission checks. Tauri provides the desktop window and native dialogs, with a small HTML/CSS/JavaScript frontend and no Node build step. The workspace supports **Rust 1.95** and uses the checked-in dependency lockfile.

The core provider, state, MCP, and credential logic builds without the desktop feature, so I keep that check in my normal test pass. `scripts/check.sh` runs the frontend asset test, Rust formatting, Clippy, the desktop test suite, and the no-desktop library test suite in one command.

```sh
cargo run --locked
bash scripts/bundle-macos.sh
```

The bundling script writes `dist/OpenDex.app`, applies a local ad-hoc signature, and creates `dist/OpenDex-build.json`. That manifest records the app version, source commit, build profile, host architecture, timestamp, toolchain, ad-hoc signing state, bundle identifier, CDHash, and executable SHA-256. I use it to match a bundle back to the source I tested. `scripts/verify-release.py` compares the built app with that manifest and current configuration. Finder can launch the preview on the Mac that built it. Developer ID signing and notarization are release tasks. The Rust and Tauri code leave room for Windows and Linux builds. I have tested the desktop on Apple Silicon macOS and scoped command isolation is implemented for macOS.

```sh
bash scripts/release.sh

# Optional installed-CLI checks. The Codex keyring check also uses disposable
# native-Keychain credentials and deletes them afterward.
cargo test --locked --test provider_transports installed_codex_server -- --ignored
cargo test --locked --test provider_transports installed_codex_keyring_format -- --ignored
cargo test --locked --test claude_transport installed_claude -- --ignored
```

I use loopback fixtures to test each wire format and MCP transport without account credentials. I verified Gemini model discovery, streaming chat, and a Code Agent file read through the desktop app against a native-protocol fixture. For release verification I run the normal checks, rebuild the macOS bundle, then use `python3 scripts/verify-release.py`. The optional Codex check exercises the installed CLI with an isolated data directory. I tested live local Ollama replies in Model Chat, including edited and retried branches, a live Code Agent file read, and the public Ollama Cloud catalog from the macOS GUI. I also switched between modes, created a thread under one of two test projects, approved a file edit and an MCP call in a disposable project, and sent a rendered MCP prompt with a text resource to a local Ollama model.

I verified that the macOS release starts with the sidebar search module packaged, that a nonmatching search hides the saved conversation, and that Escape restores it. In an isolated macOS workspace with two projects, I collapsed one project, searched for text in its saved thread, and verified that search revealed that thread and hid the other project. Clearing the query restored both projects and the original collapsed state. The frontend asset test checks that every local JavaScript import is listed in the desktop bundle configuration.

I verified conversation-level MCP selection with two loopback servers in the macOS app. I declined and approved individual tool calls under a globally permissive profile, checked that a denied tool stayed out of the model request, and ran a Code Agent conversation with an empty selection.

I exported and restored an encrypted settings backup between two local data folders in the macOS app. I kept the destination conversation, remapped a project folder, and confirmed that the restored Keychain entries authenticated requests to a model fixture and an MCP fixture. Automated tests cover wrong passphrases, tampering, and changes to the file after preview. The backup integration test uses an in-memory credential store to verify that API keys, MCP bearer keys, and Anthropic access and refresh tokens restore into new credential slots. It also checks that the encrypted file contains none of those test values in plaintext. The same test transfers a dummy ChatGPT session between separate Codex homes, loads archives without the session field, and forces a settings-write failure to verify restoration of the previous destination session. I also simulate an unsupported session backend to check that settings still restore and that the preview and completion notices request a fresh ChatGPT sign-in. I have not run this path on Windows. A real ChatGPT session has not yet been transferred between computers. I also tested Codex 0.151.0 against the native macOS Keychain with disposable API-key credentials. Codex wrote a login in one temporary home, OpenDex’s credential helper read it, and Codex recognized the copied login in a second temporary home. This checks the direct-Keychain format and destination entry naming. The optional test deletes its disposable entries afterward.

I tested selecting several attachments in Model Chat, removing one, and verifying that the text and image reached a local model endpoint fixture. I edited that message after moving the source files and verified that the branch sent the saved copies. I also dragged a file from Finder into a Code Agent thread and confirmed that the endpoint received it.

I tested SearXNG searches and Ollama page retrieval in the macOS app with local fixtures. I also used the Ollama Cloud search in the macOS app with the term “gemma”, confirmed it returned the model ID list, pinned `gemma`, and verified that the selected model persisted in the provider settings. I verified that declining a search kept it from reaching the service, that the source link opened in my browser, and that the Read only profile kept web tools out of the model request.

I tested image generation in both modes with a local Images API fixture. I verified inline Base64 and same-origin URL results, exported a byte-for-byte copy, and confirmed that a regular follow-up still used the original text model. I also stopped a pending generation and reopened its prompt.

I tested the new Claude Code transport against the installed executable without sending an inference prompt or reading subscription credentials. It returned its model list from an isolated configuration folder. Fixtures cover startup arguments, rejection of unexpected native tool requests during startup and generation, unrelated control responses, and malformed or failed initialization. A conversation fixture checks saved context, image and PDF blocks, streamed text without duplicate final output, hidden-tool rejection, and delivery of a denied tool result back to Claude. The installed CLI also initialized with the virtual OpenDex MCP server configured. The isolated installed CLI also reported that it was signed out. Account-action fixtures cover login, status, logout, and a logged-out status response with a nonzero exit code. A regression check rejects direct HTTP authentication for account-mode providers. I also ran the macOS desktop against a local Claude CLI fixture. I verified a Model Chat reply, fixture account connect and sign-out, model discovery and persistence of a second selected model, and a Code Agent project read. I denied a file creation and confirmed the file stayed absent, then allowed it on a later turn and checked the exact saved bytes and conversation results. These desktop checks exercise the CLI bridge and OpenDex permissions using a fixture.

After those fixture checks I connected live accounts in the macOS app. For Anthropic I signed in to my Claude Pro account in Settings, discovered its model list, pinned default, sonnet[1m], opus, and haiku through Discover models and manual entry, and completed a paid Model Chat turn whose saved reply records that provider and model. For ChatGPT I linked my account in Settings, confirmed the subscribed model list, and completed live turns on gpt-5.6-sol in both Model Chat and Code Agent, with the provider and model saved on every reply.

### Source layout

- `src/providers` contains inventory discovery, API payloads, and streaming decoders.
- `src/state.rs` owns the conversation and settings schema, atomic persistence, and credential access.
- `src/backup.rs` owns encrypted settings export, restore preview, and credential restoration.
- `src/agent.rs` contains project file tools, command scoping, and Git operations.
- `src/mcp.rs` contains the JSON-RPC connection and tool operations.
- `src/web.rs` sends search and page requests to the configured service and normalizes the sources.
- `src/images.rs` handles image requests, raster input detection, and file export.
- `scripts/build-manifest.py` writes the bundle build manifest.
- `src/codex.rs` connects to the optional Codex app server.
- `src/claude.rs` handles Claude Code account actions, streaming, and virtual MCP tools.
- `src/desktop.rs` coordinates turns, approval prompts, and desktop commands.
- `ui` contains the desktop screens, appearance controls, and Markdown display.

## Preview scope

This is my first local preview. My priorities are provider freedom, usable MCP settings, project-based agent work, and transparent permissions. I still have work to do on voice, image editing, computer-use tools, automations, shared/cloud history, MCP subscriptions and media-rich prompts, and platform packaging. I plan to share the source after I’ve verified the workflows I use most often.

I’m making OpenDex available under the [MIT License](LICENSE).
