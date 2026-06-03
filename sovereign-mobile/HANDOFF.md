# Sovereign Mobile — Handoff (build + verify on macOS)

You're picking up a **scaffolded Tauri 2 mobile client** authored on Linux
without the Apple toolchain. The goal of your pass is to get it building and
running on a **physical iOS device via internal TestFlight** and validate it
against the `MOBILE.md` acceptance criteria. The host-side server it talks to is
**done and tested**; the mobile app is **written but never compiled**.

- Spec: [`sovereign/docs/specs/MOBILE.md`](../sovereign/docs/specs/MOBILE.md)
- Architecture + per-file map: [`README.md`](./README.md) (read this first)

---

## 1. Status at a glance

| Piece | State | Where |
|---|---|---|
| Host `sovereign-server` (WS token streaming, provenance+citations on REST, `GET /v1/corpora` w/ `scope`/`mesh_shared`, `503+Retry-After`) | **Done, tested** (46 crate tests green incl. a real WS stream) | `sovereign/crates/sovereign-server/` |
| `@sovereign/chat-ui` shared package (leaf components + parser + buffer + types) | **Done**, desktop regression green (svelte-check 0 errors, vitest 147/147) | `packages/chat-ui/` |
| Mobile Rust core (transport, cache, connectivity, commands) | **Written, NOT compiled** | `sovereign-mobile/src-tauri/src/` |
| Mobile Svelte UI (pairing, chat, citations, connectivity banner) | **Written, NOT compiled** | `sovereign-mobile/src/` |
| iOS/Android project (`gen/`), signing, icons | **Not generated** — your `tauri {ios,android} init` step | — |
| Keychain (OS-backed token store) | **DEV STUB** — must replace before ship | `src-tauri/src/connection/keychain.rs` |

The mobile crate is **detached from the Cargo workspace** (its own `[workspace]`
in `src-tauri/Cargo.toml`) so its unverified mobile deps can't break the main
build. Expect to iterate on it; it has not type-checked or compiled.

---

## 2. Prerequisites (macOS)

- **Xcode** (full, not just CLT) + an Apple Developer account / team for signing.
- **Rust** + iOS targets: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`
  (`tauri ios init` will also prompt). For Android: Android Studio + SDK/NDK,
  `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`.
- **Node 20+** and npm.
- **Tauri CLI v2 with mobile support**: it's in `devDependencies`; use `npx tauri …`.
- **Tailscale** on the Mac/device and the host, all on the same tailnet.

## 3. First build

```bash
cd sovereign-mobile
npm install                 # installs frontend deps; the shared package is
                            # consumed as SOURCE via a Vite alias (no install)

# Sanity-check the frontend logic before native:
npm run check               # svelte-check — EXPECT to fix some errors; never compiled
npm run dev                 # browser preview at :1420 (invoke() calls no-op without Tauri)

# iOS
npx tauri ios init          # generates src-tauri/gen/apple/ (Xcode project)
npx tauri ios dev           # run on a simulator/device while iterating
npx tauri ios build         # release build → .ipa for TestFlight

