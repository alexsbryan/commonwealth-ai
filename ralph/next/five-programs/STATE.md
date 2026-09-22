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
the honest state until it exists).

Pointer keys:
**TSV** = docs/FIVE_PROGRAMS_DECISIONS.tsv · **FP** = docs/FIVE_PROGRAMS.md ·
**AL** = quality/ARCH_LAYERS.toml (leaf rows ~940-1010; packages: svrn ~1090,
ingest ~1150, cmnwlth ~1190, code ~1240, bench ~1270) ·
**LEAF** = corpus-engine-atlas-reader/ · **CE** = corpus-engine/src/ ·
**DA** = sovereign/crates/sovereign-daemon/src/ ·
**CW** = commonwealth/crates/ · **SC** = sovereign/crates/sovereign-contracts/src/

- [ ] fp-0 — depends [] — RE-MEASURE the burn-down: run the gate, paste the full per-edge list into the appendix below, and against each edge write its TSV row id (source→target pair) and which queue row owns it; any edge no queue row owns gets a new row here (decided TSV rows only — a TSV row with a non-empty decision_needed cell becomes a NEEDS-OPERATOR line instead). — read: PROMPT standing facts, TSV, FP §12 — check: the appendix lists every one of the gate's violations exactly once
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

## Appendix — the 79 at mint (09ff299b8)

Replaced by fp-0's re-measure. The mint-time clusters, from the last full
gate run: [svrn] corpus-mcp→{corpus-engine, sovereign-enrichment-build};
svrn cli-daemon→sovereign-mesh; [code] cli-dev→{cli-shared, core, store,
tools, enrichment-build, mesh, daemon, work-atlas, corpus-engine};
[svrn] cli-llm→{enrichment-catalog, enrichment-build, inference, gliner,
mesh, pods, runtime-recipe, pipeline, authoring-harness, eval, cli-mesh,
commonwealth-state, corpus-engine, corpus-engine-notes(+dev)};
[cmnwlth] cli-mesh→daemon; [svrn] core→{corpus-engine, corpus-engine-notes};
[svrn] daemon→{commonwealth-core, discovery, media, rail, state, transport,
work, mesh, code, meshapp-registry, serving-host, scheduler, work-atlas,
inference, gliner, meshapp, authoring-harness, serving-policy, grants,
code-next-edit, corpus-engine, corpus-engine-notes, tdd, compute,
corpus-engine-watchers, pods, enrichment-build, runtime-recipe};
[cmnwlth] mesh→corpus-engine; [cmnwlth] meshapp→corpus-engine.
