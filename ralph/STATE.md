# domains — the ralph queue

Protocol: `ralph/PROMPT.md`. Campaign: `quality/campaigns/domains.toml` (bars,
THE STRATEGY) and `.sovereign/features/domains/campaign.md` (decisions).

Status: `[x]` done · `[~]` in progress · `[ ]` pending. Work the first `[ ]`
row whose dependencies are all `[x]`. `HUMAN-` rows are marked by the operator
only. `REVIEW-mint-` rows add rows beneath themselves as the tree reaches them.

Run it serially — the rows share crates and manifests, and cargo work is one
worker at a time (AGENTS.md):

```
nohup python3 scripts/ralph.py supervise --workdir . --label domains \
  -- python3 scripts/ralph.py run --workdir . --label domains \
  --prompt ralph/PROMPT.md --state ralph/STATE.md --max-stall 3 --notify \
  >> ralph/log.txt 2>&1 &
```

Reviews are rows here: any unit id containing `REVIEW` routes to the review
model. Models come from `ralph/models.env` (per-host, gitignored):
`python3 scripts/ralph.py models --model <W> --review-model <R> [--variant
high] --label domains` writes it and restarts the loaded job. The supervisor's
resolver uses the review model; flags still override the file.

For a detached Mac job, add `--install-launchd` to `supervise` and to `watch`,
then run the printed `launchctl bootstrap` command. The job is one-shot: an
operator stop (an EMPTY `ralph/STOP`) requires an explicit restart. Every
terminal state is DONE, an operator stop, or an escalation; fixable blockers
receive bounded resolutions, while ready `HUMAN-` rows remain operator-only.
`python3 scripts/ralph.py watch --workdir . --label domains --install-launchd`
adds the watchdog: needs-human, stopped-without-DONE, a stale heartbeat and low
disk, re-nagging every 30 min.

On the Fedora peer: `toolbox enter sovereign-vulkan` first and launch from
inside it (opencode must be on its PATH); drop `--notify`, which is macOS-only.
The O3/O8/O9/O10 pointers are gitignored per-host files under
`.sovereign/features/` — copy those four directories and `domains/` to the
peer's checkout before launching, or the rows point at nothing.

Pointer keys:
DC = `quality/DAEMON_CORE.md` ·
SB = `sovereign/SERVING_BOUNDARY.md` ·
DM = `quality/DOMAINS.md` ·
DT = `quality/DOMAINS.toml` ·
DE = `corpus-engine/DECOMPOSITION.md`, section "Step 7, redrawn" ·
O3 = `.sovereign/features/domains-3-instrument/order.md` ·
O8 = `.sovereign/features/domains-8-understanding-readmodel/order.md` ·
O9 = `.sovereign/features/domains-9-serving-package-red/order.md` ·
O10 = `.sovereign/features/domains-10-serving-extract/order.md`

## Rung 3 — the instrument (python only, no cargo)

