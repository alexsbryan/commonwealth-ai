# serve / cmnwlth / sovereign-mesh: disentangling census

Seat, 2026-09-26, at 7c2573bf3..c2529c94c. Method: the `cargo metadata`
crate graph, labelled by ARCH_LAYERS package, and three read-only `git grep`
module maps. The seat re-checked four load-bearing claims by hand (§3 port,
§5 divergence, §8.1, `worker_state`). Code intel was not used; its graph is
frozen at 2026-09-23 (note 54002735). Rows cite this file; it is data, not a
decision (decisions stay in `ralph/decisions/`).

## 1. What the knot is

- cw-rails does not link any of the three crates. `commonwealth-rails` and the
  other `commonwealth-*` crates depend in-repo only on `commonwealth-*`,
  kernel-types, oicp-types, host-kit and oplog. None of the nine manifests
  under `commonwealth/` names sovereign-mesh, sovereign-scheduler or
  sovereign-serving-host.
- sovereign-mesh (16,560 lines, 30 modules) is the svrn daemon's in-process
  fabric library.
  - Its reverse deps are sovereign-daemon, sovereign-cli-mesh and
    sovereign-cli-llm, and nothing else.
  - The daemon starts its loops: daemon.rs:4047 `spawn_ring_sync_loop` and
    :4057 `spawn_plane_seal`.
  - Its own lib.rs:2-6 still says it "embeds the Commonwealth daemon as a
    library".
  - ARCH_LAYERS files it under `[cmnwlth]` (:1348). The label is an open
    question in FIVE_PROGRAMS_DECISIONS.tsv:3.
- A `serve` package trial adds six edges (phase-b-21: 49 → 55). Four start in
  sovereign-mesh or its test harness. The other two are cli-mesh → inference
  and serving-host → commonwealth-core.

## 2. Target layering

| layer | crates | owns |
|---|---|---|
| leaves | kernel-types, oicp-types, sovereign-contracts, host-kit | ids (NodeId); the federation wire (manifests, the model-transfer listing); venue types, the slot-alias policy and the `LocalInferenceService` port; mechanism |
| serve: engine | sovereign-inference, serving-policy | engine, placement arithmetic (fit, split, overheads) |
| serve: assembly | sovereign-compute | one engine assembly, compute child |
| serve: local serving | serving-host group L | the OpenAI wire, slot pick, own manifest |
| serve: routing | serving-host group P, sovereign-scheduler, the measurement store | ranks venues (phase-b-21) and reaches peers only through cw-rails' reach door |
| serve: faces | sovereign-serve | HTTP routes and the CLI face (weight verbs, plan, bench) |
| cmnwlth | commonwealth-*, cw-rails | node key, endpoint, roster, **peer admission**, adverts, journal carriage of opaque namespaces, reach door (FIVE_PROGRAMS.md:219-221) |
| svrn | sovereign-daemon and the rest | dials serve for inference and cw-rails for the mesh |
| — | sovereign-mesh | dissolved: every module goes to an owner (§7) |

## 3. serving-host (18,629 lines) splits cleanly

- **L, local serving (~6.6k lines).**
  - Modules: inference_adapter, fim_adapter, oicp_synthesis, slot_select,
    slot_manifest, prompt_compactor, tool_profile, source_content_validator,
    openai_http.
  - Runs in the daemon and in serve.
  - Its only reach past the leaves is `sovereign_scheduler::slot_aliases::advertised_alias_ids`
    (oicp_synthesis.rs:249,307). That is a 13-line function over contracts'
    `SLOT_ALIAS_POLICY`.
- **A, admission (~1.4k lines).**
  - Modules: admission, state.
  - It mixes in PEER admission: admission.rs:527 `peer_admission_layer`, :541
    `peer_knowledge_read_layer`, and state.rs:253-336 (peer_sched, peer_tally,
    reciprocity). FIVE_PROGRAMS.md:219-221 gives peer admission to cw-rails.
  - serve uses only the 503 renderer.
- **P, peer reach and ranking (~10.6k lines).**
  - Modules: peer_inference (+provider_impl), router_builder, venue_host,
    local_inflight, throughput_tracking, recorder, ledger, guest_lender,
    pinned_transport, pinned_worker_source, pinned_pod_snapshot,
    entry_endpoint, model_fetch, worker_eligibility, worker_state.
  - It runs ONLY in the svrn daemon today.
  - It holds every `commonwealth_core` reference in the crate.
- **P → L edges.**
  - There are three: peer_inference.rs:71-72 (`build_self_manifest`,
    `SlotManifest`) and router_builder.rs:15.
  - All three exist because the router needs its own node's manifest.
  - The port for that already exists: `LocalInferenceService::provider_manifest`
    (contracts local_inference.rs:68).
- **serving-host → commonwealth-core, 15 lines.**
  - `ids::NodeId` at six sites. It is `pub use kernel_types::NodeId`
    (commonwealth-core ids.rs:18), so the switch is mechanical.
  - `model::{ModelFileInfo, ModelFileListing, models_list_url, model_file_url}`
    (model_fetch.rs:25,62,97). These are the node-to-node model-transfer wire.
    Its server side is the daemon's routes_internal/model_files.rs:57.
  - `peer_health::PeerHealthTracker` (peer_inference.rs:431,698). It is
    std-only. Its only other user is mesh_sim; no `commonwealth/` crate
    names it.

