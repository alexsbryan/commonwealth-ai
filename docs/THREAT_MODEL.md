# Threat Model

Commonwealth AI's security and privacy model is built and tested: your
data stays on your machine, every corpus carries its own sharing posture,
and an opt-in **encrypted mode** gives you fail-closed, zero-trust-network
operation over iroh QUIC/TLS. By default you run in **trusted-network
mode** — you pool machines on a network you already control and the
perimeter is the boundary — which keeps setup friction low without giving
anything up to the outside world.

This document is the consolidated, surface-by-surface reference for how
that holds together, including the deliberate trade-offs of each mode. It
exists so the honest caveats that were previously scattered across module
docs and architecture files live in one place a deployer can read before
exposing anything. Two rules govern it:

1. **Every claim here is pinned to code.** File references are given so a
   reader can verify the mitigation actually exists. If this document and
   the code disagree, the code is the truth and the document has a bug —
   please report it.
2. **Gaps are listed, not embellished.** This project deletes security
   façades rather than leaving them in place (the unused per-session TLS
   scaffolding was removed 2026-06-15 for exactly that reason). The
   "Known gaps" section below is part of the contract, not an appendix.

Vulnerability reporting: see [SECURITY.md](../SECURITY.md). Architecture
context: `commonwealth/ARCHITECTURE.md` §9 (a summary that defers to this
document) and `sovereign/SYSTEM_OVERVIEW.md` §"Discovery and membership".

## Trust boundaries

Three zones, from most to least trusted:

- **The local machine.** Loopback callers are trusted: the desktop app,
  local CLI, and in-process callers reach the client API without a token.
  This is decided from the real socket peer address
  (`ConnectInfo<SocketAddr>`), never from a spoofable header
  (`commonwealth/crates/commonwealth-api/src/client_auth.rs`).
- **The mesh perimeter.** In trusted-network mode a Commonwealth mesh runs
  on a network you control — a tailnet, WireGuard, or a LAN behind a
  firewall. Inside that perimeter, nodes that hold the join key are peers.
  Membership is gated by a BLAKE3-hashed join key, compared in constant
  time (`commonwealth/crates/commonwealth-discovery/src/membership.rs`);
  when a joiner presents a node identity it must also carry an Ed25519
  proof-of-possession, and a bad or missing proof is rejected with 401
  (`commonwealth/crates/commonwealth-api/src/routes_internal/mesh_admin.rs`
  with the check in
  `commonwealth/crates/commonwealth-transport/src/identity.rs`).
- **Everything else.** Nothing here is designed to face the public
  internet. The fail-closed defaults below exist so that crossing this
  line requires a deliberate operator decision, never an accident.

## Network surfaces

| Surface | Default bind | Auth | Encryption |
|---|---|---|---|
| Client API `:9741` — embedded daemon (`/v1/*` OpenAI, `/api/*` Ollama shim, apps, knowledge) | `127.0.0.1` (`sovereign/crates/sovereign-mesh/src/daemon.rs`) | Loopback exempt; any non-loopback caller needs `Authorization: Bearer <client_token>`; **fail-closed** (403) when no token is configured (`client_auth.rs`). Exempt read-only paths: `/status`, `/oicp/v1/capabilities`. | Plain HTTP on the perimeter; on an encrypted mesh the listener is forced loopback and iroh QUIC/TLS is the sole ingress |
| Client API `:9741` — standalone `commonwealth` binary | `0.0.0.0` (hardcoded; `commonwealth/crates/commonwealth-daemon/src/main.rs`) | Same `client_auth` bearer layer as above | Same |
| MCP `/mcp` (rides `:9741`) | — | Loopback-only middleware, no token by design (`sovereign/crates/sovereign-mesh/src/mcp_router.rs`); permissive CORS is safe *because* of the loopback gate | — |
| Internal mesh API `:9742` (gossip, join, scheduling, corpus collaboration) | `0.0.0.0` in trusted-network mode; `127.0.0.1` in encrypted mode | **None blanket** — perimeter-trusted; join itself is key+proof gated; admin routes are per-handler loopback-only | **Encrypted-QUIC-first**; in trusted-network mode it falls back to cleartext HTTP on your perimeter, and encrypted mode (below) makes iroh QUIC/TLS the sole path |
| `sovereign-server` `:8080` (multi-tenant REST/WS, mobile-facing) | `127.0.0.1` (`sovereign/crates/sovereign-server/src/config.rs`) | API-key → tenant middleware. **Startup refuses a non-loopback bind with auth disabled** unless `allow_unauthenticated_remote = true` is set explicitly (`validate_exposure`). `/health` + `/status` unauthenticated by design. | Plain HTTP on the perimeter; iroh dial-by-key optional (`[iroh] enabled`) |
| Worker-pod daemon `:9742` (rented/cloud worker) | `0.0.0.0` | Owner-only routes; client pins the worker's certificate thumbprint from the bootstrap seed | rustls TLS (`sovereign/crates/sovereign-mesh/src/worker_daemon.rs`) |
| Tensor-split RPC `:50051/:50052` (`llama-server` ↔ `rpc-server`) | `127.0.0.1` locally; `0.0.0.0` for multi-host via `SOVEREIGN_RPC_SERVE` | **None** | **None — raw TCP.** See Known gaps |
| Desktop command bridge `:9745` (test automation) | `127.0.0.1` | Debug builds only, opt-in via `SOVEREIGN_COMMAND_BRIDGE=1`; must never ship enabled in release (`sovereign/crates/sovereign-desktop/src-tauri/src/command_bridge.rs`) | — |

