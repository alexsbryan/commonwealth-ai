<!-- ledger -->

**phase-b-71 · 2026-09-30 · pb-serve-ranks · director** — this commit
- Needed: the worker stopped pb-serve-ranks at census. Its trial premise ("only the router's sites remain" once pb-svrn-serving-ports lands) was false at 26cfffcc2. Three groups remained: the router, the OpenAI adapter, and the discovery hand-off pb-serve-distributes left behind. The test tree held 20 files (5,578 lines) naming serving-host, where the row priced 7. Measured work was ~7,550 lines against a LIFT of ~2,350.
- Chose: split by proof into three rows, in order.
  - pb-serve-ranks-discovery (~200 lines, compile-time): `mesh_ports` and `RpcWorkerDiscovery` go to the sovereign-stock root. `rpc_workers` reads a handed port, keeps its wire type, and names its absence. The two dead re-exports are deleted.
  - pb-serve-ranks-tests (~5,000 lines, moves), placed per file by what it asserts:
    - route plumbing goes to a contracts `LocalInferenceService` double in the daemon;
    - ranking alone goes to sovereign-serving-host;
    - a daemon route answered by serve goes to sovereign-stock/tests.
  - pb-serve-ranks keeps the router, the adapter, the PROOF and the fp-68 deletion, and now depends on both. Its false trial premise is corrected in place.
- Because:
  - Scope guard phase-b-29 and the charter ("splitting when proofs differ"): the three rows are proved by compile plus mesh-status tests, by moved-test counts, and by the mesh-of-two respectively.
  - Principle 11: the daemon already tests routes against contracts doubles (openai_wire_fidelity.rs:141, turn_reshape_fidelity.rs:587), and sovereign-stock/tests already hosts composed tests (serve_latency_bars).
  - Principle 12: a test goes where the thing it asserts is owned.
  - Principle 6: an absent port is named, never `[]`.
  - Nothing a user sees changes on the stock binary. `rpc_workers` keeps its type because sovereign-serve mesh_plan and mesh_bench read it.
  - BOUNDARY is not re-measured here: this commit changes no Rust.

<!-- appendix -->

## phase-b-71 · 2026-09-30 — pb-serve-ranks splits into -discovery, -tests, then the router row

<details><summary>reasoning, evidence, package</summary>

Evidence, reproduced by the director at 26cfffcc2:

- Edge-deletion trial: drop `sovereign-serving-host` from sovereign-daemon/Cargo.toml:49, then run `cargo check -p sovereign-daemon -p corpus-engine --features corpus-engine/treesitter --lib` in the toolbox, then revert Cargo.toml and Cargo.lock.
  - The run exits 101 with 9 first-pass errors: slot_manifest.rs:8; lib.rs:225, :226, :227; daemon.rs:285, :2493; provider.rs:160, :163; serve_client.rs:105.
  - The worker's run reported 19 errors, since later passes add the bootstrap.rs sites. `grep -rn sovereign_serving_host sovereign-daemon/src` lists the full set: bootstrap.rs:150-151, 196, 732, 771, 833, 850, 852, 861; provider.rs:331; daemon_cmd/serving_boot.rs:20.
- `grep -rl 'sovereign_daemon::slot_manifest\|sovereign_daemon::inference_adapter\|sovereign_serving_host' sovereign-daemon/tests` finds 20 files with 5,578 lines. A per-file count of `SovereignInferenceAdapter` and of router/`CoreSlotManifest` uses sorted them into the three classes in the row.
- `sovereign_daemon::model_fetch` and `sovereign_daemon::worker_eligibility` have no consumer outside the daemon.
- `rpc_workers` is read by sovereign-serve (mesh_plan.rs, mesh_bench) through sovereign-contracts daemon_wire/mesh.rs, so its type must not change.
- The worker's full census is archived at target/ralph/phase-b/pb-serve-ranks-needs-human.phase-b-71.md.

What would falsify this:
- A test file whose assertions need both the daemon's `AppState` and serve's router but that cannot link sovereign-stock, for example because it drives a daemon-private item. That file would need a daemon pub seam, and the -tests row names it at census.
- `mesh_ports` reading daemon state that `EmbeddedDaemon` does not expose publicly. -discovery would then widen an accessor, and must not move state.
- Either new row re-running the edge trial and finding a fourth group of sites.

FIVE_PROGRAMS is not edited. The split changes no boundary or placement rule: the §12 3a composition root and the existing test homes already cover it.

</details>
