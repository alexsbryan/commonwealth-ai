---
schema: work-order/v1
id: handed-2-assemble
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — assemble: a Runtime exists only through a named profile
engine: ralph pool; hd- rows default worker, REVIEW-build rows the stronger model
budget: 5 rows; TEST on the two behaviour rows; covered by REVIEW-audit-hd-1
---

# Order: handed-2-assemble — commission takes a Launch; nothing else can build a Runtime

## Objective

Today any crate can write `sovereign_core::RuntimeParts { .. }` and call `Runtime::new` or
`sovereign_runtime_recipe::commission(parts)` with parts it composed itself. That is how three
hosts once drifted, and how a bench could measure a configuration no user runs. After this order:
a `RuntimeParts` needs a `CommissionSeal`, which only `sovereign-core`, `sovereign-runtime-recipe`
and test code can name (layer-gate); `commission(&Launch, RecipeInputs, progress)` is the only
production door and it names the profile it built for; no host names `RuntimeParts` again.

Cut at round 1 (2026-09-17): the refusal branch (an abstention with no run that demanded it —
principle 5; the desktop already cannot link the recipe) and sovereign-server's fate, which steps
4 and 6 convert in place and which is a separate decision.

## Premises (verified 2026-09-17, file:line)

- `pub fn commission(parts: RuntimeParts) -> Arc<Runtime>` — sovereign/crates/sovereign-runtime-recipe/src/lib.rs:385-387; `pub async fn common_parts(inputs: RecipeInputs, ..) -> CommonParts` :395-455 (builds parts at :433-447); `pub struct RecipeInputs` :264-341 (doc from :259; `ToolSwitches` :343-356 stays with it); `pub struct CommonParts { pub parts: RuntimeParts, atlas_context, mcp }` :359-376; the recipe is 1,177 lines.
- `pub struct RuntimeParts` (19 fields, 9 `Option`) — sovereign/crates/sovereign-core/src/runtime.rs:444-474; `RuntimeParts::new` (9 args) :476-511; `pub fn new(parts: RuntimeParts) -> Self` :616; re-export sovereign-core/src/lib.rs:150.
- Host commissions (each a struct-update on `common.parts`): sovereign-cli-daemon/src/daemon_cmd/mod.rs:989 (`common_parts`), :1123-1127 (`sensitive_corpora`, `corpus_principal`); sovereign-cli-llm/src/chat_cmd/bootstrap.rs:296, :389-393 (`mesh_knowledge`, `routing_events`), :402 reads `common.atlas_context`; sovereign-server/src/main.rs:563, :621-622 (reads `common.parts.tools`, `common.mcp`), :631-643 (`routing_events`, `corpus_principal`, `landscape_digests`).
- `RuntimeParts::new(` call sites (21 in 12 files): recipe lib.rs:436; sovereign-core src/runtime/handlers/metalingual.rs:852, src/runtime/retrieval/history.rs:923, src/runtime/retrieval_pipeline/atlas_step_reachability_tests.rs:144, tests/main/core_tests.rs (817, 1357, 1898, 2064, 2342, 2397, 2534), tests/main/harness.rs (278, 359), tests/main/landscape_digest_splice.rs:106, tests/main/routing_moves.rs (129, 787); sovereign-daemon/src/daemon_services.rs:718 (inside `#[cfg(test)] mod fixtures` from :654); sovereign-mesh/tests/main/common/mod.rs (669, 773); sovereign-server/src/http_tests.rs:116 (`#[cfg(test)]` at main.rs:25); sovereign-tools/tests/main/smoke_tests.rs:157.
- Production `Runtime::new(`: only recipe lib.rs:386 (`git grep -n 'Runtime::new('` minus tokio/`TenantRuntime`/tests).
- `pub enum Launch` — sovereign/crates/sovereign-contracts/src/launch.rs:42 (`Daemon` :46, `Worker` :55, `ComputeChild` :64, `RpcWorker` :79, `Smoketest` :86, `Desktop` :93, `Server` :113, `Verb` :116, `Bare` :126); `parse(args, default_ui)` :175; `ONE_SHOT_VERBS` :132 does not contain `chat`, so a cli-llm verb supplies `Launch::Verb{..}` as the default, the precedent being sovereign-cli-llm/src/mesh_cmd.rs:37-46.
- `pub enum LaunchParts { Admin, Serving{serving, headless} }` — sovereign/crates/sovereign-daemon/src/daemon_services.rs:493; `pub fn assemble(&Launch, LaunchParts) -> Result<DaemonServices, AssemblyRefusal>` :571; `ServingCore.runtime: Arc<Runtime>` :202; `pub enum AssemblyRefusal { NotAnAssembler, Mismatch }` + Display + Error :516-553; re-exported sovereign-daemon/src/lib.rs:125-129. The daemon has `launch: &Launch` in scope (`run_daemon`, daemon_cmd/mod.rs:166; used at :1158). sovereign-server and the recipe already depend on sovereign-contracts (server Cargo.toml:67, recipe Cargo.toml:41).
- Layer map: sovereign-core in `runtime` (quality/ARCH_LAYERS.toml:217), recipe in `capabilities` (:277), sovereign-server in `hosts` (:317); dev-deps exempt (:6-7, arch-layers lib.rs:331); `pub struct Forbid { from, to, except, reason }` quality/arch-layers/src/lib.rs:135-142; `forbidden_by` :270-276 excepts only the `to` side; existing test `forbid_rule_fires_across_families_and_respects_except` :507; arch-layers lib.rs is 765 lines.
- Census that must stay green: sovereign-core/tests/main/runtime_commission_census.rs — `recipe_callers` matches the literal `sovereign_runtime_recipe::commission(` (:402-407), `CANONICAL_CONSTRUCTOR` = recipe lib.rs (:54), `COMMISSIONING_PROCESSES` lists the three hosts (:126-142). Desktop census sovereign-desktop/src-tauri/tests/attach_construction_census.rs:213,218 requires count 0 of `common_parts(` and `commission(` in the desktop — unaffected.
- Size gates: arch-gate `GROWTH_SLACK = 50` (corpus-engine/xtask/src/arch_gate.rs:38), approach band 800-1200 with no slack (:34, :300-312). Baselines: daemon_cmd/mod.rs 1,589 (tree 1,630), core_tests.rs 2,541 (tree 2,580) — quality/baselines/oversized.txt:56, :129.
- The Runtime's turn tool registry is already built inside the recipe (`build_tools`, lib.rs:417, :534); daemon_cmd/tool_registry.rs's 41 `.register(` calls build the `/mcp` registry (bootstrap.rs:1903), not a Runtime part.

