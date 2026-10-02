<!-- ledger -->

**phase-b-34 · 2026-09-27 · Phase B → the four operator-side steps become their rows' own last steps; no HUMAN row remains · operator**
- Needed: phase-b-33 queued four HUMAN rows for host steps a worker had never been allowed to take: HUMAN-pb-code-cutover, HUMAN-pb-notes-migrate, HUMAN-pb-flip-roster and HUMAN-pb-serve-distributes-bar. The operator: "All those human rows seem like you could resolve. You're the only agent on this machine, you have autonomy." Asked which host actions the loop's workers may take, the operator answered: "You may restart the deployed daemon, migrate ~/.svrnmesh/notes.db behind a backup, and edit the live mesh roster."
- Chose (operator):
  - pb-code-daemon-exit ends by restarting the deployed daemon through the CLI and verifying that `:9741/mcp` lists svrn's and code's tools.
  - pb-notes-memory's migration takes its own pre-migration copy (SQLite backup API, pinned by a test and a PLANT) and runs on the live store at the row's closing restart, pasting the conservation counts.
  - pb-mesh-exit-transport's series ends by retiring the five `cw-rails-lift` members from the live roster.
  - pb-serve-distributes runs its release-profile bar readings detached under the loop's `waiting` marker (24 h limit) when they cannot fit a session.
  - The four HUMAN rows are deleted, and no HUMAN row remains in the queue.
  - Nothing else on the live host is granted. phase-b-7's rule still holds for every other row.
- Because: principle 12 (the row that changes what the deployed daemon serves owns putting it in service), principle 10 (the backup is the migration code's job, not a step to remember), and the operator's grant, quoted above. Boundary gate: 44 red + 4 excepted (pb-meshapp-apps closed two since phase-b-33).
