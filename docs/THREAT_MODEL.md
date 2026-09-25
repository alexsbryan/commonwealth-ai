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
  (`sovereign/crates/sovereign-daemon/src/client_auth.rs`).
- **The mesh perimeter.** In trusted-network mode a Commonwealth mesh runs
  on a network you control — a tailnet, WireGuard, or a LAN behind a
  firewall. Inside that perimeter, nodes that hold the join key are peers.
  Membership is gated by a BLAKE3-hashed join key, compared in constant
  time (`commonwealth/crates/commonwealth-discovery/src/membership.rs`);
  when a joiner presents a node identity it must also carry an Ed25519
  proof-of-possession, and a bad or missing proof is rejected with 401
  (`sovereign/crates/sovereign-daemon/src/routes_internal/mesh_admin.rs`
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
  a corpus. That is the default (`svrn mesh grant --all-apps`), so one QR
  serves a whole wall. Two knobs narrow it and neither widens anything:
  `--app <ns>` mints a grant that reaches exactly one namespace and is
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
| ~~Client API `:9741` — standalone `commonwealth` binary~~ | ~~`0.0.0.0` (hardcoded)~~ | ~~Same `client_auth` bearer layer as above~~ | Struck 2026-09-20: the binary was deleted by `27c0fe031` (2026-08-26) and no crate of that name is in the tree, so this surface does not ship. See Known gaps entry 4. |
| MCP `/mcp` (rides `:9741`) | — | Loopback-only middleware, no token by design (`sovereign/crates/sovereign-daemon/src/mcp_router.rs`); permissive CORS is safe *because* of the loopback gate | — |
| Internal mesh API `:9742` (gossip, join, scheduling, corpus collaboration) | `0.0.0.0` in trusted-network mode; `127.0.0.1` in encrypted mode | **None blanket** — perimeter-trusted; join itself is key+proof gated and gossip carries a mesh proof; **the other routes, admin ones included, have no guard of their own** (corrected 2026-09-20: this row said they were per-handler loopback-only, and no handler reads the caller's address) | **Encrypted-QUIC-first**; in trusted-network mode it falls back to cleartext HTTP on your perimeter, and encrypted mode (below) makes iroh QUIC/TLS the sole path |
| `sovereign-server` `:8080` (multi-tenant REST/WS, mobile-facing) | `127.0.0.1` (`sovereign/crates/sovereign-server/src/config.rs`) | API-key → tenant middleware. **Startup refuses a non-loopback bind with auth disabled** unless `allow_unauthenticated_remote = true` is set explicitly (`validate_exposure`). `/health` + `/status` unauthenticated by design. | Plain HTTP on the perimeter; iroh dial-by-key optional (`[iroh] enabled`) |
| Worker-pod daemon `:9742` (rented/cloud worker) | `0.0.0.0` | Owner-only routes; client pins the worker's certificate thumbprint from the bootstrap seed | rustls TLS (`sovereign/crates/sovereign-pods/src/worker_daemon.rs`) |
| Tensor-split RPC `:50051/:50052` (`llama-server` ↔ `rpc-server`) | `127.0.0.1` — including `--rpc-worker` and `role = "anchor"`, which took `0.0.0.0` until 2026-09-20. A non-loopback `SOVEREIGN_RPC_SERVE` is refused unless `SOVEREIGN_RPC_ALLOW_PLAINTEXT_LAN=1` (or `[shared_model] allow_plaintext_lan = true`) acknowledges it (`sovereign-contracts/src/launch.rs`) | **None** | **None — raw TCP.** Members reach the worker over the member-only `RPC_ALPN` tunnel (`sovereign/crates/sovereign-mesh/src/iroh_access.rs`), which needs no LAN bind. See Known gaps |
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
  `sovereign/docs/WORK_ATLAS.md`).
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
   cannot verify. *Owner:* campaign `threat-gaps` (order `threat-gaps-close`),
   approved 2026-09-20, running. Measured for that order: the member-only encrypted
   tunnel for this traffic already exists and is in use. The `0.0.0.0` default
   bind is closed as of 2026-09-21 by `b38c3cf88`: `DEFAULT_RPC_BIND` is
   `127.0.0.1:50052`, and `RpcServe::resolve` — the one decider — answers
   `Refused { bind }` rather than `Off` for any bind reachable from another
   machine unless the operator acknowledges it with
   `SOVEREIGN_RPC_ALLOW_PLAINTEXT_LAN=1` or `[shared_model]
   allow_plaintext_lan = true`. The entry STAYS OPEN: the stream is still plaintext wherever it runs,
   and that the split completes over the tunnel on a direct path has not been
   measured on two machines (owed to `HUMAN-tg-rpc-two-machines`).
2. **The internal API `:9742` admits the group, not a member.** Narrowed
   2026-09-21 by `8885071db` (the gate) on `530db2bb2` (the credential), not
   closed. Until then, join was key-and-proof gated and gossip carried a mesh
   proof, and the remaining routes — including ones that change state
   (`/internal/mesh/quiesce`, `/internal/models/load`) — answered any caller
   that could reach the port: on a trusted network, any device on your
   tailnet/LAN. Encrypted mode narrowed that and did not close it: the
   listener is loopback-only, but the internal iroh ALPN admits any dialer so
   that a joiner can reach `/internal/join`, and forwards it to that listener
   (`sovereign/crates/sovereign-mesh/src/iroh_access.rs`, `forward_for`).
   Since `e8f7f0520` that hop carries the dialer's verified key, and its
   member name when the roster has one. Since `0f190bc47` `/internal/ring/sync`
   refuses on that key, by roster; see entry 7.
   What `8885071db` added is one gate for every other route
   (`sovereign/crates/sovereign-daemon/src/internal_gate.rs`, applied in
   `server.rs` immediately before `internal_principal_layer`, so the resolver
   runs outermost and the gate reads what it attached). It exempts exactly
   `/internal/join` and `/internal/gossip` by exact path equality, admits a
   `Principal::Member`, admits a request carrying `ProvedMeshMember` — the
   marker a valid `x-mesh-proof` earns, which a plain-IP member can now mint
   (`commonwealth-transport::mesh_proof_stamp`) — admits a true loopback
   caller that is not `Unverified`, and answers everything else 401 with a
   sentence and one `warn!` naming route, peer and principal. `[daemon]
   internal_auth` defaults to `"member"`; `"perimeter"` restores the
   behaviour every build before this one had, and says so at startup.
   What remains is the reason this is narrowed and not closed: the mesh proof
   proves the group, not which member is calling (entry 8), so on a plaintext
   mesh any member is every member on this port; and nobody has yet watched a
   non-member refused from a second machine.
   Corrected 2026-09-20: this entry said encrypted mode "already closes it",
   and the surfaces table said admin routes were loopback-only per handler;
   neither was true. Until closed: keep `:9742` off any network you do not
   control. *Closes when:* a non-member reaches only the join route, over
   iroh and over plain IP, and everything else requires a verified member.
   *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved
   2026-09-20; the live half — a real second machine refused on the shipped
   defaults, with join and gossip between members unchanged — is owed to row
   `HUMAN-tg-the-stranger`.
3. **One shared client token on `:9741`; named tokens exist beside it.**
   Narrowed 2026-09-21 by `36501a41c`, not closed. Until then every remote
   holder of the client token had the same authority and taking it back from
   one device meant rotating it for all of them.
   (`sovereign-server` on `:8080` does have per-key tenants; guest grants are
   per-bearer, scoped and expiring.) `svrn mesh token --new <label> | --list
   | --revoke <label>` now mints a bearer per device, stored 0600 under
   `<data_dir>/client-tokens/`; `revoke` drops the in-memory entry before
   deleting the file, so the refusal lands in the same daemon lifetime with
   no restart. A named token is not a new principal — it is the credential
   `client_principal::resolve` already turns into `Principal::RemoteClient`,
   with a label attached, so the admit log carries the label and no log line
   carries a token. `[daemon] client_tokens` defaults to `"shared"`, which
   keeps admitting the one shared token beside the named ones; `"named-only"`
   refuses it with a 401 naming the posture. This is not per-user tenancy:
   a named token's authority is still the whole client API, and on the
   default posture the shared secret is still a credential.
   *Closes when:* the shipped default is a per-device credential and a
   remote client's authority can be scoped, not only revoked.
   *Owner:* campaign `threat-gaps` (order `threat-gaps-close`), approved
   2026-09-20; the live half — two labelled devices, one revoked and refused
   while the other keeps working with no restart — is owed to row
   `HUMAN-tg-the-stranger`.
4. ~~**The standalone `commonwealth` binary hardcodes `0.0.0.0:9741`**
   (bearer-gated, loopback-exempt) rather than following the embedded
   daemon's loopback-first default.~~ Struck 2026-09-20 by `27c0fe031`
   (2026-08-26), which deleted the binary: `commonwealth/crates/` holds nine
   crates and none of them is `commonwealth-daemon`, so the surface this entry
   described no longer ships. The embedded daemon's loopback-first default
   (first row of the surfaces table) is the only `:9741` there is.
5. **A mesh-app window is held to the bridge in source, and nobody has
   watched one refused.** Closed in source 2026-09-21 by `00451b8ac`. Tauri
   v2 still does not gate app commands per-window (tauri#9227, open): its ACL
   check applies only to a crate carrying an app manifest, this crate's
   `build.rs` is a bare `tauri_build::build()`, so `capabilities/meshapp.json`
   could not decide WHICH commands a `meshapp-*` window reaches and a webview
   with IPC access could invoke any registered command. That is no longer what
   this entry waits on: the desktop gained its own allowlist rather than
   waiting for upstream. `meshapp::bridge_refusal(label, command)`
   (`sovereign/crates/sovereign-desktop/src-tauri/src/meshapp.rs`) is the one
   decider — pure, label and command in, refusal out — called in the invoke
   closure in `src-tauri/src/main.rs` before the handler runs, so a label
   `app_id_from_label` recognises may invoke only a name in
   `MESHAPP_BRIDGE_COMMANDS` (18) and every other label is unchanged. The
   allowlist is not hand-kept beside the shim:
   `the_bridge_allowlist_is_exactly_the_shims_commands`
   (`src-tauri/src/commands/meshapp.rs`) parses the `invoke("…")` names back
   out of `MESHAPP_SHIM` and asserts set-equality with the const, so widening
   one without the other is red.
   What remains is the live half, and it is the reason this entry is not
   struck: both of that commit's plants are SOURCE checks — the second,
   `the_invoke_closure_consults_the_bridge_decider`
   (`src-tauri/tests/command_surface.rs`), is a census of `main.rs`, and the
   crate has no mock-runtime harness, so nothing here can drive a real
   refusal from a real `meshapp-*` window (ARCH 5: a gate nobody has watched
   fail). *Closes when:* a real mesh-app window is observed being refused a
   non-bridge command. *Owner:* campaign `threat-gaps` (order
   `threat-gaps-close`), row `HUMAN-tg-the-stranger`.
6. **A mesh member can act as any other member on the CLIENT plane** —
   narrowed 2026-09-21, not closed. On the internal plane (`:9742`) the nine
   deciders that read `x-node-id` now read a principal resolved from the key
   the iroh handshake proved (`8f1ca8a0a`), the header's parser is gone with
   its file (`sovereign-daemon/src/headers.rs`, deleted), and a test in the
   normal run fails on any production read of the literal that returns
   (`sovereign-daemon/src/mesh_principal_gate.rs`). What remains is one plane
   over: `CLIENT_ALPN` is spliced to the peer router with NO identity after
   the acceptor has verified the dialer is a member
   (`sovereign-mesh/src/iroh_access.rs`, `forward_for`), so
   `client_principal::resolve` still mints `Principal::Member` from a readable
   `x-node-id` — and member B, verified as B in the handshake, can still type
   C on an inference or embedding turn and move C's peer ceiling, admission
   tally and reciprocity key. Measured from the resolver and the splice, not
   from a test (ledger A61). *Closes when:* the `CLIENT_ALPN` member arm
   forwards the verified identity the way `cwth/http/0` does since
   `e8f7f0520`, and `client_principal` prefers a tied verified key over the
   typed claim. *Owner:* unowned — proposed to the operator at A61 as one more
   row on campaign `mesh-principal` plus a fifth clause on
   `mp-principal-is-the-verified-key`; neither is approved yet.
7. ~~**Ring sync ships every namespace to every online member**, whatever the
   ring's roster says.~~ Closed 2026-09-21 by `0f190bc47`: both halves of
   `/internal/ring/sync` test the asker's verified key against the ring's
   roster through one decider (`sovereign_mesh::ring_roster::roster_names`) —
   the sender before it offers a namespace, the server before it ingests or
   selects — and a refusal names the namespace and the asker. A ring with no
   `roster.json` is still answered by membership, which is the rail's
   documented default and what the seven `REGISTERED_NAMESPACES` rely on; the
   file-rostered work plane narrows. See entry 8 for the posture this depends
   on.
8. **An asker that proves only the mesh secret is served every ring on a
   plaintext mesh.** The roster filter in entry 7 decides on the key the iroh
   handshake proved. A caller that presents no `x-mesh-*` at all is
   `Principal::Anonymous` and is served without a roster check once it is past
   the gate — on the encrypted posture that is a local process reaching a
   loopback-only `:9742`, and on a plaintext mesh, before `8885071db`, it was
   every peer that could route to the host; since that commit such a peer must
   at least carry a valid `x-mesh-proof` to be let in at all, which proves the
   group and not the member. (A caller that presents an identity the daemon cannot tie to its own
   acceptor is `Principal::Unverified` and is refused every namespace; this
   entry is about the one that claims nothing.) Disclosed in the route's own
   module header. *Closes when:* a plaintext mesh either carries a per-caller
   identity on the internal port or the roster filter refuses an unkeyed
   asker there too — which is a product decision, because it breaks a
   deployed plaintext mesh. *Owner:* campaign `threat-gaps`, bar
   `tg-stranger-refused-9742`. The product decision was taken 2026-09-20
   (ledger A58) and landed 2026-09-21 — `530db2bb2` gave a plain-IP member
   the one outbound stamp, `8885071db` the gate: `:9742` defaults to
   member-only, a plain-IP member proves membership with the mesh proof
   gossip already carries, so a plaintext mesh keeps working, and
   `internal_auth = "perimeter"` restores the earlier
   behaviour. That NARROWS this entry and does not close it: the mesh proof
   proves the group, not which member is calling, so an
   unkeyed asker must now at least hold the mesh secret, and a member who is on
   no roster for a ring can still read it over plain IP. Only the encrypted
   posture, where the handshake proves the key, closes that half; refusing it
   on a plaintext mesh would stop file-rostered rings replicating there, and
   that decision has not been taken.
9. **The internal API's other 54 routes refuse a stranger but tell its
   members apart only on the encrypted posture.** Narrowed 2026-09-21 by
   `8885071db`, not closed. Until then `internal_principal_layer` resolved a
   principal for every request on `:9742` and `/internal/ring/sync` was the
   only route that refused on it, so everything else —
   `/internal/models/load`, `/internal/ring/live`, the corpus grant
   issue/revoke pair, the model-file routes, scheduling intent and plan — was
   reached by any caller that reached the port. `internal_gate.rs` now refuses
   all of them to a caller that is neither a verified member, nor the holder
   of a valid mesh proof, nor a true loopback caller (entry 2 has the exact
   rule and the two exempt routes). What it does not do is give those 54
   routes a per-member authority: on a plaintext mesh they read
   `ProvedMeshMember`, which names no member, so any member can drive any of
   them. *Closes when:* entry 2 closes. *Owner:* campaign
   `threat-gaps` (order `threat-gaps-close`), approved 2026-09-20.
10. **A compromised node can serve bad inference.** Not defended. *By design:*
   the social trust model, documented since the first architecture draft —
   you mesh with machines whose owners you trust.
11. **The desktop main window's Content-Security-Policy has been set but not
   watched refuse.** Narrowed 2026-09-21 (row `tg-11-main-window-has-a-csp`).
   `sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json` set
   `app.security.csp` to `null` until then, so the main window — the one
   holding your conversations, corpora and mesh controls — was under no
   restraint on where it may load script from or where it may send a
   request, and any injection reaching its DOM (a rendered model answer, a
   corpus document, an imported conversation) had an open egress path. It
   now carries `default-src 'self'; script-src 'self'` (no `'unsafe-inline'`,
   no `'unsafe-eval'`), `connect-src` limited to `'self'` and the Tauri IPC
   origins, `img-src 'self' data:`, `object-src 'none'`, `base-uri 'self'`,
   `form-action 'none'` — measured against a census of every asset,
   connection and font the window actually uses, and held there by
   `src-tauri/tests/csp_census.rs`, which fails on a null policy, on
   `'unsafe-inline'`/`'unsafe-eval'` in `script-src`, and on any remote
   origin in any directive. That census is a SOURCE check: nobody has yet
   watched a running window refuse a remote load. *Closes when:* a live
   window under this policy is observed refusing one. *Owner:* campaign
   `threat-gaps` (order `threat-gaps-close`), row `HUMAN-tg-the-stranger`.

## Reporting

Found a way to break any promise above — especially a path where data
leaves a machine without the user asking? Please report privately:
[SECURITY.md](../SECURITY.md).
