<!-- ledger -->

**phase-b-102 · 2026-10-01 · phase-c · operator, putting phase-c's cleanup rows last behind a cut line** — this commit
- Needed: the operator: "I want the cleanup rows last and demarcated as such (it could be another cut line)."
- Chose: the 7 cleanup rows (pc-rails-gossip-dst, pc-test-gate-watchers-treesitter, pc-notes-sql-twin, pc-cli-base-residue, pc-notes-db-opens, pc-tiered-classify-back, pc-docs-after-cut) sit last under a `## Cut line: cleanup` heading, and the line is structural, not only typographic: ralph/next/phase-c/scope.txt lists the 13 rows above it, and phase-c's queue.toml, written at launch, sets `scope_file` to it. ralph's frozen-scope rule (`out_of_scope`, scripts/ralph.py) then holds the 7 as waiting on the operator, and phase-c can finish at the line. Moving the line is adding ids to scope.txt.
- Verified with ralph's own queue parser and the scope rule it applies: 20 rows, 13 in scope, exactly the 7 cleanup rows held; the heading ends the last in-scope row's block, so no worker reads it as part of that row.
- Because: no cleanup row changes what a user can do; the operator can ship at the line.
