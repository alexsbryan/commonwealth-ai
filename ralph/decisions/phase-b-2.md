<!-- ledger -->

**phase-b-2 · 2026-09-25 · Phase B scope and ordering · operator** — this commit
- Needed: the operator asked how long Phase B would take at the measured rate, and for optimizations that reach the same end objective with less churn.
- Chose:
  - (1) Six rows that close no red edge and make no lift pass move to a staged follow-on queue, ralph/next/phase-c/, with their design kept: the Url venue, the inference origin, bench's three dials, the contracts re-home, the provider split and the per-program config files.
  - (2) The svrn daemon adopts the shared shell and MCP dispatcher LAST, after it has shrunk (new row pb-daemon-adopts). Dead code is deleted FIRST (new row pb-delete-dead).
  - (3) The host kit is built by moving modules and leaving re-exports, and later rows repoint on touch. There is no workspace-wide rename.
  - (4) Mechanical moves use `cargo xtask refactor-apply`.
  - (5) REVIEW-pb-census verifies every row's premises before any worker runs.
  - (6) Each program's "runs alone" proof is its RUN smoke in `scripts/program-lift.sh`, not a per-row e2e harness.
  - The code server replaces the legacy `project serve` instead of porting it. svrn reaches `serve` through the existing terminal-node arm at a default loopback base, so no config migration is needed.
  - Result: 29 rows → 25, stated lift ~35.8k → ~28.6k lines, and the plan has no user-config migration.
- Because:
  - The finish (gate 0, no svrn exceptions, six lifts, one implementation per drive) is unchanged.
  - Principle 11 (reuse the terminal arm, refactor-apply, and the lift instrument).
  - Principle 8 (one "runs alone" decider).
  - Five-programs-54 (delete and move before building).
  - The operator's "no demo, no build".
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-2 · 2026-09-25 — the estimate, and a smaller Phase B with the same finish

<details><summary>reasoning, evidence, package</summary>

**The rate, measured from git** (five-programs commits, excluding ralph/, baselines and Cargo.lock):
- 44,469 changed lines over 344 commits, from 2026-09-22 00:42 to 09-25 12:01.
- 09-24, the one full unattended day on the Claude harness: 23,793 lines and 104 unit dispatches.
- In the Claude era: 17.5 worker-hours against about 36 wall-clock hours, a duty cycle of about 50%. The rest went to operator waits and resolution.

**The estimate before this change:** about 4 days of wall-clock time (bracket 3 to 7), and 30 to 50 worker-hours.
- 36k to 72k changed lines: the stated 35.8k, with a 1 to 2× bracket because the sizes were derived from inventories, not measured.
- At about 20k lines a day, that is 2 to 3.6 days of loop time. The 50% duty cycle stretches it to 3 to 7.
- Phase B carries more new behaviour, e2e tests and migrations than five-programs' moves (fp-60 alone moved 4,161 lines), so lines per hour will be lower.

**After this change:** about 3.5 days (bracket 2.5 to 5).

**Why each deferred row is outside the finish:**
- The config split, the provider split and the contracts re-home touch shared-leaf content. A lift forbids workspace crates, not third-party closure size, so none of them moves a gate edge or a lift.
- The inference origin, the Url venue and bench's dials are capabilities for users who are not yet waiting.
- The bench judge's fail-open defect is a correctness bug, so it stays in Phase B, in pb-cli-llm.

**The churn that ordering avoids.** As first written, pb-shell and pb-mcp ported the daemon's 18 route mounts and its 38-tool builder into the kit. Later rows then deleted four of those mesh route groups, the code tools and the serving bootstrap.

**Unmeasured, and the largest risk to either estimate:** lift-sandbox hazards in svrn, code and ingest (build.rs, include_str! escaping the crate root, tests reading the repo root). REVIEW-pb-census and pb-lift-instrument measure them first.

**Not done:** parallel lanes. AGENTS.md's one-cargo-worker rule and the OOM history rule them out, and builds serialize on the lock anyway: checks take about 40% of the time, so two lanes would gain at most about 1.4×.

**Falsified if:**
- a deferred row turns out to own a red edge or a lift failure at the census; it then comes back into Phase B with the edge named;
- the terminal arm cannot reach a loopback `serve` without a config change, which would bring back a migration and needs an operator decision.

</details>
