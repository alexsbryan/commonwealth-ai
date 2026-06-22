# Transport Migration — iroh for mobile first, then the mesh

Status: **Track M implemented in code** (2026-06-10, same day as the
seam); **W1 (server half) + W2 (dial info in trust ring) + W3
mechanism (`RoutedTransport` + `[iroh.transport]` config) implemented**
(2026-06-18, on iroh 1.0 stable). Remaining: W2b (join over iroh),
**doing** the W3 per-class flips (config + soak, gossip first), W4
(self-hosted relays), W5 (Tailscale optional). The plumbing for iroh
to carry live mesh traffic is in place behind config; no class is
flipped by default. Tailscale remains the production transport until
each phase's exit criteria are met, and every phase is independently
reversible.

iroh dependency: **1.0.0 stable** (shipped 2026-06-15; wire-protocol
stable — any v1 endpoint interops with any other v1 endpoint). Bumped
from `1.0.0-rc.1` on 2026-06-18; all iroh symbol use stays confined to
`commonwealth-transport/src/iroh.rs`.

Track M implementation state:

- **M1 — done in code.** `sovereign-server` gains `[iroh] enabled`
  (default off): an endpoint from `node_key` beside the store DB
  (`key_path` overridable), `IrohAcceptor` forwarding to the local
  HTTP listener on ALPN `cwth/client/0`, and an unauthenticated
  `GET /status` whose `iroh.dial` field is the pairing string.
- **M2 — done in code.** `sovereign-mobile` gains
  `EndpointKind::Iroh`: `iroh_bridge::BridgeManager` (ephemeral
  phone identity, lazy per-host localhost bridges), an async
  `client_for_host` arm, and HTTP + WS riding the bridge unchanged.
  **Pending on-device:** iOS build, LTE relay-path smoke, and the
  battery/latency measurements that are this phase's exit criteria.
- **M3 — done in code, both ends.** Phone: pairing auto-detects the
  address kind (`<64-hex>@…` ⇒ iroh — no UI toggle),
  `add_host_connection` validates the pairing string at pair time,
  dual-stack = two host rows (manual switch; automatic fallback is
  the noted decision point). Desktop: the Mobile-access settings
  panel seeds everything — the generated
  `mobile-host-server.toml` gains `[iroh] enabled` (knob:
  `mobile-host.toml` `iroh_enabled`, default on, serde-defaulted
  for pre-existing files), and the pairing card shows the
  **no-VPN code** read live from the supervised server's
  `/status.iroh.dial` (re-polled while the server starts /
  reaches its relay), alongside the tailnet address. QR rendering
  of the same string is a cosmetic follow-up.