- [x] dm3-route-census-hosts 869469ff5 — depends [] — EDIT scripts/daemon-route-census.py: its HOSTS entry for the old commonwealth-api path becomes sovereign/crates/sovereign-api/src, and a HOSTS directory that does not exist is an error, never a zero — read: O3 step 1 — check: run `python3 scripts/daemon-route-census.py` before and after the edit; paste both unique-path counts
- [x] dm3-census-skeleton 5b19eac6e — depends [] — CREATE scripts/domains-census.py with only: the registry load (tomllib over DT), a measurement `--json` last line `{"value": N, "commit": "<sha>"}` via json.dumps for co-lineage.py's _parse_value (no trailing verdict in measurement mode), scripts/lib/judgement.py's emitter for judging runs (`--self-test` and later `predicate`), exit codes 0/3/4, a `--self-test` runner that plants fixtures in a temp dir and reports caught/refused per axis (no axes yet), and the comment stripping copied from scripts/nc-extends.py — read: O3 step 2 and "Seams"; scripts/co-lineage.py _parse_value; scripts/nc-thesis.py main's --json branch — check: CENSUS
- [x] dm3-peer-outside 8e2a7640a — depends [dm3-census-skeleton] — ADD the `peer-outside` subcommand with its planted positive and negative controls in --self-test — read: O3 step 3; O3 "Adjudicated 2026-09-14"; DT [[noun]] rows — check: CENSUS; paste `python3 scripts/domains-census.py peer-outside`
- [x] dm3-shared-edges 44d598541 — depends [dm3-peer-outside] — ADD `shared-edges` with controls — read: O3 step 4; DM §10.3; DT [[edge]] rows — check: CENSUS; paste its output
- [x] dm3-word-owners 479b62fa3 — depends [dm3-shared-edges] — ADD `word-owners` with controls; `owns` matches anywhere in a type name, `owns_exact` the bare name; crates tagged kernel or back-of-house print as exempt — read: O3 step 5; O3 "Adjudicated 2026-09-14"; DM §10.1 — check: CENSUS; paste its output
- [x] dm3-atom-outside 73bd9a930 — depends [dm3-word-owners] — ADD `atom-outside` with controls; today's value is 12 with a three-binder allow-list — read: O3 step 6; DM §10.5 — check: CENSUS; paste its output
- [x] dm3-tag-workspace 1e94936ef — depends [dm3-census-skeleton] — EDIT DT: for every workspace member crate that has no [[module]] rows, append one row per `.rs` file under its `src/` (path, lines from `wc -l`, note ""), with context = the one [[context]] whose `crates` list names that crate, or "unknown" when none or several do — read: O3 "Re-sequenced 2026-09-14" bullet `queue`; the existing [[module]] rows for their shape — check: TOML; paste the count of rows added and of `unknown` rows
- [x] dm3-crate-lines-misnamed 702ab4169 — depends [dm3-atom-outside, dm3-tag-workspace] — ADD `crate-lines --crate X` and `misnamed`, with the coverage assertion (exit 4 naming an untagged `.rs`) — read: O3 step 8; O3 "Demo" D5 — check: CENSUS; paste `misnamed`
- [x] dm3-liftable 71892c541 — depends [dm3-crate-lines-misnamed] — ADD `liftable` with `--no-lift` — read: O3 step 7; O3 "Demo" D4; the [[package]] rows in quality/ARCH_LAYERS.toml — check: CENSUS; paste `liftable --no-lift`
- [x] dm3-queue ee056e0 — depends [dm3-liftable] — ADD `queue` — read: O3 "Re-sequenced 2026-09-14" bullet `queue`; THE STRATEGY block in quality/campaigns/domains.toml — check: CENSUS; paste its output
- [x] REVIEW-build-dm3-plan 6704e1deb — depends [dm3-queue] — ADD `plan [--crate X]` — read: O3 "Re-sequenced 2026-09-14" bullet `plan`; THE STRATEGY block; DT [[cluster]] rows and the banner above them (they predate the 2026-09-14 retags, so a disagreement with them is a finding for the commit body, not an error) — check: CENSUS; paste `plan --crate sovereign-mesh`
- [x] dm3-congestion f31ecdc69 — depends [dm3-queue] — ADD `congestion`, and register bar `dm-context-congestion` in quality/campaigns/domains.toml with floor = the printed value and target = floor — read: O3 step 9 — check: CENSUS; TOML
- [x] dm3-predicate-lineage 83967dafd — depends [REVIEW-build-dm3-plan, dm3-congestion] — ADD `predicate` (it must exit non-zero on today's tree and name why); run `python3 scripts/co-lineage.py measure domains`; add the quality/instruments.toml rows O3 "Scope" names — read: O3 step 11, "Lane", "Gates" — check: CENSUS; `python3 scripts/co-lineage.py --self-test`; INSTR; paste the measured rows and the predicate's failure text
- [x] REVIEW-audit-1 0d7261da2 — depends [dm3-route-census-hosts, dm3-predicate-lineage] — AUDIT rung 3: every axis has a planted positive it catches and a planted negative it refuses (ARCH 5); O3 "Seams" — the six axis-subject literals are a recorded adjudicated deviation (REVIEW_FINDINGS.md) — check: CENSUS; PREPUSH
- [x] REVIEW-build-census-constants c8a40af72 — depends [REVIEW-audit-1] — FIX the six axis-subject literals REVIEW_FINDINGS.md recorded, each fact single-spelled (ARCH 8): `_EXEMPT_CONTEXTS` and `_ATOM_EXCLUDED_ROOTS` become registry data in quality/DOMAINS.toml (the exemption rule is already stated at :19-22 and both context ids exist at :202/:226), `_COMMONWEALTH_PREFIX` derives from ARCH_LAYERS' `[[package]] name = "commonwealth"` (:905), and `_PEER_WORD`/`_ATOM_WORD`/`_MEMBER_EDGE` read the fact the registry already carries (Understanding's `owns` carries `Atom`, :41; `MemberRecord` is at :137 and :1388) — every axis value on today's tree unchanged — read: REVIEW_FINDINGS.md "Recorded, not changed" bullet 1; scripts/domains-census.py:171,174,325,490,702,703; quality/DOMAINS.toml:19-22,:41,:137,:202,:226,:1388; quality/ARCH_LAYERS.toml:905 — check: CENSUS; paste `--self-test` and the unchanged `peer-outside`, `word-owners`, `atom-outside` values
- [x] REVIEW-build-census-checks afec18805 — depends [REVIEW-build-census-constants] — FIX the two checks with no failing input REVIEW_FINDINGS.md recorded (ARCH 5): `plan`'s `share_monotone` (scripts/domains-census.py:2351) asserts the premise that makes it true — the leaving cluster's context is not the source crate's own context, so a plan proposing to move own-context lines fails loudly — and keeps the share as telemetry; `liftable`'s `_run_lift` run tier (:1549) gains a planted positive and a planted negative control (a stub lift exiting 0 -> lifted, non-zero -> not lifted) so the branch is exercised — read: REVIEW_FINDINGS.md "Recorded, not changed" bullets 2-3; scripts/domains-census.py:1549,2351 — check: CENSUS; paste the two new controls and the self-test count

