<!-- ledger -->

**phase-b-36 · 2026-09-28 · Phase B → the IP overlay is dropped at the flip; the bigger-model flow's direct RPC path is kept and proved · operator**
- Needed: pb-rails-reach halted on two premises. The director (phase-b-35, 6408eb1ca) split the guest dialer into pb-reach-guest and moved the IP overlay to pb-mesh-exit-transport unchanged: an overlay listener, a plaintext admission rule and a posture gate that no document specifies. The operator asked for due diligence ("I think you're over complicating") and for skepticism of worker reports.
- Found (the seat, verified in the tree):
  - Guest dialer: phase-b-30 Group 3 placed it in mesh-reach. `guest_tunnel.rs` is 134 lines, but it imports commonwealth-transport's iroh machinery (:38), and the director's compile trial moved ~650 lines with EXIT=0 and BOUNDARY 41 unchanged. The split stands. Its id lost the parent prefix, so the seat listed pb-reach-guest in scope.txt as a split.
  - IP overlay: an encrypted mesh routes every class over iroh with no IP fallback (daemon.rs:4404-4425). cw-rails founds only encrypted meshes (found.rs:7-9). The operator's one mesh is encrypted (its invite carries `exp=`, daemon.rs:1343). `svrn mesh create` still defaults to plaintext (mesh_cmd.rs:2476), which the worker's report did not say.
  - The bigger-model flow: ggml RPC tensors are `TrafficClass::RpcTensor`, and `IpTransport` returns no candidates for that class (mesh-reach lib.rs:63-70). But discovery's direct-IP probe reads `m.dial.addresses` (daemon.rs:2763), which cw-rails' roster leaves empty (gossip.rs:471, join.rs:172). After the flip the direct LAN/Tailscale path for a split model would have vanished silently.
- Chose (operator: "I'm fine dropping it, I just want to make sure we don't silently kill the RUN A BIGGER MODEL flow"):
  - The flip drops the overlay. The daemon's plaintext fallback retires with its endpoint.
  - `svrn mesh create` founds encrypted. Migrating a plaintext mesh is refused by name.
  - The flip's PROOF runs a model split over ggml RPC across the lift's two nodes after the flip, naming its path. PLANT: drop RpcTensor from cw-rails' reach table.
  - pb-serve-distributes keeps the direct probe by reading the member's iroh direct addresses when `dial.addresses` is empty. A test and a PLANT pin it, and a loss is named if cw-rails' record lacks the addresses.
- Because: principle 6 (the plaintext default change and the direct-path risk are named, not silent), principle 11 (iroh already carries the LAN/Tailscale IPs the probe needs), principle 5 (the bigger-model flow is proved across the flip, not assumed). Found for phase-c, not this queue: the direct-IP probe sends raw ggml RPC bytes even on an encrypted mesh, bypassing `require_encryption`'s promise.
