# phase-b — the ralph queue (RUNNING since 2026-09-26, phase-b-3)

Designed by the seat with the operator on 2026-09-25 (ralph/decisions/phase-b-1.md).
It replaces the single plan-and-mint row staged on 2026-09-24 (five-programs-39):
the rows below are OUTCOMES (ralph/PROMPT.base.md §2), each with its own proof, and
there is no mint step. Nothing runs this queue until the operator launches it at
five-programs' `HUMAN-phase-b` row. Launch line: this directory's queue.toml header.

What Phase B is: making every program takeable ALONE. "The next developer who wants
THIS but not THAT" (operator, 2026-09-25). It builds the owners the five-programs
exceptions stood in for, and it never adds one. Design: docs/FIVE_PROGRAMS.md §1,
§2 (six programs: `serve` is split out of `cmnwlth`), §2c (distributions, the host
kit, the compose rule), §4 rules 6-7, and §12 3a (the ladder, with the mechanism
rung). Finish: FIVE_PROGRAMS §12 "Done".
- boundary-gate exits 0;
- no `package = "svrn"` exception remains;
- all six lift sandboxes pass;
- each §2c drive has one implementation.

Scope (phase-b-2, 2026-09-25): only the rows the finish needs. Six rows that close no red edge and make no lift pass moved to the staged follow-on queue ralph/next/phase-c/, with their design kept: the Url venue, the inference origin, bench's three dials, the contracts re-home, the provider split and the per-program config files. Mechanical moves use `cargo xtask refactor-apply`. Each program's "runs alone" proof is its RUN smoke in `scripts/program-lift.sh`. Dead code is deleted before anything ports it. The svrn daemon adopts the shared shell and MCP dispatcher LAST, after it has shrunk.

Order: collapse before split (principle 8). The host kit, the one MCP dispatcher
and the one serving assembly land before any host is stood up. Hosts come before
their clients. Every row names the owner it extends; the appendix maps every red
edge to exactly one row.

**Pre-registered bars (set 2026-09-25, before any data; principle 7).** They apply
to pb-svrn-dials-serve.
- Serving over loopback vs in-process, n ≥ 5 runs each, same host, same model:
  first-token latency p50 at most 10% slower, and embedding throughput at batch 32
  at least 90% of in-process.
- `svrn quality check --lane retrieval-prod` and the synth lane stay inside their
  noise bands (sovereign/docs/RUNBOOK.md §6).

A miss is NEEDS_HUMAN with the numbers, never a re-tuned bar.

## Rows

- [x] pb-handover-first 075beac8a — depends [] — OUTCOME: an existing install's first boot at HEAD serves the ring history it already had (phase-b-3). On a copy of the operator's `~/.svrnmesh/rings` (10 namespaces, 14 MB), the HEAD daemon brought cw-rails up at boot (daemon_cmd/boot.rs:469 `ensure_rails`, fp-solo-clients). cw-rails logged `kv: rebuilt the store from the journals on disk namespaces=0` 380 ms BEFORE `start_daemon`'s one-time handover (daemon.rs:3156 `rail_migration::migrate_journals_to_rails`) moved the journals under it. cw-rails never rebuilt, so every namespace read empty. The broken contract is the handover's own: rail_migration.rs:4-7, "before any rail surface answers".
  - The handover runs BEFORE any bring-up, in every caller that brings cw-rails up. `ensure_rails` takes the daemon's data dir, resolved through its one existing resolver, and hands over first. Both callers do this: boot.rs, and cli-llm legacy_store.rs `rails_kv()`, since `svrn portfolio` can be the first cw-rails start on an upgraded host. start_daemon's call MOVES; it is never duplicated (principle 8). The in-process EmbeddedDaemon tests keep passing unchanged.
  - When a cw-rails already answers on the base and `<data_dir>/rings` still holds a namespace, the handover moves nothing under the live store. It leaves the namespace in place, and a warn event names the namespaces that wait and says cw-rails must restart to take them (principle 6). `migrate_media_to_rails` follows the same rule.
  - PROOF: a sovereign-daemon process-level test boots the real binary on a temp data dir seeded with a two-namespace `rings/` fixture (NEVER the operator's), with `rails_base` pinned and `CW_RAILS_DIR` set. On the FIRST boot, it reads a seeded row back through cw-rails. PLANT: restore today's order (handover after bring-up) → red. A second test runs cw-rails before the handover: the rows stay at the source, and the warn event names them.
  - Premise: `grep -n 'migrate_journals_to_rails' sovereign/crates/sovereign-daemon/src/daemon.rs` hits once, and `grep -n 'ensure_rails' sovereign/crates/sovereign-daemon/src/daemon_cmd/boot.rs` hits once and runs before `start_daemon`.
  LIFT ~250 lines. — read: ralph/decisions/phase-b-3.md, sovereign-daemon/src/rail_migration.rs, sovereign-daemon/src/daemon.rs:3140-3170, sovereign-daemon/src/daemon_cmd/boot.rs:455-480, sovereign-daemon/src/rails_client/bring_up.rs, sovereign-cli-llm/src/legacy_store.rs:15-35, sovereign-daemon/tests/solo_rails_e2e.rs — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli-llm), PLANT, LAYER, BOUNDARY (delta 0 from 51)
- [~] pb-rails-ready — depends [] — OUTCOME: cw-rails answers nothing — neither its readiness path nor a store door — until its store is projected from the journals on disk. A client's first read after any cw-rails start then never gets a false absence (phase-b-5). Found in pb-handover-first (075beac8a). `RailsDaemon::run` spawns `kv::run_forever` (commonwealth-rails lib.rs:437), which projects the store (kv.rs:527 `project_all_on_disk`) while the API is already serving. `ensure_rails` declares ready on `/v1/mesh/status`, so a first read inside that window answered absent. The proof flaked 1 in 3 until it waited for the "rebuilt the store" log line.
  - The first projection completes before the listener serves, and the pump loop then continues as today. Readiness is cw-rails' own fact (principle 12).
  - One `info` event names the projected namespace count and the projection's duration, ahead of "serving".
  - The wait for the rebuild line in sovereign-daemon tests/solo_rails_e2e.rs goes, because ready now means projected.
  - MEASURE the added startup latency on a synthetic store of the operator's size (10 namespaces, ~14 MB; never the real store), and paste it.
  - FOLDED (phase-b-6, director): MEASURE found ~9.6 ms per journal line, all of it in `journal.admit` (ed25519 unoptimized in the dev profile the deployed daemons run). A copy of the operator's store (12,863 lines) took 117.9 s, which would blow `RAILS_BRING_UP_WINDOW` (10 s). The root Cargo.toml builds curve25519-dalek, ed25519-dalek and sha2 at opt-level 3 in dev: 1.74-1.88 s over three runs. The window stays as it is, and its doc now names projection. ready.rs seeds 1,000 rows so the PLANT goes red at start 0.
  - PROOF: a commonwealth-rails test seeds a journal large enough that projection takes measurable time. It reads a seeded KV row the moment `/v1/mesh/status` first answers, over 20 starts, and every read returns the row. PLANT: restore spawn-before-project → red within the 20.
  - Premise: `grep -n 'tokio::spawn(kv::run_forever' commonwealth/crates/commonwealth-rails/src/lib.rs` hits once; `grep -n 'project_all_on_disk' commonwealth/crates/commonwealth-rails/src/kv.rs` hits inside `run_forever`.
  LIFT ~120 lines. — read: ralph/decisions/phase-b-5.md, commonwealth-rails/src/{lib.rs:400-460,kv.rs:500-560}, sovereign-daemon/tests/solo_rails_e2e.rs — check: CLEAN, LINT, TEST(commonwealth-rails), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY (delta 0 from 51)
- [x] REVIEW-pb-census e1646e2e8 — depends [] — OUTCOME: every red edge on the merged tree has exactly ONE owner row below, EVERY row's premises hold at HEAD, and the known defects are reproduced or struck. No code, apart from the TSV repair. This row front-loads the halts (phase-b-2): five-programs lost at least 25 halts to false premises, and these rows cite sites that read-only agents reported, not all of which the seat verified.
  (1) Run the gate on `cut` after the origin/main merge. Reconcile this file's appendix against it: an edge the merge added gets an owner row by the §12 3a ladder, and an edge that closed is struck with the count.
  (2) PREMISES. For EVERY row below, check each cited path, line, symbol and count with grep or ls. Where a site moved, rewrite the row text with the new site. Where a premise is false, rewrite the row under the charter: rescope, fold or strike, with the reasoning in the row. Only a fork the charter leaves to the operator becomes NEEDS_HUMAN.
  (3) Repair docs/FIVE_PROGRAMS_DECISIONS.tsv rows 31-44. They carry 11 fields, so `decision_needed` holds the delta text and the real question sits in column 10.
  (4) Reproduce each defect in the appendix's "Defects" list by reading or running it, then confirm its owner row or strike it with the evidence.
  (5) Measure the baseline that later rows report against:
  - the copy counts of the §2c drives (locators, root locks, MCP loops, tool-set builders, engine assemblies, bring-up paths);
  - bench's closure crate count.
  — read: this file whole, docs/FIVE_PROGRAMS.md §2c and §12 3a, ralph/decisions/phase-b-{1,2}.md, quality/ARCH_LAYERS.toml — check: BOUNDARY (paste the count and the owner histogram), DOCS; the body lists every row rewritten and why