## The gate — nothing below moves code until the operator marks this

- [x] HUMAN-design-review — approved 2026-09-15 — depends [] — the operator reviews and approves: DC §3.2's correction, §3.3's resolver and §4 (host crate, AppState, adapters); SB "Corrected 2026-09-14"; DM §11; DE; the proposed crate names (sovereign-daemon, sovereign-pods, sovereign-slots for sovereign-compute, understanding-vocab/-atlas/-host); whether declaring the `serving` package red (O9) is acceptable while pre-push runs boundary-gate; whether the unwired corpus ceiling (DC §3.3) is fixed before any move that touches the node

## Wave 0 — shared fixes the moves need

- [x] REVIEW-build-corpus-ceiling 0b20e3e9a — depends [HUMAN-design-review] — WIRE the per-turn Scope on the daemon and the desktop so the corpus ceiling is resolved, not absent: DC §3.3 measured `corpus_principal` set only at sovereign/crates/sovereign-server/src/main.rs:638 and `sensitive_corpora` nowhere, while TOPOLOGY §3.5 names `sensitive_corpora: None` = "all corpora eligible" as the inverted invariant; the recipe leaves both at named absence (sovereign/crates/sovereign-runtime-recipe/src/lib.rs:42-46) and the daemon commissions through sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs:988-1091; implementations exist (`LocalCorpusManager` sovereign-tools/src/local_corpus/manager.rs:2414; `NoSensitiveCorpora` sovereign-contracts/src/traits.rs:90; `TenantPrincipalResolver` sovereign-server/src/tenant.rs:102); the failing test is written first and watched red — with the ceiling unresolved the Scope must not read as all-corpora-eligible; absence is the refusing value for every caller class the tree actually serves (DC §3.3 measured the non-owner callers as unmeasured — enumerate them in the commit body) — read: DC §3.3 "Measured 2026-09-14: neither" and "The resolver, decided"; TOPOLOGY §3.5's Scope block and its two corrections; the sites above — check: LINT; TEST of each crate touched; paste the red test then the green
- [x] dm-time-reexport ae1374852 — depends [HUMAN-design-review] — EDIT sovereign/crates/sovereign-core/src/time.rs: confirm each `pub fn` has an identical body in sovereign/crates/sovereign-time/src/lib.rs, then replace the file's contents with `pub use sovereign_time::{<the same names>};` (add sovereign-time to sovereign-core's Cargo.toml if absent); any body that differs is §6 — read: ARCH principle 8 — check: LINT; TEST(sovereign-core); TEST(sovereign-time)
- [x] dm-time-repoint-api e5b6264d5 — depends [dm-time-reexport] — EDIT sovereign/crates/sovereign-api: every `sovereign_core::time::` becomes `sovereign_time::` (9 sites today; add the dependency) — check: LINT; TEST(sovereign-api)
- [x] dm-time-repoint-mesh 6f25e1a33 — depends [dm-time-reexport] — EDIT sovereign/crates/sovereign-mesh: the same rewrite (21 sites today) — check: LINT; TEST(sovereign-mesh)
- [x] dm-wire-openai-types 3b072c547 — depends [HUMAN-design-review] — MOVE sovereign/crates/sovereign-api/src/openai_types.rs -> oicp-types; the row also allows rewriting its `sovereign_serving::oicp::` imports to `crate::` paths inside oicp-types (InferenceRequirements lives in oicp-types' requirements module, SamplingMode in its completion module) — read: SB "Corrected 2026-09-14" bullet "Rule 4's count" — check: LINT; LAYER; TEST(oicp-types); TEST(sovereign-api)
- [x] dm-wire-responses-types 6444fda4c — depends [dm-wire-openai-types] — MOVE sovereign/crates/sovereign-api/src/responses_types.rs -> oicp-types — read: the same bullet — check: LINT; LAYER; TEST(oicp-types); TEST(sovereign-api)
- [x] dm-wire-repoint-mesh 548dc89f8 — depends [dm-wire-responses-types] — EDIT sovereign/crates/sovereign-mesh: `sovereign_api::openai_types::` becomes `oicp_types::openai_types::` and `sovereign_api::responses_types::` becomes `oicp_types::responses_types::` — check: LINT; TEST(sovereign-mesh)
- [x] dm-serving-dead-types 335609897 — depends [HUMAN-design-review] — DELETE from sovereign/crates/sovereign-serving the eleven zero-reference types O9's Objective bullet 1 lists, plus `Tier` and `TierQueueDepths`; run CALLERS on each first, and any caller outside sovereign-serving's own definitions and tests is §6 — read: O9 "Adjudicated 2026-09-14" bullet 1; O9 step 1 — check: LINT; TEST(sovereign-serving); TEST(sovereign-mesh-test-harness)
- [x] dm-serving-meshplan-cascade efee46c25 — depends [dm-serving-dead-types] — DELETE what the previous row left dead: MeshPlan's orphaned fields, the four zero-caller store_adapter methods and the three zero-caller harness helpers, CALLERS on each — read: SB "What is enforced, and what is not", bullet "The `MeshPlan` cascade" — check: LINT; TEST(sovereign-serving); TEST(sovereign-mesh-test-harness)
- [x] REVIEW-build-knowledge-assignment f8a2d3ebb — depends [dm-serving-meshplan-cascade] — MOVE sovereign-serving's knowledge_assignment module out by what it assigns (shards go to `sovereign-grants`, the mesh-foundation crate that owns shard assignment and already depends on corpus-engine; corpus-engine itself cannot host it because the planner names commonwealth-core), so sovereign-serving's Cargo.toml no longer names corpus-engine — read: O9 step 2 and its "Not worth continuing if" — check: LINT; LAYER; `./scripts/with-cargo-lock.sh cargo tree -p sovereign-serving -i corpus-engine` prints nothing
- [~] REVIEW-audit-2 — depends [dm-time-repoint-api, dm-time-repoint-mesh, dm-wire-repoint-mesh, REVIEW-build-knowledge-assignment, REVIEW-build-corpus-ceiling] — AUDIT wave 0 — check: TESTALL; PREPUSH

## Wave 1 — sovereign-mesh: rented pods (Compute)

- [ ] dm-pods-crate — depends [REVIEW-audit-2] — CREATE sovereign/crates/sovereign-pods in layer `runtime`, doc "Compute's remote isolation: leasing a rented machine and running work on it" — read: DM §11.2 — check: LINT; LAYER; DOCS
- [ ] dm-pods-move-worker-pod — depends [dm-pods-crate, dm-time-repoint-mesh] — MOVE sovereign/crates/sovereign-mesh/src/worker_pod.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] dm-pods-move-worker-http — depends [dm-pods-move-worker-pod] — MOVE sovereign/crates/sovereign-mesh/src/worker_http.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] dm-pods-move-subprocess-runner — depends [dm-pods-move-worker-http] — MOVE sovereign/crates/sovereign-mesh/src/worker_subprocess_runner.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] dm-pods-move-controller — depends [dm-pods-move-worker-http] — MOVE sovereign/crates/sovereign-mesh/src/worker_controller.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] dm-pods-move-worker-daemon — depends [dm-pods-move-worker-http] — MOVE sovereign/crates/sovereign-mesh/src/worker_daemon.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] dm-pods-move-multi-pod — depends [dm-pods-move-controller] — MOVE sovereign/crates/sovereign-mesh/src/multi_pod_coordinator.rs -> sovereign-pods — check: LINT; LAYER; TEST(sovereign-pods); TEST(sovereign-mesh)
- [ ] REVIEW-audit-3 — depends [dm-pods-move-subprocess-runner, dm-pods-move-worker-daemon, dm-pods-move-multi-pod] — AUDIT the pod moves; repoint sovereign-cli-llm and sovereign-cli-daemon off the shims — check: TESTALL; PREPUSH

