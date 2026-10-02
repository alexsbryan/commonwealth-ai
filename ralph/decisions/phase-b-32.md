<!-- ledger -->

**phase-b-32 · 2026-09-27 · Phase B → the scope is frozen at 44 open rows, and §2c's one-implementation item leaves the finish for phase-c · operator**
- Needed: rows grew faster than they closed. Phase B launched with 26 open rows (2dbd8d4b7) and, by 8e0ac88ae, had closed 30 and added 50, leaving 46 open. Measured in edges, the scope did not grow: 51 at phase-b-10, 49 now. The growth was splits as rows were priced (phase-b-7 +8; phase-b-30's census +18 at once, when it re-priced ~43k lines to ~200k changed), cadence audits (+5), the operator's two rows (phase-b-31), and finish items beyond separability.
- Chose (operator; the seat offered four cuts and the operator took two):
  - Freeze. The 44 open rows are Phase B, listed in ralph/next/phase-b/scope.txt, which queue.toml's `scope_file` makes the planner enforce: a row outside it waits on the operator and never dispatches. A split `<id>-<suffix>` keeps its parent's scope, and audit rows are in scope. Findings go to phase-c by default, or park if they truly block. Audits run every 10 units instead of 5. The burn-down is counted in edges, exceptions and failing lifts, not rows.
  - §2c's "each drive has one implementation" leaves the finish for phase-c. pb-daemon-adopts, which existed only for it, moves to phase-c as pc-daemon-adopts, and pb-distribution takes over its dependencies. Struck from rows: pb-code-server's bring-up and locator collapses, pb-ingest-dial-daemon's job-execution 2 → 1 (the legacy pull loop stays and calls the port), and pb-distribution's bring-up-drive collapse and its "§2c counts are 1" finish line. A row still never ADDS a copy of a drive.
  - Not taken: five programs instead of six (serve stays its own package; pb-serve-package and pb-serve-placement stay), and deferring pb-rails-idle (the idle bars stay). The operator: "We should be able to bound the program and reach completion and not sacrifice end user quality." End-user quality is not part of the cut.
- Because:
  - The finish is what makes "take THIS without THAT" true: boundary-gate 0, no `svrn` exception, six lifts, the `EmbeddedDaemon` census, and a green suite. One implementation per drive is principle 8's cleanup, not separability.
  - Principle 10: a freeze asked of a worker is remembered. A freeze the planner enforces is structural.
  - Boundary gate: 49 at 7f5b22e6b.

<!-- appendix -->

## phase-b-32 · 2026-09-27 — freeze and the §2c cut

<details><summary>reasoning, evidence, package</summary>

Row history (the seat's count over every commit touching the queue): total 1 → 29 (phase-b staged) → 25 (phase-b-2 scope) → 26 at launch → 37 (phase-b-7 re-chunk) → 52 (by 09-26 13:43, splits and the serve/mesh census) → 55 (phase-b-29) → 74 (phase-b-30) → 76 (phase-b-31). Done went 0 → 30 over the same span.

The triage of the 46 open rows against the finish: 33 close a named edge, retire an exception or pass a lift. 8 are prerequisites that close nothing themselves (pb-mesh-exit-core, pb-rails-origins, -reach, -membership, -parity, pb-bench-dials, pb-svrn-serving-ports, pb-serve-placement). One is the HUMAN lanes reading. Only pb-daemon-adopts served §2c alone. pb-serve-distributes' "engine assembly 3 → 2" stays, because it is a consequence of the daemon dropping sovereign-inference, not extra work. pb-work-donor's "stays at one drive" is a constraint, not a collapse.

The two cuts not taken were priced like this. Five programs would have deferred pb-serve-package, most of pb-serve-placement (~12,200 lines moved; pb-mesh-exit-mesh still needs the measurement store it reads) and pb-mesh-dissolve's ~7,450-line fold. pb-rails-idle is ~150 lines of harness plus a fix priced from the trace.

Enforcement: scripts/ralph.py `out_of_scope` / `held_ids`, with a test in which a row added after the freeze waits while a split and an audit row do not. Planting its removal turned the test red.

</details>