## 4. sovereign-scheduler (6.9k lines) is pure ranking arithmetic

- It defines no wire types and writes no file.
- Its decision log is written by serving-host recorder.rs:45-50, and only when
  `SOVEREIGN_DECISION_LOG` is set. Nothing but tests reads it back.
- So the "decision log" in §2's `cmnwlth` cell is not this log.
- `venue` is a re-export of contracts.
- mesh_sim (sovereign-mesh-test-harness) runs the real `scheduler_core::rank`.
  It is the ranker's test instrument.

## 5. `svrn mesh plan` and `bench`

- Both talk only to the svrn daemon (:9741). They never talk to cw-rails
  (mesh_cmd.rs:449-452, mesh_travel.rs:42-46).
- The arithmetic is already serve's: sovereign-inference rpc_warm_cache.rs and
  rpc_distribution.rs.
- **Divergence (principle 8).**
  - The loader weights each device by `quantize_vram(effective(i, b))`: VRAM
    minus the compute buffer, the host extra and the context share
    (rpc_distribution.rs:1081-1100).
  - The preview weights by `quantize_vram(vram[d])` on raw VRAM
    (mesh_cmd.rs:1256 at c2529c94c; :1327 at 7c2573bf3), under comments that
    claim the SAME call (:1250, :1260 at c2529c94c).
  - The aggregate gates differ too: the loader's is RD:2081-2084 (a floor plus
    a quorum); the preview's is MC:1303,:1400 (neither).
  - The plan tests pass only `overheads: None`.
- bench runs no model itself. It drives the running engine through `/status`,
  placement and `device_memory`. serve has no such route yet.

## 6. Measurements

- cw-rails carries `mesh-measurements` as opaque signed journal lines
  (commonwealth-state peer_preferences.rs:263). It holds no record type.
- Every validation, per-key cap, dedupe and roster check runs in the daemon or
  the CLI:
  - daemon mesh_http.rs:649-800, bootstrap.rs:1315-1346, and sovereign-mesh
    rail_kv_pump.rs:323;
  - the CLI's mesh_bench.rs, which mints the Verdict.
  - So phase-b-22's "cw-rails' rail validates and caps" describes the daemon's
    code, not cw-rails'.
- mesh_measurements.rs names only serde, sha2, tracing and
  `sovereign_contracts::rebrand`, so it moves whole.

## 7. sovereign-mesh modules and their fate (PROPOSED; a dissolution census verifies each)

| fate | modules (lines) | consumers outside the crate |
|---|---|---|
| duplicates of cw-rails' endpoint; retire with pb-mesh-exit-* | gossip 1543, iroh_access 1652, iroh_watchdog 1043, join 704, mesh_discovery 475, ring_sync 679, rail_kv_pump 588 (the daemon's sealer; cw-rails kv.rs mirrors it), rail_bind 64, ring_checkpoint 41 | daemon; cli-mesh (discovery, checkpoint) |
| serve (phase-b-22) | mesh_measurements 3054, measurements_rail 792, measurements_wire 54 | daemon, cli-mesh |
| svrn: the daemon's dial ports and guest door | rail_port 316, ledger_port 674, peer_adapter 253, guest_pages 312, guest_source 184, guest_tunnel 134, canonical_pull 330, deep_link 333 | daemon, cli-mesh, cli-llm, core |
| split by owner | capabilities 490 (serve declares, cw-rails advertises), state 184 (UI MeshState), fabric 744 (FabricPart), persist 979 (node id is already `contracts::node_identity`), ring_roster 699, media_route 137 | daemon, cli-mesh, cli-llm, grants |
| delete | 13 re-export shims in lib.rs. decision_replay, predicted_time and worker_pod have no user today | daemon, cli-llm, cli-mesh |

## 8. Findings no row names

1. **Peer admission.** It sits in serving-host's A group, owned by the
   daemon: admission.rs:527,541 and state.rs:253-336. FIVE_PROGRAMS.md:219-221
   gives it to cw-rails.
2. **Namespace narrowing is enforced in neither process.**
   - `MeshRosterSource::install` (sovereign-mesh ring_roster.rs:322) is called
     only from daemon tests.
   - cw-rails' `MembershipRosterSource` registers no namespace (rail.rs:119-128).
   - Only the CLI and the guest door refuse a narrowing roster.
3. **No row dissolves sovereign-mesh.** The rows only cut its consumers, and
   principle 12 says a crate whose uses go to zero while its ability stays was
   drawn wrong.
4. **Small items.**
   - serving-host worker_state.rs (87 lines) has no user.
   - scheduler lib.rs:4 says it "reads no clock", but yield_backoff.rs:81,85,118
   call `Instant::now()`.
   - measurements_rail.rs:312-317 compiles `pub mod tests` into production.
   - sovereign-mesh's `dst` feature is enabled by no manifest, yet ci.yml:599
     relies on it.
   - A module cycle: fim_adapter ↔ inference_adapter.