## Wave 1 — sovereign-mesh: serving

- [ ] dm-serving-stub-crates — depends [REVIEW-audit-2] — CREATE sovereign/crates/sovereign-scheduler in layer `contract` and sovereign/crates/sovereign-serving-host in layer `runtime`, each doc naming its role from SB "The two tiers" — read: SB "The two tiers"; O9 step 4 — check: LINT; LAYER; DOCS
- [ ] dm-serving-package-rows — depends [dm-serving-stub-crates] — EDIT quality/ARCH_LAYERS.toml: add `[[package]] name = "serving"`, its [[forbid]] rows and its two grandfathered [[exception]] rows exactly as O9 Objective bullet 2 and SB "The rules" state — read: O9 Objective bullet 2; SB "The rules" and the "Grandfathered" paragraph — check: TOML; LAYER; BOUNDARY with red EXPECTED, naming serving's fused modules — paste the red
- [ ] dm-serving-lift-skeleton — depends [dm-serving-package-rows] — CREATE scripts/serving-lift.sh from scripts/cw-work-lift.sh's skeleton: steps 1-4 real, steps 5-8 abstain with the reason "package not yet extracted" — read: O9 step 5; SB "What is enforced, and what is not" Tier 2 — check: `scripts/serving-lift.sh --sandbox; echo exit=$?` reports verdict 0 or could-not-judge naming the step — paste it
- [ ] dm-sched-move-tier — depends [dm-serving-stub-crates, dm-wire-repoint-mesh] — MOVE sovereign/crates/sovereign-mesh/src/tier.rs -> sovereign-scheduler (measured 2026-09-14: it names no `crate::` module) — read: SB "The rules" rule 1 — check: LINT; LAYER; TEST(sovereign-scheduler); TEST(sovereign-mesh)
- [ ] dm-sched-move-slot-aliases — depends [dm-sched-move-tier] — MOVE sovereign/crates/sovereign-mesh/src/slot_aliases.rs -> sovereign-scheduler (it names no `crate::` module) — check: LINT; LAYER; TEST(sovereign-scheduler); TEST(sovereign-mesh)
- [ ] REVIEW-mint-scheduler-cycle — depends [dm-sched-move-slot-aliases] — MINT the rest of the scheduler half. Measured 2026-09-14, `crate::` references: scheduler_core -> decision_log, decision_replay, mesh_sim, oicp_select, predicted_time, tier, yield_backoff; oicp_select -> peer_inference; predicted_time -> decision_log, decision_replay; decision_log -> decision_replay, decision_trace, mesh_sim, tier; decision_replay -> decision_log, decision_trace, mesh_sim, oicp_select, scheduler_core; decision_trace -> decision_log, peer_inference; throughput_tracking -> decision_log, peer_inference; yield_backoff -> decision_log. Those eight are one cycle, so mint REVIEW-build rows that cut the back-edges to peer_inference and mesh_sim and split decision_log's recording sink and pick_slot_for_oicp out to the host, then ONE move row for all eight, then sovereign-cli-daemon's repoint — read: SB "Corrected 2026-09-14" bullets 1-2; SB "The five entries" (b) and (d); O10 steps 3-5
- [ ] REVIEW-mint-serving-host — depends [REVIEW-mint-scheduler-cycle, dm-pods-move-worker-pod] — MINT the knot into sovereign-serving-host: the VenueSource and guest-lookup ports; peer_inference, inference_adapter, oicp_synthesis, guest_lender, pinned_worker_source, entry_endpoint, prompt_compactor, worker_eligibility, pinned_transport, pinned_pod_snapshot, source_content_validator, fim_adapter, and sovereign-api's admission and principal; PeerInferenceEndpoint's definition to sovereign-scheduler as Venue; LocalInferenceService collapsing onto InferenceProvider; the stub-endpoint harness for lift steps 5-8; and a final DEMO-d1-serving-lift row expecting verdict 1 — read: SB (whole); O10 steps 6-9; DC §4.2 "Identity is a reader"; DM §11.2