# Android (also possible on Linux)
npx tauri android init
npx tauri android build
```

`cargo check` the core in isolation (it has its own workspace):
```bash
cd sovereign-mobile/src-tauri && cargo check
```
This pulls `tauri` + system webview deps; on the Mac that's via Xcode. Fix the
inevitable first-compile errors here before `tauri ios build`.

---

## 4. Pin-time tasks (do these during the first build)

1. **Keychain — the load-bearing one.** `src-tauri/src/connection/keychain.rs`
   defines a `CredentialStore` trait and a **dev-only file-backed stub**
   (`DevFileCredentialStore`) that writes tokens to a file. This is **not
   secure and must not ship.** Implement a real `CredentialStore` backed by the
   **iOS Keychain** and **Android Keystore**, then swap it in at
   `src-tauri/src/lib.rs` (the `credentials = Box::new(DevFileCredentialStore…)`
   line). Two paths:
   - A community secure-storage Tauri plugin (confirm it backs the OS keychain
     on *both* platforms and supports Tauri 2 mobile), or
   - A thin custom plugin: Swift `SecItem` (iOS) / Kotlin `KeyStore` (Android)
     behind the same trait. Token key convention: `sovereign.token.<host_connection_id>`.
   Acceptance §3's `.ipa` inspection assumes the token is in the Keychain, not in
   SQLite — keep it that way (the `credential` table holds only tenant metadata).
2. **App icons.** `tauri.conf.json` references `icons/icon.png`; `tauri ios init`
   generates the icon set, or run `npx tauri icon path/to/icon.png`.
3. **Confirm plugin/dep versions.** `tauri = "2"`, `tauri-plugin-sql` (if you
   choose JS-side SQL — we use `rusqlite` in the core instead), `tokio-tungstenite
   = "0.29"`, `reqwest 0.12` (rustls). Pin to whatever the toolchain resolves.
4. **`tauri.conf.json` review.** Identifier is `ai.commonwealth.sovereign.mobile`;
   `bundle.iOS.minimumSystemVersion = 14.0`, `android.minSdkVersion = 24`. Adjust
   to your provisioning. CSP is tight (`connect-src 'self'`); all host I/O goes
   through the Rust core, so the WebView needs no remote connect — keep it tight.

---

## 5. Standing up a host to test against

On a host node (your Linux box / desktop) inside the `sovereign-vulkan` toolbox:

1. Build + run the server (binary is `sovereign-server`):
   ```bash
   cargo build --release -p sovereign-server
   # config: copy sovereign/sovereign-server.toml and add api-key auth + a corpus
   ./target/release/sovereign-server --config sovereign-server.toml
   ```
2. Minimal config (matches `crates/sovereign-server/src/config.rs`):
   ```toml
   [server]
   bind = "0.0.0.0:8080"          # reachable over the tailnet
   max_concurrent_turns = 4        # busy guard → 503 + Retry-After
   retry_after_secs = 2

   [auth]
   mode = "api_key"
   keys = { "sk-test-token" = "alex" }   # token → tenant_id

   [inference]
   model = "models/fast.gguf"
   primary_model = "models/primary.gguf"
   context_size = 4096

   [store]
   path = "data/sovereign.db"
   ```
3. Install at least one **knowledge corpus** on the host (e.g. a small Wikipedia
   shard or a watched folder). For the *rigorous* §4 check, install a corpus with
   a **distinctive fact absent from the model's parametric knowledge**, then ask
   about it — a correct answer carrying a citation to that corpus proves retrieval.
4. Pairing inputs for the app's Pairing screen:
   - **Tailnet address** = the host's MagicDNS name + port, e.g.
     `beefymac.tailXXXX.ts.net:8080` (NOT a public IP — the client only dials this).
   - **Tenant** = `alex`; **Token** = `sk-test-token` (the `[auth].keys` key).
5. Smoke the host endpoints directly (sanity before the app):
   ```bash
   curl -H 'Authorization: Bearer sk-test-token' http://<host>:8080/v1/corpora
   # WS stream (needs a corpus + a created conversation id):
   websocat -H 'Authorization: Bearer sk-test-token' \
     'ws://<host>:8080/v1/conversations/<id>/stream'
   # then send: {"type":"message","data":{"content":"hello"}}
   # expect: a series of {"type":"token",...} then one {"type":"complete",...}
   ```

> The host API is **stateful over the conversation id** — the phone sends only
> the new turn + id; the Runtime owns history/compaction/retrieval. Don't add
> full-history resend on the client.

---

## 6. Architecture you need to know to debug

**The contract that makes the shared UI work:** the Rust core re-emits the SAME
Tauri events the desktop chat FSM consumes — `message-start` (synthesised on the
first WS token to create the streaming placeholder), `message-chunk`,
`message-complete`, `message-error`. See `src-tauri/src/remote/stream.rs` ↔
`src/lib/events.ts`. If chunks render but the bubble stays empty, the
`message-start`→`SEND_START` wiring is the first place to look (the FSM's
`MESSAGE_CHUNK` is guarded on `messageId === streamingMessageId`).

- **Transport:** `remote/client.rs` (HTTP, parses `503`→`HostBusy`),
  `remote/stream.rs` (WS), `remote/dto.rs` (wire types), `remote/map.rs`
  (server provenance/citations → the `metadata` blob `RoutingMeta` reads).
- **Cache:** `cache/schema.rs` (the ERD tables), `cache/store.rs` (cache-first
  reads + reconcile; stream completion writes message+provenance+citations in
  one transaction).
- **Connectivity:** `connectivity/monitor.rs` emits `connectivity-changed` with
  `OffTailnet | HostDown | HostBusy | Reachable`. `reachability.rs::tailnet_present()`
  is a **stub returning true** — implement real interface/Tailscale-LocalAPI
  detection so OffTailnet vs HostDown is accurate (the fail-closed guarantee
  holds regardless — the client only ever dials the one tailnet address).
- **Shared vs per-app:** `@sovereign/chat-ui` is aliased to
  `packages/chat-ui/src` in `vite.config.ts` + `tsconfig.json`. It ships the
  leaf components + parser + buffer + types. The xstate FSM
  (`src/lib/machines/chat.machine.ts`) and the markdown renderer
  (`src/lib/utils/markdown.ts`) are **per-app copies** — they import npm
  packages, which can't be alias-shared without an npm workspace (see Gotchas).

**Tauri arg convention:** JS `invoke` uses camelCase keys (`conversationId`),
Tauri maps to the snake_case Rust params (`conversation_id`). Keep `api.ts`
camelCase.

---

## 7. Acceptance checklist (`MOBILE.md` §"Must-pass" / "Supporting")

- [ ] **§1 Tailnet-only / fail closed** — off-tailnet, the app shows OffTailnet
      and attempts no other route. (Client only holds the one `tailnet_address`.)
- [ ] **§2 Authenticated** — wrong/absent token → host 401; app surfaces it.
- [ ] **§3 Remote + streamed** — tokens render live, persist on completion.
      **Inspect the `.ipa`: no GGUF / no inference runtime in the bundle.**
- [ ] **§4 Corpus used** — answer carries ≥1 citation; tapping it shows the
      snippet (`resolve_citation`). Use the distinctive-fact corpus for rigor.
- [ ] **§5 Provenance shown** — `inference_backend` (e.g. "… @ peer BeefyMac")
      visible (RoutingMeta footer).
- [ ] **§6 Busy legible** — set `max_concurrent_turns=1`, fire two turns →
      second surfaces "host busy", not an error/hang.
- [ ] **§7 Conversation corpus private** — the `conversations` `CORPUS_REF`
      shows `scope=local` / `mesh_shared=false`; cited local sources get the 🔒
      badge; a *different* tenant can't retrieve it.
- [ ] **§8 Long conversations host-managed** — a conversation longer than the
      model's context window still answers coherently; the phone sent only the
      new turn (verify on the wire / host logs).
- [ ] **§9 Three connectivity states** — off-tailnet / host-down / host-busy are
      distinct, actionable banners.
- [ ] **§10 Offline read, not search** — kill the host: cached convos read fine;
      app doesn't block/error on launch; semantic search is unavailable (not faked).
- [ ] **§11 Cache survives restart** — relaunch renders instantly from cache,
      then reconciles.

---

## 8. Known gaps / follow-ups (not blocking the milestone)

- **`indexed_in_corpus` is always `false`** — modeled end-to-end (DTO/cache/UI)
  but the server doesn't populate it yet; needs the KnowledgeView ingest to
  expose per-conversation index status. The conversation corpus still appears in
  `/v1/corpora` regardless.
- **`ttft_ms` is `None`** — the runtime doesn't stamp time-to-first-token on
  persisted provenance yet. Surface it once captured.
- **Cache reconcile is `updated_at`-based** — the Phase-1 REST projection
  doesn't surface the Lamport `version`. Add `version` to the server's
  `MessageEntry` + a `synced_version` on conversations for precise reconcile.
- **WS reconnect on backgrounding** — iOS suspends sockets; `stream.rs` marks an
  interrupted message `streaming` and the design is "re-fetch via
  `get_conversation` on reconnect." Wire/verify the reconnect trigger.
- **Fuller sharing via npm workspace** — to share the FSM + markdown too (not
  just leaves), convert the repo's JS to an npm workspace so `packages/chat-ui`
  resolves its npm deps from a hoisted `node_modules`. Deferred to protect the
  desktop build.

## 9. Gotchas

- **Alias-source + npm deps:** a file in `packages/chat-ui` that `import`s an npm
  package won't type-check (no `node_modules` up its tree). That's why the FSM +
  markdown are per-app. Don't move them into the shared package without the
  workspace change above.
- **Dev keychain stub** writes tokens to a plaintext file — replace before any
  real install (see §4.1).
- **Detached workspace:** run `cargo` from inside `sovereign-mobile/src-tauri`,
  not the repo root (the root workspace deliberately excludes it).
- **Don't re-upload history or embed on the client** — long-context is host-side
  by design; adding client-side context management breaks the thin-client
  invariant (§8) and the `.ipa` cleanliness (§3).
