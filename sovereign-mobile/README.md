# Sovereign Mobile (Tauri 2 — iOS + Android)

A **thin presentation + control client** for a Sovereign host, per
`sovereign/docs/specs/MOBILE.md`. It runs **no local inference, no
embedding, and no Runtime**: it sends queries to a host node's
`sovereign-server` over the **tailnet**, authenticates as a tenant, and
renders the streamed result. The phone is a *client, not a mesh peer*.

This is the **Tauri** mobile surface (not a native SwiftUI app): a Rust
core (`src-tauri/`) owns all transport/security/cache concerns and a
Svelte 5 frontend (`src/`) renders chat, reusing the shared
`@sovereign/chat-ui` package the desktop app also consumes.

## Why a Rust-core transport (not JS fetch)

The desktop chat FSM consumes abstract events — `message-chunk`,
`message-complete`, `message-error` — and never knows the transport.
The mobile Rust core opens the remote HTTP/WebSocket connection, injects
the tenant token, enforces tailnet-only reachability, writes the SQLite
cache, and **re-emits those same events**, so the shared render surface
runs unchanged. Token-in-Keychain, fail-closed-off-tailnet, and the
`503`→"host busy" distinction are policy that belongs in Rust, behind a
single auditable choke point (the WebView/JS never sees the token).

## Architecture

```
src-tauri/src/
  lib.rs              tauri::mobile_entry_point; builder; command + plugin registration
  main.rs             desktop-dev shim (cargo run on a laptop) → lib::run()
  state.rs            AppState: ApiClient, SQLite pool, active HostConnection, monitor handle
  error.rs            crate Error + Serialize (Tauri command results)
  connection/
    store.rs          HOST_CONNECTION CRUD in SQLite (client-owned, source of truth)
    keychain.rs       CREDENTIAL token via OS keychain (trait + stub; real impl is a pin-time task)
  cache/
    schema.rs         cached-projection tables (CONVERSATION/MESSAGE/PROVENANCE/CITATION/CORPUS_REF)
    store.rs          cache-first reads + reconcile (synced_version / server_version)
  remote/
    client.rs         reqwest HTTP; Authorization: Bearer; parses 503 + Retry-After
    stream.rs         tokio-tungstenite WS → decode ServerEvent → re-emit message-* events
    dto.rs            serde mirror of the Phase-1 server JSON
    map.rs            server Complete{provenance,citations} → the metadata blob RoutingMeta expects
  connectivity/
    monitor.rs        OffTailnet | HostDown | HostBusy | Reachable; emits connectivity-changed
  commands/
    host.rs           add/list/set-default host connections; get_connectivity
    conversation.rs   create/list/get/delete (cache-first, then reconcile)
    chat.rs           send_message_stream → opens WS, drives stream.rs
    corpus.rs         list_corpora; resolve_citation (corpus_id, chunk_id) → snippet

src/                  Svelte 5 frontend (imports @sovereign/chat-ui)
  App.svelte          router: Pairing | ConversationList | Chat, by connection state
  lib/api.ts          invoke() bridge mirroring desktop command names
  lib/events.ts       listen() wiring → chat.machine
  lib/screens/        PairingScreen, ConversationListScreen, ChatScreen
  lib/ui/             ConnectivityBanner (three distinct states), CitationSheet
  lib/components/     AssistantMessage (mobile's own thin orchestration over shared leaves)
  lib/machines/       chat.machine.ts (per-app copy — see note below)
  lib/utils/          markdown.ts (per-app copy — see note below)
```

### Conversations (per `MOBILE.md`)

A conversation is two things, and the client treats them differently:

1. **Live chat state** in the host's StateStore — the phone caches
   `CONVERSATION` + `MESSAGE` rows for display (cache-first reads,
   offline-readable, reconciled on reconnect). `CONVERSATION.indexed_in_corpus`
   reflects whether the host has indexed it yet (carried through the DTO +
   cache; the server populates it once the KnowledgeView ingest exposes
   per-conversation status — a follow-up).
2. **A host-side conversation corpus** (the `conversations-*` recipe +
   `conversation_atlas` pipeline) — once indexed, retrievable like any
   other corpus. The phone only *references* it as a `CORPUS_REF`
   (`category = "conversations"`); it never builds or stores it.

**Long-context is host-side.** `send_message_stream` sends only
`{ conversation_id, new turn }`. The host's Runtime owns history
compaction + retrieval-over-prior-turns and the embed slot; the phone
never re-uploads history or embeds anything. This is the "stateful host
API" the spec calls for — **resolved**: `sovereign-server` is stateful
over the conversation id.

**Privacy posture is visible.** `CORPUS_REF.scope` / `mesh_shared` ride
the `/v1/corpora` response (derived from `IndexInfo.mesh_sharing`); the
conversation corpus is always `scope=local`, `mesh_shared=false`. Cited
sources that are local-only get a 🔒 badge (`corporaStore.isPrivate`).
Conversation privacy is **identity-scoped** (the recommended default):
the server scopes conversations by `tenant:conversation_id`, so a
tenant's conversations follow their token across devices and are
isolated from other tenants on the same host.

**Offline = read, not search.** Cached conversations render offline;
semantic search over them requires the host's embed + corpus, so it is
simply unavailable offline (the app has no offline-search path to fake).

### Shared vs per-app

`@sovereign/chat-ui` (at `packages/chat-ui`) is consumed as **source via
a Vite/tsconfig alias** (no build step). It shares the prop-driven leaf
components (`RoutingMeta`, `SourceAttribution`, `SourcePopover`,
`ThinkBlock`, `NextStepButtons`), the content parser, the stream buffer,
and the chat types. Files that import npm packages — the xstate FSM
(`chat.machine.ts`) and the `marked`/`katex` renderer (`markdown.ts`) —
stay **per-app** because alias-source files type-check from the package
dir, which has no `node_modules`. Unifying them too needs hoisted deps
(an npm workspace); deferred.

## Build & verify

**On this Linux box (no Xcode):**
- `cargo check` the core — requires the Tauri Linux build deps
  (`gtk3-devel`, `webkit2gtk4.1-devel`, …; see `sovereign/scripts/bootstrap-linux.sh`)
  and the Android SDK/NDK for `tauri android` targets.
- `npm install && npm run dev` — preview the frontend in a browser
  against a reachable `sovereign-server`.
- `npm run check` (svelte-check) + `npm test` (vitest) — frontend logic.
- `npx tauri android init` + `npx tauri android build` — Android is
  buildable on Linux.

**On a Mac (required for the milestone target):**
- `npx tauri ios init`, signing/provisioning, `npx tauri ios build`.
- Install on a **physical iOS device via internal TestFlight** — the
  `MOBILE.md` validation target. iOS Keychain entitlements + `.ipa`
  inspection (no model/inference runtime in the bundle) happen here.

## Status — scaffold

This is a **scaffold authored on Linux without the Tauri mobile
toolchain**; it has not been compiled. It is **excluded from the Cargo
workspace** (own detached `[workspace]` in `src-tauri/Cargo.toml`) so it
cannot destabilise the verified workspace build. Pin-time tasks before
first build: choose the iOS-Keychain/Android-Keystore plugin (see
`connection/keychain.rs`), confirm `tauri-plugin-*` versions, and run
`tauri {ios,android} init`. The Phase-1 host side this client targets
(`sovereign-server`: WS token streaming, provenance + citations on
responses, `GET /v1/corpora`, `503 + Retry-After`) **is** implemented
and tested.