## Wave 1 — sovereign-mesh: small leavers

- [ ] dm-mesh-move-turn-approval — depends [REVIEW-audit-2] — MOVE sovereign/crates/sovereign-mesh/src/turn_approval.rs -> sovereign-core (measured: no `crate::` references; if it names a crate sovereign-core does not depend on, §6) — read: DC §4.3 table — check: LINT; LAYER; TEST(sovereign-core); TEST(sovereign-mesh)
- [ ] dm-mesh-move-reading-formatters — depends [REVIEW-audit-2] — MOVE sovereign/crates/sovereign-mesh/src/reading_formatters.rs -> corpus-engine-vocab; the row also allows rewriting `corpus_engine::enrichment::atlas::AtomEnvelope` to `crate::atoms::AtomEnvelope` (AtomEnvelope is defined in corpus-engine-vocab/src/atoms.rs); vocab may name only kernel-types — read: DC §4.3 table — check: LINT; LAYER; TEST(corpus-engine-vocab); TEST(sovereign-mesh)
- [ ] REVIEW-build-research-run-dir — depends [REVIEW-audit-2] — MOVE sovereign-mesh's research_run_dir into sovereign-core's deep_research; it calls `crate::research_http::is_live`, which stays with the host, so pass that in instead of importing it — read: DC §4.3 table — check: LINT; LAYER; TEST(sovereign-core); TEST(sovereign-mesh)

