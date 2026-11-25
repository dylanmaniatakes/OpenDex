# Feature roadmap

Status of the seventeen requests from 2026-11-25. Each entry says what existed
before, what this change adds, and what the next concrete step is.

Legend: **Shipped** (works in this build), **Partial** (some of it works),
**Staged** (design + integration points, no code path yet), **Blocked** (an
external constraint, with the workaround).

---

## 1. Markdown files as quick reference

**Shipped (first pass).** New `src/context.rs`.

On every Agent or relay turn, OpenDex surveys the project for the markdown a
human would read first:

- `AGENTS.md`, `OPENDEX.md`, `CLAUDE.md` are inlined (4,000 chars each) because
  they are instruction files.
- `README.md`, `CONTRIBUTING.md`, and every `docs/*.md` are reduced to their
  heading plus first line of prose.
- At most 10 files, 12,000 chars total, 512 KB per source file. The block is
  wrapped in `<project_reference>` and injected before the conversation. The
  model is told it is a map and must `read_file` before relying on details.
- Wrapper tags inside a document are escaped so a project file cannot close the
  block early.

Next: a Thread menu entry to preview these docs in the app, nested `docs/**`
support, an opt-out per conversation, and honouring `.gitignore` when scanning
subfolders.

## 2. GitHub Copilot as a native provider

**Staged.** Copilot's chat endpoint needs a short-lived Copilot token minted
from a GitHub OAuth token (device flow), plus editor-version headers. Doing it
half-way would leave a provider that authenticates but cannot call models.

Plan, in order:

1. Add `ProviderKind::Copilot` with endpoint `https://api.githubcopilot.com`.
2. New `src/copilot.rs`: device-flow (`/login/device/code` →
   `/login/oauth/access_token`), poll, store the GitHub token in the keyring
   under the provider id (the existing `credential(id, target)` helper).
3. Exchange the GitHub token for a Copilot token (`/copilot_internal/v2/token`)
   and cache it until its `expires_at`; refresh on 401.
4. Send `Editor-Version`, `Copilot-Integration-Id` headers and reuse the
   OpenAI-compatible wire format already in `src/providers/wire.rs`.
5. Expose model discovery from `/models`; the account card mirrors the existing
   "Anthropic account" UI.

Decision needed: is using an unofficial endpoint acceptable, given it is
outside GitHub's published API terms? The alternative is a proxy the user
controls.

## 3. Voice OAuth for OpenAI

**Blocked by provider policy.** OpenAI's `/v1/audio/speech` accepts API keys
and workload-identity tokens; ChatGPT account OAuth tokens are not accepted by
the audio API. The ChatGPT sign-in already in OpenDex (`ChatGptAccount` →
Codex bridge) covers text models only and cannot mint a speech token.

Real OAuth options for speech:

- **Azure OpenAI** supports Microsoft Entra ID bearer tokens on the same audio
  endpoints. That is a genuine OAuth path and would slot into `voice.rs` as one
  more `Kind` with an account-auth mode, mirroring `AnthropicAuth`.
- Keep the existing API-key path for direct OpenAI.

Recommendation: add Azure OpenAI + Entra ID if OAuth for voice is a hard
requirement; do not fake an OpenAI OAuth flow that the endpoint will reject.

## 4. More customization

**Partial + shipped.** The existing Appearance page already covered theme,
accent, text scale, density, conversation width, sidebar width, radius, and
motion. Added:

- Six accent swatches next to the colour picker.
- Five more typefaces (see item 5).
- A desktop-notification preference.

Next ideas: per-mode accent, message bubble style (plain / cards), thread list
density, and an exportable theme file. Custom CSS is intentionally not offered:
the window renders model output, so a stylesheet is a script-injection surface.

## 5. More fonts / custom font upload

**Shipped.** `Appearance` gained `custom_font: Option<CustomFont>` and five
CSS font stacks: humanist, geometric, rounded, slab, and custom.

- `import_font` copies a `.ttf/.otf/.woff/.woff2` (max 8 MB) into
  `<app data>/fonts/<sha256>.<ext>` and returns a display name.
- `font_data` returns a base64 `data:` URL, so the CSP only needs
  `font-src 'self' data:` and never a `file://` origin.
