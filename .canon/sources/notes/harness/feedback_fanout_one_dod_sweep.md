# Parallel workers run targeted cargo tests; the seat runs ONE definition-of-done sweep when they all return — concurrent full suites are…

Operator direction, 2026-08-20, on a three-worker fan-out: *"I think when you
spawn three you should just run a single definition of done sweep when they all
return upward (they can do targeted cargo tests)."*

Why: this is a correctness rule, not an efficiency one. `sovereign-test.sh`
exits 5 on unattributable results by design — concurrent nextest runs
overwrite the shared JUnit report, so the counts are not yours. N workers each
running the unscoped suite either collide into exit 5, or hit the worse case: one
reads a green summary that belongs to a peer and reports it as its own verdict. A
fan-out where every worker runs the full suite is a fan-out that **cannot produce
an honest verdict**. The cost saving (N × 14 min on a memory-capped box) is real
but secondary.

How to apply:
- Workers get targeted gates: `sovereign-lint.sh --human` (scoped), and
  `sovereign-test.sh --human` with `--package <crate>` / `--changed` /
  `--filter <test-name>`. Cheap structural gates (`cargo xtask layer-gate`,
  `nc-boundary.py`) stay with the worker — they prove that worker's specific move.
- Worker verdicts report the full suite as **"not-run-by-design, deferred to the
  seat's sweep"** — never as passed, never as failed. That is a fifth honest
  state beside the four in [[feedback-report-actual-metric-comparisons]]'s
  lineage (passed / failed / could-not-judge / never-ran), and it exists because
  the run was deliberately not theirs to make.
- Seat runs ONE `--full` lint + ONE unscoped suite after all workers return.
  That is the definition of done for the whole wave.
- Amend workers mid-flight if you already dispatched them with full gates —
  `SendMessage` reaches a running agent at its next tool round.
- Corollary: if the sweep goes red, attribution is the seat's job. Diff by
  worker before assigning blame, and check first whether the failure predates the
  fan-out — on this repo four `cargo xtask quality` gates were already failing
  before any of it started.

Still true inside the targeted forms: gate on exit codes, never a summary line; a
zero-test run exits 4 and is NOT green (`--filter` matches the TEST NAME, not the
file, so a typo verifies nothing).

Related: [[feedback-full-suite-not-repeated-filters]] (its "one full run, not
repeated filtered runs" is about a SINGLE session's own gating and is not in
tension with this — the seat still runs exactly one full run per wave),
[[feedback-plan-before-implement-seat-holds-forest]], [[feedback-harden-inner-ring-expand-outward]].