## Wave 1 — the host crate and the node's state

- [ ] dm-daemon-crate — depends [REVIEW-audit-2] — CREATE sovereign/crates/sovereign-daemon in layer `mesh-api`, doc listing DC §4.1's module families (assemble, edge, turn, jobs, node, resources, store, adapters) as text only; add to quality/ARCH_LAYERS.toml the two [[forbid]] rows DC §4.1 names (sovereign-mesh -> sovereign-daemon, sovereign-mesh-test-harness -> sovereign-daemon) — read: DC §4.1 — check: LINT; LAYER; TOML; DOCS
- [ ] REVIEW-mint-appstate — depends [dm-daemon-crate] — MINT the dissolution of AppState per DC §4.2: one row per owner group moving its fields out of `AppStateInner` in sovereign/crates/sovereign-api/src/state.rs behind the existing accessors, in the order Answering (3), Workbench (1), collaborative ingest (9), node (10), Serving (20), Fabric (20); then the identity reader, the SelfClaims port for `capabilities::build_local_capabilities`, the in-flight gauge created first, and each install slot replaced by construction. Every new type name is checked with `sovereign code converge noun <Name> --corpus-id commonwealth-ai` — read: DC §4.2 (whole); DC §4.1 paragraph "EmbeddedDaemon splits by owner"
- [ ] REVIEW-mint-principal — depends [REVIEW-mint-appstate] — MINT DC §3.3's resolver: the Principal type in sovereign-contracts, one resolution in the daemon's edge, admission's keys derived from it — read: DC §3.3; SB "Corrected 2026-09-14" bullet "Entry (c)"
- [ ] REVIEW-mint-daemon-move — depends [REVIEW-mint-appstate] — MINT the moves into sovereign-daemon, module by module in dependency order, of every module DT tags `host` in sovereign-mesh and sovereign-api, plus the composition half of sovereign-cli-daemon listed in DC §4.1's table — read: DC §4.1 and §4.3; DT `plan."sovereign-api"` tables; `python3 scripts/domains-census.py plan --crate sovereign-mesh`
- [ ] REVIEW-mint-mesh-rest — depends [REVIEW-mint-daemon-move, REVIEW-mint-serving-host] — MINT sovereign-mesh's remaining leavers: workbench (fim_adapter, lsp_tier, commit_harvest, projects, reindexer); back-of-house (mesh_sim, dst, scoreboard — the harness forbid's except list must gain sovereign-scheduler, so a HUMAN row goes first); the donor loop and ingest drivers to sovereign-daemon jobs; the renames each landed cluster carries (DT [[noun]] rows); a final DEMO-d5-misnamed row expecting sovereign-mesh at 100% fabric — read: DM §11.2; DC §3.2 and §4.3; DT [[noun]]; `python3 scripts/domains-census.py plan --crate sovereign-mesh`

