# Spec — Attach a File for Tools (vision · audio transcription)

Status: **P1 shipped** (MVP) · P2/P3 pending · Depends on: the MCP-client
feature (`sovereign_tools::mcp`, `SetupConfig.mcp_servers`, `sovereign mcp
demo-server`).

> **P1 landed** — `AttachedFile`/`build_tool_files_preamble` +
> `attached_files` on `send_message_stream`/`send_message`
> (`commands/chat.rs`); `attachToolFile` + chips in `ChatView.svelte`;
> `attachedFiles` on the `sendMessageStream`/`sendMessage` api wrappers; a
> `read_memo(path)` reference tool + e2e on the demo server. No Runtime,
> `ToolContext`, or document-RAG changes — exactly as scoped below. P2
> (`ToolContext.attached_files` + the `7693f16b` fold-in) and P3 (polish)
> remain.

## Context

The MCP-client wiring lets the assistant call external HTTP MCP servers, and a
vision/transcription capability rides behind such a server as a **text-in /
text-out** tool — `describe_image(path)`, `ocr(path)`, `transcribe_audio(path)`
— so the local model never has to be multimodal. The one thing those tools need
is a **filesystem path** the server can read.

Tracing the desktop attach flow showed it can't supply one. Attaching goes
through `upload_document_asset(file_path)` → `DocumentAssetManager::prepare` →
text-extract → chunk → embed; what survives is a `DocumentAsset`
(`sovereign-core/src/types/document.rs:21`) with `id`, `filename` (name only),
`word_count`, `chunk_count`, `index_id`, a skeleton — **no path, no handle to
the raw bytes** (`source_key()` = `asset:{id}`). It is a text-RAG pipeline: for
an image it would OCR-or-fail and discard the file. The absolute path *is*
captured at the door (`upload_document_asset`'s `file_path`) and thrown away
only because the document pipeline doesn't need it.

This spec adds a small, **parallel** affordance — "attach a file for a tool" —
that binds a dropped file's path to the turn and surfaces it to the model, so it
passes the path to a (local) MCP tool. No RAG, no asset store, and (for the MVP)
no change to the Runtime.

## Goals / Non-goals

**Goals**
- Drag/drop or pick an **image or audio** file in chat; the assistant can
  describe / OCR / transcribe it via a registered MCP tool.
- Reuse the MCP feature unchanged — the tool, the descriptor-enrichment
  (synthesized example so the planner calls it), the `Permission::Network`
  approval gate.
- Keep the Runtime untouched for the MVP by riding the existing
  augmented-message rail.

**Non-goals**
- RAG ingestion of the file — the existing document-attach path is unchanged.
- Media **out** (image generation, TTS) — blocked by the adapter's text-only
  result extraction (`mcp/client.rs::call_tool` keeps only `type=="text"`
  content); a separate effort.
- Sending file **content** to a remote server — the path only helps a *local*
  server. Remote file transfer (upload / served URL) is out of scope.
- A multimodal `CompletionRequest` — the model stays text-only; the tool does
  the modality work.

## Design

### Data model
A turn-scoped list, supplied by the frontend, not persisted as an asset:

```rust
// MVP: desktop-local, mirroring `FocusedChunkRef` in commands/chat.rs.
#[derive(serde::Deserialize)]
pub struct AttachedFile { pub path: String, pub name: String, pub kind: FileKind }
pub enum FileKind { Image, Audio, Other }   // from extension
```

`kind` drives the prompt hint and the attach-routing; it is not authoritative
(the tool's own schema validates the real argument).

### The rail (MVP): augmented-message prepend — no Runtime change
`send_message_stream` already prepends a labelled context block for
`context_chunks` **before** `handle_message_stream` sees the message
(`commands/chat.rs:58-93`, "keeping the runtime untouched"). Add the symmetric
`attached_files` param and a `build_tool_files_preamble` that prepends a
kind-aware block:

```
▸ attached file: standup.m4a  (audio)
  path: /home/u/recordings/standup.m4a
  To work with this file, call a tool with its path (e.g. transcribe it).
```

Flow, reusing everything already built:
1. The path + kind hint land in the (augmented) user message.
2. The router's tool-relevance gate matches the user's intent ("transcribe…")
   to the registered `mcp_<server>_transcribe_audio` (visible in
   `router.tool_gate`).
3. The planner — using the example we synthesize from the tool's input schema —
   emits `transcribe_audio(path="/home/u/recordings/standup.m4a")`.
4. The executor's Network gate prompts (desktop) / add-time trust (CLI); the
   local server reads the file and returns text; synthesis answers.

This is *exactly* the path a user-typed path already takes — the preamble just
removes the "paste the path yourself" step.

### Frontend: route the attach by kind
Today attach → DocumentPicker → document-RAG. Split on file kind:
- **Image / audio → tool-file.** Capture the absolute path (the Tauri drop
  event and the dialog plugin both yield OS paths — `upload_document_asset`
  already proves a path is available), add to the turn's `attached_files`, show
  a composer chip ("📎 standup.m4a — passed to a tool"). No ingestion.
- **Text / PDF / other → existing document flow**, unchanged.

Kind-based default is the fluent MVP (an image is obviously not a RAG document).
An explicit "attach as document vs for a tool" override is a later affordance.

### Surfacing tiers
- **Tier 1 (MVP)** — the augmented-message preamble above. The model relays the
  path; no backend-runtime change. As reliable as a typed path.
- **Tier 2 (hardening)** — `ToolContext.attached_files: Vec<String>`, threaded
  through the executor's `ToolContext` construction so a tool can read the
  attached paths deterministically instead of trusting the model to relay them.
  This is also the structured home for the `attached_doc` asset-id TODO
  (`AttachedDocumentSearchTool`, decision `7693f16b`): both answer "what
  files/assets is this turn bound to?" — resolve them together.

### Local-server requirement + privacy
The path is only meaningful to a server that can read it — a **local** MCP
server, which is also the privacy-aligned case (bytes never leave the machine).
- A path string surfaced to a *remote* tool would leak the path (not the
  content). MVP documents "attach-for-tools assumes a local tool server."
  Optional hardening: suppress path injection when only non-loopback servers are
  configured, or warn in the chip.
- Two consent points already exist before any call: the user's explicit attach
  and the executor's Network approval.

### Path stability
MVP passes the **original** path (the file sits where it was dropped; the tool
reads it immediately) — avoids copying large audio memos. Hardening option: copy
to `~/.sovereign/tool-files/<turn>/<name>` for a stable, controllable path (and
a future served-URL for remote servers) — deferred.

## Implementation (phased)

**P1 — MVP (vision + audio end-to-end, no Runtime change), all in the desktop crate**
- `AttachedFile` + `FileKind` (desktop-local, like `FocusedChunkRef`).
- `attached_files: Option<Vec<AttachedFile>>` on `send_message_stream` (and
  `send_message` for parity) + `build_tool_files_preamble`.
- `ChatView`: kind-based attach routing, the per-turn `attached_files` state,
  the composer chip; `api.ts` wrapper passes it through.

**P2 — Deterministic hardening**
- `ToolContext.attached_files: Vec<String>`; thread through the executor's
  `ToolContext` construction sites; fold in `7693f16b` (asset-id) as the sibling
  field.

**P3 — Polish**
- Multi-file; the document-vs-tool override; optional temp-copy for path
  stability; remote-server path-suppression/warn.

## Verification

- **Reference tool** — extend `sovereign-cli-llm/src/mcp_demo_server.rs` with
  `read_memo(path)` (or `ocr_image(path)`) that reads a committed fixture file
  at `path` and returns its **sealed** text. The sealed value proves the path
  round-tripped through a real tool call, exactly like `get_clearance_code`.
- **e2e** (deterministic, CI-safe) — spawn the reference server; build a
  tool-files-augmented message pointing at the fixture; run the agent with
  `DeterministicInference` driving a plan that calls the tool with that path;
  assert the sealed text appears in the answer and the tool was called with the
  fixture path.
- **Live** — run a local OCR/Whisper MCP server, drag in a real screenshot /
  voice memo, ask; see the description / transcript. `router.tool_gate` confirms
  selection fired.

## Files to touch

| Concern | File |
|---|---|
| Attach DTO + preamble + command param | `sovereign-desktop/src-tauri/src/commands/chat.rs` (model on `FocusedChunkRef`, `build_context_augmented_message`) |
| Frontend attach routing + chip + state | `sovereign-desktop/src/lib/components/ChatView.svelte` |
| api.ts pass-through | `sovereign-desktop/src/lib/api.ts` (extend `sendMessageStream`) |
| (P2) tool-visible paths | `sovereign-core/src/types/routing.rs` (`ToolContext`) + `sovereign-core/src/executor.rs` (construction sites) + `attached_document_search.rs` (`7693f16b`) |
| Reference tool + e2e | `sovereign-cli-llm/src/mcp_demo_server.rs` |

## Why this is small

The MVP is one desktop-crate surface: it reuses an existing prepend rail
(`context_chunks`), the whole MCP call path (router gate → planner example →
executor → local server), and the existing approval gate. The model never
changes; the document-RAG pipeline never changes. "Attach an image and ask
what's in it" becomes the typed-path flow that already works, minus the typing.