- [ ] pb-test-load — depends [REVIEW-pb-census] — OUTCOME: the full suite is green under Phase B's load, by removing the causes and never by lengthening a timeout (phase-b-4). Three flakes were known on 2026-09-26:
  - (1) Two in-process daemon tests with a 10 s serve bound: `local_only_boot.rs:306` and `rails_base_config`'s `a_booted_daemons_ring_rail_dials_the_declared_door`. They went red in 3 of 6 TESTALL runs, at 10.33-10.35 s, while fp-solo-hermetic's two extra real-binary boots were in the suite. Each passes in about 1.9 s alone (612cdd77b's body).
  - (2) `the_join_child_serves_venues_and_nothing_else` (sovereign-daemon/tests/main/admin_join_serves_venues_e2e.rs:93; census 2026-09-25: it is NOT in sovereign-cli-daemon's join_child/tests.rs, which holds `free_pair()` and a different test): `free_port()` (:34, used at :97-98) releases the port before the joiner binds it. It went red in 1 of 5 runs with "Address already in use" (69d30f52e's body).
  - (3) `corpus_lifecycle::install_pause_resume_lifecycle` went red once, at 4.9 s. Its message was overwritten before anyone read it.
  - MEASURE FIRST: run TESTALL at least 5 times on this row's base, with per-test verdicts from the JUnit report and every failure message kept (principle 7). A flake that does not reproduce in those runs is reported with its run count and left unfixed.
  - Run the daemon-booting tests (in-process `EmbeddedDaemon` boots and binary boots, starting from the set fp-solo-hermetic's census names) in one nextest `[test-groups]` group with a bounded `max-threads`. `.config/nextest.toml` has no group today. Membership comes from ONE rule, so a new daemon test cannot silently fall outside the group (principle 10).
  - join_child: the joiner's port is never picked and then released. Either the test holds it until hand-off, or the joiner binds port 0 and reports the port it bound.
  - corpus_lifecycle: capture its failure output, reproduce it, then classify it. Fix it here only if the cause is load; otherwise write NEEDS_HUMAN with the message.
  - Raise no timeout in these tests (principle 7: a judge is never tuned in one direction).
  - PROOF, pre-registered: 5 consecutive TESTALL runs are green after the change, and the suite's wall time rises no more than 60 s over the median of the measurement runs. PLANT: remove the test group and re-run TESTALL until it is red or 5 runs pass; paste either.
  LIFT ~200 lines, mostly test code and config. — read: ralph/decisions/phase-b-4.md, .config/nextest.toml, sovereign-daemon/tests/main/{local_only_boot.rs,rails_base_config.rs,binary_boot_rails_census.rs,admin_join_serves_venues_e2e.rs,corpus_lifecycle.rs} — check: CLEAN, LINT, TESTALL ×5, PLANT, LAYER, BOUNDARY
- [ ] pb-delete-dead — depends [REVIEW-pb-census] — OUTCOME: code that nothing reaches is gone BEFORE any later row ports it (phase-b-2: delete before move).
  - Delete sovereign-contracts/src/mobile_host.rs, which writes config for and locates the deleted sovereign-server (no `sovereign/crates/sovereign-server/` exists) and shells out to tailscale (:158-169). CENSUS 2026-09-25: it has TWO callers, not one. (a) sovereign-cli-llm mobile_cmd.rs:22,100,108 (through the `sovereign_core::mobile_host` re-export, sovereign-core lib.rs:23). (b) The desktop's src-tauri/src/mobile_host_setup.rs:47,110,111,235, reached by three Tauri commands in commands/config_setup.rs:23-49 (`get_mobile_pairing`, and the start/stop pair at :47/:49), which SettingsPanel.svelte:145 invokes. Decided here: no replacement mobile host exists in the tree, so there is no pointer to give. `svrn mobile` and the three Tauri commands answer a NAMED ABSENCE ("the mobile host was the sovereign-server binary, which was deleted; no mobile host ships"). The commands keep their signatures, so the Svelte side and commands.generated.ts are unchanged. Delete mobile_host_setup.rs's body and the re-export. Today both surfaces already fail at `resolve_server_binary()`, so the delta is the wording of the failure. Name it in the body.
  - Delete `Launch::Server` (sovereign-contracts/src/launch.rs:129, plus its arms at :270 and :284 and its tests at :964-988), which describes a binary that no longer exists.
  - Delete `server::serve` (sovereign-daemon/src/server.rs:674-714). Census: no caller. `git grep` finds only comments naming it (tests/main/canonical_pull_e2e.rs:90, knowledge_fanout.rs:93, ring_sync_loop_tests.rs:147, sovereign-mesh-test-harness simulated_node.rs:181). Repoint those comments to the bind they meant.
  - STRUCK (census 2026-09-25): the legacy pull loop stays. `auto_ingest::discover_and_spawn_pull_loops` (auto_ingest.rs:798) is still driven at :183, and nothing shows `process:v1` carrying the traffic, so ingest_executor.rs:18-24's precondition is unmet. It stays for pb-ingest-dial.
  - Fix the stale citations to the deleted sovereign-server: sovereign-tools mcp_surface.rs:4-9, mcp_router.rs:37-39, corpus-mcp mcp.rs:3, mcp_demo_server.rs:15 and tool_bundle.rs:10. Also bin/sovereign-daemon.rs:2-4, which says "the cmnwlth binary's own main" (principle 3).
  - PROOF: `git grep` for each deleted symbol returns nothing, and LINT and TESTALL are green. There is nothing to PLANT, because a deletion's proof is that the build and tests still pass without the code.
  LIFT ~800 changed lines, mostly deletions. — read: the files above — check: CLEAN, LINT, TESTALL, LAYER, BOUNDARY
- [ ] pb-lift-instrument — depends [REVIEW-pb-census] — OUTCOME: ONE instrument proves any program builds and RUNS outside the monorepo: `scripts/program-lift.sh [--sandbox] <program>`. Its RUN step is each program's "runs alone" proof, the one decider for that question (principle 8, phase-b-2). Later rows ADD their program's smoke here instead of writing their own process-level e2e harness.
  - Collapse the twins `scripts/cw-rails-lift.sh` and `scripts/cw-work-lift.sh` onto it. Keep both names as thin wrappers so existing callers still work.
  - Each program's closure seed and forbidden set are read from quality/ARCH_LAYERS.toml's package map and `[[forbid]]` rows (principle 8); the regex copies go.
  - `workspace-hack` is dropped from copied manifests, as fp-solo-lift did.
  - Each program's RUN smoke is DATA, one TOML entry per program: the binary, its args, a temp root, the requests and the expected answers. Principle 9.
  - Four verdicts with distinct exits (principle 5). An absent precondition of the RUN step (an invite, a model file) is exit 3 naming it.
  - Programs: svrn, ingest, cmnwlth, serve, code, bench. Until pb-serve-program lands, `serve` is the serving crates' closure with no smoke.
  - Record every program's verdict at this commit as the baseline. Most are 0, and that is the honest scoreboard.
  - (phase-b-4) The cmnwlth RUN step reaches an ONLINE media offerer. When every offerer is offline, the step exits 3 and names them. The route's refusal body is kept (no `curl -f`). Measured on 2026-09-26 with a fresh invite: the join WORKS at HEAD (admitted to Meshsonics, 13 members on the roster), and the roster converged. But both media offerers (Alexs-MacBook-Pro and -2) were offline: the route answered 409 "'Alexs-MacBook-Pro' is offline — a bridge to it would accept and then never answer", and the old script reported value 0 with an empty reason.
  - (phase-b-4) The run retires the member it joined as, so the operator's roster does not grow. The 2026-09-26 run left `cw-rails-lift` node 70b77ed2… on the roster. `svrn mesh forget-member` could not match the id that `svrn mesh status` prints. At HEAD that verb reads cw-rails' roster, which is solo, while status reads the daemon's. Name that, or fix it if this row's retire step needs it.
  - PROOF: cmnwlth passes with cw-rails' existing smoke, or its media step exits 3 naming the offline offerers. It then passes for real once pb-membership's mesh-of-two carries a fixture origin. A PLANT that adds a sovereign-* dep to commonwealth-rails turns cmnwlth red.
  LIFT ~800 lines, scripts and data only. — read: scripts/cw-rails-lift.sh, scripts/cw-work-lift.sh, scripts/co-lineage.py `measure_bar` (the verdict contract), quality/ARCH_LAYERS.toml package map — check: LIFT(cmnwlth) passes; `scripts/cw-rails-lift.sh --sandbox` has an unchanged verdict; PLANT(add `sovereign-time` to commonwealth-rails [dependencies] → LIFT(cmnwlth) value 0); paste the six-program verdict table
- [ ] pb-hostkit — depends [pb-lift-instrument] — OUTCOME: every program claims its data root through ONE lock, from a neutrally named leaf that cw-rails can take (FIVE_PROGRAMS §2c, §12 3a rung 4).
  - Name it with `svrn code converge noun <Name>` first.
  - Build it by MOVING, not by renaming (phase-b-2). Its first content is the host-ish modules of `sovereign-cli-base` (877 lines at the census, principle 11): the dirs (dirs.rs), the dispatcher locator (dispatcher.rs) and help (help.rs). Move them with `cargo xtask refactor-apply`, and put the recipe in the commit body.
  - `sovereign-cli-base` re-exports them at their historical paths, so NO consumer changes in this row. Later rows repoint the files they already edit ("repoint on touch", the fp-14 precedent). pb-distribution deletes the alias once it has no consumers.
  - Move cli-base's program vocabulary to its owners: guest_link.rs and rail.rs go to sovereign-cli-mesh, and urls.rs (`daemon_base_url`, `daemon_v1_base`, `v1_url`, `v1_models_url`) goes to sovereign-turn-client (the dialer). Each goes by recipe, re-exported until repointed. CENSUS 2026-09-25: `client_daemon_base` itself is NOT in cli-base. It is sovereign-contracts setup_config.rs:1723, which urls.rs:31 wraps, and phase-c's pc-config-split owns its move and its parse-error substitution. It stays where it is in this row.
  - MOVE sovereign-contracts/src/run_lock.rs into the kit, re-exported at its historical path. Then, in its OWN commit because Windows gains enforcement, re-base it on std `File::try_lock`, which drops libc.
  - Collapse these onto it: sovereign-core/src/deep_research/state.rs:195 (a second type named RunLock, `acquire` at :207); cw-rails' fp-solo-lift lock (commonwealth-rails/src/lib.rs:141-154, std `try_lock` on `ROOT_LOCK`); corpus-engine-scip's fs4 lock (scip_graph.rs:484) only if its semantics match (otherwise the body says why not).
  - ARCH_LAYERS: add the kit's leaf row with `allow = ["workspace-hack"]` and a size cap of 2,500 code lines. The cap is the operator's to change and is never ratcheted.
  - PROOF: LIFT(cmnwlth) still passes with the kit in cw-rails' closure. Two processes on one root refuse, for the daemon and for cw-rails. The copy counts go down, with the numbers in the body.
  LIFT ~900 lines. — read: sovereign-cli-base/src, sovereign-contracts/src/run_lock.rs, sovereign-core/src/deep_research/state.rs:190-240, commonwealth-rails/src/lib.rs:130-170, corpus-engine/xtask/src/refactor_apply.rs (the recipe shape), quality/ARCH_LAYERS.toml leaf rows — check: CLEAN, LINT, TEST(<the kit>), TEST(commonwealth-rails), TEST(sovereign-daemon), LAYER, LIFT(cmnwlth), PLANT(remove the try_lock claim → TEST(<the kit>) red), BOUNDARY
- [ ] pb-mcp — depends [pb-hostkit] — OUTCOME: ONE MCP dispatcher lives in the host kit, and corpus-mcp is its first adopter. The code server (pb-code-server) is the second. The svrn daemon adopts it LAST (pb-daemon-adopts), after its code tools have left, so nothing is ported that a later row deletes (phase-b-2).
  - The dispatcher: lift `dispatch` (sovereign-daemon/src/mcp_router.rs:408) and `handle_tool_call` (:484-~639), plus the version, alias and exposure logic in sovereign-tools/src/mcp_surface.rs (`MCP_TOOLS_ALWAYS` :42, `MCP_TOOLS_SPEC_GATED` :162, `MCP_TOOLS_RETIRED` :172, `MCP_TOOL_ALIASES` :255). CENSUS 2026-09-25: no type named `McpService` exists anywhere in the tree. The dispatch is two free fns. Name the kit's dispatcher type with `svrn code converge noun` first. Lift means move by recipe, with a re-export left behind, so the daemon keeps compiling on the same code.
  - Framings: stdio, in the corpus-mcp mcp.rs shape, and HTTP+SSE with axum behind a feature.
  - The MCP method set becomes ONE enum beside oicp-types' jsonrpc (principle 9). There are three string matches today: corpus-mcp mcp.rs:49, sovereign-cli-llm mcp_demo_server.rs:102 and mcp_router.rs:427.
  - A call-log PORT replaces mcp_router's `Arc<NoteStore>` (mcp_router.rs:31,168,298,411,488).
  - Tool exposure becomes a manifest field. `MCP_TOOLS_ALWAYS`, `SPEC_GATED`, `RETIRED` and `ALIASES` become data.
  - The tool set is built from `ToolBundle`s (sovereign-contracts tool_bundle.rs:67). The bundles `CodeIntelTools`/`NotesTools` (sovereign-code/src/bundle.rs) become the code server's assembly in pb-code-server.
  - Delta, in its own commit: corpus-mcp moves to protocol-version negotiation and isError results, a behaviour change named in the body.
  - PROOF: corpus-mcp's stdio loop is gone, and its tests plus `tests/no_inference_stack.rs` pass on the kit's dispatcher. PLANT: add a fourth method string match in corpus-mcp, and the enum-exhaustiveness test goes red.
  LIFT ~1,000 lines. — read: mcp_router.rs, mcp_surface.rs, corpus-mcp/src/{mcp.rs,tools.rs}, tool_bundle.rs, sovereign-code/src/bundle.rs, ralph/decisions/five-programs-39.md — check: CLEAN, LINT, TEST(<the kit>), TEST(corpus-mcp), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY
- [ ] pb-shell — depends [pb-hostkit] — OUTCOME: ONE HTTP server shell lives in the host kit, and the small hosts adopt it first. The svrn daemon adopts it LAST (pb-daemon-adopts), after pb-daemon-mesh-exit, pb-code-clean and pb-svrn-dials-serve have deleted its duplicate routes and serving bootstrap (phase-b-2).
  - One `serve(listeners, bundles, shutdown: impl Future)` owns: peer addresses (the repeated "bare axum::serve drops ConnectInfo" comment becomes code), the loopback guard (lifted from the daemon's `pub(crate)` loopback_guard.rs:126 by recipe, re-exported), body limits (server.rs:30-46), bind retry and mount tracing.
  - Route bundles return `(name, Router)` as ONE value.
  - Adopters in this row: cw-rails (api.rs:119-133, internal.rs:50-75), the compute child (child_main.rs:223-330), and the meshapp dev and ring show near-twins (sovereign-cli-mesh meshapp_cmd.rs:246-268, ring_cmd/show.rs:94-112; census 2026-09-25: there is no ring_cmd/dev.rs, and the twin is `ring show`).
  - Health spellings stay as they are, because they are wires. Name them in the body.
  - PROOF: LIFT(cmnwlth) passes with the kit's shell in cw-rails. The adopters' tests pass, and each one's mount trace names its routes. PLANT: mount a route outside the bundle list in cw-rails, and the mount-trace test goes red.
  LIFT ~800 lines. — read: sovereign-daemon/src/{loopback_guard.rs,server.rs:30-46}, commonwealth-rails/src/{api.rs,internal.rs}, sovereign-compute/src/child_main.rs:223-330, sovereign-cli-mesh/src/{meshapp_cmd.rs:240-270,ring_cmd/show.rs:85-115} — check: CLEAN, LINT, TEST(<the kit>), TEST(commonwealth-rails), TEST(sovereign-compute), TEST(sovereign-cli-mesh), LIFT(cmnwlth), PLANT, LAYER, BOUNDARY
- [ ] pb-serving-assembly — depends [REVIEW-pb-census] — OUTCOME: a hot reload builds exactly what cold start builds, from ONE engine assembly that the serving package owns.
  - Today three paths build the engine, and they have drifted:
    - daemon build/inference.rs `load_provider` (:67-485);
    - daemon provider.rs `LlamaCppFactory` (struct :24, impl :36-164, installed at daemon_cmd/boot.rs:1054), which calls `EmbeddedLlamaCpp::load_full_with_families` directly, never reads `[engine] kind`, installs no extra/edit/rerank slots and no compute layer, and passes `ModelFamily::Unknown`;
    - sovereign-compute child_main.rs:354 (`load_dual`), :374 (`load_single_distributed`) and :385 (`EmbedOnlyProvider::load`).
  - Census 2026-09-25, reproduced by reading: `build_provider` (provider.rs:37-62) calls `cfg.models()` (setup_config.rs:1878), which refuses only an unpopulated section. Nothing on the path reads `[engine] kind` or `[compute] distributed_primary`, so a remote-kind node with a populated `[models]` loads GGUFs on reload.
  - One `assemble_serving(cfg) -> ServingParts { provider, reload_factory, child_trampoline }` in the serving package, beside `engine_factory::register_engine` (sovereign-inference/src/engine_factory.rs:141). All three paths call it. Serving admission (containment, fast≠primary; engine_factory.rs:15-20 handed it to the daemon) moves with it.
  - Deltas (the fix), each in its own commit:
    - a reload on a `kind="remote"` node stops loading GGUFs;
    - a reload under `[compute] distributed_primary` stops loading the withheld primary in-process;
    - embed keeps its manifest family.
  - PROOF: a test builds the provider through the cold path and the reload path for the configs {llama, remote, distributed_primary} and asserts identical slot sets. PLANT: restore the direct load in the reload path, and the test goes red.
  LIFT ~800 lines. — read: sovereign-daemon/src/{build/inference.rs,provider.rs}, sovereign-inference/src/engine_factory.rs, sovereign-compute/src/child_main.rs — check: CLEAN, LINT, TEST(sovereign-inference), TEST(sovereign-compute), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY
- [ ] pb-serving-kinds — depends [pb-serving-assembly] — OUTCOME: a new model kind is ONE registration, and rerank and NER are served kinds.
  - Served kinds get a registry on register_engine's precedent. A kind registers its manifest role, in-process loader, OICP route, oicp-client method and compute-child role once. Slot roles stay a closed enum (SLOT_ALIAS_POLICY, principle 9).
  - Rerank:
    - Today it is env-only (`SOVEREIGN_RERANK_MODEL_PATH`, inference.rs:369), with two loaders on one env var (reranker_standalone.rs:119).
    - It has no route and no client method, and `SplitInferenceProvider` falls through to NotImplemented (traits.rs:477). Census 2026-09-25: `git grep '/v1/rerank'` finds no route, boot.rs:930 passes `RerankWiring::AlreadyInProvider` (so the recipe loads none, lib.rs:934), and the only production `rerank_batch` callers in the daemon's closure are forwarders (sovereign-compute manager.rs:1217). The slot is installed (build/inference.rs:369) and read by nothing.
    - It gains a `[models]` key, `/v1/rerank`, the client method and a child role.
  - NER: GLiNER is served through the existing `EntityExtractor` port (traits.rs:207) as a kind. The double load goes: daemon boot.rs:598 (`bootstrap::load_gliner_extractor`, bootstrap.rs:89-112, which honours the env knob through `sovereign_gliner::labeled::configured_model_id`, labeled.rs:128) and sovereign-runtime-recipe lib.rs:667→832-859 (`load_gliner`, which always takes `gliner_ner::DEFAULT_MODEL_ID`) load two different models whenever the env var is set. Reproduced by reading, 2026-09-25.
  - This row ALSO closes sovereign-daemon → sovereign-gliner (census 2026-09-25: the fp-12 list named it, and pb-meshapp-rehome defers it here). Its sites are the boot load above plus assets_http.rs:182-419, the daemon's GLiNER model probe, download and path (`configured_model_id`, `probe_model_available`, `models_root`, `download_model`). They move with the kind's registration, since the kind owns its manifest role and its asset.
  - runtime-recipe takes both as PORTS: `rerank: Option<Arc<dyn InferenceProvider>>`, since `inference_to_rerank_fn` is provider-agnostic (reranker_standalone.rs:20-24), and the boot extractor. This closes sovereign-runtime-recipe → sovereign-inference (retires the fp-69 exception) and → sovereign-gliner.
  - Delta, in its own commit: daemon turns GAIN rerank (`RerankWiring::AlreadyInProvider` recorded none, lib.rs:250-256). Measure it with `svrn quality check --lane retrieval-prod`, which must stay in or above its band.
  LIFT ~1,200 lines. — read: engine_factory.rs, sovereign-inference/src/{engine.rs:1556-1760,reranker_standalone.rs}, sovereign-contracts/src/traits.rs:200-480, sovereign-runtime-recipe/src/lib.rs:240-260,820-1010, sovereign-daemon/src/daemon_cmd/boot.rs:580-600 — check: CLEAN, LINT, TEST(sovereign-inference), TEST(sovereign-runtime-recipe), TEST(sovereign-daemon), PLANT(register rerank without its route → the kind-registry test red), `cargo xtask env-gate`, LAYER, BOUNDARY (expect −2, runtime-recipe → gliner and daemon → gliner, and the fp-69 exception row deleted)
- [ ] pb-serve-program — depends [pb-serving-kinds, pb-hostkit, pb-shell] — OUTCOME: `serve` runs ALONE. One binary answers `/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank` and `/v1/models` with no mesh, no knowledge server and no cw-rails. This is the "local model server" developer (FIVE_PROGRAMS §2 row `serve`).
  - FIRST, rename the phrase collision: daemon code calls cw-rails "the mesh's serving process" (census 2026-09-25: daemon.rs:3146,3152, setup_config.rs:1456 `rails_base` docs, admin_http.rs:91). Principle 8: one name.
  - ARCH_LAYERS: a `serve` package whose members are sovereign-inference, sovereign-compute, sovereign-serving-host and sovereign-serving-policy, moved from cmnwlth. Measure the gate before and after, and name every edge the re-map moves.
  - Its own `[[bin]]` in its own crate. It promotes the compute child's server (sovereign-compute server.rs, which already has a supervisor, a wire and 5 routes) and puts serving-host's `inference_adapter` (the OpenAI translation) in front of it. It gets the host-kit lock and shell.
  - It reads ITS sections (models, engine, compute, shared_model) from the existing config file through the existing SetupConfig. The per-program file split is phase-c (phase-b-2), so there is no config migration here.
  - `/v1/models` and the OICP manifest are built from its OWN provider (`oicp_synthesis::build_self_manifest` reads `resident_slots()`). Never from cw-rails' ledger. Census 2026-09-25, narrower than first stated: the daemon's `list_models` (routes_inference.rs:749) answers from its OWN provider manifest when it has local inference (`manifest_rows`, :792). Only a node with NO local inference falls to `store_rows` (:960), which makes per-request cw-rails round trips (`inference_plan`, `list_models_with_origins`, `get_llama_server_address`; state.rs:1224-1320, through rails_client/ledger.rs; principles 1 and 12). serve always has its own provider, so it never takes that path. The fallback's fate is pb-svrn-dials-serve's: svrn's `/v1/models` answers from serve's.
  - The rpc-worker and compute-child trampolines move with it (compute manager.rs:309/356/390, inference rpc_distribution.rs:2405).
  - PROOF: add serve's RUN smoke to program-lift.sh's data. With the mock engine kind, chat, embeddings, rerank and `/v1/models` each answer on a temp root with cw-rails absent, and LIFT(serve) passes. PLANT: point `/v1/models` back at a rails dial, and LIFT(serve) goes red.
  LIFT ~1,300 lines. — read: SERVING_BOUNDARY.md, sovereign-compute/src/{server.rs,wire.rs,child_main.rs,manager.rs}, sovereign-serving-host/src/{inference_adapter.rs,oicp_synthesis.rs}, sovereign-daemon/src/{routes_inference.rs,state.rs:1290-1320}, quality/ARCH_LAYERS.toml package map — check: CLEAN, LINT, TEST(sovereign-compute), TEST(sovereign-serving-host), LIFT(serve), PLANT, LAYER, `cargo xtask env-gate`, BOUNDARY (paste before and after)
- [ ] pb-svrn-dials-serve — depends [pb-serve-program] — OUTCOME: svrn answers chat with serving in another process. The fp-10 and fp-68 exceptions are RETIRED by the owner, not by exception. The stock distribution dials `serve`, and a phone or single-binary distribution may still link serve's library face (FIVE_PROGRAMS §2c).
  - REUSE the terminal-node arm, which already means "this node holds no weights and dials an entry node" (`SplitInferenceProvider`, sovereign-daemon build/inference.rs ~:84-230; principle 11). svrn reaches `serve` at `[node] entry` when it is set. Otherwise it uses a default loopback base constant, documented on both sides the way `DEFAULT_RAILS_BASE` is.
  - svrn STOPS READING serve's sections (models, engine, compute, shared_model). So there is NO config migration: the shared file keeps them, and serve reads them (phase-b-2).
  - The census checks that the terminal arm's hard-coded model "primary" and its missing bearer (build/inference.rs:189,204) are right for a loopback serve. It names any gap, which is fixed in its own commit.
  - The daemon's serving bootstrap is deleted.
  - svrn brings `serve` up with `ServingHost::ensure_reachable` (reach.rs:323) at user-action moments only (daemon start, a verb). It never brings it up on a refused dial (ARCH_LAYERS :1493-1500). An unreachable serve is a named absence (§4 rule 3).
  - svrn's `/v1/models` answers from serve's.
  - The edges sovereign-daemon → sovereign-inference, → sovereign-compute and → sovereign-serving-host close. Delete their `[[exception]]` rows.
  - BEFORE the switch commit, run the header's pre-registered bars and paste them: first-token latency, embedding throughput, retrieval-prod and synth. A miss is NEEDS_HUMAN with the numbers.
  - Delta: a stock install runs two processes. Setup and `svrn daemon status` name both.
  - PROOF: add svrn's RUN smoke to program-lift.sh. The daemon and serve start on a temp root, and a chat turn answers. PLANT: stop serve mid-smoke, and the turn reports absence, never an empty answer.
  LIFT ~1,300 lines. — read: sovereign-daemon/src/{build/inference.rs,daemon_cmd/boot.rs,state.rs}, the fp-10/fp-68 exception rows, sovereign-turn-client/src/reach.rs, sovereign-daemon/src/rails_client.rs (the DEFAULT_RAILS_BASE precedent) — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli-daemon), the bars above, PLANT, LIFT(svrn) (paste the verdict; it may still be 0 for edges later rows own), LAYER, BOUNDARY (census 2026-09-25: expect delta 0 and the three exception rows fp-10 ×2 and fp-68 deleted. Excepted edges are not in the violation count, so "−3" was wrong)
- [ ] pb-membership — depends [pb-hostkit] — OUTCOME: cw-rails is the node's ONE mesh endpoint and holds its ONE node key. Founding, joining, admission and mDNS move to cw-rails (reversing five-programs-21's disclaimer, operator 2026-09-25), and the fp-9 exception (sovereign-daemon → commonwealth-discovery) is RETIRED.
  - commonwealth-discovery is already on cw-rails' except list (ARCH_LAYERS ~:686).
  - Solo mode (fp-solo-lift) covers a lone node.
  - The daemon's `~/.svrnmesh/node_key` retires. cw-rails signs fp-74's attestation at the daemon's request.
  - Delta: an existing install's key migrates in the SAME commit as the flip, because peers' rosters hold the daemon's key (five-programs-38 (2)).
  - Delta (phase-b-4): an install that ran HEAD before this row ALSO holds a solo cw-rails key (`~/.commonwealth-rails/node_key`, minted by fp-solo-clients' bring-up). The operator's RuggedFox has held one since 2026-09-26. The daemon's key is the roster identity and wins; the solo key retires and is never promoted (principle 8, identity from essence). The migration test covers this two-key node as well.
  - LIFT (phase-b-4): once cw-rails can found a mesh, the cmnwlth lift's run step founds its own two-node mesh inside the sandbox: a founder cw-rails plus the lifted joiner, never the operator's mesh. Repeated runs then stop adding members to the operator's roster. The sandbox founder declares a fixture media origin, so the media step measures there too. It is two processes, so it is not the "mesh of one" that scripts/cw-rails-lift.sh:66-69 refuses.
  - PROOF: an e2e founds a mesh with cw-rails alone, and a second cw-rails joins by invite. A svrn daemon on a joined node signs through rails. Migration test: a node with only the old daemon key keeps its roster identity after upgrade. PLANT: skip the migration, and the roster test goes red.
  LIFT ~1,500 lines. — read: commonwealth-rails/src/{lib.rs:20-40,identity.rs}, commonwealth-discovery, sovereign-contracts/src/node_identity.rs, sovereign-daemon mesh join/admission sites, ralph/decisions/five-programs-{21,38,39}.md — check: CLEAN, LINT, TEST(commonwealth-rails), TEST(commonwealth-discovery), TEST(sovereign-daemon), LIFT(cmnwlth), PLANT, LAYER, BOUNDARY (expect the fp-9 exception row deleted)
- [ ] pb-daemon-mesh-exit — depends [pb-membership] — OUTCOME: the svrn daemon is no longer a mesh endpoint. It dials cw-rails for everything mesh.
  - Delete the daemon's copies of routes cw-rails serves. Duplicate ownership is principle 8. The routes: `/v1/mesh/status` (mesh_http.rs:36), `/v1/mesh/kv/*` (routes_mesh_kv.rs:44-47), `/v1/rail/{append,log}` and `/internal/gossip` (server.rs:277-278, :349).
  - Close the transport residue: peer_contact, iroh, identity, mesh_proof and fanout. Census 2026-09-25: 84 non-test src lines name `commonwealth_transport` in 22 files (not ~42 in 19). The same grep finds `commonwealth_core::` on 218 lines and `sovereign_mesh::` on 185. Together that exceeds this row's lift, so the "split by edge" clause below applies from the start: one commit series per edge, transport first. First check whether `identity::load_or_create_client_token`/`generate_bearer_token` are svrn's own client-API auth; if they are, they MOVE to svrn and are not dialed. The census found them used as client auth: client_auth.rs:100 re-exports the first, daemon.rs:3276 calls it, and routes_guest_session.rs:164 mints guest bearers with the second.
  - Close the inherited REVIEW-mint-fp-core-dial and REVIEW-mint-fp-mesh-dial residue (their five-programs rows carry the sites).
  - Edges: sovereign-daemon → commonwealth-transport, → commonwealth-core, → sovereign-mesh.
  - PROOF: the daemon e2e suite passes with cw-rails serving every mesh route, and each deleted route on the daemon answers a named pointer where a client might still call it. PLANT: re-mount one deleted route, and the duplicate-route test goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: five-programs STATE.md rows REVIEW-mint-fp-core-dial and REVIEW-mint-fp-mesh-dial, sovereign-daemon/src/{mesh_http.rs,routes_mesh_kv.rs,server.rs,daemon.rs}, commonwealth-rails/src/api.rs — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(commonwealth-rails), PLANT, LAYER, BOUNDARY (expect −3)
- [ ] pb-work-doors — depends [pb-membership] — OUTCOME: svrn submits and takes work through cw-rails' own doors. cw-rails has owned the journal and the fold since fp-45 (49578c1e2).
  - cw-rails gets two doors: submit, and take/complete. Acts are sealed with the one node key (pb-membership).
  - Act DTOs go into oicp-types beside JobKind, JobRequirements and JobUnit (§12 3a rung 2, federation wire).
  - sovereign-cli quality_check_cmd/distribute.rs (census 2026-09-25: the path; 18 `commonwealth_work` lines) and the daemon's residue (32 non-test lines in 5 files: work_donor.rs 19, ingest_executor.rs 10, daemon.rs, rails_client.rs and work_donor_checkout.rs 1 each) dial the doors. Executors stay program-side.
  - Rejected: §12's daemon submitter route, which is a proxy (five-programs-39).
  - Edges: sovereign-cli → commonwealth-work, sovereign-daemon → commonwealth-work.
  - PROOF: an e2e submits a job from `svrn`, a donor takes it through the door, and the fold shows it complete. PLANT: an unsealed act is refused.
  LIFT ~900 lines. — read: commonwealth-work/src/{executor.rs,process.rs}, commonwealth-rails/src/api.rs:98, sovereign-cli/src/quality_check_cmd/distribute.rs, sovereign-daemon/src/{work_donor.rs,ingest_executor.rs} — check: CLEAN, LINT, TEST(commonwealth-rails), TEST(commonwealth-work), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY (expect −2)
- [ ] pb-code-server — depends [pb-mcp, pb-shell] — OUTCOME: code intelligence runs ALONE as an MCP server with no LLM, no knowledge server and no mesh (the "code intel for my agent" developer). It REPLACES the legacy `project serve` (sovereign-cli-dev project_cmd/serve.rs, which today hand-builds its tool set with 27 `register` calls at :389-588 and mounts the daemon's `mcp_router` at :696, with `FeatureRoot`/`McpNotifier` at :666/:675) instead of porting it (phase-b-2). `svrn serve` and `project serve` become the new server's spellings.
  - `svrn code mcp` = the host kit's dispatcher and shell + CodeIntelTools + NotesTools (decision notes) + the work-atlas bundle. The atlas is optional: it dials cw-rails' KV directly, never the daemon's `/v1/mesh/kv` proxy. When cw-rails is down it is absent by name (the null object `Withheld`, tool_bundle.rs:302).
  - Connect-or-spawn goes through `ServingHost`.
  - One SCIP loader (tool_registry.rs:384 and sovereign-cli-shared/src/scip.rs:31 collapse).
  - One freshness path: the `Reindexer`. The 30 s mtime poll goes.
  - The watcher runtime is owned here, which closes sovereign-daemon → corpus-engine-watchers.
  - SpecWatcher (sovereign-tools/src/spec_watcher.rs, 380 lines) and the spec-gated list move to sovereign-code by recipe, which closes sovereign-cli-dev → sovereign-tools. Census: serve.rs:678 is cli-dev's one live `sovereign_tools` use. tools_cmd/registry.rs:21 only names mcp_surface in a doc comment, which is repointed.
  - The legacy serve's import of `mcp_router` goes, which closes sovereign-cli-dev → sovereign-daemon (fp-11).
  - Defects, fixed in their own commits:
    - `code_search`'s `inf.embed(query).await.unwrap_or_default()` (sovereign-code/src/code_search.rs:123; census: the file moved there from sovereign-tools) becomes a named fall-back, never a silent swap to FTS;
    - the in-memory notes fallback (serve.rs:234-252: `NoteStore::open(&notes_db_path)` fails over to `":memory:"`) becomes a refusal that names the path.
  - Delta: the server no longer refuses to start while the daemon runs (serve.rs:41-62).
  - PROOF: add code's RUN smoke to program-lift.sh. On a fixture repo with no daemon and no model, `symbols` and `callers` answer and LIFT(code) passes. PLANT: make the atlas dial the daemon proxy, and with the daemon absent the smoke goes red.
  LIFT ~1,400 lines. — read: sovereign-code/src/bundle.rs, sovereign-cli-dev/src/project_cmd/serve.rs, sovereign-code/src/code_search.rs, corpus-engine-watchers/src/reindexer.rs, sovereign-work-atlas — check: CLEAN, LINT, TEST(sovereign-code), TEST(sovereign-cli-dev), TEST(sovereign-work-atlas), LIFT(code), PLANT, LAYER, BOUNDARY (expect −3)
- [ ] pb-ingest — depends [REVIEW-pb-census] — OUTCOME: an index builds in CI with only an embeddings endpoint (the "ingest in CI" developer). Ingest is a library plus ONE CLI (FIVE_PROGRAMS §2 row `svrn ingest`).
  - The working precedent is `corpus-mcp ingest` (corpus-mcp/src/ingest.rs:319-329; its `tests/no_inference_stack.rs` pins the closure).
  - Fold `svrn corpus ingest`'s workflow path into that one CLI. That path posts to the daemon and runs the notebook workflow (census 2026-09-25: both files are in sovereign-cli-LLM, not sovereign-cli: corpus_cmd/ingest.rs:108-110 → `workflow_cmd::run_assembled`, and workflow_cmd.rs:595 is the daemon-unreachable refusal). It is a second ingest implementation (principle 8).
  - ONE endpoint-resolution decider for embedder and chat: corpus-mcp's host.rs discovery ladder, reused. `code index` uses it later.
  - Fixed in their own commits:
    - the chat model is no longer resolved under `--no-enrich` (corpus-mcp/src/ingest.rs:222-242 resolves it unconditionally and refuses when none is listed; `args.no_enrich` is first read at :282). Reproduced by reading;
    - GLiNER absence is named, not silently skipped (corpus-engine/src/engine/ingest.rs:1926, `if let Some(extractor) = self.chunk_entity_extractor()` has no else branch). Reproduced by reading.
  - The work plane is one OPTIONAL caller, never a requirement.
  - PROOF: add ingest's RUN smoke to program-lift.sh. It ingests a fixture folder against a stub embeddings server with no chat model, the index opens, and LIFT(ingest) passes. PLANT: resolve chat under `--no-enrich`, and the smoke goes red.
  LIFT ~900 lines. — read: corpus-mcp/src/{ingest.rs,host.rs}, sovereign-cli-llm/src/{corpus_cmd/ingest.rs,workflow_cmd.rs}, corpus-engine/src/engine/ingest.rs:100-130,1400-1420,1920-1935 — check: CLEAN, LINT, TEST(corpus-mcp), TEST(corpus-engine), LIFT(ingest), PLANT, LAYER, BOUNDARY
- [ ] pb-code-index — depends [pb-code-server, pb-ingest] — OUTCOME: indexing a project needs only the code program, and `project init` works with no daemon.
  - `code_index` plus incremental (890 + 605 = 1,495 lines, sovereign-cli-shared) and `code_refresh` (561 lines; census 2026-09-25: it is sovereign-CLI/src/code_refresh.rs, not cli-shared or cli-dev) move into the code program. D5 places them there. fp-5's refusal falls, because code now ships in the default distribution.
  - `code index` and `project refresh` stop refusing without the daemon (code_index.rs:260-271,451-455).
  - `project init`'s index step dials code by connect-or-spawn with an explicit `--fts-only` when there is no embedder. This closes D6, sovereign-cli → corpus-engine (project_init/mod.rs:30,516).
  - Embedder through pb-ingest's one endpoint decider.
  - Defect, fixed in its own commit: with no embedder, write `vector=false` and say so in the banner. Never zero vectors stamped with `configured_embed_model_name()` (project_init/mod.rs:505-513).
  - cli-dev's own index sites also go here: code_cmd.rs:102,1555,1563 (`code finalize`/`code watch`: `CorpusEngine::new`, `update::watch::CodeWatcher`) and project_cmd/mod.rs:15.
  - Edges: sovereign-cli-shared → corpus-engine (code_index.rs:39,282-297,357 only), sovereign-cli-dev → sovereign-cli-shared, sovereign-cli → corpus-engine (project_init/mod.rs:30,516 only; census 2026-09-25 confirms no other site).
  - REASSIGNED by the census: sovereign-cli-dev → corpus-engine (fp-34 residue) cannot close here. Four of its seven site groups belong to rows that run after this one or name nothing yet: backlog_cmd/score.rs:15 (pb-code-clean), git_archaeology_cmd.rs:17, project_cmd/audit/mod.rs:289, and tools_cmd/registry.rs:42,107. Its owner is now pb-code-clean.
  - PROOF: an e2e runs `svrn project init` then `svrn code index` on a fixture repo with no daemon and no embedder, and `code_search` answers from FTS with the banner naming it. PLANT: stamp the model name, and the metadata test goes red.
  LIFT ~1,500 lines. — read: sovereign-cli-shared/src/{code_index.rs,code_index_incremental.rs,scip.rs}, sovereign-cli/src/{project_init/mod.rs,code_refresh.rs}, sovereign-cli-dev/src/{code_cmd.rs,project_cmd/mod.rs} — check: CLEAN, LINT, TEST(sovereign-cli), TEST(sovereign-cli-dev), TEST(sovereign-code), LIFT(code), PLANT, LAYER, BOUNDARY (expect −3)
- [ ] pb-code-clean — depends [pb-code-server, pb-code-index] — OUTCOME: the code program links nothing of svrn, and svrn serves no code tool, so `symbols` is served in exactly one place.
  - The daemon stops registering code tools. svrn's in-process Runtime (tool_registry.rs:43) dials code's MCP and reports absence when code is unreachable (§4 rule 3).
  - A moved tool called on svrn answers a named pointer: "moved to svrn code; `svrn project init` updates your config" (five-programs-39).
  - The scaffold (sovereign-cli/src/project_init/scaffold.rs:509-536) writes both servers. This repo's .mcp.json and .opencode/opencode.json change with it.
  - The daemon stops constructing the work atlas (bootstrap.rs, tool_registry.rs). The mesh test tree's atlas test moves to the work-atlas crate's own tests.
  - sovereign-cli-dev backlog_cmd/score.rs:15 (`corpus_engine::…::ChatPrompt`) and :19 (`sovereign_enrichment_build::inference_client`) repoint to oicp-client, a package_leaf (ARCH_LAYERS :913) that already sends json_schema (oicp-client/src/lib.rs:758-767).
  - (census 2026-09-25) cli-dev → corpus-engine closes here, moved from pb-code-index. The sites pb-code-index and pb-code-server leave are:
    - score.rs:15 (above);
    - git_archaeology_cmd.rs:17 `read_atlas_atoms`: a PATH repoint, since it lives in the understanding-vocab leaf (read.rs:45);
    - project_cmd/audit/mod.rs:288-300, `compose_publish_recipe_nudge`, a best-effort nudge that builds a `RecipeRegistry`. Delta, in its own commit: the nudge is dropped from `project audit`, because recipes are ingest's vocabulary. Name it in the body;
    - tools_cmd/registry.rs:42,107, where the `sovereign tools call` builder constructs a `CorpusEngine` for its knowledge tools. Those tools are svrn's. They answer the five-programs-39 named pointer to svrn's surface, the same one this row gives moved code tools on svrn.
  - Edges: sovereign-daemon → sovereign-code (fp-11), sovereign-daemon → sovereign-work-atlas, sovereign-mesh → sovereign-work-atlas (dev; the one site is sovereign-mesh/tests/main/work_atlas_store.rs), sovereign-cli-dev → sovereign-enrichment-build (score.rs:19 is the only live use), sovereign-cli-dev → corpus-engine.
  - PROOF: calling `symbols` on the svrn MCP surface returns the pointer, and on code's it answers. LIFT(code) passes. PLANT: re-register one code tool on the daemon, and the one-home test goes red.
  LIFT ~1,100 lines. — read: sovereign-daemon/src/{tool_registry.rs,bootstrap.rs}, scaffold.rs, sovereign-cli-dev/src/{backlog_cmd/score.rs,git_archaeology_cmd.rs,project_cmd/audit/mod.rs:280-320,tools_cmd/registry.rs} — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli), TEST(sovereign-cli-dev), TEST(sovereign-mesh), LIFT(code), PLANT, LAYER, BOUNDARY (expect −5)
- [ ] pb-notes-split — depends [pb-mcp, pb-code-server] — OUTCOME: svrn keeps its memory with NO code program, and code keeps its decision notes (the "svrn without code" developer).
  - notes.db holds four populations by `kind`, in three files. There are three path deciders: sovereign-cli-dev backlog_cmd/item.rs:30, sovereign-cli-llm awareness_cmd/store_open.rs:60 and sovereign-contracts middleware.rs:165. One query returns 68 notes from one directory and 6,811 from another (item.rs:27).
  - Split by OWNER (principle 12):
    - svrn's memory (lessons Global, written at lessons.rs:1076,1120 and read at :623; the tool_decision dossier at memory.rs:1584-1607 and commitments at runtime/handlers/commissive.rs:86, Session) goes to svrn's own store, converging on the StateStore memory port (`save_memory`, sovereign-contracts traits.rs:1277);
    - decision notes go to code;
    - the tool-call log goes to the MCP host's call-log port (pb-mcp);
    - recipe-author notes (Feature scope) go to ingest.
  - One path decider per store.
  - Delta: existing rows MIGRATE in the same commit as the switch.
  - Fixed in their own commits:
    - chat's `knowledge_lookup` stops treating every kind as evidence, and its `unwrap_or_default` error collapse becomes a named absence (sovereign-tools knowledge_lookup/mod.rs:294-302: `note_evidence` calls `read_notes` with an empty kinds filter, then `.unwrap_or_default()`; reproduced by reading);
    - lessons are not gossip-eligible unless the user opts in. Verify the desktop's `private: false` write first.
  - Closes the four notes-factory edges: sovereign-cli, sovereign-cli-llm, sovereign-daemon and sovereign-tools → corpus-engine-notes.
  - PROOF: an e2e runs svrn chat with the code program absent, and a lesson is written, recalled after restart, and not visible to code's `notes` tool. Migration test: a fixture notes.db splits with row counts conserved. PLANT: skip the migration, and the count test goes red.
  LIFT ~1,500 lines. — read: sovereign-contracts/src/{notes.rs,middleware.rs,traits.rs:1260-1290}, corpus-engine-notes/src/port.rs, sovereign-core memory/lessons/commissive, knowledge_lookup/mod.rs — check: CLEAN, LINT, TEST(sovereign-core), TEST(corpus-engine-notes), TEST(sovereign-daemon), PLANT, LAYER, BOUNDARY (expect −4)
- [ ] pb-ingest-dial — depends [pb-ingest, pb-work-doors] — OUTCOME: svrn executes no ingest in-process. Its residue dials the ingest CLI.
  - sovereign-tools' MCP tools that EXECUTE ingest (census 2026-09-25: 259 non-test `corpus_engine::` lines in 56 files, the largest being knowledge_view/manager.rs and conv_tiered_provider.rs) submit jobs or exec the ingest CLI. The daemon's corpus-engine residue (115 lines in 33 files) does the same.
  - The legacy pull loop is deleted ("replaced, not yet deleted", ingest_executor.rs:18-24).
  - `IngestExecutor` retires ONLY after `process:v1` has been watched carrying real ingest traffic. Paste the run.
  - sovereign-mesh → corpus-engine (5 lines in 4 files) and sovereign-runtime-recipe → corpus-engine (12 lines, all in lib.rs) take ports or dial.
  - sovereign-tools → sovereign-recipe-author (bundles.rs:445,486, fp-29 RecipeProjectStore) is re-placed by the ladder.
  - Edges: sovereign-daemon, sovereign-tools, sovereign-mesh and sovereign-runtime-recipe → corpus-engine; sovereign-tools → sovereign-recipe-author.
  - PROOF: an e2e runs a svrn chat turn that triggers an ingest, and it completes through the CLI with the daemon linking no corpus-engine writer. PLANT: call the engine in-process, and LAYER goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: sovereign-daemon/src/ingest_executor.rs, sovereign-tools knowledge_view, the phase-b appendix ingest-dial lines — check: CLEAN, LINT, TEST(sovereign-tools), TEST(sovereign-daemon), TEST(sovereign-runtime-recipe), PLANT, LAYER, BOUNDARY (expect −5)
- [ ] pb-ingest-rehome — depends [pb-ingest-dial] — OUTCOME: every ingest capability lives in ingest.
  - corpus-mcp is split by verb: ask and search stay svrn; ingest, recipe and serve-pull go to ingest. This closes corpus-mcp → corpus-engine and → sovereign-enrichment-build.
  - RESCOPED by the census (2026-09-25). The split alone does NOT close corpus-mcp → corpus-engine, because the ask/search half reads the atlas walk through corpus-engine: tools.rs:35-39,915,973,1099, ask.rs:31-32,371,400, and host.rs:96 (`embed_http::http_embed_fn`). The existing owner is the reader leaf corpus-engine-atlas-reader (package_leaf, ARCH_LAYERS :990; §11 "The atlas read surface").
    - REPOINT (paths only): `Grounding`/`Degradation`, `read_current_summary` and `read_atlas_ontology` already live there (ground/report.rs, summary.rs).
    - Goes with ingest's half: the atlas WRITES (tools.rs:915 `write_atlas_ontology`) and `Pipeline` (:973).
    - The residue: `open_walk_provider` (corpus-engine/src/enrichment/atlas/provider.rs), `read_or_compute_atlas_summary`, the `CorpusEngine` at tools.rs:39, and `embed_http`. The §12 3a ladder places each, into the reader leaf by recipe where it is a read. A leaf move that would widen the budget for every package is §6.
  - "Watch a folder" becomes an ingest verb (§4 rule 1). sovereign-tools watched/enrich.rs and atlas_*'s `EnrichConfig::load` dial it, which closes sovereign-tools → sovereign-enrichment-catalog.
  - Grants keeps deciding and hands the merge to ingest as a work unit (auto_recover.rs:574,600, shard_manager.rs:703), which closes sovereign-grants → corpus-engine. Census 2026-09-25: the edge ALSO carries types. `CorpusEngine` is held at auto_recover.rs:60,204 and shard_manager.rs:8, and knowledge_assignment.rs:8,46,405 names `SourceFileRecord`/`SourceFileStatus`. Those types need a home by the ladder too, or the edge stays red after the merge moves.
  - fp-43, the authoring harness's drive home, closes sovereign-cli-llm → and sovereign-daemon → sovereign-authoring-harness.
  - corpus-engine's bundled recipe source (recipe_source/bundled.rs, five-programs-52) lifts to svrn's composition roots.
  - PROOF: LIFT(ingest) and LIFT(svrn) are pasted (svrn may still be 0 for edges owned by later rows). A grants merge e2e completes as a work unit. PLANT: link corpus-engine from grants, and LAYER goes red.
  LIFT ~1,500 lines; split by edge if the census exceeds it. — read: corpus-mcp/src, corpus-engine-atlas-reader/src/{summary.rs,ground/report.rs}, corpus-engine/src/enrichment/atlas/provider.rs, sovereign-tools/src/local_corpus/watched/enrich.rs, sovereign-grants/src/{auto_recover.rs,shard_manager.rs,knowledge_assignment.rs}, five-programs STATE.md row fp-43, corpus-engine/src/recipe_source/bundled.rs — check: CLEAN, LINT, TEST(corpus-mcp), TEST(sovereign-grants), TEST(sovereign-tools), LIFT(ingest), PLANT, LAYER, BOUNDARY (expect −6)
- [ ] pb-cli-llm — depends [pb-serve-program, pb-ingest] — OUTCOME: sovereign-cli-llm is a client. It links no serving, ingest or mesh internals, and bench's judges tell a foreign server's absence from a verdict.
  - RE-CENSUS FIRST, because five-programs-23's own falsifier is now true: `help` lives in sovereign-cli-base since fp-98; `sovereign_core::{types, setup_config, traits, tool_manifest, error, rebrand}` re-export contracts (sovereign-core/src/lib.rs:66-69); the 25 `::remote` refs re-export oicp-client. Repoint those (paths only).
  - Bench's turn drive (chat_cmd; census 2026-09-25: 65 `chat_cmd::` refs in 28 files outside chat_cmd/ itself) dials svrn.
  - The three local GGUF loads dial serve: router_fit_cmd.rs:403 and router_cache_cmd.rs:231 (`EmbedOnlyProvider::load`) dial serve's embedder, leaving one embedder decider. Census correction: inner_chaos/recall.rs:752 is a RERANKER load (`reranker_standalone::StandaloneReranker::load`), so it dials serve's `/v1/rerank`, which pb-serving-kinds adds and this row depends on through pb-serve-program.
  - Node identity repoints to `sovereign_contracts::node_identity` (portfolio_cmd/mod.rs:59, `sovereign_mesh::persist::load_or_generate_self_node_id`; partitions.rs:863, `sovereign_mesh::persist::load_node_id`). The hand re-implementation of `resolve_self_node_id`'s precedence (partitions.rs:861-880) goes.
  - The ingest half moves to the ingest CLI.
  - Defects, fixed in their own commits (kept from the deferred bench-dials row, phase-b-2):
    - the forced-choice judge fails OPEN on non-sovereign servers and becomes could-not-judge. Census 2026-09-25, site corrected. The `.ok()?` is sovereign-CORE's `forced_choice_ab` (runtime/grounding/judge.rs:143, `.ok()?` at :180). It folds "the server ignored `x_forced_choice` and returned text" into the same `None` as a transport error. The runtime gate fails open on `None` by documented contract (judge.rs:195-197), and that stays. The fix is on bench's side: `bench_cmd/live_runner.rs:773` (the one bench wrapper, pinned by xtask judge_funnel_gate) and its callers at :429, :482, :555, :615 and :731 report could-not-judge on `None`, never a verdict;
    - the quality-check precondition probe honours `SOVEREIGN_DAEMON_URL`. Census: it is sovereign-CLI's quality_check_cmd/exec.rs:31-37, whose `PortListening` probe connects to a hard-coded 127.0.0.1. Reproduced by reading.
  - Edges: cli-llm → sovereign-enrichment-catalog, sovereign-enrichment-build (normal and dev), sovereign-gliner, sovereign-pipeline, sovereign-eval, corpus-engine, sovereign-inference, sovereign-mesh.
  - PROOF: LAYER passes with those deps removed from cli-llm's Cargo.toml, and the cli-llm e2e journeys pass. PLANT: re-add one local GGUF load, and LAYER goes red. Also PLANT the judge fail-open, and the judge test goes red.
  LIFT ~1,600 lines; split by edge if the census exceeds it. — read: FIVE_PROGRAMS §11 "The cli-llm split", ralph/decisions/five-programs-{11,23}.md, sovereign-cli-llm/src, sovereign-core/src/runtime/grounding/judge.rs:140-200, sovereign-cli/src/quality_check_cmd/exec.rs — check: CLEAN, LINT, TEST(sovereign-cli-llm), TEST(sovereign-eval), TEST(sovereign-cli), PLANT, LAYER, BOUNDARY (expect −9)
- [ ] pb-pods-verb — depends [pb-membership] — OUTCOME: provisioning pods is a cmnwlth verb.
  - `svrn pipeline pod {up,pool,list,down}` (pipeline_cmd.rs:674 onward) plus worker_pod_provider.rs (391 lines) move to sovereign-cli-mesh as `svrn mesh pod …`.
  - `sovereign_pipeline::pod` (340 lines, with no ingest consumer) moves to sovereign-pods. cli-mesh already declares sovereign-pods (Cargo.toml:50) and uses it nowhere.
  - Delta: the old spelling answers with a named pointer to the new one (principle 6).
  - Closes sovereign-cli-llm → sovereign-pods and 5 of the mesh sites.
  - PROOF: `svrn mesh pod list` answers against the pod provider test double, and `svrn pipeline pod list` prints the pointer and exits non-zero. PLANT: remove the pointer, and the CLI contract test goes red.
  LIFT ~1,400 lines, mostly moves. — read: sovereign-cli-llm/src/{pipeline_cmd.rs,worker_pod_provider.rs}, sovereign-pipeline/src/pod.rs, sovereign-cli-mesh/Cargo.toml — check: CLEAN, LINT, TEST(sovereign-cli-mesh), TEST(sovereign-pods), `sovereign contract census`, PLANT, LAYER, BOUNDARY (expect −1)
- [ ] pb-meshapp-rehome — depends [pb-membership, pb-serving-kinds] — OUTCOME: the svrn daemon embeds no mesh application.
  - The seven fp-12 pairs are re-homed by the §12 3a ladder: sovereign-daemon → sovereign-meshapp-registry, sovereign-meshapp, sovereign-grants, code-next-edit, sovereign-tdd, sovereign-pods and sovereign-gliner.
  - gliner is closed by pb-serving-kinds; confirm it.
  - code-next-edit goes to the program §2 says serves FIM.
  - pods follows the worker exec split (TSV:23).
  - fp-47's app registry gets its one owner (sovereign-daemon → commonwealth-media).
  - The five-programs rows fp-12 and fp-47 carry the census and the HUMAN answers.
  - PROOF: each moved app's journey passes on its new host. PLANT: re-add one dependency, and LAYER goes red.
  LIFT ~1,500 lines; split by pair if the census exceeds it. — read: five-programs STATE.md rows fp-12, fp-47, HUMAN-fp12-daemon-embeds, HUMAN-fp47-app-registry — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-meshapp), TEST(sovereign-grants), PLANT, LAYER, BOUNDARY (expect −7)
- [ ] pb-daemon-adopts — depends [pb-daemon-mesh-exit, pb-code-clean, pb-svrn-dials-serve, pb-notes-split, pb-ingest-dial, pb-meshapp-rehome] — OUTCOME: the svrn daemon is a composition of the host kit. What remains of its routes are bundles on the ONE shell, and what remains of its tools are bundles on the ONE MCP dispatcher, so each §2c drive has one implementation (phase-b-2: the daemon adopts last, once it has shrunk).
  - Its remaining `mounted.push`/`mount_names.push` pairs (44 pushes at daemon.rs:3547-3699 at the census) and the two parallel matches `host_router_names`/`host_routers` (daemon_services.rs:456-487) collapse onto route bundles.
  - Its binds move onto `serve(...)`.
  - mcp_router's dispatch is replaced by the kit's dispatcher (pb-mcp names it), and its hand-rolled `is_localhost` (mcp_router.rs:209) goes.
  - Its tool registry (tool_registry.rs, the knowledge half that remains) is built from bundles.
  - The `sovereign tools call` builder (sovereign-cli-dev tools_cmd/registry.rs, 35 tools, whose header predicts drift) is built from the same bundles.
  - PROOF: the final copy-count table shows one bring-up, one root lock, one engine assembly, one MCP dispatch, one tool-set build, one route mounting and one job execution, against the census baseline. LIFT(svrn) passes. PLANT: mount one route outside a bundle, and the mount-trace test goes red.
  LIFT ~1,000 lines. — read: sovereign-daemon/src/{daemon.rs,daemon_services.rs,mcp_router.rs,tool_registry.rs}, sovereign-cli-dev/src/tools_cmd/registry.rs, the census baseline — check: CLEAN, LINT, TEST(sovereign-daemon), TEST(sovereign-cli-dev), `sovereign-cli-dev/tests/mcp_surface_e2e.rs`, LIFT(svrn), PLANT, LAYER, BOUNDARY
- [ ] pb-distribution — depends [pb-daemon-adopts, pb-code-index, pb-ingest-rehome, pb-cli-llm, pb-pods-verb] — OUTCOME: distributions are declared, every program runs alone, and Phase B is done.
  - `[[distribution]]` rows in quality/ARCH_LAYERS.toml, extending `[thin_surfaces]`, for: the `svrn` dispatcher (sovereign-cli), the setup wizard (sovereign-cli-daemon setup), sovereign-service and the desktop shell.
  - Each distribution may exec program binaries and link the wire leaves, the host kit, sovereign-turn-client and declared library faces. Its own code is capped as wiring (the operator sets the cap). layer-gate enforces it.
  - fp-25, setup's exec phase (sovereign-cli-daemon → sovereign-inference), closes: setup execs `serve` for model validation and download. That changes the Windows sidecar build contract (scripts/stage-daemon-sidecar.sh:94-95, `SOVEREIGN_SIDECAR_FEATURES`), which five-programs-38 names; update it in the same commit.
  - Delete the `sovereign-cli-base` re-export alias if it has no consumers left. If some remain, name them.
  - FINISH:
    - boundary-gate exits 0;
    - no `package = "svrn"` exception remains;
    - all six LIFTs pass;
    - the §2c copy counts are 1, with the final table in the body;
    - full suite and lint --full are green.
  LIFT ~1,000 lines. — read: FIVE_PROGRAMS §2c and §12 "Done", quality/ARCH_LAYERS.toml `[thin_surfaces]`, five-programs STATE.md row fp-25 and HUMAN-fp25-setup-host — check: CLEAN, `./scripts/sovereign-lint.sh --human --full`, TESTALL, PREPUSH, LIFT(svrn), LIFT(ingest), LIFT(cmnwlth), LIFT(serve), LIFT(code), LIFT(bench), LAYER, BOUNDARY (exit 0)

## Appendix — the handoff list

Written by five-programs' `REVIEW-handoff-phase-b` on 2026-09-25, at the gate on `cut` after 83fbdc620:
**boundary-gate 51 violation(s)**, 51 dep edges (49 normal, 2 dev) and 0 structural. The build.rs line closed with fp-105.
Every edge has exactly one appendix line in ralph/next/five-programs/STATE.md. The per-edge census, with sites
and counts, lives on that line, and the class is repeated here.

Owner histogram: 16 edges to the 8 inherited rows (fp-12 7, fp-11 2, fp-43 2, fp-25 1, fp-34 1, fp-47 1,
REVIEW-mint-fp-core-dial 1, REVIEW-mint-fp-mesh-dial 1). 35 edges to `phase-b`: 32 were NEEDS-OPERATOR lines,
and 3 are the ingest-dial class that HUMAN-fp7 (a) folded fp-7 and fp-29 into. Unowned: 0. There are also 5
excepted edges that are not red and that Phase B retires.

### Inherited rows (re-mint into their campaigns)

- [svrn] sovereign-cli-daemon → sovereign-inference — fp-25 (setup's exec phase, HUMAN-fp25 (a))
- [code] sovereign-cli-dev → sovereign-daemon — fp-11
- [svrn] sovereign-daemon → sovereign-code — fp-11 (the code MCP host; five-programs-39's MCP rule)
- [code] sovereign-cli-dev → corpus-engine — fp-34 (read mounts after fp-11's /mcp root; `code finalize`/`code watch` residue)
- [svrn] sovereign-cli-llm → sovereign-authoring-harness — fp-43 (the drive home, now an ingest dial)
- [svrn] sovereign-daemon → sovereign-authoring-harness — fp-43
- [svrn] sovereign-daemon → commonwealth-media — fp-47 (app registry's one owner)
- [svrn] sovereign-daemon → sovereign-meshapp-registry / sovereign-meshapp / sovereign-grants / code-next-edit / sovereign-tdd / sovereign-pods / sovereign-gliner — fp-12 (seven pairs)
- [svrn] sovereign-daemon → commonwealth-core — REVIEW-mint-fp-core-dial (fp-6 roster landed; residue)
- [svrn] sovereign-daemon → sovereign-mesh — REVIEW-mint-fp-mesh-dial (fp-6..8 verbs landed; residue)

### phase-b (open classes: each needs an operator decision or a Phase B host)

- ingest-dial class (corpus-mcp membership; HUMAN-fp7 (a) keeps canonical pull and the executor daemon-side):
  [svrn] corpus-mcp → corpus-engine, [svrn] corpus-mcp → sovereign-enrichment-build,
  [svrn] sovereign-daemon → corpus-engine (183 residue leaves), [cmnwlth] sovereign-mesh → corpus-engine (6),
  [svrn] sovereign-tools → sovereign-recipe-author (fp-29: RecipeProjectStore, the shim is load-bearing),
  [svrn] sovereign-tools → corpus-engine (170 residue refs that EXECUTE ingest as MCP tools),
  [svrn] sovereign-runtime-recipe → corpus-engine
- cli-llm split (five-programs-23; shared CLI-helper home, bench turn drive): [svrn] sovereign-cli-llm →
  sovereign-enrichment-catalog, → sovereign-enrichment-build (normal and dev), → sovereign-gliner,
  → sovereign-pipeline, → sovereign-eval, → corpus-engine (397 residue leaves), → sovereign-inference (3 local GGUF loads)
- node identity (five-programs-38/-39, lands with fp-9/fp-10's retirement): [svrn] sovereign-cli-llm → sovereign-mesh
- provisioning: [svrn] sovereign-cli-llm → sovereign-pods
- notes factory: [svrn] sovereign-cli → corpus-engine-notes, [svrn] sovereign-cli-llm → corpus-engine-notes,
  [svrn] sovereign-daemon → corpus-engine-notes, [svrn] sovereign-tools → corpus-engine-notes
- code_index's home (fp-5's refusal stands, REVIEW-mint-fp-cli-shared-leaf 59c38d4b8): [svrn] sovereign-cli-shared →
  corpus-engine, [code] sovereign-cli-dev → sovereign-cli-shared
- wire boundary (§12 recommendation untaken; fp-45's `process` executor seam): [svrn] sovereign-cli → commonwealth-work,
  [svrn] sovereign-daemon → commonwealth-work
- D6 keep, daemon-route closing condition untaken: [svrn] sovereign-cli → corpus-engine
- transport leaf question: [svrn] sovereign-daemon → commonwealth-transport
- work-atlas question (five-programs-54): [svrn] sovereign-daemon → sovereign-work-atlas, [cmnwlth] sovereign-mesh →
  sovereign-work-atlas (dev, fp-84's)
- watcher runtime owner (TSV:12): [svrn] sovereign-daemon → corpus-engine-watchers
- [svrn] sovereign-runtime-recipe → sovereign-gliner — the runtime lane's entity-extractor probe (load_gliner)
- [svrn] sovereign-tools → sovereign-enrichment-catalog — the watched-folder config writer's owner
- [code] sovereign-cli-dev → sovereign-tools — SpecWatcher + the MCP surface list
- [code] sovereign-cli-dev → sovereign-enrichment-build — scoring types (ChatPrompt)
- [cmnwlth] sovereign-grants → corpus-engine — dial or drop

### Excepted edges Phase B retires (not red; quality/ARCH_LAYERS.toml [[exception]] rows with `package = "svrn"`)

- sovereign-daemon → commonwealth-discovery — fp-9
- sovereign-daemon → sovereign-inference, sovereign-daemon → sovereign-compute — fp-10
- sovereign-daemon → sovereign-serving-host — fp-68
- sovereign-runtime-recipe → sovereign-inference — fp-69

### Non-edge item

- corpus-engine's default recipe/asset source (five-programs-52): `corpus-engine/src/recipe_source/bundled.rs` is the
  one corpus-engine module that names the `sovereign-recipes` data crate (`corpus_engine_recipes`), behind
  `default_source()` / `default_assets()`. It lifts out to svrn's composition roots once svrn dials ingest. Owner:
  the ingest serving campaign.

### Owner rows (phase-b-1, 2026-09-25; reconciled by REVIEW-pb-census against the merged tree)

REVIEW-pb-census ran the gate on `cut` at b7fbc9909, which contains origin/main (merge c65607b10):
**51 violation(s)**, and the same 51 edges as the handoff list above. The merge added no edge and closed none.
Two ownerships were made explicit or moved, with the count unchanged. cli-dev → corpus-engine moved from
pb-code-index to pb-code-clean, and daemon → gliner went to pb-serving-kinds.

Owner histogram (51):
- pb-cli-llm 9;
- pb-meshapp-rehome 7;
- pb-ingest-rehome 6;
- pb-ingest-dial 5;
- pb-code-clean 5;
- pb-notes-split 4;
- pb-code-index 3;
- pb-code-server 3;
- pb-daemon-mesh-exit 3;
- pb-serving-kinds 2;
- pb-work-doors 2;
- pb-pods-verb 1;
- pb-distribution 1.

Unowned: 0. Each row's BOUNDARY expectation matches its count.

Each red edge has exactly one owner row:

- **pb-code-clean:** daemon → sovereign-code (fp-11); daemon → work-atlas; mesh → work-atlas (dev); cli-dev → enrichment-build; cli-dev → corpus-engine (fp-34; moved here from pb-code-index by REVIEW-pb-census).
- **pb-code-index:** cli-shared → corpus-engine; cli-dev → cli-shared; cli → corpus-engine (D6).
- **pb-code-server:** cli-dev → daemon (fp-11); daemon → corpus-engine-watchers; cli-dev → sovereign-tools.
- **pb-ingest-rehome:** cli-llm → authoring-harness and daemon → authoring-harness (fp-43); corpus-mcp → corpus-engine; corpus-mcp → enrichment-build; tools → enrichment-catalog; grants → corpus-engine; the recipes default source (the non-edge item).
- **pb-ingest-dial:** daemon, tools, mesh and runtime-recipe → corpus-engine; tools → recipe-author.
- **pb-meshapp-rehome:** daemon → commonwealth-media (fp-47); six fp-12 pairs (meshapp-registry, meshapp, grants, code-next-edit, tdd, pods). The seventh, daemon → gliner, is pb-serving-kinds'.
- **pb-daemon-mesh-exit:** daemon → commonwealth-core; daemon → sovereign-mesh; daemon → commonwealth-transport.
- **pb-cli-llm:** the cli-llm split class (8 edges) and cli-llm → sovereign-mesh (node identity).
- **pb-pods-verb:** cli-llm → sovereign-pods.
- **pb-notes-split:** the notes factory (4 edges).
- **pb-work-doors:** cli → commonwealth-work; daemon → commonwealth-work.
- **pb-serving-kinds:** runtime-recipe → gliner; daemon → gliner (fp-12's seventh pair); runtime-recipe → inference (the fp-69 exception).
- **pb-distribution:** cli-daemon → inference (fp-25).

Excepted edges and their owner rows:

- **pb-membership:** daemon → commonwealth-discovery (fp-9).
- **pb-svrn-dials-serve:** daemon → inference and daemon → compute (fp-10); daemon → serving-host (fp-68).

### Defects found in the 2026-09-25 diligence (REVIEW-pb-census reproduces or strikes each)

Verdicts from REVIEW-pb-census, 2026-09-25. Every one was reproduced by reading at HEAD; none needed a run, and none was struck. Sites are in the owner rows.

- **Hot reload bypasses the engine factory.** On a `kind="remote"` node it loads GGUFs, and under `distributed_primary` it loads the withheld primary in-process (provider.rs:37-62). REPRODUCED: nothing on the path reads `[engine] kind` or `distributed_primary`. Owner: pb-serving-assembly.
- **Daemon `/v1/models` makes a per-request round trip to cw-rails,** which means a self-fact stored in another process and a status produced by a blocking call. REPRODUCED, NARROWED: only the no-local-inference fallback does this (`store_rows`, routes_inference.rs:960). A node with local inference answers from its own manifest (:792). Owner: pb-serve-program, which never takes the path; pb-svrn-dials-serve retires it.
- **GLiNER is loaded twice in one daemon,** with different models when the env var is set (boot.rs:598, runtime-recipe lib.rs:667→832). REPRODUCED. Owner: pb-serving-kinds.
- **The daemon installs a rerank slot that nothing reads;** there is no route, and the recipe opts out. REPRODUCED: no `/v1/rerank` route, and boot.rs:930 passes `AlreadyInProvider`. Owner: pb-serving-kinds.
- **`code_search` silently swaps to full-text search** on an embed error (sovereign-code/src/code_search.rs:123). REPRODUCED. Owner: pb-code-server.
- **`project serve` falls back to an in-memory notes store,** so writes vanish at exit (serve.rs:234-252). REPRODUCED. Owner: pb-code-server.
- **`project init` stamps a model name on zero vectors** (project_init/mod.rs:506-513). REPRODUCED. Owner: pb-code-index.
- **The chat model is resolved under `--no-enrich`,** and GLiNER absence is skipped silently (corpus-mcp ingest.rs:222-242; engine/ingest.rs:1926). REPRODUCED. Owner: pb-ingest.
- **`knowledge_lookup` collapses an error into an empty result,** and it treats every notes kind as evidence (knowledge_lookup/mod.rs:294-302). REPRODUCED. Owner: pb-notes-split.
- **The forced-choice judge fails open on non-sovereign servers,** and the quality probe ignores `SOVEREIGN_DAEMON_URL`. REPRODUCED, SITE CORRECTED. The collapse is core's judge.rs:180, and the fix is on bench's side (live_runner.rs:773 and its callers). The probe is sovereign-cli's quality_check_cmd/exec.rs:31-37. Owner: pb-cli-llm.
- **A `client_daemon_base` parse error becomes the default port.** REPRODUCED (setup_config.rs:1727-1729, `.unwrap_or_else(|_| default_client_port())`), and pc-config-split names it. Owner: phase-c's pc-config-split.
- **`mobile_host.rs` targets the deleted sovereign-server,** and so does `Launch::Server`. REPRODUCED, WIDER: the desktop is a second caller. Owner: pb-delete-dead.
- **Stale citations:** all six REPRODUCED.
  - sovereign-tools mcp_surface.rs:4-9;
  - mcp_router.rs:37-39;
  - corpus-mcp mcp.rs:3;
  - mcp_demo_server.rs:15;
  - tool_bundle.rs:10;
  - bin/sovereign-daemon.rs:2-4, which still calls itself "the cmnwlth binary's own main".

  Owner: pb-delete-dead.

### Census baseline (REVIEW-pb-census, 2026-09-25, at b7fbc9909): the §2c drive copy counts later rows report against

- **Bring-up paths: 3.** The decider is `ServingHost::ensure_reachable` (sovereign-turn-client reach.rs:323; production callers in sovereign-cli-mesh mesh_cmd.rs, sovereign-daemon rails_client/bring_up.rs, and the desktop's serving_host.rs and mobile_host_setup.rs). There are two private copies: sovereign-cli-daemon daemon_cmd/lifecycle.rs:581 `start_daemon`'s ready probe, and sovereign-cli serve_cmd.rs:83 `spawn_background`.
- **Sibling-binary locators: 1 shared + 5 private, by name.**
  - Shared: `locate_sibling`, reach.rs:418.
  - Private: sovereign-cli quality_check_cmd/exec.rs:98 `locate_binary`, sovereign-cli serve_cmd.rs:302 `locate_dev_bin_for_spawn`, sovereign-cli-dev drift_cmd_orchestrator.rs:54 `resolve_sovereign_bin`, sovereign-contracts mobile_host.rs:198 `resolve_server_binary` (pb-delete-dead removes it), and the desktop's daemon_binary.rs:66 `daemon_binary`.
  - This is a lower bound from a fn-name grep. phase-b-1's "7 identical sibling locators" came from a different method, and the dispatcher's own sovereign-cli sibling.rs/dev_bin.rs/daemon_bin.rs are the exec mechanism, not counted here.
- **Root locks: 4.** sovereign-contracts run_lock.rs:173 (libc flock), sovereign-core deep_research/state.rs:195 (a second `RunLock`), corpus-engine-scip scip_graph.rs:484 (fs4) and commonwealth-rails lib.rs:154 (std `try_lock`).
- **MCP dispatch loops: 3.** sovereign-daemon mcp_router.rs:408 `dispatch`, corpus-mcp mcp.rs:49 and sovereign-cli-llm mcp_demo_server.rs:102.
- **Tool-set builders: 7 production `ToolRegistry::new()` sites**, with test modules excluded:
  - sovereign-daemon tool_registry.rs:48;
  - sovereign-runtime-recipe lib.rs:547;
  - sovereign-cli-dev project_cmd/serve.rs:389 and tools_cmd/registry.rs:163 (35 `register` calls);
  - sovereign-workflow-host lib.rs:223;
  - sovereign-cli-llm knowledge_gym_cmd/production.rs:247 and recipe_agent_live_trial.rs:1352.
- **Engine assemblies: 3.** sovereign-daemon build/inference.rs:67 `load_provider`, provider.rs:36 `LlamaCppFactory`, and sovereign-compute child_main.rs:354/374/385.
- **Route mounting: 1 daemon mount list of 44 `push`es** (daemon.rs:3547-3699) plus `host_routers`/`host_router_names` (daemon_services.rs:456-487). pb-shell's four adopters each call `axum::serve` themselves.
- **Job execution: 2 paths for ingest.** commonwealth-work `JobExecutorRegistry` (executor.rs:374) with `IngestExecutor`, and the legacy pull loop (auto_ingest.rs:798, driven at :183).
- **bench's closure: 245 crates.** That is `cargo tree -p sovereign-eval -e normal --prune workspace-hack`, 7 of them workspace crates: kernel-types, oicp-types, sovereign-contracts, sovereign-eval, sovereign-time, understanding-vocab and workspace-hack. Without the prune, hakari's unified set makes it 660.
