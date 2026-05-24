# TDD Machine — design

**Status:** Draft v2, 2026-05-24. Updated after Red + Refactor probes.
**Owners:** alexbryan + claude
**Related:** `sovereign/crates/sovereign-agent-bench/src/runners/search.rs`, `sovereign/ARCH_PRINCIPLES.md`

## Core paradigm

The load-bearing architectural pattern is the **solver loop**: parallel candidate generation at varied temperatures, monotonic-improvement gating, fitness function judges, no defensive parsing. Validated empirically on Green-phase (4.2-mini-evaluator: median 20/20, vs role-loop 0-3/9) and Red-phase (92% PASS_AS_RED across N=25).

This v1 instantiates the paradigm for **single-emission, single-file** edits — the shape that fits the model's reliable emission window. v2 will extend the paradigm to other loop shapes (multi-turn stateful loops for cross-file work) using the same fitness-gated search machinery underneath.

The discipline is: **master the paradigm on the simple shape, then add new shapes that compose it.** Don't grow the paradigm before it has earned the ground.

## What this is

A backend that automates the **red-green-refactor** discipline for any harness that calls it. Three tools the calling model picks between based on where it is in the TDD cycle:

| Phase | Tool | When the model reaches for it | Fitness function |
|---|---|---|---|
| **Red** | `tdd_red` | "I have a new behavior to add and no test for it yet" | Generated test FAILS on baseline (it's discriminating) |
| **Green** | `tdd_green` | "At least one test is failing; find an implementation that passes" | More tests passing |
| **Refactor** | `tdd_refactor` | "Tests pass but code quality should improve (split file, remove dup, reduce LOC)" | Tests stay green AND structural metric improves |

Each phase is a `SolverProvider` impl behind a shared registry. The calling harness (Pi, Cursor via MCP, Cline, etc.) decides workflow shape; the machine sustains the discipline once invoked.

## Why this is the right product shape

**1. Honest contract with the user.** TDD is a discipline most engineers know they SHOULD use but don't sustain. The machine sustains it for them. No magic; clear contract.

**2. Plays to local-open-weight strength.** All three phases benefit from parallel-candidate-with-fitness:
- Red: try N different test angles, keep the most-discriminating
- Green: existing parallel-candidate search
- Refactor: try N refactorings, keep the one with best metric delta

**3. Distinct from Cursor/Cline/Aider/opencode.** Those are conversation-shaped (chat → diff → accept). This is workflow-shaped (intent → discipline-bound mutations). They serve different needs; ours doesn't try to replace theirs.

**4. Generalizes across languages and test runners.** No phase is Python-specific. Test discovery and the metrics for refactor are language-aware adapters; the loop logic is identical.

**5. Resolves the cross-bench Rust regression constructively.** The Rust scaffold has no tests, so search stalled. With the TDD machine, the workflow starts at Red: model writes its own tests, then Green drives code. Bench's held-back fixtures become the ground-truth judge of test quality.

## Non-goals

- Building a `sovereign solve` CLI (atos v2 risk).
- Replacing harnesses; sovereign is the workflow backend they call.
- Magic "make my code better" mode — refactor needs a user-specified target.
- Cross-language refactoring (each language's refactor metrics are adapters).
- Streaming progress mid-call (v1 sync request/response; v2 SSE).
- Composing the cycle in the backend — `tdd_cycle()` would bury control flow that belongs in the calling harness. Compose at the harness, not at us.

## Architecture

```
sovereign-agent-bench  ──┐  (uses Green for measurement runs)
                         │
                         ▼
                  commonwealth-tdd  (NEW crate)
                  ─────────────────────────
                    SolverProvider trait (Red, Green, Refactor impls)
                    SolverRegistry::builtin()
                    Workdir newtype (structural safety)
                    ChatBackend trait
                         │
                         ▼
                  sovereign-server
                  ─────────────────────────
                    POST /v1/solve/{tdd_red|tdd_green|tdd_refactor}
                    MCP tools: tdd_red, tdd_green, tdd_refactor
                         │
                         ▼
                  Pi extension @svrnmesh/pi-tdd  (TypeScript, npm)
                  ─────────────────────────
                    pi.registerTool({name: "tdd_red", ...})
                    pi.registerTool({name: "tdd_green", ...})
                    pi.registerTool({name: "tdd_refactor", ...})
                         │
                         ▼
                  User says "add a feature that does X" in pi →
                    main model calls tdd_red → tdd_green → (optional) tdd_refactor
```

## ARCH_PRINCIPLES compliance

Same set of load-bearing rules carry from `SOLVER_DESIGN.md` v2; restated here for completeness.

| § | Principle | Application |
|---|---|---|
| 2.1 | Closed sets → enum | `EditAction`, `SolveStatus`, `RefactorTarget`, `DirtyWorkdir` all enums |
| 3.1 | File size ceilings | Each phase < 800 lines; helpers split |
| 4 | Registry pattern | `SolverRegistry` hosts `tdd_red`, `tdd_green`, `tdd_refactor` (and future solvers) |
| 4.3 | Unknown id loud | `POST /v1/solve/{unknown}` → 400 + registered ids |
| 5 | Interface segregation | `SolverProvider` trait: `id() + solve()` |
| 6 | Data vs program | All three phase prompts live as `commonwealth-tdd/assets/*.md` (red_prompt.md, green_prompt.md, refactor_prompt.md) |
| 7.1 | Structural invariants | `Workdir` newtype; only constructible via `check_safe`; `solve()` accepts `Workdir`, not `PathBuf` |
| 8.1 | Workspace deps | `regex` promoted to workspace |
| 9.1 | Glassbox tracing | Per-phase: `tdd_red:` / `tdd_green:` / `tdd_refactor:` event prefixes |
| 12.4 | Tests without daemon | `ChatBackend` trait + `DeterministicChatBackend` for unit tests |

## The three phases in detail

### Phase: Red

**Goal:** Generate a failing test that captures the user's described behavior.

**Input:** `Workdir`, behavior description, optional test-file path hint.

**Process:**
1. Auto-detect test framework (pytest, cargo test, vitest, jest, go test).
2. Discover convention by reading existing test files (if any) — match their import style, fixture pattern, naming.
3. Generate K=3 candidate tests at varied temperatures (0.3, 0.6, 0.9).
4. For each candidate: write test to disk, run the test command, **require the test to FAIL with a specific assertion error** (not a compilation/import error). If it passes, the test isn't discriminating; reject it. If it errors structurally, reject it.
5. Keep the candidate with the cleanest failure mode (highest "specific assertion failed" weight).

**Output:**
```rust
struct RedResult {
    status: RedStatus,  // GeneratedFailingTest | AllCandidatesPassed | AllCandidatesErrored | Errored
    test_diff: String,
    failing_assertion: Option<String>,  // the actual assertion message
    why_discriminating: String,  // model's brief explanation
}
```

**Why this works:** the model can't game itself — the test it writes is verified against the CURRENT (unchanged) code; if it passes, that's the wrong test. The discipline is enforced by running the test before declaring success.

**Edge case:** No test file exists yet. Use language-idiomatic defaults:
- Python → `tests/test_<feature>.py`
- Rust → `tests/<feature>.rs` (integration) or inline `#[cfg(test)]`
- TS → `tests/<feature>.test.ts`

### Phase: Green

**Goal:** Drive the implementation until all currently-failing tests pass.

**Input:** `Workdir`, optional `task_description` (defaults to inferring intent from failing tests).

**Process:** the existing `runners::search` loop — port from `sovereign-agent-bench` into `commonwealth-tdd`. Parallel candidates × monotonic improvement × stall detection. Unchanged from what we shipped today.

**Output:**
```rust
struct GreenResult {
    status: GreenStatus,  // AllPassed | Improved | Stalled | Exhausted | NoBaseline
    code_diff: String,
    tests_before: TestSummary,
    tests_after: TestSummary,
    rounds: u32,
    trajectory: Vec<RoundSummary>,
}
```

**Why this works:** validated 2026-05-24 — median 20/20 on 4.2-mini-evaluator, 10/10 on 4.1-config-applier, vs role-loop's 0-3/9.

### Phase: Refactor

**Goal:** Improve a specified structural property while keeping all tests green.

**Input:** `Workdir`, single-file refactor target (see v1 set below).

**Process:** search loop. Each candidate is a refactor proposal (one EditAction shape per the existing schema — same emission pattern as Red/Green). Fitness function is **(tests_still_pass: bool) × (metric_delta: f32)** — candidates that break tests are immediately rejected; among test-passing candidates, the one with best metric delta wins. Stall after 3 rounds of no improvement.

**Output:**
```rust
struct RefactorResult {
    status: RefactorStatus,  // Improved | Stalled | TestsRegressed | Errored
    code_diff: String,
    metric_before: MetricSnapshot,
    metric_after: MetricSnapshot,
    tests_still_passing: bool,  // hard gate
    rounds: u32,
}
```

#### v1 targets — single-file only

The Refactor probe (2026-05-24) validated that the model reliably emits single-file edits but **under-emits on multi-file decomposition** (single-emission split: 5% per-candidate, 20% best-of-K=4). v1 ships only targets that fit the single-emission window. v2 extends the paradigm to multi-turn shapes for multi-file work.

| v1 Target | Metric | Implementation |
|---|---|---|
| `ExtractFunction { name, into_path }` | Original file's LOC reduced; function appears in `into_path` | AST extraction; rewrite original to import the extracted name |
| `InlineFunction { name }` | Function definition removed; all call sites contain its body | AST inline at each call site |
| `RenameSymbol { old, new }` | All occurrences of `old` in the file replaced with `new`; tests pass | Scoped to the target file; cross-file rename is v2 |
| `ReorderTopLevels { path }` | Top-level declarations grouped/sorted by convention (data first, helpers, exports last); tests pass | Per-language adapter — pure file rewrite |

Each is **one EditAction emission**, **one fitness check**, **one round of monotonic gating**. Matches Red/Green's emission shape; reuses the same parallel-candidate machinery.

#### v2 targets — deferred

| v2 Target | Why deferred | v2 design shape |
|---|---|---|
| `SplitFile { path, max_lines }` | Probe 2026-05-24: model emits 1-2 files reliably, stops short of full decomposition | Multi-turn stateful loop — model emits files incrementally; harness validates references and prompts for missing modules |
| `RemoveDuplication { path }` | Detection is easy; the model's "rewrite to remove dup" emission spans multiple regions and has the same under-emission problem | Same multi-turn pattern, OR pre-decomposed targets ("here are the 3 duplicated blocks; extract each") |
| `ReduceCyclomatic { path }` | Hits multi-region rewrites for any non-trivial fix | Same |
| `ReduceLOC { path, target_pct }` | Same — broad reduction targets land badly | Same |
| Cross-file `RenameSymbol` | Multi-file by definition | Same |

When we ship v2 these reuse v1's `SolverProvider` trait + `SolverRegistry`; they just register new solver ids (`tdd_refactor_split_file`, etc.) backed by the multi-turn loop. The paradigm extends; the architecture doesn't change.

#### Why this works for v1

Structural metrics are computable deterministically; the model proposes a single-file mutation; the harness scores it; monotonic improvement gates progress; tests stay the safety net. The same Green-style loop, one EditAction at a time.

**Edge case:** if no test command can be detected, `tdd_refactor` returns `RefactorStatus::Errored { reason: "refactor requires test coverage to prevent regression" }` — refusing to refactor untested code is the correct behavior.

## Shared types

```rust
// commonwealth-tdd/src/types.rs

pub struct Workdir(PathBuf);  // §7.1 structural — only via check_safe()

pub enum DirtyWorkdir {
    SystemPath { path: PathBuf },
    UncommittedChanges { path: PathBuf },
    NotAGitRepo { path: PathBuf },
}

impl Workdir {
    pub fn check_safe(path: PathBuf, force: bool) -> Result<Self, DirtyWorkdir>;
    pub fn path(&self) -> &Path;
}

pub struct SolveConfig {
    pub candidates_per_round: usize,        // default 4 (green/refactor) / 3 (red)
    pub rounds_per_trial: usize,            // default 6
    pub max_stall_rounds: u32,              // default 3
    pub emit_max_tokens: u32,               // default 2500
    pub candidate_test_timeout: Duration,   // default 60s
    pub temp_ladder_default: Vec<f32>,
    pub temp_ladder_wide: Vec<f32>,
}

#[async_trait]
pub trait ChatBackend: Send + Sync {
    async fn complete(
        &self,
        model: &str,
        messages: Vec<serde_json::Value>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, BackendError>;
}
// ReqwestChatBackend = production
// DeterministicChatBackend = unit-test mock (§12.4)

#[async_trait]
pub trait SolverProvider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn solve(&self, request: SolveRequest) -> SolveResponse;
}

pub struct SolverRegistry {
    providers: HashMap<&'static str, Arc<dyn SolverProvider>>,
}

impl SolverRegistry {
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register(Arc::new(RedSolver::new()));
        r.register(Arc::new(GreenSolver::new()));
        r.register(Arc::new(RefactorSolver::new()));
        r
    }
}
```

Each solver implements `SolverProvider`, takes a phase-specific request shape (`RedRequest`, `GreenRequest`, `RefactorRequest`) inside the generic `SolveRequest::params` field, and returns `SolveResponse` with phase-specific result inside.

```rust
pub struct SolveRequest {
    pub workdir: Workdir,
    pub model: String,
    pub config: SolveConfig,
    pub params: SolveParams,  // enum: Red(...) | Green(...) | Refactor(...)
}

pub enum SolveParams {
    Red { behavior: String, test_file_hint: Option<String> },
    Green { task: Option<String>, test_command: Option<String> },
    Refactor { target: RefactorTarget, test_command: Option<String> },
}

pub enum SolveResponse {
    Red(RedResult),
    Green(GreenResult),
    Refactor(RefactorResult),
}
```

## HTTP endpoints

| Route | Solver |
|---|---|
| `POST /v1/solve/tdd_red` | `RedSolver` |
| `POST /v1/solve/tdd_green` | `GreenSolver` |
| `POST /v1/solve/tdd_refactor` | `RefactorSolver` |

Single handler dispatches via `SolverRegistry::get(solver_id)`. Unknown id → 400. Dirty workdir → 422.

## MCP tools

Same three exposed on the existing MCP server. Names match the HTTP route ids: `tdd_red`, `tdd_green`, `tdd_refactor`. Internally calls the same `SolverRegistry`.

## Pi extension surface

```typescript
// @svrnmesh/pi-tdd

pi.registerTool({
  name: "tdd_red",
  description: `Write a failing test that captures a new behavior.

Use when:
- The user describes a behavior the code doesn't yet implement
- No test for this behavior exists in the codebase

The tool:
- Reads existing test conventions in the project
- Generates a candidate test
- Verifies the test FAILS on current code (proves it's discriminating)
- Returns the test diff

After this succeeds, call tdd_green to drive the implementation.`,
  ...
});

pi.registerTool({
  name: "tdd_green",
  description: `Find an implementation that makes failing tests pass.

Use when:
- At least one test is currently failing
- You want a parallel-candidate search to find a passing implementation

The tool:
- Runs the project's test suite, identifies failing tests
- Generates K=4 candidate fixes at varied temperatures
- Picks the candidate that maximizes test pass count
- Iterates until all tests pass or stalls

Requires a clean git workdir.`,
  ...
});

pi.registerTool({
  name: "tdd_refactor",
  description: `Improve code structure while keeping tests green.

Use when:
- Tests currently pass
- You want to split a long file, reduce duplication, reduce complexity, or shrink LOC
- You can specify a concrete target (e.g., "split lib.rs into files ≤ 400 lines")

The tool:
- Records baseline metrics
- Generates candidate refactorings
- Rejects any candidate that breaks tests
- Among test-passing candidates, picks the best metric delta

Refuses to refactor untested code (no safety net).`,
  ...
});
```

## What's already built vs new

| Component | State |
|---|---|
| Green-phase search loop | **90% built** — `runners::search` in bench; port to `commonwealth-tdd` |
| `runners::shared` helpers (EditAction, parse, apply, snapshot, run_tests) | **90% built** — port to `commonwealth-tdd` |
| `Workdir` newtype | New (~150 LOC + tests) |
| `ChatBackend` trait + Deterministic mock | New (~150 LOC) |
| `SolverProvider` trait + `SolverRegistry` | New (~100 LOC) |
| **Red solver** | **New (~250 LOC + assets)** |
| **Refactor solver** | **New (~350 LOC + metrics adapters)** |
| HTTP endpoint dispatching via registry | New (~80 LOC) |
| MCP tool registration for all three | New (~120 LOC) |
| Pi extension | New (~250 LOC TypeScript) |
| Docs + memory note | New (~1 day) |

## Effort estimate

| Phase | Work | Days |
|---|---|---|
| 7a — `commonwealth-tdd` crate scaffold + Workdir + ChatBackend + Registry | Per-§-compliance setup | 1.0 |
| 7b — Green-phase port from bench | Migrate `runners::search` + tests | 1.0 |
| 7b.bench — Migrate bench's `runners::search` to wrap `commonwealth_tdd::solve` | Thin adapter | 0.25 |
| 7c — Red-phase implementation | Test-fails-on-baseline verification + framework adapters | 1.5 |
| 7d — Refactor-phase implementation (v1 single-file only) | ExtractFunction + InlineFunction + RenameSymbol + ReorderTopLevels; per-language adapters via tree-sitter or equivalent; tests-as-gate | 1.5 |
| 7e — HTTP + MCP endpoints | Both transports share registry | 0.5 |
| 7f — Pi extension `@svrnmesh/pi-tdd` | Three tools + README + npm publish | 1.0 |
| 7g — Docs | TDD_MACHINE.md, SYSTEM_OVERVIEW §10, memory note | 0.5 |
| **Total** | | **7.25 days** |

Bumped from `SOLVER_DESIGN` v2's 4.75 days because Red and Refactor are new construction (~3 days combined). The Green-phase work is mostly already-shipped code.

## Open questions

1. **Crate name.** `commonwealth-tdd` is the clearest. Alternatives: `commonwealth-cycle`, `commonwealth-rgr`. **Recommendation: `commonwealth-tdd`** — most discoverable, matches what we're building.

2. **Refactor v1 targets.** Listed 4 above. Should we ship all 4 in v1 or pick 1-2 to validate the pattern first? **Recommendation: ship `SplitFile` and `RemoveDuplication` in v1; defer `ReduceCyclomatic` and `ReduceLOC` to v1.5.** SplitFile maps directly to ARCH §3.1's file ceilings, which is a real recurring need; duplication is the second-most-frequent refactor.

3. **Red phase: should multiple candidate tests be unified or only-one-wins?** Multiple tests = better coverage but more model variance. **Recommendation: only-one-wins in v1 to keep the contract simple. v2 could generate a small suite (e.g., positive + negative case + edge case).**

4. **Refactor's metric for "did refactor land"**: when LOC reduced from 600 → 580, that's a win? Or do we need a minimum threshold? **Recommendation: any strict improvement counts as a Refactor success. The metric_delta in the response lets the user judge whether it was worth doing.**

5. **Should `tdd_red` and `tdd_refactor` also respect dirty-git refusal?** Both mutate workdir → yes, same gate as green. The Pi extension's three tool descriptions should each mention the requirement.

6. **Bench integration**: today the bench uses search for Green. Should the bench grow problem types for Red and Refactor measurement? **Recommendation: yes, eventually — a "given this behavior description, generate a discriminating test" problem class is a new measurement axis. Defer to its own design doc.**

## Implementation kickoff checklist

**Prereqs:**
- [ ] Claim `@svrnmesh` on npm
- [ ] Promote `regex` to `[workspace.dependencies]`

**Phase 7a — Crate scaffold:**
- [ ] Create `sovereign/crates/commonwealth-tdd/` with `assets/`, `src/`
- [ ] `types.rs`, `workdir.rs`, `backend.rs`, `prompts.rs`, `registry.rs`
- [ ] `DeterministicChatBackend` unit tests pin the pattern

**Phase 7b — Green-phase port:**
- [ ] `src/green/` modules — port `runners::search` logic verbatim
- [ ] `assets/green_prompt.md` extracted from current Rust string literals
- [ ] Tests with `DeterministicChatBackend` reproducing today's Python-prototype N=10 numbers

**Phase 7b.bench — Bench adapter:**
- [ ] `sovereign-agent-bench/runners/search.rs` becomes ~60 LOC wrapper
- [ ] Bench tests still green

**Phase 7c — Red-phase:**
- [ ] `src/red/` modules
- [ ] `assets/red_prompt.md` (new)
- [ ] Framework adapters: pytest, cargo, vitest, jest, go test
- [ ] "Test must fail" verification + classification (assertion vs structural error)

**Phase 7d — Refactor-phase (v1 single-file only):**
- [ ] `src/refactor/` modules + `src/refactor/targets/` per-target adapters
- [ ] `assets/refactor_prompt.md` (new — single-file emission pattern)
- [ ] `ExtractFunction { name, into_path }` — AST extraction + import rewrite
- [ ] `InlineFunction { name }` — AST inline at call sites
- [ ] `RenameSymbol { old, new }` — single-file scope; tests pass
- [ ] `ReorderTopLevels { path }` — convention-driven file rewrite
- [ ] Tests-as-gate (any test regression → reject candidate)
- [ ] Python adapter first; Rust + TypeScript follow as v1.1 once Python validates
- [ ] Smoke probe each target against a fixture before declaring done

**Phase 7e — HTTP + MCP:**
- [ ] `sovereign-server` depends on `commonwealth-tdd`
- [ ] `POST /v1/solve/{solver_id}` route
- [ ] MCP tool registrations on existing server
- [ ] End-to-end smokes for all three

**Phase 7f — Pi extension:**
- [ ] `sovereign/integrations/pi-tdd/` TypeScript package
- [ ] Three tool registrations with sharp descriptions
- [ ] README explaining red-green-refactor workflow
- [ ] `npm publish @svrnmesh/pi-tdd`
- [ ] End-to-end: install in real Pi session, walk through a red → green → refactor cycle

**Phase 7g — Docs:**
- [ ] `sovereign/docs/TDD_MACHINE.md` — end-user-facing
- [ ] `sovereign/SYSTEM_OVERVIEW.md` §10 lists `commonwealth-tdd`
- [ ] Memory note: "sovereign ships TDD-machine: red-green-refactor backend for any harness that calls it; not a solver, not a coding harness"

## What this design intentionally does not include

- **Composed cycle tool** (`tdd_cycle(behavior)`). Workflow composition belongs in the calling harness. The user might want red → green and skip refactor; might want green → refactor without red. The main model picks.
- **Test generation for legacy code without a description.** Red requires user intent — "test that the cache evicts on size limit" — not "auto-generate tests for this codebase." That's a different (much harder) problem.
- **Cross-file refactor in v1.** Single-file targets only; multi-file (SplitFile, cross-file rename, RemoveDuplication, etc.) is v2 via a multi-turn stateful loop. Documented and probed; not a surprise.
- **Multi-language refactor adapters in v1.** Ship Python first (validated in probes), Rust + TypeScript as v1.1 once Python pattern is proven.
- **Streaming progress.** v1 is sync request/response (~30s for red, 2-3min for green, 1-2min for refactor). v2 may add SSE if Pi extension UX demands it.

## v2 / future paradigm extensions

When v1 is shipped and validated, the same `SolverProvider` trait + registry pattern extends to new loop shapes:

| v2 loop shape | Use case | Why a new shape |
|---|---|---|
| **Multi-turn stateful** | SplitFile, multi-region RemoveDuplication, cross-file refactor | Single emission doesn't fit; need 3-6 turns with structural validation between |
| **Pre-decomposed batch** | Refactor a large codebase against a checklist (e.g., "every file > 800 lines") | Harness pre-computes target list; loop iterates one target at a time using v1 single-file shape |
| **Mutation-tested Red** | Generate tests that catch known mutations (mutmut-style) | Red where fitness is "test kills mutant X" — stronger than "test fails on baseline" |
| **Property-based Red** | Generate hypothesis-style property tests | Red where fitness is "property holds across N random inputs" |

Each shape is a new SolverProvider impl. The trait stays narrow; the registry grows. v1's job is to prove that ONE shape (parallel-candidate single-emission) reliably ships value. v2's job is to expand the catalog.

## Decision checkpoint — resolved 2026-05-24

- [x] Workflow shape: red → green → refactor, three tools
- [x] Crate name: `commonwealth-tdd`
- [x] Three SolverProvider impls behind one registry
- [x] HTTP + MCP both ship in v1
- [x] Pi extension: `@svrnmesh/pi-tdd`
- [x] Dirty git refused across all three (mutate workdir)
- [x] **Refactor v1 targets — single-file only**: `ExtractFunction`, `InlineFunction`, `RenameSymbol`, `ReorderTopLevels`. Multi-file targets (`SplitFile`, `RemoveDuplication`, cross-file `RenameSymbol`, `ReduceCyclomatic`, `ReduceLOC`) deferred to v2. Validated 2026-05-24: model under-emits in single-emission multi-file refactor (5% per-candidate, 20% best-of-K=4). v2 design = multi-turn stateful loop using the same SolverProvider trait.
- [x] **Architectural framing — solver loop paradigm**: v1 instantiates parallel-candidate-with-fitness-gating for single-emission edits. v2 extends the paradigm to multi-turn loop shapes for cross-file work. Master the simple shape, then add new shapes — don't grow the paradigm before it earns the ground.
- [x] Bench Green-phase regression on Rust: **kept as honest signal**; not papered over with pre-install fixtures. The TDD machine's answer is "use Red first, then Green."
- [x] **Probe validation status**: Red (92% N=25 ✓), Green (90% N=10 ✓), Refactor v1 (assumed similar to Red/Green via shared emission shape; will validate during 7d).