- The settings page imports, previews, and removes a font; choosing "Custom"
  activates it. Filenames are validated against traversal, and only the file
  name is stored in `workspace.json`.

Next: read the font's name table for a proper family name (currently the file
stem), keep more than one custom font, and preview before saving.

## 6. "Always allow" for commands

**Shipped.** Approval cards gain **Always allow** when the tool has a
category-wide rule. `resolve_approval(id, allow, always)` persists
`agent::always_allow(...)`, which switches the permission profile to **Custom**
and sets that one category (read, write, or command) to Allow. Deny and Allow
once behave as before.

Deliberately not offered for MCP or web calls: those have no single rule to
flip, and for remote VPS approvals the decision belongs to the server's policy.
`src/agent.rs` tests cover the upgrade and the unrelated categories staying put.

Next: scope the upgrade to a command pattern or to one project instead of the
whole category, and show the resulting profile in the approval card.

## 7. Built-in memory

**Shipped (project scope).** New `src/memory.rs`.

- Notes live in `.opendex/memory.md` at the workspace root, visible and
  editable by the user and versionable with the project.
- The file is injected as `<project_memory>` (8 KB prompt budget, 64 KB hard
  cap) into local and relay turns.
- The `remember` tool appends one bullet (2,000-byte note cap), gated by the
  write permission. The file is refused if it is a symlink or the folder is a
  symlink.
- Settings → Instructions has an editor for the current conversation's
  project memory (`thread_memory` / `save_thread_memory`).

Next: a global (cross-project) memory file, automatic summarisation when the
file fills, and searching memory instead of inlining all of it.

## 8. Third-party tool / integration support

**Exists today via MCP**, and the groundwork is the existing stack: stdio and
Streamable HTTP servers, per-tool allow/deny, prompts, resources, and the
workspace bridge for Hermes. `src/mcp.rs` is the integration point.

Missing: a one-click catalog. Concrete next step is a preset row in the MCP
settings page (Filesystem, GitHub, Playwright, Memory, Computer Use) that fills
the existing `serverForm` with a name, command, args, and env placeholder.
OAuth-protected MCP servers would need a small token broker; the keyring helper
already exists.

## 9. More search engines (open and API-key)

**Shipped.** `src/web.rs` now supports four services:

- **SearXNG** (self-hosted/open) — already present.
- **Ollama Cloud** — already present, also the only page-fetch backend.
- **Brave Search** — `GET api.search.brave.com/res/v1/web/search`,
  `X-Subscription-Token`.
- **Tavily** — `POST api.tavily.com/search`, bearer auth.

API keys are stored in the OS credential store keyed by the config's UUID
(`read_secret(id, "web-search", ...)`), deleted when the service is replaced,
and never serialised into `workspace.json`. Parsing is covered by unit tests
for both result shapes, duplicate URLs, and non-HTTP results.

Next: Exa, Google Programmable Search, Kagi; page fetch for Brave/Tavily;
per-engine result limits and freshness options.

## 10. Custom instruction presets

**Shipped.** `InstructionPreset { id, name, instructions }` in `src/state.rs`
with validation (60-char unique names, 32,000-char body, 40 presets max).

- `save_presets` replaces the list atomically and returns the validated copy.
- Settings → Instructions has add/edit/remove/apply.
- Applying copies the text onto the thread, so editing a preset later does not
  silently change a running conversation.
- The composer tools menu lists the presets for one-click application.

Plan first and Goal mode are built-in menu actions implemented as the same
instruction mechanism (item 17). Next: per-mode presets and import/export.

## 11. API for third-party / companion apps

**Staged (design only).** The app already depends on `hyper` for the workspace
bridge, so a second listener is cheap; the risk is authority, not code.

Proposed minimal API (opt-in, off by default):

- Bind `127.0.0.1` only, random port written to `<app data>/api.json`, bearer
  token in the keyring. Pairing screen shows a QR/URL, never logs the token.
- `GET /v1/status` → provider/model/thread summary.
- `GET /v1/threads?since=` and `GET /v1/threads/{id}/messages`.
- `POST /v1/threads/{id}/messages` → same path as `send_message`, streamed as
  SSE events (`accepted`, `delta`, `tool`, `approval`, `checkpoint`).
- `POST /v1/approvals/{id}` → allow once/always/deny, reusing
  `resolve_approval`.