## Steps

1. **Mint the seal**, one commit (nothing depends on the crate yet). New zero-dep crate
   `sovereign-commission-seal` (layer `runtime`): `pub struct CommissionSeal(());` with
   `pub fn mint() -> Self`; no `Clone`/`Copy`/`Default`. The same row registers it as a workspace
   member in the root `Cargo.toml` `[workspace] members` (members are enumerated by name, not
   globbed) and in `[workspace.dependencies]` (Cargo.toml:283), and assigns it a layer in
   the `runtime` layer in `quality/ARCH_LAYERS.toml` (:215): `evaluate` requires the COMPLETE
   workspace-member name set (quality/arch-layers/src/lib.rs:280-283) and pushes
   `Violation::UnassignedCrate` for a member no layer matches (:297), so a member with no layer
   fails the gate. Row `hd-2-seal-crate`. No PLANT on this row.
2. **The from-side forbid.** `[[forbid]]` gains `except_from` — a from-side exemption with the same
   wildcard semantics as `except`: `pub struct Forbid { from, to, except, reason }`
   quality/arch-layers/src/lib.rs:135-142, and `forbidden_by` :270-276 filters on the `to` side
   only today. A unit test lands beside `forbid_rule_fires_across_families_and_respects_except`
   (:507). Then the rule row in quality/ARCH_LAYERS.toml: `from = "*"`,
   `to = "sovereign-commission-seal"`, `except_from = ["sovereign-core", "sovereign-runtime-recipe"]`.
   `git grep -n except_from -- quality/arch-layers quality/ARCH_LAYERS.toml` is empty today, so this
   is a schema addition, not a config edit. Rejected alternative: two `[[exception]]` rows — that
   table is a burn-down list whose header says entries are expected to disappear. Row
   `REVIEW-build-hd-2-forbid-from`; its PLANT is the layer-gate red on
   `sovereign-server -> sovereign-commission-seal`.
3. **Seal `RuntimeParts` in core.** Field `pub seal: CommissionSeal`; constructor `RuntimeParts::sealed(seal, ..the nine)`; `Runtime::new` destructures `seal: _`. For ONE row `RuntimeParts::new` survives as a delegator that mints (so downstream tests still compile), and the recipe plus core's own seven files move to `sealed`. Row `hd-2-seal-core`.
4. **Downstream tests take the seal through a dev-dependency; delete `RuntimeParts::new`.** Row `hd-2-seal-tests`.
5. **Commission takes a Launch.** Move `RecipeInputs`, `CommonParts`, `common_parts` and `commission` from recipe lib.rs to `src/commission.rs` (lib.rs re-exports `commission`, `RecipeInputs`, `CommonParts` so `sovereign_runtime_recipe::commission(` stays literal for the census). `RecipeInputs` gains the five host slots (`mesh_knowledge`, `routing_events`, `sensitive_corpora`, `corpus_principal`, `landscape_digests`, types as runtime.rs:458-467); `CommonParts.parts` becomes `runtime: Arc<Runtime>`; `common_parts` goes private; `CommissionSeal::mint()` appears once, inside it. `pub async fn commission(launch: &Launch, inputs: RecipeInputs, progress: &dyn RecipeProgress) -> CommonParts` emits `tracing::info!(target: "capability", launch = launch.as_str(), "runtime: commissioned")` exactly once — the line the hd-7 DEMO pastes. Hosts: daemon passes its `launch`; chat passes a literal `Launch::Verb{name: "chat", ..}` (mesh_cmd.rs:37-46 is the precedent); server passes `&Launch::Server`. A unit test pins that the trace names the launch it was given. `commission` returns `CommonParts`, NOT a `Result`: the refusal branch and the `Launch` match were cut at round 1, and `AssemblyRefusal` (sovereign-daemon/src/daemon_services.rs:522) is in layer `mesh-api` while the recipe is layer `capabilities` (quality/ARCH_LAYERS.toml:277, :307), so importing it would be an upward edge that fails the LAYER check this row's own check list runs. SYSTEM_OVERVIEW.md:250 updated (one line). Row `REVIEW-build-hd-2-commission` — the PLANT lands here.

