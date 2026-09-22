# five-programs — the ralph queue

Protocol: `ralph/next/five-programs/PROMPT.md` (read it FIRST — it carries the
standing facts so no row has to). Catalog: `docs/FIVE_PROGRAMS_DECISIONS.tsv`
(79 rows; each queue row names the TSV pair it works). Decisions:
`docs/FIVE_PROGRAMS.md` §12 (six decisions, all taken). Method + refusal
history: §11.

Baseline when this queue was minted: **boundary-gate 79 at 09ff299b8**
(branch `cut`, Phase A atlas carve landed, arch/clock/docs gates ✓ clean,
nothing pushed). The per-edge list lives in the appendix at the bottom; row
fp-0 re-measures it.

NEEDS-OPERATOR (not queued — the §12 decisions do not cover them):
`commonwealth-transport` leaf question (TSV, 116 refs) ·
`sovereign-work-atlas` placement blocked on its own deps (§12 open item) ·
the state-wire durable store owner (§12 decision 4 leaves the red edge as
the honest state until it exists; covers daemon→commonwealth-state 89 and
cli-llm→commonwealth-state 2) ·
`sovereign-cli → commonwealth-{work,rail}`: the wire boundary is priced and
refused twice (§12) but NOT taken (TSV sovereign-cli→commonwealth-work, 18) ·
`sovereign-cli → corpus-engine` project_init: D6 keeps the zero-vector index;
the closing condition (route init through the daemon) is the §12 singleton,
untaken (TSV, 3) ·
the notes factory — which non-svrn host constructs the concrete NoteStore
(TSV sovereign-cli→corpus-engine-notes 4, sovereign-cli-llm→corpus-engine-notes
4; fp-3/fp-13 move only the types) ·
`sovereign-cli-llm → sovereign-mesh` node identity: daemon mesh surface or
leaf-extract persist::load_node_id (TSV, 12) ·
`sovereign-cli-llm → sovereign-pods` owner-side pod provisioning surface
(TSV, 19) ·
`sovereign-cli-llm → sovereign-authoring-harness` drive home: bench host or
leaf (TSV, 3; fp-18 is the daemon-side half) ·
`sovereign-cli-dev → sovereign-enrichment-build` scoring types:
RemoteApiProvider repoint or engine types down (TSV, 2) ·
`corpus-mcp` membership (§12's open checklist item): the non-atlas
ingest-dial residue of corpus-mcp→corpus-engine plus
corpus-mcp→sovereign-enrichment-build (4) ·
`sovereign-code → corpus-engine` (dev) e2e_code_intel: minted fixture or
move to [ingest] (TSV, 3) ·
`sovereign-grants → corpus-engine` canonical-merge: dial or drop (TSV, 22) ·
`sovereign-core` router_calibration.rs:1253 include_str embed — gate
structural #1; §12's singleton names relocation to sovereign-core/data/calibration
as the candidate ·
corpus-engine build.rs — gate structural #2: compile-time vendoring of
sovereign-recipes vs the package no-build.rs rule.

Pointer keys:
**TSV** = docs/FIVE_PROGRAMS_DECISIONS.tsv · **FP** = docs/FIVE_PROGRAMS.md ·
**AL** = quality/ARCH_LAYERS.toml (leaf rows ~940-1010; packages: svrn ~1090,
ingest ~1150, cmnwlth ~1190, code ~1240, bench ~1270) ·
**LEAF** = corpus-engine-atlas-reader/ · **CE** = corpus-engine/src/ ·
**DA** = sovereign/crates/sovereign-daemon/src/ ·
**CW** = commonwealth/crates/ · **SC** = sovereign/crates/sovereign-contracts/src/

- [~] fp-0 — depends [] — RE-MEASURE the burn-down: run the gate, paste the full per-edge list into the appendix below, and against each edge write its TSV row id (source→target pair) and which queue row owns it; any edge no queue row owns gets a new row here (decided TSV rows only — a TSV row with a non-empty decision_needed cell becomes a NEEDS-OPERATOR line instead). — read: PROMPT standing facts, TSV, FP §12 — check: the appendix lists every one of the gate's violations exactly once
- [ ] fp-16 — depends [fp-0] — DIAL serving-host (§12 D2 — the serving cluster is cmnwlth's own process): the daemon's serving-host use (TSV sovereign-daemon→sovereign-serving-host, 51 refs: VenueHost trait DA venue_host.rs:18, AttachedPrincipal DA routes_ollama.rs:230, SlotManifest DA slot_manifest.rs:16, DA state/serving.rs) becomes the /v1 serving dial; the VenueHost port trait moves to sovereign-contracts (fp-13 port shape), construction stays with the serving process; absence reported ("a daemon alone serves no model"). — read: TSV row, §12 D2, DA venue_host.rs — check: scoped lint 0; gate delta recorded
- [ ] fp-17 — depends [fp-0] — EXTRACT serving-policy's arithmetic into a leaf (TSV sovereign-daemon→serving-policy, 5 refs): fair-sched arithmetic + the pipeline_aliases table move to a vocabulary leaf (the rail-core pattern; test it against contracts per D3 first, mint the leaf only if the leaf test — no fs, no store — holds and contracts does not), serving-policy re-imports, the daemon repoints. — read: TSV row, AL leaf rows (~940-1010) — check: scoped lint 0; AL leaf row added; gate drops 1
- [ ] fp-18 — depends [fp-0] — MOVE the harness drive to its host home (TSV sovereign-daemon→sovereign-authoring-harness, 3 refs): run_over_frozen_sample + Declaration/HarnessRun (DA recipe_http.rs:620, :27) get a home outside [svrn] — a leaf if the drive passes the leaf test (no fs — it likely does NOT; frozen samples are read from disk), else the [bench] program; the daemon repoints or dials. CONDITIONAL: the cli-llm side of this question is NEEDS-OPERATOR (drive home); if the operator's answer lands differently, the operator edits this row. — read: TSV row, sovereign-authoring-harness/src — check: scoped lint 0; gate delta recorded
- [ ] fp-19 — depends [fp-0] — DIAL the atlas build (TSV sovereign-daemon→sovereign-enrichment-build, 2 refs): the daemon's atlas_builder.rs (ParsedBuild::from_inputs :41, build_with_progress_with_embedder :53) becomes a call into the ingest program's build surface; absence reported. — read: TSV row, DA atlas_builder.rs — check: scoped lint 0; gate drops 1
- [ ] fp-20 — depends [fp-0] — REPOINT the daemon's media routes to cw-rails (TSV sovereign-daemon→commonwealth-media, 33 refs; the routes are ALREADY served — /v1/mesh/media, /v1/mesh/media/fanout, /v1/mesh/publish — nothing new to build): DA media_presence.rs:109, publish_http.rs:33, media_reach.rs:44, origin_fanout.rs:50 become client stubs; absence reported. — read: TSV row, CW commonwealth-rails routes — check: scoped lint 0; daemon Cargo.toml drops commonwealth-media; gate drops 1
- [ ] fp-21 — depends [fp-0, fp-6] — DIAL the daemon's in-process /v1/rail mount (TSV sovereign-daemon→commonwealth-rail, 51 refs): DA routes_rail.rs:35 admit and the rail serving sites become cw-rails clients (cw-rails already serves /v1/rail/{log,append,live} per the TSV fix); wire types are already in the commonwealth-rail-core leaf. — read: TSV row, DA routes_rail.rs, §12 D2 — check: scoped lint 0; gate delta recorded
- [ ] fp-22 — depends [fp-0, fp-7] — DIAL the work donor (TSV sovereign-daemon→commonwealth-work, 46 refs; §12 D2 names -work's leaf dead): DA work_donor.rs:80-90 and ingest_executor.rs:84 act sites become cw-rails wire calls; the work-MODEL types re-derive as contracts DTOs; absence reported. — read: TSV row, §12 D2, DA work_donor.rs — check: scoped lint 0; gate delta recorded
- [ ] fp-23 — depends [fp-0, fp-2] — REPOINT sovereign-cli-daemon's watcher-schema consts (TSV sovereign-cli-daemon→corpus-engine-watchers, 8 refs, const): Registry/ProjectEntry consts import the sovereign-contracts home fp-2 created; drop corpus-engine-watchers from cli-daemon's Cargo.toml. — read: TSV row, fp-2's diff — check: scoped lint 0; gate drops 1
- [ ] fp-24 — depends [fp-0, fp-9] — REPOINT sovereign-cli-daemon's 2 join-key refs (TSV sovereign-cli-daemon→sovereign-mesh) to the pure leaf fp-9 extracted; drop sovereign-mesh from cli-daemon's Cargo.toml. — read: TSV row, fp-9's diff — check: scoped lint 0; gate drops 1
- [ ] fp-25 — depends [fp-0] — SPLIT the setup verb's inference needs (TSV sovereign-cli-daemon→sovereign-inference, 28 refs): setup_planner goes portable (types to sovereign-contracts per D3), the 18 binary/HTTP refs (rpc-worker/llama-log/hardware-detect, §11) dial the daemon's /v1 surface; absence reported. — read: TSV row, §11 setup, sovereign-cli-daemon/src — check: scoped lint 0; gate delta recorded
- [ ] fp-26 — depends [fp-0] — GRANDFATHER the keep edge (TSV sovereign-cli-llm→sovereign-cli-mesh, 1 ref; §12's Keep class names "cli→cli-mesh"): add the one [[exception]] row naming exactly this edge in quality/ARCH_LAYERS.toml (package svrn, reason: §12 keep class + the TSV fix cell "keep"); nothing else in AL moves. — read: TSV row, §12 class table Keep row, AL exceptions — check: gate drops 1; the exception names only this edge
- [ ] fp-27 — depends [fp-0] — MOVE the 3 real-SQL tests beside their owner (TSV sovereign-core→corpus-engine-notes, 11 refs, test-only; §12 D6 taken: "move them to the owner's test tree, not grandfather"): the tests move into corpus-engine-notes' test tree; sovereign-core drops the dev-dependency. — read: TSV row, §12 D6, sovereign-core tests — check: scoped lint 0; TEST(corpus-engine-notes) exit=0; gate drops 1
- [ ] fp-28 — depends [fp-0] — PORT the enrichment catalog reader (TSV sovereign-tools→sovereign-enrichment-catalog, 6 refs): list_enriched_corpora_in gets its port trait in sovereign-contracts, impl in the owner; tools injects instead of linking. — read: TSV row, sovereign-tools/src — check: scoped lint 0; gate drops 1
- [ ] fp-29 — depends [fp-0] — PORT the recipe FeatureStore (TSV sovereign-tools→sovereign-recipe-author, 2 refs, trait): the FeatureStore port trait goes to sovereign-contracts; the pub use shim drops. — read: TSV row — check: scoped lint 0; gate drops 1
- [ ] fp-30 — depends [fp-0] — MOVE mesh_http/roster_repair DTOs into contracts::daemon_wire (TSV sovereign-cli-mesh→sovereign-daemon, 12 refs; fix cell "step-10 de-embed"): the DTOs move (D3 shape — wire vocabulary to contracts), cli-mesh dials the daemon instead of linking it; guest_door rendering home per the fix cell. — read: TSV row, sovereign-cli-mesh/src — check: scoped lint 0; cli-mesh Cargo.toml drops sovereign-daemon; gate drops 1
- [ ] fp-31 — depends [fp-0] — EXTRACT client SetupConfig + the rebrand accessor into sovereign-contracts (TSV sovereign-cli-dev→sovereign-core, 22 refs, type-only): sovereign-core re-imports at historical paths (a re-export, never a twin — ARCH §10.6); cli-dev drops sovereign-core if nothing else remains (record what remains). — read: TSV row, sovereign-core SetupConfig — check: scoped lint 0; gate delta recorded
- [ ] fp-32 — depends [fp-0] — PORT StateStore (TSV sovereign-cli-dev→sovereign-store, 2 refs, type-only): the StateStore trait moves to sovereign-contracts (mirrors RecipeNotes, §11); construction at the composition root. — read: TSV row — check: scoped lint 0; gate drops 1
- [ ] fp-33 — depends [fp-0] — DIAL mesh-KV + node identity for cli-dev (TSV sovereign-cli-dev→sovereign-mesh, 9 refs): the daemon exposes mesh-KV + node-identity routes (step-10 de-embed); resolve_self_node_id leaf-extracts per the fix cell; cli-dev dials. — read: TSV row — check: scoped lint 0; gate delta recorded
- [ ] fp-34 — depends [fp-0] — DIAL the project index-build route for cli-dev (TSV sovereign-cli-dev→corpus-engine, 16 refs): the daemon exposes the project code-corpus build route (step-10 de-embed); cli-dev dials instead of linking corpus-engine for the build half; the atlas-read half belongs to fp-14. — read: TSV row — check: scoped lint 0; gate delta recorded
- [ ] REVIEW-mint-fp-core-dial — depends [fp-0, fp-6] — REVIEW-MINT the daemon→commonwealth-core residue (TSV sovereign-daemon→commonwealth-core, 422 refs; the leaf-vs-dial question in the TSV cell is DECIDED by §12 D2 — no substrate leaf, the daemon dials): after fp-6, measure the remaining commonwealth_core:: sites in sovereign-daemon, classify each site dial/DTO per D2, and mint atomic rows directly below in §2's grammar. — read: TSV row, §12 D2, fp-6's diff — check: every minted row has one VERB, a grep-able premise and §5 checks; mint count in the commit body; cap 8 — past it, NEEDS_HUMAN with the count, never queue growth
- [ ] REVIEW-mint-fp-mesh-dial — depends [fp-0, fp-6, fp-7, fp-8] — REVIEW-MINT the daemon→sovereign-mesh residue (TSV sovereign-daemon→sovereign-mesh, 250 refs; cell question decided by D2 — cw-rails is the owner): after fp-6/7/8, measure the remaining sovereign_mesh:: sites in sovereign-daemon, classify dial/DTO, mint atomic rows directly below. — read: TSV row, §12 D2, the three verb rows' diffs — check: as the core mint; cap 8 — past it, NEEDS_HUMAN
- [ ] REVIEW-mint-fp-cli-llm-split — depends [fp-0, fp-14] — REVIEW-MINT the cli-llm split (TSV rows sovereign-cli-llm→{eval 58, enrichment-build 25, gliner 12, pipeline 7, enrichment-catalog 3, enrichment-build dev, corpus-engine 355 enrich half}; the fix cells decide "cli-llm bench-crate split" + "cli-llm ingest-crate split"): measure the cmd module file sets (bench_cmd; enrich/corpus/awareness/govern cmds; chat_cmd) after fp-14's repoint, mint the split rows (bench half → [bench], ingest half → [ingest]) plus the chat-dial residue row (cli-llm→inference: SplitInferenceProvider → daemon /v1 dial) if it survives, directly below. — read: TSV rows, §12 class table placement row, sovereign-cli-llm/src — check: as the core mint; cap 8 — past it, NEEDS_HUMAN
- [ ] REVIEW-mint-fp-atlas-residue — depends [fp-0, fp-14] — REVIEW-MINT the non-atlas corpus-engine residue in sovereign-core + sovereign-tools (TSV sovereign-core→corpus-engine 65, of which 39 atlas-read are fp-14's; TSV sovereign-tools→corpus-engine 286, of which 172 atlas-read are fp-14's): after fp-14, measure the remaining surface (TSV: CorpusEngine/meta_atlas/Wikipedia residue in core; CorpusEngine/recipe residue in tools), classify dial vs move per D1/D2, mint atomic rows directly below. — read: TSV rows, fp-14's diff — check: as the core mint; cap 8 — past it, NEEDS_HUMAN
- [ ] fp-1 — depends [fp-0] — MOVE the scheduler venue vocabulary into sovereign-contracts: `VenueSource` + `InferenceVenue` (sovereign-scheduler venue.rs) and the pure `slot_aliases::resolution_alias_keys` move to SC (a `venue.rs` module); scheduler and daemon import them from contracts; scheduler keeps its impls and fs-touching decision_trace. §12 decision 3 shape (vocabulary to the two owners). TSV pair sovereign-daemon→sovereign-scheduler (9 refs: DA venue_host.rs:16, DA build/inference.rs:132+495). — read: TSV row, SC traits.rs (import pattern), sovereign-scheduler/src/venue.rs — check: daemon Cargo.toml has no sovereign-scheduler; scoped lint 0; gate count drops by 1
- [ ] fp-2 — depends [fp-0] — MOVE the watcher `projects` schema into sovereign-contracts (§12 decision 3): the schema type(s) corpus-engine-watchers publishes that cross the daemon boundary move to SC; corpus-engine-watchers re-imports at historical paths. TSV pair sovereign-daemon→corpus-engine-watchers. — read: TSV row, corpus-engine-watchers/src (find the projects schema by its consumers in DA), §12 decision 3 — check: scoped lint 0; daemon's watcher-schema refs point at SC; gate drops only if that was the pair's last use — record what remains
- [ ] fp-3 — depends [fp-0] — MOVE the notes DTO set into sovereign-contracts (§12 decision 3): the notes DTO types crossing the daemon boundary (TSV row names them) move to SC; owners re-import. TSV pair sovereign-daemon→corpus-engine-notes (21 refs; the row lists EmbedFn/GlinerFn/PropagationSinkFn/NodeRoster/RosterEntry/NotePropagationEvent/ProjectDocsStore/decision_extractor as the port extension — move the TYPES that are vocabulary, leave construction with the owner per §12). — read: TSV row, §12, corpus-engine-notes/src — check: scoped lint 0; record remaining refs
- [ ] fp-4 — depends [fp-0] — SPLIT runtime-recipe (§12 decision 5): the runtime ASSEMBLY half moves to [svrn] membership; the ingest lane stays. AL package rows + the crate's internal split follow §12's line (assembly = the runtime's own lifetime). TSV row sovereign-cli-llm→sovereign-runtime-recipe. — read: TSV row, §12 decision 5, sovereign-runtime-recipe/src layout — check: scoped lint 0; AL rows updated; gate delta recorded
- [ ] fp-5 — depends [fp-0] — MOVE tools::notes + cli-shared::code_index to their owning programs (§12 decision 5): `sovereign-tools::notes` (patterns, diff_extract, response_mine) and `sovereign-cli-shared::{code_index, scip, observation, rail}` thin half — code-program items go [code]; cli-shared keeps only the thin dispatcher helpers. TSV rows sovereign-cli-dev→sovereign-cli-shared and sovereign-cli-dev→sovereign-tools. — read: TSV rows, §12 decision 5, sovereign-cli-shared/src — check: scoped lint 0; AL membership updated; gate delta recorded
- [ ] fp-6 — depends [fp-0] — DIAL the roster verbs (§12 decision 2): cw-rails owns forget_member (DA roster_repair.rs:90) and ring_roster (DA ring_sync.rs:218); the daemon handlers become client stubs that report ABSENCE when the serving process is down (never a silent fallback — §12 decision 2, principle 6). Extend the cw-rails route first, then swap the daemon's in-process call for the client. TSV pairs sovereign-daemon→commonwealth-rail / →commonwealth-core (roster parts). — read: TSV rows, §12 decision 2, commonwealth-rails routes (the /v1/mesh/* precedent), the two DA sites — check: scoped lint 0; the two daemon sites dial, not embed; gate delta recorded
- [ ] fp-7 — depends [fp-6] — DIAL canonical_pull + the ingest-run dialect (§12 decision 2): DA auto_ingest.rs:354 canonical_pull and the ingest_executor's in-process CorpusEngine construction (DA ingest_executor.rs:88) become calls into the ingest program's serving surface; absence reported, never defaulted. TSV pairs sovereign-daemon→corpus-engine (ingest dialect parts), sovereign-mesh→corpus-engine (canonical_sync). — read: TSV rows, §12 decision 2, §11:961 (project init keep — do NOT touch it) — check: scoped lint 0; gate delta recorded
- [ ] fp-8 — depends [fp-6] — DIAL rail_kv_pump + guest_source (§12 decision 2): the daemon's rail KV pump and guest tunnel source become cw-rails clients; `guest_route::open_route`'s tunnel handle stays the MESH's (decision 6: wire not keep — the daemon dials the daemon's guest surface). TSV rows sovereign-cli-llm→commonwealth-state / →sovereign-mesh (guest_route parts). — read: TSV rows, §12 decisions 2+6 — check: scoped lint 0; gate delta recorded
- [ ] fp-9 — depends [fp-6] — DIAL mesh_discovery + iroh + the discovery reads (§12 decision 2): the daemon's mdns::MdnsDiscovery + hardware::read_disk_free_bytes become cw-rails reads, and the join-key validation half extracts to a pure leaf per the TSV (membership::hash_join_key/validate_join_key_format family — fs-free, then a [[package_leaf]] row with allow = kernel-types/workspace-hack + its deps). TSV pair sovereign-daemon→commonwealth-discovery (14 refs). — read: TSV row, CW commonwealth-discovery/src/membership.rs:23-110 — check: scoped lint 0; daemon Cargo.toml drops commonwealth-discovery; gate drops 1
- [ ] fp-10 — depends [fp-7] — DIAL inference/rpc-worker + compute (§12 decision 2): the daemon's in-process inference, rpc-worker and compute supervisions become serving-surface clients reporting ABSENCE ("a daemon alone serves no model"). TSV pairs sovereign-daemon→sovereign-inference (42) / →sovereign-compute (25). — read: TSV rows, §12 decision 2 — check: scoped lint 0; gate delta recorded
- [ ] fp-11 — depends [fp-10] — DIAL code tools via a project-scoped /mcp root (§12 decision 2 + §11 probe): project serve needs the daemon to mount a project-scoped /mcp root; the daemon's sovereign-code/tool_registry mount becomes the client. TSV pair sovereign-daemon→sovereign-code (32 refs in one mount file). — read: TSV row, §11 project-serve probe, DA tool_registry.rs — check: scoped lint 0; gate delta recorded
- [ ] fp-12 — depends [fp-10] — DIAL grants/queue + watchers + meshapp + registry + code-next-edit + tdd + pods + gliner + runtime-recipe assembly (§12 decision 2, the remaining daemon embeds, one commit per PAIR): same shape — cw-rails (or the owning program's serving surface) owns the verb, the daemon dials, absence is reported. TSV pairs: sovereign-daemon→sovereign-grants, →sovereign-meshapp(-registry), →code-next-edit, →sovereign-tdd, →sovereign-pods, →sovereign-gliner, →sovereign-runtime-recipe. — read: TSV rows, §12 decision 2 — check: scoped lint 0 per pair; gate delta recorded per commit
- [ ] fp-13 — depends [fp-0] — PORT the TSV `ports` rows (trait in contracts + impl in owner + injection; the consumer must stop CONSTRUCTING — construction stays with the owner): pairs sovereign-daemon→sovereign-meshapp, →sovereign-meshapp-registry, →sovereign-runtime-commission, →sovereign-gliner (port half), →corpus-engine-watchers (port half), notes port extension (the construction-factory question is NEEDS-OPERATOR — do only the type moves fp-3 left). — read: TSV rows' `fix` cells, §12 — check: per pair, the daemon no longer constructs the owner's runtime; scoped lint 0; gate delta recorded
- [ ] fp-14 — depends [fp-1, fp-2, fp-3, fp-5] — PLACE per-closure repoints, one commit per consumer crate: for each of tools / cli-llm / core / corpus-mcp / daemon / meshapp / mesh / cli-dev whose OTHER rows have landed, repoint its remaining atlas-read paths to corpus_engine_atlas_reader + its vocab-shim paths to understanding_vocab, then drop corpus-engine from its Cargo.toml IF nothing else remains; a crate with residue left keeps the edge and records what remains in the appendix. THE FOLD: this is the W5 repoint — zero yield alone, done here where it closes edges. — read: PROMPT standing facts, TSV pairs (*→corpus-engine ×8) — check: per crate either the edge closes (gate drops) or the appendix names the exact remaining refs
- [ ] fp-15 — depends [fp-14] — RE-MEASURE + reconcile: run the gate, update the appendix, requeue any edge the TSV covers that no row handled, and verify the three finish conditions in FP §11. If the gate is 0 and the finish conditions hold, write ralph/DONE. — read: FP §11 finish conditions, the appendix — check: gate count + finish conditions, each quoted

## Appendix — fp-0 re-measure (gate at 38a084b36, 2026-09-22: 79 violation(s) = 77 dep edges + 2 structural; same total as the mint)

Owner key: `fp-N` = the queue row minted for it · `REVIEW-mint-fp-<x>` = a minted REVIEW row that mints the atomic rows once its deps land · `NEEDS-OPERATOR` = the TSV row's decision cell is open — see the block at the top.

### [svrn] — 59 dep edges + 1 structural

- [svrn] corpus-mcp → corpus-engine — TSV corpus-mcp→corpus-engine (32) — fp-14 (atlas carve); the non-atlas ingest-dial residue is NEEDS-OPERATOR (corpus-mcp membership)
- [svrn] corpus-mcp → sovereign-enrichment-build — TSV corpus-mcp→sovereign-enrichment-build (4) — NEEDS-OPERATOR (corpus-mcp membership)
- [svrn] sovereign-cli → commonwealth-work — TSV sovereign-cli→commonwealth-work (18) — NEEDS-OPERATOR (§12 wire-boundary recommendation untaken)
- [svrn] sovereign-cli → corpus-engine — TSV sovereign-cli→corpus-engine (3) — NEEDS-OPERATOR (D6 keep; the daemon-route closing condition is untaken)
- [svrn] sovereign-cli → corpus-engine-notes — TSV sovereign-cli→corpus-engine-notes (4) — NEEDS-OPERATOR (the notes factory)
- [svrn] sovereign-cli-daemon → sovereign-inference — TSV sovereign-cli-daemon→sovereign-inference (28) — fp-25
- [svrn] sovereign-cli-daemon → sovereign-mesh — TSV sovereign-cli-daemon→sovereign-mesh (2) — fp-24
- [svrn] sovereign-cli-daemon → corpus-engine-watchers — TSV sovereign-cli-daemon→corpus-engine-watchers (8) — fp-23
- [svrn] sovereign-cli-llm → sovereign-enrichment-catalog — TSV sovereign-cli-llm→sovereign-enrichment-catalog (3) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-enrichment-build — TSV sovereign-cli-llm→sovereign-enrichment-build (25) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-inference — TSV sovereign-cli-llm→sovereign-inference (31) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-gliner — TSV sovereign-cli-llm→sovereign-gliner (12) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-mesh — TSV sovereign-cli-llm→sovereign-mesh (12) — NEEDS-OPERATOR (node identity)
- [svrn] sovereign-cli-llm → sovereign-pods — TSV sovereign-cli-llm→sovereign-pods (19) — NEEDS-OPERATOR (provisioning)
- [svrn] sovereign-cli-llm → sovereign-runtime-recipe — TSV sovereign-cli-llm→sovereign-runtime-recipe (6) — fp-4
- [svrn] sovereign-cli-llm → sovereign-pipeline — TSV sovereign-cli-llm→sovereign-pipeline (7) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-authoring-harness — TSV sovereign-cli-llm→sovereign-authoring-harness (3) — NEEDS-OPERATOR (drive home)
- [svrn] sovereign-cli-llm → sovereign-eval — TSV sovereign-cli-llm→sovereign-eval (58) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-llm → sovereign-cli-mesh — TSV sovereign-cli-llm→sovereign-cli-mesh (1) — fp-26
- [svrn] sovereign-cli-llm → commonwealth-state — TSV sovereign-cli-llm→commonwealth-state (2) — NEEDS-OPERATOR (the durable store owner, D4)
- [svrn] sovereign-cli-llm → corpus-engine — TSV sovereign-cli-llm→corpus-engine (355) — fp-14 (atlas) + REVIEW-mint-fp-cli-llm-split (bench/enrich halves)
- [svrn] sovereign-cli-llm → corpus-engine-notes — TSV sovereign-cli-llm→corpus-engine-notes (4) — NEEDS-OPERATOR (the notes factory)
- [svrn] sovereign-cli-llm → sovereign-enrichment-build (dev) — TSV sovereign-cli-llm→sovereign-enrichment-build (25; the dev-dependency half of that row) — REVIEW-mint-fp-cli-llm-split
- [svrn] sovereign-cli-shared → corpus-engine — TSV sovereign-cli-shared→corpus-engine (6) — fp-5
- [svrn] sovereign-cli-shared → commonwealth-rail — TSV sovereign-cli-shared→commonwealth-rail (3) — fp-5
- [svrn] sovereign-core → corpus-engine — TSV sovereign-core→corpus-engine (65) — fp-14 (39 atlas) + REVIEW-mint-fp-atlas-residue (residue)
- [svrn] sovereign-core → corpus-engine-notes — TSV sovereign-core→corpus-engine-notes (11) — fp-27
- [svrn] sovereign-daemon → commonwealth-core — TSV sovereign-daemon→commonwealth-core (422) — fp-6 (roster) + REVIEW-mint-fp-core-dial (residue, D2)
- [svrn] sovereign-daemon → commonwealth-discovery — TSV sovereign-daemon→commonwealth-discovery (14) — fp-9
- [svrn] sovereign-daemon → commonwealth-media — TSV sovereign-daemon→commonwealth-media (33) — fp-20
- [svrn] sovereign-daemon → commonwealth-rail — TSV sovereign-daemon→commonwealth-rail (51) — fp-21
- [svrn] sovereign-daemon → commonwealth-state — TSV sovereign-daemon→commonwealth-state (89) — NEEDS-OPERATOR (the durable store owner, D4)
- [svrn] sovereign-daemon → commonwealth-transport — TSV sovereign-daemon→commonwealth-transport (116) — NEEDS-OPERATOR (header item, open leaf question)
- [svrn] sovereign-daemon → commonwealth-work — TSV sovereign-daemon→commonwealth-work (46) — fp-22 (D2)
- [svrn] sovereign-daemon → sovereign-mesh — TSV sovereign-daemon→sovereign-mesh (250) — fp-6+fp-7+fp-8 (verbs) + REVIEW-mint-fp-mesh-dial (residue)
- [svrn] sovereign-daemon → sovereign-code — TSV sovereign-daemon→sovereign-code (34) — fp-11
- [svrn] sovereign-daemon → sovereign-meshapp-registry — TSV sovereign-daemon→sovereign-meshapp-registry (40) — fp-12+fp-13
- [svrn] sovereign-daemon → sovereign-serving-host — TSV sovereign-daemon→sovereign-serving-host (51) — fp-16
- [svrn] sovereign-daemon → sovereign-scheduler — TSV sovereign-daemon→sovereign-scheduler (9) — fp-1
- [svrn] sovereign-daemon → sovereign-work-atlas — TSV sovereign-daemon→sovereign-work-atlas (27) — NEEDS-OPERATOR (header item)
- [svrn] sovereign-daemon → sovereign-inference — TSV sovereign-daemon→sovereign-inference (42) — fp-10
- [svrn] sovereign-daemon → sovereign-gliner — TSV sovereign-daemon→sovereign-gliner (11) — fp-12+fp-13
- [svrn] sovereign-daemon → sovereign-meshapp — TSV sovereign-daemon→sovereign-meshapp (35) — fp-12+fp-13
- [svrn] sovereign-daemon → sovereign-authoring-harness — TSV sovereign-daemon→sovereign-authoring-harness (3) — fp-18 (conditional on the drive-home answer)
- [svrn] sovereign-daemon → serving-policy — TSV sovereign-daemon→serving-policy (5) — fp-17
- [svrn] sovereign-daemon → sovereign-grants — TSV sovereign-daemon→sovereign-grants (80) — fp-12
- [svrn] sovereign-daemon → code-next-edit — TSV sovereign-daemon→code-next-edit (23) — fp-12
- [svrn] sovereign-daemon → corpus-engine — TSV sovereign-daemon→corpus-engine (179) — fp-7 (ingest dialect) + fp-14 (atlas)
- [svrn] sovereign-daemon → corpus-engine-notes — TSV sovereign-daemon→corpus-engine-notes (21) — fp-3+fp-13
- [svrn] sovereign-daemon → sovereign-tdd — TSV sovereign-daemon→sovereign-tdd (4) — fp-12
- [svrn] sovereign-daemon → sovereign-compute — TSV sovereign-daemon→sovereign-compute (25) — fp-10
- [svrn] sovereign-daemon → corpus-engine-watchers — TSV sovereign-daemon→corpus-engine-watchers (41) — fp-2+fp-13
- [svrn] sovereign-daemon → sovereign-pods — TSV sovereign-daemon→sovereign-pods (10) — fp-12
- [svrn] sovereign-daemon → sovereign-enrichment-build — TSV sovereign-daemon→sovereign-enrichment-build (2) — fp-19
- [svrn] sovereign-daemon → sovereign-runtime-recipe — TSV sovereign-daemon→sovereign-runtime-recipe (10) — fp-12+fp-13
- [svrn] sovereign-tools → sovereign-enrichment-catalog — TSV sovereign-tools→sovereign-enrichment-catalog (6) — fp-28
- [svrn] sovereign-tools → sovereign-recipe-author — TSV sovereign-tools→sovereign-recipe-author (2) — fp-29
- [svrn] sovereign-tools → corpus-engine — TSV sovereign-tools→corpus-engine (286) — fp-14 (172 atlas) + REVIEW-mint-fp-atlas-residue (residue)
- [svrn] sovereign-tools → corpus-engine-notes — TSV sovereign-tools→corpus-engine-notes (9) — fp-5 (D5 names the response_mine home)
- [svrn] sovereign-core: include_str embed (router_calibration.rs) — no TSV row — NEEDS-OPERATOR (the §12 singleton)

### [cmnwlth] — 5 dep edges

- [cmnwlth] sovereign-cli-mesh → sovereign-daemon — TSV sovereign-cli-mesh→sovereign-daemon (12) — fp-30
- [cmnwlth] sovereign-cli-mesh → sovereign-cli-shared — TSV sovereign-cli-mesh→sovereign-cli-shared (86) — fp-5
- [cmnwlth] sovereign-grants → corpus-engine — TSV sovereign-grants→corpus-engine (22) — NEEDS-OPERATOR (dial or drop)
- [cmnwlth] sovereign-mesh → corpus-engine — TSV sovereign-mesh→corpus-engine (6) — fp-7+fp-14
- [cmnwlth] sovereign-meshapp → corpus-engine — TSV sovereign-meshapp→corpus-engine (11) — fp-14

### [code] — 10 dep edges

- [code] sovereign-code → corpus-engine (dev) — TSV sovereign-code→corpus-engine (3) — NEEDS-OPERATOR (e2e fixture or move)
- [code] sovereign-cli-dev → sovereign-cli-shared — TSV sovereign-cli-dev→sovereign-cli-shared (135) — fp-5
- [code] sovereign-cli-dev → sovereign-core — TSV sovereign-cli-dev→sovereign-core (22) — fp-31
- [code] sovereign-cli-dev → sovereign-store — TSV sovereign-cli-dev→sovereign-store (2) — fp-32
- [code] sovereign-cli-dev → sovereign-tools — TSV sovereign-cli-dev→sovereign-tools (13) — fp-5
- [code] sovereign-cli-dev → sovereign-enrichment-build — TSV sovereign-cli-dev→sovereign-enrichment-build (2) — NEEDS-OPERATOR (scoring types)
- [code] sovereign-cli-dev → sovereign-mesh — TSV sovereign-cli-dev→sovereign-mesh (9) — fp-33
- [code] sovereign-cli-dev → sovereign-daemon — TSV sovereign-cli-dev→sovereign-daemon (3) — fp-11
- [code] sovereign-cli-dev → sovereign-work-atlas — TSV sovereign-cli-dev→sovereign-work-atlas (26) — NEEDS-OPERATOR (header item)
- [code] sovereign-cli-dev → corpus-engine — TSV sovereign-cli-dev→corpus-engine (16) — fp-14 (atlas) + fp-34 (build dial)

### [ingest] — 3 dep edges + 1 structural

- [ingest] sovereign-runtime-recipe → sovereign-core — TSV sovereign-runtime-recipe→sovereign-core (11) — fp-4
- [ingest] sovereign-runtime-recipe → sovereign-tools — TSV sovereign-runtime-recipe→sovereign-tools (18) — fp-4
- [ingest] sovereign-runtime-recipe → sovereign-inference — TSV sovereign-runtime-recipe→sovereign-inference (6) — fp-4
- [ingest] corpus-engine: build.rs — no TSV row — NEEDS-OPERATOR (compile-time vendoring of sovereign-recipes vs the package no-build.rs rule)