- Hard limits: one in-flight turn per thread, 1 MB bodies, loopback only,
  constant-time token compare.

Not implemented: it needs a decision on whether the token is per-device and
revocable, and how approvals from a remote device should be shown locally.

## 12. Computer use

**Staged; possible today through MCP.** There is no built-in screenshot/mouse
tool. Adding an MCP server such as Playwright (browser) or a desktop-automation
server gives the model those tools through the existing permission gate.

Gaps: MCP approval is per tool, not per action, so "click anywhere" is one
allow decision; screenshots would enter the conversation as MCP results and
need redaction policy. Next: ship the MCP preset, add a "computer use" tool
category with its own rule, and a screen-capture preview before the first call.

## 13. Secure credential store for remote execution

**Partial.** Provider, MCP, voice, and now search keys already live in the
macOS Keychain / Windows Credential Manager / Secret Service through `keyring`;
the model never receives them. Missing is a *tool* that injects a secret into a
command without revealing it.

Design: Settings → Secrets stores named entries (e.g. `deploy-prod`). A
`run_with_secret` tool accepts only a secret name, program, and args; OpenDex
resolves the value at exec time, passes it as an environment variable, and
redacts the value from stdout/stderr before returning the result. Approval card
shows the name, never the value. This maps directly onto `agent::execute`'s
command path and `state::read_secret`.

## 14. Video generation

**Staged.** Images already work through OpenAI-compatible
`/images/generations` and ComfyUI workflows (`src/images.rs`,
`src/images/comfy.rs`). Video needs async jobs, which images do not.

Plan:

- `video::Config { provider, model, size, seconds, source_image }`, same
  resolution rules as `images::Config`.
- Two backends: ComfyUI workflow (Wan/LTX templates in `src/images/comfy.rs`
  style) and an HTTP job API (OpenAI `/v1/videos` create + poll; Runway/Luma
  follow the same shape).
- A `Job` trait with `submit`, `poll`, `cancel`; progress events on the existing
  turn event channel; output saved as an attachment with a `video/*` MIME and
  rendered with `<video controls>`.
- Caps: 512 MB, 10 minutes, one job at a time, cancel on turn stop.

## 15. Popup menu for plan/goal/image/video

**Shipped.** The composer now has one **+** menu holding: Plan first, Goal
mode, saved instruction presets, Web search toggle, Generate image, Generate
video (disabled, "Soon"), and Add MCP context. Attach and dictation stay on the
bar. The menu closes on outside click, Escape, or after an action, and each
item reports its state (web On/Off, video Soon).

Next: keyboard shortcuts and a "recent" section once video lands.

## 16. Desktop notifications

**Shipped.** `tauri-plugin-notification` plus a small `notify` command. OpenDex
notifies when a reply finishes while the window is in the background, and when
an approval is needed in another thread. The toggle is in Appearance →
Desktop notification; macOS asks for permission on the first notification.

Next: separate toggles for replies, approvals, and errors; click-to-focus;
a sound preference.

## 17. Plan mode and Goal mode

**Shipped as instructions, not new modes.** Adding `Mode::Plan`/`Mode::Goal`
would touch tool gating, handoff, routing, and every `match mode`. Instead the
menu writes a thread-instruction preset:

- **Plan first** asks for a plan and an explicit stop before editing.
- **Goal mode** asks for the goal, completion criteria, and a closing report.

Both toggle off on a second click. Next, if a first-class flow is wanted:
`Mode::Plan` with tools disabled except read, an "Approve plan" action that
switches the thread to Agent, and a checklist UI for goal criteria.

---

## Verification for this change

- `cargo test --no-default-features --lib` — parser, context, memory, presets,
  web engines, permissions.
- `cargo test --all-targets` — includes the desktop feature and MCP bridge.
- `cargo fmt --all -- --check`.
- UI files pass `node --check`; no browser run on the VPS.

## Suggested order for the next session

1. Copilot device-flow provider (item 2) — highest user-visible gap.
2. `run_with_secret` (item 13) — makes remote work safe without sharing keys.
3. MCP preset catalog (item 8) and computer-use category (item 12).
4. Local API + companion pairing (item 11) — needs a product decision first.
5. Video jobs (item 14) — biggest single feature; start with ComfyUI only.
