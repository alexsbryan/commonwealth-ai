<!-- ledger -->

**phase-b-41 · 2026-09-28 · pb-code-clean → split: the edge half is done, LIFT(code) is pb-code-clean-lift · director** — this commit
- Needed: pb-code-clean closed its two edges (BOUNDARY 30 → 28, PLANT watched) and LIFT(code) now builds outside the monorepo, but 35 of the lifted closure's own tests fail in the sandbox. The worker stopped under the scope guard and asked four things: split or raise the LIFT, how the lift supplies repo data files, what the refactor factory's monorepo self-tests are in a lift, and whether the lift's TMPDIR under `$HOME/.cache` stays.
- Chose: split. pb-code-clean is `[x]` at 677f5a7a2 on its edges and PLANT; pb-code-clean-lift (in scope by phase-b-32's prefix rule, added to pb-distribution's depends) owns the LIFT pass. Its rule: no test in the code closure climbs out of its crate root; repo data arrives through a knob. (2) `carry` accepts a file, the ruler rides the existing `CO_BACKLOG_RULER`, the anchors mirror test reads through the loader's existing `SOVEREIGN_WORKSPACE_DIR`, and both files keep ONE copy in quality/. (3) Mechanism tests move onto TempDir fixtures, live-data tests read `SOVEREIGN_WORKSPACE_ROOT`, and any that cannot hold on the lift's own tree move to the monorepo's gates. (4) TMPDIR stays, and spec.rs's negative walk is hardened. The IgnoreFilter bug is fixed as a product delta. Two out-of-row findings went to phase-c.
- Because: the two halves need different proofs (BOUNDARY and PLANT vs the lift's test step) on disjoint files, which is the charter's split test. Principle 11 and extend-never-re-own: every knob and the `carry`/`tree` mechanisms already exist, so the only new code is `carry` taking a file. Principle 12: a test asserting THIS repo's register is the monorepo's, not the code program's. Principle 5: an unset knob panics, and nothing skips. Boundary gate: EXIT=1, 28 violations, 0 `[code]`, at 677f5a7a2.

<!-- appendix -->

## phase-b-41 · 2026-09-28 — pb-code-clean splits at the lift; the lift's four forks decided

<details><summary>reasoning, evidence, package</summary>

Reproduced this session:
- `scripts/ralph-check.sh boundary`: boundary-gate FAILED (28 violation(s)), no `[code]` line.
- corpus-engine-watchers reindexer.rs:1432-1440: `path.components().any(...)` over the absolute path against `HARD_EXCLUDE` (which lists `.cache`, `build`, `dist`, `target`). queue.toml:49 sets `TMPDIR=$HOME/.cache/program-lift`, so every lift fixture is ignored. This is a product bug: any project under such a directory never reindexes.
- `/home/alexbryan/.sovereign` exists, so spec.rs:234's "elsewhere" TempDir under `~/.cache/program-lift` walks up to a marked root, as the package said.
- target/ralph/phase-b/lift-code-nff.log holds 35 `panicked` lines. refactor_wire.rs:494 panics "…/sandbox-code/crates/sovereign-cli-dev is not inside a git repository", and destination.rs:494/603 panic in `repo_root()`.
- ruler.rs:16 `RULER_ENV = "CO_BACKLOG_RULER"`, registered at env-flags.toml:736. scripts/co-backlog.py:205 also reads quality/backlog-ruler.toml, and sovereign-cli seat_cmd.rs:380 reads quality/operational-anchors.toml, so neither file has a single-program owner (rung 1 does not fire).
- read_notes.rs:139-141: the anchors loader already honours `SOVEREIGN_WORKSPACE_DIR` before its ascent.
- program_lift.py:525 `carry` uses `copytree` (directories only), and :533 `tree` is the ingest precedent (phase-b-40).

Not re-run: the lift itself (the kept sandbox's log is the census, and nothing it depends on changed in this commit).

Correction the package asked to record: the LINT scope quoted in 4273cfbf2 and f68f53a25 came from a stale log. Inside the toolbox RALPH_QUEUE is dropped, so LINT wrote target/ralph/lint.log. The exit=0 claims stand, and the WORKSPACE-scope run at 677f5a7a2 covers every commit before it.

What would falsify this: code_mcp_e2e.rs:375 still failing after the watcher fix (then it is a separate fault, censused in the row); a refactor live-data test that has no monorepo home passing LAYER (the row packages it); the lift's total fix exceeding twice its ~500 LIFT (split again by proof).

REVIEW-AFTER: pb-code-clean-lift lands. Fork (3)'s "move to the monorepo's gate surface" is decided in principle only, and the row's trial settles where.

</details>
