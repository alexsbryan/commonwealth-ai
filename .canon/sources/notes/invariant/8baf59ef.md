# A CHILD-DISTRIBUTED PRIMARY'S PLACEMENT MUST BE REPORTED BY THE PARENT, WHICH PLANNED IT — NOT BY THE CHILD, WHICH CANNOT BE ASKED (fixed…

A CHILD-DISTRIBUTED PRIMARY'S PLACEMENT MUST BE REPORTED BY THE PARENT, WHICH PLANNED IT — NOT BY THE CHILD, WHICH CANNOT BE ASKED (fixed 2026-07-29, committed in deda2acf).

THE BUG. `resident_slots()` hardcoded `total_blocks: 0, local_blocks: 0, workers: []` for a primary served by a distributed child process. So `/status` rendered every two-node 122B load as `48 local · 1 node · 0 hops · link=local` — a single-machine load that was not happening.

WHY IT LOOKED UNFIXABLE. The obvious owner of the truth is `LAST_PRIMARY_PLACEMENT`, but that global is written by the loader IN THE CHILD'S PROCESS. The parent cannot read another process's statics, and there is no request/response channel for it. Reading it in the parent yields the parent's own (empty) copy — which is exactly what the zeros were.

THE INSIGHT: THE PARENT ALREADY HAD IT. The parent plans and warms the cut before it ever spawns the child, and persists it as a `DistributionHandoff` at `~/.sovereign/compute-distribution/<name>.json`. The child is handed the cut; it does not choose it. So the placement was never missing — it was sitting in the parent, one accessor away.

THE FIX. Pure `DistributionHandoff::placement()` (sovereign-compute/src/distribution.rs) + `DynamicChildSlot.live_handoff`, set in `respawn_distributed` and CLEARED in `retire`, surfaced via `DynamicChildSlot::placement()` into `resident_slots` (manager.rs). Device i → `endpoints[i]`; an index past the end is the host, matching the loader's plan order (RPC workers first, host last — rpc_distribution.rs:518).

WHY THE CLEAR MATTERS: `live_handoff` must be dropped in `retire`, or a retired child keeps advertising a placement that no longer exists. The handoff FILE deliberately outlives the child (it is the warm-cache key); only the in-memory liveness marker is per-generation.

WHY IT WAS LOAD-BEARING, NOT COSMETIC. `svrn mesh bench` derives `placement_digest` from this report, so before the fix EVERY distributed run was keyed as 1-node-local. Measurements were being filed under a topology that did not produce them, and no lookup could ever match. The first valid two-node record (10.48 tok/s, 36+12 blocks, key pd2:5019ecfb0aa9cf76) exists because of this fix — its 36 local + 12 @BeefyMac line IS the evidence the placement is real.

GENERAL RULE: when a fact seems to live only in a child process, check whether the parent DECIDED it. Anything the parent handed down, the parent still holds.