## Wave 3 — corpus-engine: the vocab door (disjoint from wave 1)

- [ ] dm-vocab-compile-fail-test — depends [REVIEW-audit-2] — ADD the compile-fail test O8 step 2 describes, and watch it behave as O8 step 2 says it must today — read: O8 step 2 and check 10; DM §10.5 "The correction" — check: TEST(corpus-engine-vocab); paste the test's result
- [ ] dm-vocab-door-move — depends [dm-vocab-compile-fail-test] — EDIT: move `read_atlas_atoms`, `read_atlas_edges`, `read_atlas_cross_corpus_edges` and `read_atlas_ontology` from corpus-engine/src/enrichment/atlas/writer.rs into a new corpus-engine-vocab/src/read.rs, leaving `pub use corpus_engine_vocab::read::{...};` in writer.rs; premise: each body uses only std, serde_json and types defined in corpus-engine-vocab — read: O8 check 1 and step 3; DE "The door" — check: LINT; LAYER; TEST(corpus-engine-vocab); TEST(corpus-engine)
- [ ] dm-vocab-atlas-dirname — depends [dm-vocab-door-move] — EDIT: move the constant `ATLAS_DIRNAME` from writer.rs into corpus-engine-vocab beside the readers, re-exported at the old path — read: DE "The read-port leaf, measured again", its third paragraph — check: LINT; TEST(corpus-engine-vocab); TEST(corpus-engine)
- [ ] dm-vocab-bypass-corpus-mcp — depends [dm-vocab-door-move] — EDIT corpus-mcp/src/tools.rs: delete the hand-rolled `fn read_atoms` and call corpus-engine-vocab's `read_atlas_atoms` — read: O8 step 5 and check 8 — check: LINT; TEST(corpus-mcp)
- [ ] dm-vocab-bypass-tools — depends [dm-vocab-door-move] — EDIT sovereign/crates/sovereign-tools: route the atoms.json parses in src/knowledge_view/atlas_digest.rs and src/catalog_ingest.rs through the vocab readers — read: O8 step 4 — check: LINT; TEST(sovereign-tools)
- [ ] dm-vocab-bypass-core — depends [dm-vocab-door-move] — EDIT sovereign/crates/sovereign-core: the atoms.json parse in evidence_loop/anchoring.rs, through the vocab reader — read: O8 step 4 — check: LINT; TEST(sovereign-core)
- [ ] dm-vocab-bypass-cli-llm — depends [dm-vocab-door-move] — EDIT sovereign/crates/sovereign-cli-llm: the atoms.json parses in corpus_scrub_cmd.rs, bench_cmd/scaffold.rs and enrich_cmd/diagnose.rs, through the vocab reader — read: O8 step 4 — check: LINT; TEST(sovereign-cli-llm)
- [ ] dm-vocab-bypass-rest — depends [dm-vocab-door-move] — EDIT the remaining untyped walks of atoms.json, found with `git grep -n 'atoms.json' -- '*.rs'` in build/steps.rs and flywheel/mining.rs, through the vocab reader — read: O8 step 4 — check: LINT; TEST of each crate touched
- [ ] REVIEW-build-vocab-seal — depends [dm-vocab-bypass-corpus-mcp, dm-vocab-bypass-tools, dm-vocab-bypass-core, dm-vocab-bypass-cli-llm, dm-vocab-bypass-rest, dm-vocab-atlas-dirname, dm3-atom-outside] — SEAL the door: a private deserialize-only wire twin, `AtomsFile` no longer Deserialize, vocab's readers the only constructors; the compile-fail test flips — read: DM §10.5 "The correction"; O8 step 8; the precedent in corpus-engine/src/index/evidence.rs — check: LINT; TEST(corpus-engine-vocab); CENSUS; paste `python3 scripts/domains-census.py atom-outside`
- [ ] REVIEW-audit-4 — depends [REVIEW-build-vocab-seal] — AUDIT the door — check: TESTALL; PREPUSH
- [ ] REVIEW-mint-wave-3 — depends [REVIEW-audit-4] — MINT the Understanding carve per DE: FieldSkeleton and the articulation types into vocab, the stream_axes split, the index read-port leaf, the enrichment-pass port moving to the engine, the completion closure ports converging, the pure and host tiers as crates, corpus-mcp's exception row — read: DE (whole section); O8

