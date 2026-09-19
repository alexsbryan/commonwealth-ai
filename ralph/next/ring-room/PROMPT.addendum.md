<!-- section: vars -->
prefix = rr
<!-- section: intro -->
# ralph — the {{queue}} campaign, one unit per session

You are a worker executing ONE unit of the `{{queue}}` campaign
(`quality/campaigns/ring-room.toml`, child of `ring-apps`). The order it executes
is `.sovereign/features/ring-room-week1/order.md`; its Demo section is the
definition of done and the rows below are its Steps, one commit each. A fresh session starts every iteration: this
file, `{{state}}` and the repo are your whole memory. `ralph/STATE.md`
is ANOTHER campaign's queue (domains) - never open it, never mark it. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

<!-- section: review-audit -->
**`REVIEW-audit-rd-<n>`.** Run TESTALL and PREPUSH. Read `git log` and
`git diff` since the previous audit against `sovereign/ARCH_PRINCIPLES.md`
("The twelve"). Fix what you find, behaviour-preserving, and record each finding in
`ralph/REVIEW_FINDINGS.md`: principle, path:line, fixed-in hash. A red gate
you cannot make green: §6.

<!-- section: checks-queue-1 -->
| TOML | `scripts/ralph-check.sh toml` | exit=0 |
<!-- section: checks-queue-2 -->
| NODE(d) | `scripts/ralph-check.sh node d` (node 20 and npx are in the toolbox; pin every npm version the row names) | exit=0 |
| DEMO | `scripts/ralph-check.sh demo` — starts and stops its OWN throwaway daemons under `SOVEREIGN_DATA_DIR`, never the deployed one | exit=0 and five rows reading PASSED; OR exit=4 where every non-PASSED row reads COULD-NOT-JUDGE naming only commits outside this campaign (the operator's 2026-09-18 no-push decision) — paste the rows either way. exit=1 (any FAILED) is §6 with the rows |
| DEMO-BG | `scripts/ralph-check.sh demo-bg` — starts the full demo DETACHED (it runs ~25 min, past the 10-minute foreground limit) | prints `started pid=` |
| DEMO-WAIT | `scripts/ralph-check.sh demo-wait` — polls up to 9 min; exit=3 means still running: call it AGAIN, as many times as it takes; then read it exactly as DEMO | as DEMO |
<!-- section: hard-rules-scope -->
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`,
  `scripts/ralph*`, or ANYTHING under `commonwealth/crates/commonwealth-rail/` or
  `commonwealth/crates/commonwealth-rail-core/` — the RING RAIL (zero diffs there
  beyond the roster-door hunk the operator permitted is the campaign predicate;
  a row that seems to need one is §6). `commonwealth-rails/` is the rails DAEMON,
  not that rule. Never stop or restart the DEPLOYED daemon (the one `svrn daemon status`
  names); the throwaway podman nodes `scripts/ring-room-demo.sh` (and `scripts/ring-doc-demo.sh`, which it sources) start under its own
  `SOVEREIGN_DATA_DIR` are the script's to start and stop, exactly as
  `scripts/ring-offers-demo.sh` does.
