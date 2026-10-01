<!-- ledger -->

**phase-b-110 · 2026-10-01 · pb-distribution-f13-rails-stall · seat, filing F13's re-seal finding to phase-c** — this commit
- Needed: F13's proof (37208c988) hands on a census finding "not built (scope guard)" and records it only in target/ralph/phase-b/preflight-forks.md, an untracked file: activity-private re-seals on every write-bearing tick (its live set, 2,870 on the deployed node, is above SEAL_AFTER_OWN_OPS 2,000 and the snapshot's own rows count toward the bar), and each snapshot is O(n^2) in RingJournal::append.
- Chose: pc-rails-reseal-loop, above phase-c's cut line with the bugs: F13 took it off the request path, so the stall is fixed; what remains is CPU spent continuously on an active node.
- Because: a finding in an untracked file is filed nowhere (phase-b-97, -104, -109).
- F13's proof, fact-checked: before the fix the sandbox pair read p95 5,332 ms (max 8,226, 6 failed probes); after, /status and /v1/models busy p95 5.4/5.5 ms vs ~7 ms idle over 787 probes and 2 seal cycles, 0 failed; a stopped cw-rails is named absent in the fan-out plan and /status; PLANT 1 (inline wait) 3,005 ms, PLANT 2 (roster default) two tests red.
