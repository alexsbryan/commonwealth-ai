<!-- ledger -->

**phase-b-19 · 2026-09-26 · HUMAN-pb-mesh-traffic → option (d) · operator** — this commit
- Needed: HUMAN-pb-mesh-traffic offered three options, and the operator rejected all of them. (a) makes the daemon an inference origin behind cw-rails, (b) keeps the daemon's endpoint until phase-c, and (c) lets the classes go dark. The operator asked for more due diligence and a more principled option.
- Chose: option (d), the first application of phase-b-18's layering rule. Each traffic class moves with the program that owns it, and cw-rails is the node's one endpoint, forwarding to registered origins.
  - A new row, pb-rails-origins, builds the registry, the table-driven acceptor, the generic reach door, declared advertisement and `RailsTransport`.
  - pb-meshapp-apps, pb-mesh-exit-transport and pb-meshapp-grants depend on it.
  - pb-mesh-exit-transport also depends on pb-svrn-dials-serve.
  - pb-mesh-exit-mesh's premise and pb-meshapp-grants' host premise are answered by rule 8.
  - The HUMAN row is marked answered.
- Because:
  - Principles 8 and 12. After pb-svrn-dials-serve the daemon holds no weights, so (a) would make svrn a second owner of serve's manifest. (b) fails the objective's gate 0, and (c) breaks its journeys clause.
  - Principle 11. Three read-only censuses found every part already present (the appendix names them). The row is ~600 lines against (a)'s "well over 1,500", and nobody could find the census behind that figure.
  - Principle 6. All three original options missed the `cwth/http/0` classes, which peers dial on six route families for three owners. (d) routes them by prefix, with no wire change.
  - The queue simulates 27 units to pb-distribution with no unknown dependency. The only halts left are the two lane-measurement HUMAN rows.
  - Boundary gate: 51, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-19 · 2026-09-26 — the mesh-traffic fork resolves to "classes move with their owner; cw-rails forwards to registered origins"

<details><summary>reasoning, evidence, package</summary>

Three read-only censuses ran at 164e87069 through 194e15a49.

**What rides the daemon's endpoint.** The ALPN set comes from sovereign-mesh iroh_access.rs:570, and routing from `forward_for` (:437). By class:
- gossip, join and ring sync belong to cmnwlth;
- corpus control and knowledge search belong to svrn;
- model transfer and rpc-warm belong to serve (bulk);
- the member client (peer inference, streaming) belongs to serve;
- guest is split between svrn and cmnwlth;
- media, apps and offers are declared origins;
- rpc (bulk tensors) belongs to serve.

Identity: peers address this node by the daemon's key, 46d0c1fb…. cw-rails runs solo on its own key, cee1e416…. cw-rails' identity.rs uses the same file name and format, so the flip can carry the daemon's key, and pb-mesh-exit-mesh already requires that it does.

**What cw-rails already has.**
- `spawn_routed`, an ALPN → loopback table (commonwealth-transport iroh.rs:1050).
- `spawn_admitting_forward` (:1097).
- `Forward` (iroh_identity_forward.rs:86-111).
- `admit_spliced_origin` (commonwealth-media identity.rs:99-148).
- `PublishedApps`, a claim/renew/TTL registry (api.rs:333-372).
- The reach door (api.rs:224-293, reach.rs:336-392).
- The trait `PeerTransport::endpoints`, which the daemon constructs at one site (daemon.rs:424-452).

The inventory reviewer estimated what is missing at 500–750 non-test lines. The row carries 600.

**What the design already commits to.**
- FIVE_PROGRAMS.md:44 gives cmnwlth "the node key" and "adverts any origin, inference included".
- Line 45 gives serve its weights and its own OICP manifest.
- pb-serve-program moves the rpc trampolines to serve.
- phase-c STATE:21 has serve's router read cw-rails' roster.

No added hop: both acceptors already splice to 127.0.0.1, and both dial sides already go through per-peer loopback bridges (iroh.rs:969-996). The move changes which process the legs run in, and adds none.

Two findings were routed separately in ce6f5af1c: pb-svrn-dials-serve would darken the peer-facing manifest, and cw-rails never advertises App.

What would falsify this:
- pb-rails-origins' census finds a class that cannot be admitted by table (policy that needs per-request program state inside cw-rails). That class then needs its owner to answer admission, which changes the table's shape.
- The mesh-of-two measurement shows a cross-process loopback leg costing measurably more than the in-process one, on the streaming or bulk classes.

</details>