Browser CORS: the `:9741` client surface deliberately ships **no** CORS
layer (`routes_ollama.rs` module doc — "honest disclosure over silent
exposure"); `sovereign-server` applies permissive CORS only when auth is
enabled (`[server] cors = "auto"`), so an unauthenticated server never
invites cross-origin browser calls.

## Two operating modes

Pick per how much you trust the network your machines sit on. Both are
deliberate; neither is a placeholder.

- **Encrypted mode — opt-in, fail-closed, zero-trust-network.** A mesh
  created with `require_encryption` moves every node onto the iroh
  dial-by-key transport (QUIC/TLS keyed by each node's Ed25519 identity),
  with **no plaintext fallback** (`RoutedTransport::with_required`),
  loopback-only local listeners, and join carried only over an encrypted
  founder-dialed channel with a 24h-TTL invite — so the join secret never
  crosses the wire in clear. The posture is monotonic: a stale or hostile
  peer gossiping `require_encryption = false` cannot demote the mesh
  (`commonwealth-core::mesh::Mesh::merge_from`), and if the iroh endpoint
  can't bind the daemon refuses to start rather than run plaintext. Dial
  info (each node's relay + direct addresses) is per-node Ed25519-signed,
  so a peer past the join gate cannot strip or substitute another node's
  reachability to force it offline (`commonwealth-core::dial_sig`).
- **Trusted-network mode — the default.** You pool machines on a network
  you already control — a tailnet, WireGuard, or a LAN behind a firewall —
  and the perimeter is the trust boundary. Since the iroh migration this
  mode dials peers over encrypted QUIC *first*, using the trusted network
  only as a fallback when a direct encrypted path isn't available; on that
  fallback, inter-node `:9742` traffic is plain HTTP. This is a documented
  posture, not an oversight — the mitigation is the perimeter, and the
  unused per-session TLS scaffolding was deleted rather than left looking
  load-bearing.
- **The worker-pod path is always TLS**, with the certificate thumbprint
  pinned by the owner from the bootstrap seed.
- **One exception, in either mode: multi-host tensor-split RPC is raw
  TCP.** It sits outside the transport seam, so we don't claim end-to-end
  encryption while it's in use. See Known gaps.

## Data custody

- **Corpora carry their sharing posture in the recipe.** `CorpusMeta`
  has `license`, `mesh_sharing` (byte-level redistribution allowed?),
  `query_sharing` (may federated queries read it?), and `scope = "local"`
  to pin a corpus off-mesh entirely
  (`corpus-engine/src/recipe.rs`). Shipped recipes set these per source
  (e.g. SEP is `mesh_sharing = false`).
- **Work-atlas privacy is structural.** Private claims/observations are
  written to a separate store that never gossips, enforced at the store,
  gossip, and read layers (`~/.sovereign/work-atlas.toml`,
  `docs/WORK_ATLAS.md`).
- **Answers cite sources.** Retrieval provenance is recorded and surfaced
  (`[Source: …]` citations, message provenance metadata), so data that
  leaves a node does so as attributed retrieval results, not anonymous
  bulk export.

## Known gaps

These are real, current, and deliberate to disclose:

1. **Tensor-split RPC is plaintext and unauthenticated.** Anyone who can
   reach `SOVEREIGN_RPC_SERVE`'s port can read activations and submit
   work. Run it only inside the perimeter; never claim end-to-end
   encryption while it is in use. (Activations are float tensors, not
   text, but activation-inversion attacks recovering input fragments are
   published research — see `commonwealth/ARCHITECTURE.md` §9.)
2. **The internal API `:9742` has no blanket auth in trusted-network
   mode.** The perimeter is the mitigation. A hostile device *inside* your
   tailnet/LAN is inside the trust ring. Encrypted mode closes this — the
   listener is loopback-only and iroh is the sole ingress.
3. **One shared client token, not per-user tenancy, on `:9741`.** Every
   remote holder of the client token has the same authority.
   (`sovereign-server` on `:8080` does have per-key tenants.)
4. **The standalone `commonwealth` binary hardcodes `0.0.0.0:9741`**
   (bearer-gated, loopback-exempt) rather than following the embedded
   daemon's loopback-first default.
5. **Tauri v2 does not gate app commands per-window** (tauri#9227): a
   webview with IPC access can invoke any registered command. Tracked
   upstream; relevant only if untrusted content ever gets a webview.
6. **A compromised node can serve bad inference.** Not defended —
   social trust model, by design, documented since the first
   architecture draft.

## Reporting

Found a way to break any promise above — especially a path where data
leaves a machine without the user asking? Please report privately:
[SECURITY.md](../SECURITY.md).
