<!-- ledger -->

**phase-c-3 · 2026-10-02 · pc-rpc-probe-identity · seat, ruling the lane's mechanism fork** — this commit
- Needed: the lane's census (400f8f1cc, nothing built) found the row's outcome unreachable as worded: raw ggml RPC has no identity a probe could check (launch.rs:620), and by default the worker binds loopback (launch.rs:593) while serve always advertises `rpc_port` with `rpc_iroh: true` (rails_mesh.rs `anchor_claims`), so in `auto` mode a host's first direct probe of `<member IP>:<rpc_port>` can only ever reach something that is not the worker, and holds it sticky. It left three mechanisms in ctl/NEEDS_HUMAN.md, untracked.
- Chose: A. `AnchorProfile.rpc_direct` (skipped when false), set by serve only for a non-loopback bind the operator allowed; the direct probe and the record-port probe-host fallback run only for a worker that declares it; a record without the field is bridge-only; a pre-cut `/status` port keeps main's path, traced as unproven. C (an identity on the direct path, the tunnel-proxy sidecar of transport lib.rs:73) is phase-d's new pd-rpc-direct-identity. B is not taken.
- Because: A is the only option that closes the hole for every default member and costs none of them anything (their direct path never reached the worker), and it is not a charter fork: no leaf, exception, ratchet raise or user-visible change. The lane's open sub-question (legacy records) is answered by `git grep rpc_iroh 18f783f44 -- oicp-types` = nothing: main's AnchorProfile has neither field, so every record lacking `rpc_direct` is a cut build carrying `rpc_iroh`, and bridge-only loses no peer. B makes a deliberately LAN-bound worker pay QUIC on every tensor transfer, a throughput change users see.

<!-- appendix -->

## phase-c-3 · 2026-10-02 — pc-rpc-probe-identity takes the declared-bind gate; an identity on the plaintext path is phase-d's

<details><summary>reasoning, evidence, package</summary>

Seat's check of the census at c02715799: `select_rpc_endpoint` (sovereign-serving-host rpc_discovery/endpoint.rs:20-45) tries the bridge first only under `SOVEREIGN_RPC_TUNNEL=always`, otherwise `reachable_rpc_endpoint` (a bare TCP connect, :69) before the bridge, then the probe host; `anchor_claims` (sovereign-serve rails_mesh.rs:534) sets `rpc_port: Some(..)` and `rpc_iroh: true` unconditionally; `RpcServe::resolve` (sovereign-contracts launch.rs:593-597) refuses a non-loopback bind without `allow_plaintext_lan`. The lane's package follows verbatim; its ctl/ copy was removed so the wave would not halt on a ruled fork, and the lane, which has no commits, fast-forwards onto this commit when it resumes.

### pc-rpc-probe-identity: raw ggml RPC has no identity to probe; the fix is a fork the row does not state

#### (a) Unit and row

`pc-rpc-probe-identity` (ralph/next/phase-c/STATE.md:48): "the RPC worker's
direct-IP probe proves the peer it reached is the member it meant, not any
process answering the port ... census first (which identity cw-rails already
holds for that member, and whether the tensor bridge's own handshake already
binds it)."

#### (b) Census, at 400f8f1cc (read only, nothing built)

1. The premise holds. `reachable_rpc_endpoint`
   (sovereign/crates/sovereign-serving-host/src/rpc_discovery/endpoint.rs:69)
   accepts the first `TcpStream::connect` to `ip:rpc_port` that succeeds in
   600 ms. The candidate IPs are `dial.addresses`, or `dial.iroh_direct_addrs`
   when those are empty (`direct_candidates`, :50, from bc323187d).
2. The identity cw-rails holds for the member is its iroh key,
   `PeerContact::node_pubkey`. Its direct addresses travel in signed dial info
   (commonwealth-rails gossip.rs:295 `sign_dial_info`), so the IP list is the
   member's own claim. What answers at that IP and port is not proven.
3. The tensor bridge already binds identity. `bridge_rpc_endpoint`
   (rpc_discovery.rs:213) goes through `IrohTransport::endpoints`
   (commonwealth-transport/src/iroh.rs:328), which returns nothing without
   `node_pubkey` and otherwise bridges over QUIC to that key on `RPC_ALPN`.
   A process that does not hold the member's key cannot complete it.
