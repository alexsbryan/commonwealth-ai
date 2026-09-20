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
- **Guests.** A holder of an ephemeral guest grant
  (`sovereign/crates/sovereign-grants/src/guest_grant.rs`, 2026-08-27; the
  crate was `commonwealth-knowledge` until the pack split) is strictly
  weaker than a member and strictly weaker than a
  `client_token` holder. They present a short-lived, revocable bearer,
  and may call only the exact paths their `Scope` set names — matched
  EXACTLY, never by prefix (`GuestGrant::permits_path`) — today
  `/v1/models` + `/v1/chat/completions` for a model scope; the three rail
  routes + `/v1/guest/ask` for a rail scope. On `/v1/chat/completions` they
  reach only the models the scope lists; `/v1/guest/ask` carries no model
  list, because the turn it runs is the daemon's own and the router picks
  the slot (`routes_guest_ask.rs`, `collect_turn`) — the bound there is the
  handler, below.
  They never enter `Mesh.members`, never receive `mesh_secret`, and never
  learn the invite key — so "a guest cannot invite people" is structural:
  `Scope` has no variant that could express it. The grant lives only in
  the issuing node's memory and is never gossiped, so revocation is
  immediate rather than eventually-consistent. A grant with a rail scope
  (`/v1/rail/append|log|live`) is also the key to the
  **guest door**: `[daemon] guest_bind = "host:port"` (default off) puts
  the same Guest router on a bind the room's WiFi reaches, listening only
  while such a grant is live and closed at the last expiry
  (`sovereign/crates/sovereign-daemon/src/guest_door.rs`), even on an
  encrypted mesh. **A wall grant reaches every rail namespace this door's
  owner registered for guests in `[daemon.guest_pages]`, for the grant's
  TTL, and nothing else on the rail** — the resource declares and the
  credential identifies, the same shape a recipe's `mesh_sharing` has for
  a corpus. That is the default (`svrn mesh grant --wall`), so one QR
  serves a whole wall. Two knobs narrow it and neither widens anything:
  `--rail <ns>` mints a grant that reaches exactly one namespace and is
  refused the rest by name, and an entry written
  `<ns> = { dir = "…", guests = "read" }` serves its page and refuses a
  guest's append. A namespace the daemon writes on its own behalf — the
  work plane, the KV rings, the atlas, `mesh-measurements` — can never be
  declared: it is refused at config load AND again at the route, so a
  registry that was wrong is not the only guard. A request that names an
  undeclared namespace is a 403 naming it, never a fall-through to another
  app. It adds one unauthenticated route, the ring page at
  `/ring/<namespace>/` served from that registry and never from outside
  its bundle; `[daemon] guest_page_dir` still puts a single app at the bare
  `/ring/`, and with that key unset the bare prefix is an index of the
  declared apps a live grant reaches.
  The page reads the bearer from the URL fragment, which the browser never
  sends. A guest's door-issued session carries a NAME and no scope, and
  `[daemon] guest_sessions` says what it is recognised under — `"door"`
  (default: any live grant this door minted, so one person walking between
  this wall's apps is named once) or `"grant"` (the link it was claimed on
  alone). Neither setting changes reach: `permits_path` on the bearer
  presented is still the only decider, and a session cannot outlive the
  grants it is recognised under. The door also answers `/status` and `/oicp/v1/capabilities` to
  anyone on that network, as every non-loopback bind does.
  A rail scope also carries `POST /v1/guest/ask`
  (`sovereign/crates/sovereign-daemon/src/routes_guest_ask.rs`), which is how
  the room answers a question for someone who holds no membership. Its bound
  is the handler, not the path: the turn runs IN-PROCESS as the door's own
  principal, in one conversation whose id is `sha256(bearer)` — so a second
  grant's holder cannot name the first's — and the reply carries only
  `{answer, epistemic_state}`. No conversation id and no message id leave the
  door, and no `/v1/conversations*` route is in any `Scope`, so the surface a
  guest would need to read somebody else's chat is neither granted nor
  mounted for them. The grounded turn fans out across the mesh exactly as a
  member's does, which means a guest's question can reach a sibling node's
  corpus — that is the capability, and the citation says which member held
  the passage.