- **M4 — not started** (gated on M2's device measurements).

**2026-06-10, e2e CONFIRMED on hardware:** a real iPhone paired with
the no-VPN code and ran chat end to end over iroh (n0 canary relay +
hole-punched paths) — the first Sovereign mobile session with no
Tailscale on the phone. **LTE relay-path test PASSED the same day**
(query served with the phone off Wi-Fi — the canonical
hostile-network case). Pairing ergonomics shipped alongside: the
pairing string is `id@relay` only (direct addrs dropped — iroh
discovers them post-connect; `dial_full` keeps them for debugging),
and pairing is a QR deep link (`sovereign://pair#…` → "Open in
Sovereign" → all fields pre-filled; raw-JSON QRs lose to iOS
Camera's open-the-relay-URL-in-Safari affordance). Remaining M2 exit
criterion before the M4 default flip: battery observation over a few
days of normal use.

Loopback proof:
`cargo test -p sovereign-mesh --features iroh-experimental` includes
`http_bridge_reaches_acceptor_via_pairing_string` — the phone-side
bridge dialing a host-side acceptor on the client ALPN via a
round-tripped pairing string, plain reqwest end to end.

Companion docs: `SYSTEM_OVERVIEW.md` §5 "The PeerTransport seam",
`MOBILE.md` (the phone's current tailnet-only contract),
`commonwealth/docs/getting-started.md` (today's Tailscale setup).

---

## 0. Where we already are (shipped 2026-06-10)

The foundation this migration stands on is in-tree and tested:

| Piece | Where | What it gives the migration |
|---|---|---|
| `PeerTransport` trait | `commonwealth-transport` | `(PeerContact, TrafficClass) → base URLs`; every mesh peer dial goes through it. Call sites are transport-blind. |
| `IpTransport` | same | Today's tailnet/LAN path, golden-tested byte-identical to pre-seam behaviour. The permanent fallback. |
| Ed25519 node identity | `<data_dir>/node_key`, `MemberRecord.node_pubkey` | The seed **is** a valid iroh `SecretKey`; the pubkey **is** the iroh EndpointId. Join proof-of-possession + anti-downgrade merge already enforce it. Gossip self-stamps it, so the whole mesh already distributes dial-by-key identities. |
| `IrohTransport` + `IrohAcceptor` spike | `commonwealth-transport/src/iroh.rs` (feature `iroh`, pinned `1.0.0-rc.1`) | Proven shape: localhost byte-tunnel ↔ iroh bi-stream; a real gossip round dialed by pubkey passes e2e (`cargo test -p sovereign-mesh --features iroh-experimental`). |
| Mobile `endpoint_kind` | `HOST_CONNECTION.endpoint_kind` (`'tailnet'` today) | The phone's host entry is transport-tagged; `EndpointKind::parse` fails loudly on kinds from the future. |

Identity, in other words, is done. What remains is **connectivity**:
binding endpoints, distributing dial info, and flipping traffic
classes one at a time.

---

## 1. Why mobile first

1. **Highest pain, smallest surface.** The phone is the only device
   that forces a *person* to run a second app (the Tailscale client,
   toggled on, draining battery, fighting iOS VPN lifecycle). And it
   is a pure client — one HTTP+WS connection to `sovereign-server`,
   not N peer traffic classes. Migrating it touches one dial site.
2. **The tunnel already covers both protocols.** The phone speaks
   reqwest HTTP + tokio-tungstenite WebSocket — both plain TCP. The
   spike's byte-tunnel carries them unmodified; no protocol work.
3. **It exercises the hard part of iroh (NAT traversal + relays) in
   the most forgiving setting.** Phone↔host across LTE/hostile Wi-Fi
   is exactly the hole-punch + relay-fallback case. If latency or
   battery disappoint, we learn it on one device with a one-line
   rollback (`endpoint_kind` back to `'tailnet'`), not on the mesh.
4. **It ships the user-visible win first**: "install one app." House
   members' phones stop needing a VPN client before any mesh
   internals change.

The mesh keeps running on Tailscale throughout Track M.

---

## 2. Track M — the phone (client ↔ host, not a mesh peer)

The phone stays a *client* (no gossip, no shard serving — MOBILE.md's
contract is unchanged). Only the pipe changes.

### M1 — host side: `sovereign-server` reachable over iroh

- The host daemon builds one iroh `Endpoint` from `<data_dir>/node_key`
  (the identity that already exists), relays **enabled** (n0's public
  relays initially — see W4 for self-hosting), ALPN `cwth/client/0`.
- An `IrohAcceptor` forwards accepted bi-streams to the local
  `sovereign-server` listener (`bind = 0.0.0.0:8080` today; forward
  to `127.0.0.1:8080`). Zero changes to sovereign-server itself —
  auth stays bearer-token, the transport is below it.
- Gated by a `[server.iroh]` config block (off by default).
- New glassbox surface: `/status` gains
  `iroh: { endpoint_id, relay_url, home_relay_latency_ms }` so "is
  the host dialable by key" is a one-curl question.

**Exit criteria:** host serves an authed `/v1` request arriving over
iroh in a LAN test AND from a phone on LTE (relay path), with the
tailnet path untouched.

### M2 — phone side: iroh in the Tauri Rust core

- Add the iroh dep to `sovereign-mobile/src-tauri` behind the same
  experimental-feature posture as the mesh spike. (iroh runs on
  iOS/Android; the Rust core is exactly where it belongs — this is
  the "iroh embeds in your app instead of a companion VPN app"
  payoff.)
- New `EndpointKind::Iroh` arm. The `tailnet_address` column (opaque
  per kind, by design) stores the dial info:
  `"<64-hex-endpoint-id>@<relay-url>"`.
- `AppState::client_for_host` gets its second arm: lazily start a
  per-host localhost byte-tunnel (the phone-side mirror of
  `IrohTransport::bridge_for`) and hand `ApiClient::new` the bridge
  address. HTTP and the WS stream both ride it unchanged.
- Connectivity monitor: `off_tailnet` generalises to "transport
  unreachable" (for the iroh kind: can't reach relay / dial fails).
  The fail-closed posture is kept — wrong network ⇒ no traffic, just
  like today.
- Battery + latency measurement is part of this phase, not an
  afterthought: idle keepalive cost (iroh maintains QUIC
  keepalives; measure on-LTE idle drain) and time-to-first-token
  vs the tailnet path, recorded in the PR.

**Exit criteria:** full chat session (stream + citations + history)
over iroh on LTE; reconnect after airplane-mode toggle; measured
battery/latency deltas published.

### M3 — pairing + dual-stack

- Pairing today hand-enters a tailnet address + token. The host's
  pairing surface (QR / deep link / settings page) now emits
  `{ endpoint_id, relay_url, tailnet_address, token }` — the phone
  writes **two host rows** (kind `iroh` default-preferred, kind
  `tailnet` fallback) or one row with a fallback annotation —
  decision point at implementation; two rows is simpler and the
  monitor already handles per-host status.
- The phone tries the iroh host first and falls back to the tailnet
  row when it's unreachable. That dual-stack window is the rollback
  story: deleting the iroh row is a full revert.

**Exit criteria:** a fresh phone pairs over iroh with **no Tailscale
app installed**, and an existing phone upgrades without re-pairing.

### M4 — iroh default for mobile

- New pairings default to the iroh kind; docs drop the "install
  Tailscale on your phone" prerequisite (MOBILE.md "fail-closed
  off-tailnet" language generalises to per-kind reachability).
- Tailnet rows remain supported indefinitely (the enum is open).

---

## 3. Track W — the mesh (peer ↔ peer)

Starts only after M2's measurements look good — the phone is the
canary for iroh's path quality on our real networks. Each W phase is
one PR-sized, independently-landable step.

### W1 — every daemon binds an iroh endpoint

**Server half — done in code (2026-06-18).** `EmbeddedDaemon::start_daemon`
builds the iroh `Endpoint` from `<data_dir>/node_key` (the identity
gossip already publishes as `MemberRecord.node_pubkey`, so a known
member is a dialable member) alongside the existing HTTP listeners,
when `[iroh] enabled = true`. One endpoint serves **both** ALPNs;
`IrohAcceptor::spawn_routed` dispatches each accepted connection by its
negotiated ALPN to the matching loopback listener — `cwth/http/0`
(the existing `ALPN` const, internal/9742-equivalent) → internal
router, `cwth/client/0` (`CLIENT_ALPN`) → client/9741-equivalent
router. This lifts the spike's single-forward-target limitation and is
why the port-rewrite logic in `IpTransport` has no iroh analogue: the
*class chooses the ALPN*, not a port. Strictly additive and fail-soft
(`MeshIrohAccess::start` returns `None` on any bind failure; the
`IpTransport`/tailnet path is untouched). Glassbox: a startup
`tracing::info!` logs the endpoint id + dial string.

Implementation: `sovereign-mesh/src/iroh_access.rs` (`MeshIrohAccess`,
held in `DaemonState::Running` so it drops with the daemon);
`commonwealth-transport/src/iroh.rs` (`IrohAcceptor::spawn_routed` +
shared `build_relayed_endpoint`); config `[iroh] enabled` on
`SetupConfig` (spec calls it `[mesh.iroh]`; in the unified
`~/.sovereign/config.toml` it is the top-level `[iroh]` section,
matching `sovereign-server`'s). iroh is compiled into `sovereign-mesh`'s
default build (runtime-gated, mirroring `sovereign-server`'s M1).
Proof: `commonwealth-transport` `routed_acceptor_dispatches_by_alpn`
+ `routed_acceptor_drops_unknown_alpn` (hermetic, dial-by-key, no
relays).

**Client half — resolution done in W2 (below); routing in W3.**
`IrohTransport` now selects ALPN by `TrafficClass`
(Inference/StatusProbe → client ALPN, everything else → internal) and
resolves its dial target from the gossiped `PeerContact` — so it can
*dial peers* by key. It is not yet *wired* as a live transport: that
needs a `RoutedTransport` to compose it with the IP fallback (W3).
Until W3 the daemon is reachable by key (W1) and *can* dial by key
(W2) but still routes its own peer traffic over `IpTransport`.

### W2 — dial info rides the trust ring

**Done in code (2026-06-18).**

- `MemberRecord` gained `relay_url: Option<String>` +
  `iroh_direct_addrs: Vec<SocketAddr>` (both serde-defaulted +
  `skip_serializing_if`, same wire-compat pattern as `node_pubkey`).
  Crucially these are MUTABLE reachability, so — unlike the immutable,
  anti-downgrade-preserved `node_pubkey` — they ride normal last-seen
  LWW: a newer record replaces them (even to `None` when a node turns
  iroh off). Locked by `mesh::tests::iroh_dial_fields_serde_back_compat_and_mutable_lww`.
- The daemon self-stamps its own dial info every gossip round
  (`gossip.rs`, beside the pubkey stamp). The info is DYNAMIC (relay +
  hole-punched addrs appear after bind), so it's a pull-provider:
  `MeshIrohAccess::dial_info_provider()` captures the live endpoint and
  is installed on `AppState` (`install_self_iroh_dialinfo`, type-erased
  so `commonwealth-api` needs no iroh dep). Within one interval of
  binding, the whole mesh learns how to dial this node by key.
- `peer_contact()` carries the fields into `PeerContact`;
  `IrohTransport` resolves its `EndpointAddr` from them (relay +
  direct addrs + pubkey), `add_known_peer` demoted to a test-only
  fallback (merged when present). A peer with a key but no relay/addr
  has no path and is not dialable (falls through to IP in a routed
  composition). **Mesh membership = dialability** — knowing a member
  record is sufficient to dial it. Proof:
  `iroh_transport_dials_from_contact_dial_info` (dials a host purely
  from `PeerContact` direct addrs, no seeding).

**W2b — join over iroh: DONE for encrypted meshes (2026-06-22).** The
join deep link now carries `&iroh=<endpoint-id>@<relay-or-addr>[,…]`
(the founder's `format_dial_string`, percent-encoded) plus an `&exp=`
TTL. When present, `join::perform_encrypted_join` builds a one-shot iroh
endpoint, `HttpBridge`-dials the founder BY KEY, and tunnels
`/internal/join` over the QUIC channel — fail-closed, no plaintext
fallback — so the join secret never crosses the wire in clear and the
joiner authenticates the founder. This shipped as part of the
encrypted-mesh feature (a mesh created with `require_encryption`); a
plaintext mesh still joins over the legacy IP path with the `?relay=`
hint. Remaining nicety: a `--encrypt`-less mesh has no `iroh=` in its
invite, so the `?relay=` wart persists for plaintext meshes only.

### W3 — per-class flips via `RoutedTransport`

**Mechanism done in code (2026-06-18); the flips themselves are an
operator/soak exercise.** The ~20-line composition the seam was
designed for now exists — `commonwealth-transport/src/routed.rs`:

```rust
struct RoutedTransport {
    per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>>,
    default:   Arc<dyn PeerTransport>,   // IpTransport
}
// endpoints(): primary-for-class candidates THEN default's,
// concatenated — callers already try-in-order, so per-dial fallback
// to the tailnet path is free and automatic. note_success() routes
// feedback to the producing transport by label prefix.
```

Wiring: `[iroh.transport] <class> = "iroh"` (under `[iroh]`, only
active when `enabled`) selects which classes flip;
`sovereign-mesh::iroh_access::iroh_routed_classes` maps the config to
`TrafficClass`es, and `EmbeddedDaemon::start_daemon` installs a
`RoutedTransport{ per_class: class→IrohTransport(this endpoint),
default: IpTransport }` when ≥1 class is flipped (otherwise the plain
`IpTransport` install stands — zero change for non-iroh deployments).
Routes set with `enabled=false` are ignored with a warning. Proof:
`routed.rs` unit tests (route / fallback-order / note_success
dispatch) + e2e `routed_transport_prefers_iroh_then_falls_back_to_ip`
(iroh-first candidate serves a real request; a no-iroh-path peer
degrades to the IP candidate alone). `IrohTransport` selects the ALPN
per class (`alpn_for_class`), so a flipped class reaches the right
peer router.

What remains is **doing** the flips — each is a one-line config change
+ a soak, in this order, one class per step:

1. **Gossip** — lowest risk (10s cadence, 3s timeout, anti-entropy
   self-heals anything missed), highest signal (constant traffic =
   constant path-quality telemetry). Exit: a week of converged
   gossip with `transport=iroh` reach-ok logs dominating.
2. **ControlPlane + KnowledgeSearch** — request/response, short
   timeouts, user-visible only as search latency.
3. **ModelTransfer** — large bodies; this is where the optional
   **iroh-blobs** upgrade slots in later (content-addressed,
   resumable, BLAKE3-verified — a strict improvement over
   `/internal/v1/models/file/*` + `X-Sha256`), but the byte-tunnel
   path works first without it.
4. **Inference** — last because streaming latency is the product.
   A/B with the bench suite before and after (wikipedia/SEP judge +
   TTFT comparisons; regression gate = no flip).
- Config (as implemented): `[iroh.transport] gossip = "iroh" | "ip"`
  etc. (nested under `[iroh]`, not a top-level `[mesh.transport]`); the
  installed transport is glassboxed per-resolution (`transport=` field
  already in every trace line, plus a one-time startup log of the
  flipped classes).

### W4 — self-hosted relays

- Deploy `iroh-relay` (fully self-hostable) — one cheap VPS to
  start; `relay_url` in mesh config + gossiped per member. n0's
  public relays remain the bootstrap default; the self-hosted fleet
  is the sovereignty/monetisation end-state (the relay fleet is the
  thing a subscription can sell, à la Nabu Casa).
- Exit: a join + a week of mesh traffic with n0 relay URLs nowhere
  in config or logs.

### W5 — Tailscale becomes optional

Decommission criteria, all required:

- [ ] Every `TrafficClass` flipped and soaked (W3 complete).
- [ ] Join + rejoin proven over iroh from off-LAN (W2).
- [ ] Phone on iroh by default (M4).
- [ ] Relays self-hosted or consciously delegated (W4).
- [ ] The **raw-TCP exception resolved or accepted** (§4).

Then "install Tailscale" moves from prerequisite to optional
appendix in getting-started.md — needed only for the §4 case below
and for ssh/ops convenience. The tailnet is never actively removed;
`IpTransport` remains the default-fallback forever.

---

## 4. What stays on IP (the honest exception)

Distributed inference spawns **third-party binaries** —
`llama-server` / `rpc-server` (`SOVEREIGN_RPC_WORKERS`,
`SOVEREIGN_RPC_TENSOR_SPLIT`) — speaking raw TCP to IPs. An
application-layer transport only covers protocols we own. Options,
deliberately deferred until W5 forces the decision:

- **A. Keep an IP overlay on inference rigs only.** Tailscale/
  WireGuard between the 2–3 GPU hosts; phones and storage-only
  members never need it. Cheap, boring, probably right.
- **B. Tunnel-proxy sidecar.** Per-worker localhost TCP listener ↔
  iroh stream (the same `pump()` the spike uses), with
  `SOVEREIGN_RPC_WORKERS` pointing at the local proxies. Doable;
  adds a hop to the hottest path in the system — needs tok/s
  measurement before adoption.
- **C. Wait for upstream.** llama.cpp RPC transport pluggability or
  iroh TUN-style interfaces may make this moot.

The seam's `RpcWorker` discovery already resolves worker hosts via
`StatusProbe`, so whichever option wins, discovery doesn't change.

---

## 5. Risks and rollbacks

| Risk | Containment |
|---|---|
| iroh RC → 1.0 churn | All symbols confined to `commonwealth-transport/src/iroh.rs` (+ the small mobile arm); pin bumps are one-file PRs. |
| Path quality (relay latency, hole-punch failure on weird CGNAT) | Concatenated-candidates fallback in W3 means a failed iroh dial degrades to the tailnet path per-request, automatically. Phone-first ordering (Track M) measures this before the mesh depends on it. |
| Battery on phone | M2 exit criteria include measured idle drain; rollback is the tailnet host row. |
| Mixed-version meshes | Identity fields are serde-defaulted; `RoutedTransport` only flips classes when configured; a pre-iroh peer is simply never dialed by key (no `node_pubkey`/`relay_url` → `IrohTransport` yields no candidates → IP fallback). |
| Relay outage (self-hosted) | Direct paths survive relay loss for established/LAN pairs; config can list multiple relays; n0 public relays as emergency fallback. |
| Double-copy overhead of byte-tunnels | Accepted for migration; the hyper-on-iroh-streams upgrade (serve axum directly on bi-streams) removes it later without touching call sites — it's all behind `endpoints()`. |

Every W3 flip is a config revert. M3's dual-stack is a row delete.
Nothing in this plan deletes the IP path.

---

## 6. Verification ladder

- **Per phase:** the phase's exit criteria above, plus
  `lint_status`/`test_status` `fresh_passing` (iroh stays
  feature-gated out of default gates until W1 graduates it; at that
  point the feature flips to default-on in the daemon crates and the
  spike e2e joins the normal suite).
- **W3 flips:** ci-bench core gate (`scripts/sovereign-ci-bench.sh`)
  before/after per class; inference flip additionally A/B'd on
  judge + TTFT with real before→after numbers in the PR.
- **End-state e2e (the doc's closing claim, testable):** a fresh
  machine with **no VPN installed** joins the mesh from a deep link,
  gossips, serves knowledge, and answers a chat — and a phone with
  no VPN pairs and streams. When that test passes, "every place we
  rented a coordination plane, we are the coordination plane."
