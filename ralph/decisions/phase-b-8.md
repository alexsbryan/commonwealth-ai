<!-- ledger -->

**phase-b-8 · 2026-09-26 · lift baseline owners · seat** — this commit
- Needed: pb-lift-instrument's baseline (bc984cc46) failed all six program lifts, each for a stated reason. The appendix gave every red edge one owner, but not every lift blocker. Two of the blockers belonged to no row: bench's HAND-SPELLED-PATH and missing RUN smoke, and serve's uncarried `[patch.crates-io]` fork. The queue's `lift` check also put the sandbox on `/tmp`, a 63G RAM-backed tmpfs where ingest's cold target reached 47G before the quota (1d611fcab).
- Chose:
  - Each blocker goes to the row that already owns the program's proof: pb-ingest (evidence_reds), pb-serve-program (carry the fork), pb-cli-llm (bench's path and smoke) and pb-membership (the fixture origin, plus retiring the five `cw-rails-lift` roster members the lifts left).
  - pb-hostkit's LIFT(cmnwlth) proof names the baseline's out-of-row cause, and the RUN runs once.
  - The `lift` check sandboxes under ~/.cache/program-lift with a persistent target.
- Because:
  - Principle 5: a lift nobody owns is never-ran forever.
  - Principle 7: an instrument that cannot carry a real dependency is validated before it judges serve.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-8 · 2026-09-26 — every lift gets an owner row; the lift check leaves tmpfs

<details><summary>reasoning, evidence, package</summary>

The new check was run at HEAD against bench: `ralph-check`'s argv, toolbox, and TMPDIR on ~/.cache. The planner refused in 0.7 s with the baseline's HAND-SPELLED-PATH, the same verdict line as bc984cc46. The persistent target costs disk (the union of the six closures; ingest alone was 47G cold) against 617G free, and buys warm crates.io deps on every later LIFT check. Workspace crates rebuild regardless, because the sandbox path is new each run, so a warm target cannot hand a lift a stale workspace artifact.

Falsifier: a LIFT verdict that differs between a warm and a cold target at the same commit. Then the target goes back to per-run.

</details>