4. The direct path cannot bind identity as things stand. The far end is
   ggml's `ggml_backend_rpc_start_server`
   (sovereign-inference/src/rpc_worker_main.rs:217), which "authenticates
   nothing and encrypts nothing" (sovereign-contracts/src/launch.rs:620). The
   protocol has no channel a host could use to ask "are you member M?".
   commonwealth-transport/src/lib.rs:73 calls RpcTensor "the one remaining
   plaintext path" and says closing it "needs a tunnel-proxy sidecar, which
   nobody has built."
5. This makes it worse than the row says. By default the worker binds
   LOOPBACK. `RpcServe::resolve` (launch.rs:577ff) refuses a non-loopback
   `SOVEREIGN_RPC_SERVE` unless `allow_plaintext_lan` is set. Serve still
   advertises `rpc_port` with `rpc_iroh: true` (sovereign-serve
   rails_mesh.rs:534-546), and that record says nothing about whether the bind
   is reachable from off-host. A host in `auto` tunnel mode (the default,
   rpc_discovery.rs:39) therefore probes `<member LAN IP>:<rpc_port>` first.
   For a default-configured member, the only thing that can answer there is
   NOT the worker. If anything does, the host hands ggml that endpoint as
   `direct-ip`, records it as the member's (`rpc_endpoint_nodes`), and holds
   it sticky across later probe misses (`Reaffirm::Held`).

No build, test or PLANT ran: the census stops before code (§3 step 2, §6).

#### (c) Decide

1. **Mechanism.** No probe-side check can prove identity over raw ggml RPC.
   Pick one:
   - **A (recommended): the worker declares a reachable bind.** Add
     `rpc_direct: bool` to `AnchorProfile` (oicp-types/src/capabilities.rs:201,
     skipped when false, like `rpc_iroh`). Serve sets it only when
     `RpcServe` resolved a non-loopback bind, which means the operator
     acknowledged plaintext LAN. `select_rpc_endpoint` (endpoint.rs:20) then
     runs the direct probe only for a member declaring it, and otherwise goes
     straight to the iroh bridge, which does bind identity. No default-config
     member loses anything, because its direct path could never reach the
     worker. A member that opted into plaintext LAN keeps the raw-TCP fast
     path, and it stays unproven inside the boundary the operator declared;
     the trace should say so. This is a wire-field addition, so api-gate and
     the daemon `/status` `rpc_worker` record (routes_status.rs:289) need the
     same flag. The open question is legacy records with no field: treat them
     as declaring nothing (bridge only), or probe as today and log it?
   - **B: bridge-first by default.** Make `auto` mean the bridge first whenever
     `rpc_iroh` and `node_pubkey` are present (today that is `always`,
     rpc_discovery.rs:37). It is smaller, but a LAN worker that deliberately
     binds the LAN pays QUIC on every tensor transfer. That throughput change
     is visible to end users, and per the charter it is the operator's call.
   - **C: an identity handshake on the direct path.** This is the
     tunnel-proxy sidecar named in transport lib.rs:73: a new component and
     an architecture question. It belongs in phase-d, not a phase-c row.
2. **Row rewrite.** For A, the outcome should read: "the direct probe runs
   only for a worker whose record declares a reachable bind; every other
   worker is reached over the identity-bound bridge." Proof: a test where a
   stranger listens on the member's direct address and the worker declares
   loopback, and the chosen endpoint is the bridge, not `direct-ip`. PLANT:
   drop the `rpc_direct` gate, and `direct-ip` is chosen. LIFT is about 60
   lines across oicp-types, sovereign-serve and sovereign-serving-host, plus
   tests.
3. If C is chosen, the row moves to ralph/next/phase-d/STATE.md as a `pd-`
   row, and phase-c marks this one with that pointer.

#### (d) Resume

Edit or mark the row in ralph/next/phase-c/STATE.md, then
`rm ralph/next/phase-c/ctl/STOP ralph/next/phase-c/ctl/NEEDS_HUMAN.md`.

</details>