- **Everything else.** Nothing here is designed to face the public
  internet. The fail-closed defaults below exist so that crossing this
  line requires a deliberate operator decision, never an accident.

## Network surfaces

| Surface | Default bind | Auth | Encryption |
|---|---|---|---|
| Client API `:9741` — embedded daemon (`/v1/*` OpenAI, `/api/*` Ollama shim, apps, knowledge) | `127.0.0.1` (`sovereign/crates/sovereign-daemon/src/daemon.rs`) | Loopback exempt; any non-loopback caller needs `Authorization: Bearer <token>`, matched full-token-first then guest-grant (`client_auth.rs`); **fail-closed** (403) when no token is configured. Exempt read-only paths: `/status`, `/oicp/v1/capabilities`. | Plain HTTP on the perimeter; on an encrypted mesh the listener is forced loopback and iroh QUIC/TLS is the sole ingress |
| Client API `:9741` — standalone `commonwealth` binary | `0.0.0.0` (hardcoded; `commonwealth/crates/commonwealth-daemon/src/main.rs`) | Same `client_auth` bearer layer as above | Same |
| MCP `/mcp` (rides `:9741`) | — | Loopback-only middleware, no token by design (`sovereign/crates/sovereign-daemon/src/mcp_router.rs`); permissive CORS is safe *because* of the loopback gate | — |
| Internal mesh API `:9742` (gossip, join, scheduling, corpus collaboration) | `0.0.0.0` in trusted-network mode; `127.0.0.1` in encrypted mode | **None blanket** — perimeter-trusted; join itself is key+proof gated and gossip carries a mesh proof; **the other routes, admin ones included, have no guard of their own** (corrected 2026-09-20: this row said they were per-handler loopback-only, and no handler reads the caller's address) | **Encrypted-QUIC-first**; in trusted-network mode it falls back to cleartext HTTP on your perimeter, and encrypted mode (below) makes iroh QUIC/TLS the sole path |
| `sovereign-server` `:8080` (multi-tenant REST/WS, mobile-facing) | `127.0.0.1` (`sovereign/crates/sovereign-server/src/config.rs`) | API-key → tenant middleware. **Startup refuses a non-loopback bind with auth disabled** unless `allow_unauthenticated_remote = true` is set explicitly (`validate_exposure`). `/health` + `/status` unauthenticated by design. | Plain HTTP on the perimeter; iroh dial-by-key optional (`[iroh] enabled`) |
| Worker-pod daemon `:9742` (rented/cloud worker) | `0.0.0.0` | Owner-only routes; client pins the worker's certificate thumbprint from the bootstrap seed | rustls TLS (`sovereign/crates/sovereign-pods/src/worker_daemon.rs`) |
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
  gossip, and read layers (`~/.svrnmesh/work-atlas.toml`,
  `docs/WORK_ATLAS.md`).
- **Answers cite sources.** Retrieval provenance is recorded and surfaced
  (`[Source: …]` citations, message provenance metadata), so data that
  leaves a node does so as attributed retrieval results, not anonymous
  bulk export.

## Known gaps — open work

These are real and current. Disclosing one is the first step, not the last:
each entry says what closes it and which campaign or order owns that.
`unowned` means nobody is assigned yet, and it is said so rather than left to
read as acceptance. "By design" appears only where the maintainer has decided
the gap stays. An entry is struck, with the commit, by the change that closes
it.

1. **Tensor-split RPC is plaintext and unauthenticated.** Anyone who can
   reach `SOVEREIGN_RPC_SERVE`'s port can read activations and submit
   work. Until closed: run it only inside the perimeter; never claim
   end-to-end encryption while it is in use. (Activations are float tensors,
   not text, but activation-inversion attacks recovering input fragments are
   published research — see `commonwealth/ARCHITECTURE.md` §9.)
   *Closes when:* the RPC stream rides an authenticated, encrypted transport
   (the iroh path the rest of the mesh uses) or the port refuses a peer it
   cannot verify. *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20, queued behind `mesh-principal`. Measured for that order: the member-only encrypted
   tunnel for this traffic already exists and is in use; what is open is the
   `0.0.0.0` default bind.
2. **The internal API `:9742` has no blanket auth, in either mode.** Join
   is key-and-proof gated and gossip carries a mesh proof; the remaining
   routes, including ones that change state (`/internal/mesh/quiesce`,
   `/internal/models/load`), answer any caller that can reach them. In
   trusted-network mode that is any device on your tailnet/LAN — a hostile
   device *inside* the perimeter is inside the trust ring. Encrypted mode
   narrows it but does not close it: the listener is loopback-only, but the
   internal iroh ALPN admits any dialer so that a joiner can reach
   `/internal/join`, and splices it to that listener
   (`sovereign/crates/sovereign-mesh/src/iroh_access.rs`, `forward_for`).
   Corrected 2026-09-20: this entry said encrypted mode "already closes it",
   and the surfaces table said admin routes were loopback-only per handler;
   neither was true. Until closed: keep `:9742` off any network you do not
   control. *Closes when:* a non-member reaches only the join route, over
   iroh and over plain IP, and everything else requires a verified member.
   *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20, queued behind `mesh-principal`.
3. **One shared client token, not per-user tenancy, on `:9741`.** Every
   remote holder of the client token has the same authority.
   (`sovereign-server` on `:8080` does have per-key tenants; guest grants are
   per-bearer, scoped and expiring.) *Closes when:* a remote client holds a
   credential of its own that can be revoked without rotating everyone's.
   *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20, queued behind `mesh-principal`.
4. **The standalone `commonwealth` binary hardcodes `0.0.0.0:9741`**
   (bearer-gated, loopback-exempt) rather than following the embedded
   daemon's loopback-first default. *Closes when:* it binds loopback unless
   configured otherwise, as the embedded daemon does. *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20, queued behind `mesh-principal`.
   Measured for that order: the binary was deleted on 2026-08-26, so this
   entry is expected to be struck, not built.
5. **Tauri v2 does not gate app commands per-window** (tauri#9227): a
   webview with IPC access can invoke any registered command. Relevant only
   if untrusted content ever gets a webview. *Closes when:* upstream lands
   per-window gating, or the desktop gains its own per-window command
   allowlist. *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20, queued behind `mesh-principal`. Measured for that order: the
   desktop ships no app-command manifest, so a mesh-app window can invoke
   every host command, not only the bridge's.
6. **A mesh member can act as any other member on the call plane.** The
   node's Ed25519 key is verified in the iroh handshake and signs every rail
   op, but knowledge search, the capabilities fetch, the admission tally and
   the reciprocity ledger decide on `x-node-id`, a header the caller
   supplies. *Closes when:* those deciders read the verified key and an
   unverified principal is refused. *Owner:* campaign `mesh-principal`
   (`.sovereign/features/mesh-verified-principal/order.md`), queued.
7. **Ring sync ships every namespace to every online member**, whatever the
   ring's roster says, so a ring shared among a few machines is readable by
   the whole mesh. *Closes when:* ring sync reads by roster. *Owner:* campaign `mesh-principal`, queued.
8. **A compromised node can serve bad inference.** Not defended. *By design:*
   the social trust model, documented since the first architecture draft —
   you mesh with machines whose owners you trust.

## Reporting

Found a way to break any promise above — especially a path where data
leaves a machine without the user asking? Please report privately:
[SECURITY.md](../SECURITY.md).
