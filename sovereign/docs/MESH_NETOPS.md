# Mesh network operations — egress, trust, observability

For the network/security team approving a Sovereign mesh deployment. It answers
the three questions a change-control review asks first: **what does a node talk
to** (egress allowlist), **what is the trust model on the wire**, and **how do
we see what it's doing** (observability). Facts here are pinned to **iroh 1.0.0**
(the transport dependency); the relay/DNS hostnames and ports are read from that
release and change only with a dependency bump.

Scope: the mesh transport (`commonwealth-transport`), not the hub↔user edge
(ordinary HTTPS through your ingress — see `ENTERPRISE_FLEET_DEPLOY.md`).

---

## 1. Egress allowlist by mode

A node's outbound footprint depends on one config knob, `[iroh] discovery`
(plus `[iroh] relay_urls`). Pick the row that matches your deployment.

### Mode A — default (n0 public infrastructure)

`[iroh] enabled = true`, no `discovery`/`relay_urls`. The node uses n0's public
relays and n0's public DNS/pkarr address-lookup.

| Purpose | Destination | Protocol / port | Required? |
|---|---|---|---|
| Relay fallback (carries traffic when UDP blocked) | `use1-1.relay.n0.iroh.link`, `usw1-1.relay.n0.iroh.link`, `euc1-1.relay.n0.iroh.link`, `aps1-1.relay.n0.iroh.link` | **TCP 443** (WebSocket/TLS) | yes — the always-works path |
| Relay QUIC address discovery | same relay hostnames | **UDP 7842** | preferred; falls back to 443 if blocked |
| Direct hole-punched paths (peer↔peer) | peer public IPs (dynamic) | **UDP**, ephemeral ports | preferred; relay covers its absence |
| Peer address publish/resolve (pkarr) | `dns.iroh.link` | **UDP/TCP 53** (DNS) + **TCP 443** (pkarr publish) | yes in Mode A |

Minimum to function on a hostile network: **outbound TCP 443 to the four relay
hostnames + DNS resolution of `*.iroh.link`.** Everything else is an
optimization the relay path covers.

### Mode B — self-hosted relay, n0 DNS still used

`relay_urls = ["https://relay.you.example:443"]`, `discovery` unset. Relays move
to your box; **address-lookup still uses `dns.iroh.link`.** Not a
no-third-party posture — see §2.

