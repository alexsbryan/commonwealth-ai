<!-- ledger -->

**phase-b-42 · 2026-09-28 · pb-code-clean-lift → trialled, dispatchable · director** — this commit
- Needed: the planner refused pb-code-clean-lift because phase-b-41 wrote the row without a `- trial` bullet (queue.toml `dispatch_requires`).
- Chose: trial the row and add the bullet; no rewrite. The 35-panic census reproduces from the kept lift log, 4 targets, same split as the row (watcher 3 + e2e 1, repo data 22, refactor 8, spec 1). The watcher recipe was applied, tested and reverted. The row's check line stays. It now says that TEST(corpus-engine-watchers) cannot see the reindexer tests, so the watcher PLANT is watched through LIFT(code). That scoped-gate gap goes to phase-c as pc-test-gate-watchers-treesitter.
- Because: the charter makes an untrialled rewrite undecidable, and this row is edge-less (BOUNDARY 28 by construction, LAYER untouched), so its trial is the one product change plus the premises it rests on. Principle 5: the trial found that the row's named TEST check could not have watched its own PLANT fail. Boundary gate: EXIT=1, 28 violations (phase-b-41 at 677f5a7a2; this commit touches no Rust).

<!-- appendix -->

## phase-b-42 · 2026-09-28 — pb-code-clean-lift gets its trial; the watcher fix is proven in-tree

<details><summary>reasoning, evidence, package</summary>

Reproduced this session, at 30bc1bee1:
- target/ralph/phase-b/lift-code-nff.log: 35 `panicked` lines. Failed targets: corpus-engine-watchers --lib (3), sovereign-cli-dev --lib (29), sovereign-cli-dev --test code_mcp_e2e (1), sovereign-code --lib (2). Panic sites: reindexer.rs:1691/1708/1719; backlog_cmd item.rs:282 ×8, score.rs:316 ×6, :302, :286, ruler.rs:228 ×2, add.rs:289/315/332 (21); read_notes.rs:633; destination.rs:494 ×6, :603; refactor_wire.rs:494; spec.rs:235; code_mcp_e2e.rs:375.
- `TMPDIR=$HOME/.cache/program-lift-trial cargo test -p corpus-engine-watchers --features treesitter --lib ignore_filter` in the monorepo (sovereign-vulkan toolbox): 0 passed, 3 failed at the same three lines. The bug does not need the lift, only a root under a HARD_EXCLUDE name.
- The recipe applied (`IgnoreFilter { root }` from `build_ignore_filter`, `rel = path.strip_prefix(&self.root).unwrap_or(path)`, +4 −1) makes the full lib pass under the same TMPDIR: 79 passed, 0 failed. It was reverted with `git checkout`, and the tree was clean after.
- Without `--features treesitter` the lib has 54 tests and none match `ignore_filter`. cargo-scope.sh:120 `resolve_features` does not name `corpus-engine-watchers/treesitter`, and ralph-check.sh:57-58 passes only `--package`.
- .cargo/config.toml:111 sets SOVEREIGN_WORKSPACE_ROOT. program-lift.toml:109 is the `tree` precedent. program_lift.py:527 `carry` is `copytree`-only.
- work_in_flight could not judge (cw-rails down at :9747). No ralph worker or cargo process was live on this host (pgrep), and the loop was halted on this package.

Not trialled: whether each refactor live-data test holds on the lift's own tree. The row already decides that per test, and its fallback is bounded (move to monorepo gates with a compile+LAYER trial, or package).

What would falsify this: the worker's TEST(corpus-engine-watchers) run showing the ignore_filter tests (then resolve_features changed under us and the phase-c row is moot); code_mcp_e2e.rs:375 still failing after the watcher fix (a separate fault, which the row already routes to a census).

</details>
