<!-- ledger -->

**phase-b-22 · 2026-09-26 · pb-serve-cli-face → placement measurement is serve's; row narrowed, pb-serve-placement split off · director** — this commit
- Needed: pb-serve-cli-face stopped at census before any code. The original scope measured ~8,700 lines, not ~1,200, and `plan` and `bench` are built on sovereign-mesh's placement-measurement store, which cw-rails' rail also reads. Moving them to serve as-is would give serve a `sovereign-mesh` edge.
- Chose:
  - The measurement store and its wire are serve's. cw-rails carries the `mesh-measurements` namespace as registered data, with no measurement vocabulary of its own (§4 rule 8). `hardware_fingerprint` goes to kernel-types.
  - pb-serve-cli-face narrows to `warm-cache` and `fetch-model`, which move now to sovereign-serve's binary with their `svrn` spellings kept (~400 lines).
  - A new row, pb-serve-placement, depends on pb-serve-cli-face and pb-rails-origins. It moves `plan`, `bench`, remote_gguf, mesh_travel and the store as a series (~12,000 lines, nearly all moves). pb-serve-package now depends on it.
- Because:
  - FIVE_PROGRAMS §2 gives serve "its placement config". A measurement record is the observed outcome of a placement (model fingerprint × placement digest × machine witness), and phase-b-21 already placed the planner in serve under rung 1.
  - Sharing records across the fleet is mesh-facing, so rule 8 answers where it lives: the owner holds the data and cw-rails forwards it. Today rail_kv_pump.rs:323 loads serve's file at seal time. That is a component holding another's lifecycle (principle 12).
  - The split is by proof. warm-cache and fetch-model add no edge today. Moving the store needs the rail to carry a registered namespace, which is pb-rails-origins' work.
  - Boundary gate: 49 at d23dcf3ef (EXIT=1), unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-22 · 2026-09-26 — placement measurement is serve's and cw-rails carries its namespace; the weight verbs move first

<details><summary>reasoning, evidence, package</summary>

The worker's facts, reproduced at d23dcf3ef:
- `wc -l`: mesh_cmd.rs 5,247, mesh_bench.rs 2,288, mesh_bench/tests.rs 1,611, remote_gguf.rs 444, mesh_travel.rs 400, sovereign-mesh mesh_measurements.rs 3,054, measurements_wire.rs 54.
- `run_mesh` (mesh_cmd.rs:28-67) dispatches `warm-cache`, `plan`, `bench` and `fetch-model` alongside the roster verbs.
- warm-cache's only crate uses are `sovereign_inference::embedded::{default_cache_dir, warm_cache_from_gguf}`.
- fetch-model uses `sovereign_mesh::model_fetch::{list_peer_files, fetch_model_to_dir}`, `SetupConfig`, reqwest, and a read of `sovereign_root()/mesh.json` for peer URLs (:3711).
- `git grep mesh_measurements` outside cli-mesh finds sovereign-mesh's measurements_rail.rs, rail_kv_pump.rs:154,323, ring_roster.rs:252 and state.rs:62. It also finds the daemon's mesh_http.rs:578-764 and bootstrap.rs:1340, daemon tests that name `MEASUREMENTS_APP_ID`, and two doc comments.
- measurements_rail.rs interprets the records: `mm::to_wire`/`from_wire`, `wire_key` and `MAX_RUNS_PER_KEY`.
- sovereign-mesh/Cargo.toml:10 names commonwealth-core.
- sovereign-serve's Cargo.toml names only inference, compute, serving-host and host-kit.
- `cd corpus-engine && cargo xtask boundary-gate` in the toolbox reads 49, EXIT=1.

The package's three options:
- (i) The store stays cmnwlth's and serve names it. Refused: serve → sovereign-mesh → commonwealth-core fails pb-serve-package's other half ("the one who wants serve takes no commonwealth-core"). Its variant, a shared crate for the record types, is a new leaf, which is the operator's.
- (ii) The store is serve's. Chosen. It follows from §2 plus rule 8, and phase-b-21 had already put placement in serve. The package called it "a much larger row". Size is not a reason to leave an owner wrong. The size is handled by splitting on proof and landing as a series.
- (iii) A serve verb emits GGUF facts, and cli-mesh keeps `plan` and `bench`. Refused: it keeps placement in cmnwlth against §2, and it adds a process wire that no row names.

Face: warm-cache and fetch-model go on sovereign-serve's binary. It already links what they need, and principle 11 applies: reuse the binary rather than mint a crate. For plan and bench the choice is left to pb-serve-placement's census. A CLI crate inside the serve package is not a leaf.

What would falsify this:
- pb-serve-placement's census finds that cw-rails needs to interpret a measurement record in order to answer a peer. Examples would be admission or a gossip decision keyed on it, as opposed to carrying and deduplicating it. Then the data is shared, and it goes to the operator.
- The `svrn mesh warm-cache` or `fetch-model` spelling cannot be kept through the dispatcher. That is end-user-observable, so it goes to the operator.
- Moving the store changes the measurement file's path or format. User data would move without a migration, which is the operator's call.

REVIEW-AFTER: the rail currently validates records and caps them per key (`MAX_RUNS_PER_KEY`) on its own side. This decision moves both checks to serve, before serve appends. A peer on older code that appends an invalid record would then be refused only by readers. Check that `from_wire`'s read-side refusal still holds for that peer.

</details>