| Purpose | Destination | Protocol / port |
|---|---|---|
| Relay | `relay.you.example` | TCP 443 (+ your relay's QUIC port if set) |
| Address publish/resolve | `dns.iroh.link` | UDP/TCP 53 + TCP 443 |
| Direct paths | peer IPs | UDP ephemeral |

### Mode C — sovereign / no third party

`discovery = "none"` (optionally with your own `relay_urls`). Builds from iroh's
`Minimal` preset: **no n0 relay, no n0 DNS — the node contacts no
iroh-operated infrastructure.**

| Deployment | Destinations | Notes |
|---|---|---|
| Flat LAN / single VPC | peers' gossiped addresses only (UDP direct) | no relay needed; drop `relay_urls` |
| Multi-subnet / behind NAT | your own relay (TCP 443) + peer IPs (UDP) | `relay_urls = ["https://relay.internal:443"]` |
| Air-gapped | in-boundary peers + optional in-boundary relay | zero egress beyond the boundary |

This is the mode a sovereignty/air-gap review should require. Verify it with the
startup log (§3) showing `n0_services=false`.

### Always-present (all modes) — the mesh's own listeners

These are **intra-fleet**, never public. Restrict to the fleet subnet.

| Purpose | Port | Bind |
|---|---|---|
| Client API (`/v1`, `/status`) | TCP **9741** | `0.0.0.0` (peers dial it); loopback-only on an encrypted mesh |
| Internal API (gossip, knowledge, corpus) | TCP **9742** | `0.0.0.0`; loopback-only on an encrypted mesh |
| Latency probe | UDP (gossiped port) | LAN telemetry; degradable |
| Distributed-inference RPC (`SOVEREIGN_RPC_WORKERS`) | TCP, operator-set | raw TCP between GPU anchors — see §2 |

On an **encrypted mesh** (`require_encryption`) the 9741/9742 listeners bind
loopback-only and the iroh acceptor is the sole network ingress; nothing above
listens on a routable interface except via iroh.

---

## 2. Trust model — state it plainly

- **Plaintext mesh (default): membership *is* trust.** The join key admits a
  node; once admitted, the internal API (9742) has **no per-request auth** —
  any member can call any peer's internal endpoints. iroh key-dialing gates
  *who can connect*, not *what a member may do*. Appropriate for a fleet you
  operate end to end; **not** a zero-trust boundary between mutually-suspicious
  members.
- **Encrypted mesh (`require_encryption`, founder-set at creation) — recommend
  as the enterprise default.** Every traffic class rides iroh QUIC/TLS in
  fail-closed mode (no plaintext fallback), listeners are loopback-only, dial
  info is signed per node, and joiners are admitted only over a
  founder-key-dialed channel. Trade-off: it is newer and less soak-tested than
  the plaintext path (see the burn-in items in the transport migration plan).
- **What a relay can and cannot see.** Payloads are encrypted end to end; a
  relay (n0's or yours) sees **metadata** — which endpoint keys connect, timing,
  and volume — never prompts, model output, or corpus content. If that metadata
  is sensitive, use Mode C with your own relay (or none).
- **The residual plaintext: distributed-inference RPC.** When a model is split
  across GPU boxes, `llama-server`/`rpc-server` speak **raw TCP** between them,
  outside the mesh transport. This stays on your IP network by design — anchors
  must share a LAN/VPC, which a GPU fleet already does. It is the one path an
  encrypted mesh does **not** cover; do not claim blanket end-to-end encryption
  while a tensor split is running.
- **Proxy support.** The relay's TCP/443 connection honors `HTTP_PROXY` /
  `HTTPS_PROXY`, including **Basic** auth via `https://user:pass@proxy:443`.
  **NTLM and Kerberos proxies are not supported** (upstream iroh limitation) —
  those networks need a Basic-auth-capable egress proxy or a direct 443 allow.

---

## 3. Observability — confirm it from the node, not by inference

- **Startup egress log.** Every endpoint logs one line at bind (target
  `transport`, INFO):
  ```
  iroh egress posture ... n0_services=<bool> relays=<n0-default|none|your-urls> proxy=<redacted|none>
  ```
  Grep it to confirm, per node, exactly what the node will touch. `proxy=` shows
  the proxy in use with credentials redacted.
- **`sovereign doctor`** includes an `iroh_egress` check: reports whether mesh
  traffic is on iroh and via which path, plus the proxy posture.
- **`sovereign mesh transport`** (or `GET /v1/mesh/status` → `iroh_transport`):
  per-peer live path — `direct` (hole-punched), `relayed` (via a relay),
  `mixed`, or `idle`. This is the "is anyone actually on the relay?" surface.
  Empty means the mesh is on the IP path (no iroh).
- **`GET /v1/mesh/status`** also carries member liveness, in-flight load, and
  (on a shared-model fleet) the elected host — scrapeable for a dashboard.

---

## 4. Approval checklist

- [ ] Deployment mode chosen (A/B/C) and the matching egress rules applied.
- [ ] For a no-third-party requirement: `discovery = "none"` on every node,
      confirmed by the startup log showing `n0_services=false`.
- [ ] Encrypted mesh (`require_encryption`) if members aren't mutually trusted.
- [ ] 9741/9742 (and any RPC ports) restricted to the fleet subnet.
- [ ] Proxy is Basic-auth-capable (or direct 443 allowed); NTLM/Kerberos ruled
      out or fronted.
- [ ] GPU anchors for distributed inference share an IP network (LAN/VPC).
- [ ] Dashboard scrapes `/v1/mesh/status`; `sovereign doctor` clean.

---

## 5. Open validation (not yet proven — do not represent as tested)

The transport is unit/e2e/soak-verified in a loopback namespace, but these
real-network proofs are outstanding (see `~/.claude/plans/mesh-enterprise-hardening.md`):

- Two machines on genuinely different networks, no VPN, one behind symmetric NAT
  (runbook: `docs/runbooks/TWO_NETWORK_JOIN_TEST.md`).
- A self-hosted `iroh-relay` proven to carry a mesh with n0 disabled.
- The OS-level all-UDP-drop firewall + an authenticated forward proxy in front
  of the relay, on real infrastructure.

**Partially proven:** the load-bearing "UDP unavailable → the relay carries
traffic over TCP" behavior IS covered by a hermetic test
(`commonwealth-transport` `relay_tcp_only`, feature `iroh-test-utils`): a relay
with QUIC/UDP disabled + endpoints with all direct paths cleared complete a
round-trip. Basic-auth proxy support is verified in the iroh-relay source. What
remains is the field variant (real firewall + real proxy) above.

Until the field tests pass on real infrastructure, treat the un-proven rows as
*intended* posture — validated by construction, source review, and the hermetic
relay/UDP test, not yet by field test.
