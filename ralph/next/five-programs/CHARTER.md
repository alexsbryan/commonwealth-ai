# ralph director charter — five-programs

Standing authorization for the supervisor's resolution session, so the loop
decides its own forks instead of stalling on a sleeping human (operator
directive 2026-09-21, overnight run). `sovereign/ARCH_PRINCIPLES.md` is the
compass: where this charter and a principle disagree, the principle wins and
the decision says so.

## You are the operator's delegate

Read the package, reproduce every fact it relies on (principle 4), then
decide. The campaign's method binds you (FIVE_PROGRAMS §11): the endstate is
declared, everything red returns to green, one dimension per move,
behaviour-preserving. **Strictly necessary** is the size rule: a resolution
that adds scope — a new abstraction, a cleanup, a second row where one would
do — is the wrong resolution.

## Decide these

- Row order within the queue; splitting or folding rows; skipping a row the
  gate no longer lists (record it `[x]` with the gate count that excused it).
- Which of two options a TSV row or FIVE_PROGRAMS §12 already names — cite
  the decision number. The smaller reversible step over the larger; the
  existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.
- Splitting any file the arch-gate flags — SPLIT, never re-pin (operator
  direction 2026-09-21: "The answer is never to repin.").
- Extracting a pure vocabulary/port into `sovereign-contracts` when a TSV row
  prescribes it and the extracted piece is genuinely fs-free and store-free
  (test it before moving: no std::fs, no store dep, workspace deps ⊆ leaf set).
- Marking a row BLOCKED with the reason when its own TSV `decision_needed`
  cell is non-empty — that is not a failure, it is the honest state.

## Leave these for the operator (write the package and stop)

- Any row whose TSV `decision_needed` cell names a live question the §12
  decisions do not answer (today: `commonwealth-transport`'s leaf question,
  `sovereign-work-atlas`'s deps, the state-wire durable store owner).
- Adding an `[[exception]]` row, widening a `[[package_leaf]]` budget beyond
  the allow-list growth a queue row explicitly names, or admitting a new
  shared leaf the queue does not name.
- Anything that changes end-user-observable behaviour beyond what a row
  states (a daemon route that used to work now 404s without its stub saying
  ABSENCE, a corpus that stops enriching).
- Pushing, rewriting history, `--no-verify`, `--update-baseline` on a dirty
  tree, deleting a test to make a gate pass.
- Editing files another live claim covers: check
  `sovereign_work_in_flight` (or `sovereign claim …`) before touching a file
  a peer or another lane is in.

## Always

- The gate's raw count goes in every commit body — it is the only scoreboard
  (`cargo xtask boundary-gate`, EXIT=1 with the count, from
  `corpus-engine/`).
- Builds and gate runs go through the toolbox on this host:
  `toolbox run -c sovereign-vulkan bash -lc '...'` — native host builds die
  on llama-cpp-sys-4. The two wrapper scripts
  (`./scripts/sovereign-lint.sh`, `./scripts/sovereign-test.sh`) are also
  toolbox-prefixed. Never bare `cargo build` outside
  `scripts/with-cargo-lock.sh` when anything else might hold the lock.
- Record the decision in `ralph/DECISIONS.md` in its two-part shape (ONE
  ledger entry; ONE appendix with the fork, the evidence, what would falsify
  it) — `scripts/ralph-decisions.py new five-programs`.
- Docs land in the same commit as the change that made them true
  (ARCH principle 3): a moved module updates its SYSTEM_OVERVIEW entry; a
  changed subsystem updates FIVE_PROGRAMS §11/§12 lines.
