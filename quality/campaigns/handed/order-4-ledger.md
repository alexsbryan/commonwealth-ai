---
schema: work-order/v1
id: handed-4-ledger
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — ledger: every step-execution store answers or refuses
engine: ralph pool; one hd- row, default worker
budget: 1 row; one TEST(sovereign-core) run
---

# Order: handed-4-ledger — StepExecutionStore has no default bodies

## Objective

The replay guard that keeps a non-idempotent tool from running twice reads
`StepExecutionStore::find_execution`. Every method of that trait defaults to "succeeded, nothing
recorded", so a store that forgets to implement it silently disables the guard: a crash between
`Started` and `Completed` replays the side effect. After this order the trait has no default
bodies, an impl that omits a method does not compile, and the one test mock that had nothing to
say refuses by name instead of pretending to record.

## Premises (verified 2026-09-17, file:line)

- `pub trait StepExecutionStore` — sovereign/crates/sovereign-contracts/src/traits.rs:1805-1840; default bodies at :1808-1810 (`record_started`), :1814-1821 (`mark_completed`), :1824-1826 (`mark_failed`), :1837-1839 (`find_execution`); the doc at :1798-1803 states "Every method defaults to a no-op so non-durable contexts (test mocks …) are unaffected".
- Implementations (all four methods present in the three real ones): sovereign/crates/sovereign-store/src/memory.rs:300-339; sovereign/crates/sovereign-store/src/postgres.rs:790-868 (module behind `#[cfg(feature = "postgres")]`, sovereign-store/src/lib.rs:5-6); sovereign/crates/sovereign-store/src/sqlite/step_execution.rs:7 (methods at :8, :32, :49, :61); `impl StepExecutionStore for MockStore {}` — sovereign/crates/sovereign-core/tests/main/core_tests.rs:388.
- `StateStore` is a plain supertrait with no blanket impl (traits.rs:1960-1973); its impls are exactly core_tests.rs:390, memory.rs:715, postgres.rs:1594, sqlite.rs:345.
- Callers: sovereign-core/src/executor.rs:805 (`find_execution`, `?`), :848 (`record_started`, `?`), :903 (`mark_completed`), :927 (`mark_failed`, result discarded); all inside `if descriptor.idempotency == Idempotency::NonIdempotent` (:803). sovereign-store/tests/main/step_execution_replay.rs uses real stores.
- `MockStore` appears only in core_tests.rs (40 hits there, 0 elsewhere under sovereign-core/tests/main). Its one NonIdempotent tool (:1498) is exercised by `executor_tool_denied_permission_skips` (:1512), which returns `StepOutput::Skipped` at executor.rs:760 before :804.
- `Error::NotImplemented(String)` — sovereign/crates/sovereign-contracts/src/error.rs:97; `Error` is imported in core_tests.rs:9.
- core_tests.rs is declared `#[path = "main/core_tests.rs"] mod core_tests;` (sovereign-core/tests/main.rs:27-28); a `#[path]` on a nested non-inline module resolves relative to the directory of core_tests.rs.
- SYSTEM_OVERVIEW.md:2358-2360 repeats the defaults claim ("its methods default to no-ops so non-durable mocks are unaffected").
- Oversized baselines: core_tests.rs 2,541 (tree 2,580), traits.rs 2,036 (tree 2,077); slack 50 (corpus-engine/xtask/src/arch_gate.rs:38).

## Steps

1. Delete the four default bodies (each method ends in `;`) and rewrite the trait doc to say every
   store must answer; a store that keeps no ledger returns an error, because "no prior attempt" is
   the answer that re-runs a side effect. Row `hd-4-ledger-total`.
2. In the same row, give `MockStore` an explicit refusal — all four methods return
   `Err(Error::NotImplemented("MockStore keeps no step ledger; a test that reaches the ledger uses InMemoryStateStore".into()))` —
   written in a new file `sovereign/crates/sovereign-core/tests/main/core_tests_ledger.rs` declared
   in place of the empty impl as `#[path = "core_tests_ledger.rs"] mod ledger;` (child module, so
   the private `MockStore` is `super::MockStore`); fix SYSTEM_OVERVIEW.md:2359-2360 to match. Row `hd-4-ledger-total`.

## Seams

- Do NOT touch: the executor (executor.rs:796-930, including the discarded `mark_failed` at :927); the three real store impls (already total); `HealthStore` and the other defaulted store traits (HT names only the ledger; `durable-state` is not a rung).
- Files other rungs also touch: `sovereign-core/tests/main/core_tests.rs` (hd-2 `hd-2-seal-core` edits seven `RuntimeParts::new(` calls — this row changes core_tests.rs by 0 lines so the pair fits the 11-line oversized slack; either order works, but not concurrently); `sovereign/SYSTEM_OVERVIEW.md` (every rung; a peer's uncommitted edits were on the tree on 2026-09-17 and are committed as of round 2 — re-check `git status --short` before `git add` rather than trusting either statement).

## Done when

- The commit body pastes the PLANT: `mod ledger;` replaced by `impl StepExecutionStore for MockStore {}` gives LINT red E0046 naming `record_started`, `mark_completed`, `mark_failed` and `find_execution`. It then pastes the green LINT after the revert.
- LINT and TEST(sovereign-core) exit 0.
- `sed -n '/^pub trait StepExecutionStore/,/^}/p' sovereign/crates/sovereign-contracts/src/traits.rs | grep -n 'Ok('` prints nothing.
- `git grep -n 'impl StepExecutionStore for MockStore {}'` prints nothing.

## Kill

- A `StepExecutionStore`/`StateStore` implementor appears that is neither a real store nor MockStore (a wrapper, a feature-gated mock) and cannot answer honestly — stop and name it.
- TEST(sovereign-core) shows a test that reaches the ledger through `MockStore`: move that one test to `InMemoryStateStore` in this row; if more than two, stop (the mock is load-bearing and the refusal needs a decision).