## Wave 2 — sovereign-api dissolves

- [ ] REVIEW-mint-wave-2 — depends [REVIEW-mint-daemon-move] — MINT what is left of sovereign-api after wave 1 took its host modules and route shells: workspace's decision_extractor, answering's ATOS inversion, workbench's next-edit crate, auto_recover into sovereign-grants, and its three [[exception]] rows to zero — read: DT `plan."sovereign-api"`; DC §4.1 and §4.3; `python3 scripts/domains-census.py plan --crate sovereign-api`

## Behaviour rungs and the loop's tail

- [ ] HUMAN-behaviour-rungs — depends [REVIEW-mint-serving-host] — the operator approves which behaviour-changing rungs run, and when: GateReason and its privacy/latency split (SB "What is enforced, and what is not", Tier 3); the trust-level wire enum in oicp-types (SB "Corrected 2026-09-14", last bullet); pods onto commonwealth-work units (DM §11.2); the legacy ingest lease onto ingest:v1 (DC §3.2); the /status slot DTO convergence (SB "Corrected 2026-09-14", LocalInferenceService bullet); the [compute] config table split (DM §11.2); the corpus ceiling (DC §3.3)
- [ ] REVIEW-mint-behaviour — depends [HUMAN-behaviour-rungs] — MINT rows only for the rungs the operator approved in their acknowledgment of HUMAN-behaviour-rungs, each with its failing test written first — read: that acknowledgment
- [ ] REVIEW-mint-wave-n — depends [REVIEW-mint-wave-2, REVIEW-mint-wave-3, REVIEW-mint-mesh-rest] — MINT the next wave from the head of `python3 scripts/domains-census.py queue`; when the queue reads 0 and `predicate` exits 0, add a last row that writes ralph/DONE — read: THE STRATEGY block in quality/campaigns/domains.toml