## Seams

- Do NOT touch: `daemon_cmd/tool_registry.rs` (the `/mcp` registry, HT excluded `registries`); the seven TURN_EXECUTION_SITES harnesses (dispatch, not assemble); `TurnFrame::Complete` and every turn exit (hd-1); the MEANING of `corpus_principal`/`sensitive_corpora` — they move from a struct-update into `RecipeInputs` fields with identical values (hd-6 / warrant own principal -> Scope); `EmbeddedDaemon`/`LaunchParts` shape (unchanged; the nesting is by `ServingCore.runtime`).
- Files other rungs also touch: `sovereign/SYSTEM_OVERVIEW.md` (every rung; a peer's edits were uncommitted on 2026-09-17, committed as of round 2 — re-check `git status --short`); `quality/ARCH_LAYERS.toml` + `quality/arch-layers/src/lib.rs` (hd-3's layer-gate rule for `SetupConfig::save`); `sovereign-core/tests/main/core_tests.rs` (hd-4 — 11 lines of oversized slack shared, see Atoms); `sovereign-contracts/src/setup_config.rs` (hd-3's config flock).
- Peer state: `sovereign/crates/sovereign-mesh/Cargo.toml` and `sovereign/SYSTEM_OVERVIEW.md` carried uncommitted peer edits on 2026-09-17 and BOTH are clean as of round 2. The rule stands as a premise the row checks, not as a claim about now: `hd-2-seal-tests` verifies `git status --short sovereign/crates/sovereign-mesh/Cargo.toml` is empty before editing, and stops (§6) if it is not. Domains CUT rows `dm-daemon-cli-composition` and `REVIEW-build-daemon-embedded-split` would relocate `daemon_cmd/mod.rs` and `daemon_services.rs`; they must not be re-dispatched while hd-2 runs.
- hd-7 owns the single bench DEMO row and runs LAST in the queue; it pastes this order's `capability` trace line.

## Done when

- `REVIEW-build-hd-2-commission`'s commit body pastes the PLANT red (E0433 on `sovereign_commission_seal` in `sovereign-server/src/main.rs`; E0603/E0425 on `sovereign_runtime_recipe::common_parts`) and the green LINT after revert; `REVIEW-build-hd-2-forbid-from`'s body pastes layer-gate red on `sovereign-server -> sovereign-commission-seal` and green after revert.
- LINT, LAYER, TEST(sovereign-runtime-recipe), TEST(sovereign-core) exit 0.
- These return nothing:
  - `git grep -n 'RuntimeParts::new(' -- '*.rs'`
  - `git grep -n 'RuntimeParts' -- sovereign/crates/sovereign-cli-daemon sovereign/crates/sovereign-cli-llm sovereign/crates/sovereign-server/src/main.rs`
  - `git grep -n 'pub async fn common_parts\|pub parts: RuntimeParts\|pub fn commission(parts' -- sovereign/crates/sovereign-runtime-recipe`
- `git grep -n 'CommissionSeal::mint' -- sovereign/crates ':!*/tests/*' ':!*_tests.rs'` names only `sovereign-runtime-recipe/src/commission.rs` plus `#[cfg(test)]` blocks (core's metalingual.rs/history.rs test modules, `daemon_services.rs` fixtures) — the worker lists each hit with its enclosing cfg.

## Kill

- A non-test path other than the recipe needs a `Runtime` from custom parts (a second mint site in production). None exists today.
- A process that commissions is not `Daemon | Worker | Server | Verb` (a new `Launch` variant would be needed): stop and redraw — HT bar `hd-structural` kill.
- The layer-gate owner refuses `except_from` (step 2, row `REVIEW-build-hd-2-forbid-from`) and no existing rule can restrict inbound edges: the seal is then convention, not structure — stop and report rather than landing it (HT bar `hd-structural` kill).
- Folding `common_parts` needs per-host branching inside the recipe beyond the five data fields (an adapter per host): stop.
