<!-- ledger -->

**phase-b-106 · 2026-10-01 · phase-c · operator, parallelizing what is left of the campaign; the seat's layout** — this commit
- Needed: the operator: "parallelize as much as possible what's left. We need to wrap this whole campaign up within a couple days (Phase C included)."
- Chose:
  - Phase B is frozen at its open rows (the seat's proposal, adopted by the instruction): a new finding goes to phase-c (or phase-d), unless it reddens a ship-gate bar on the stock install path, which is the operator's ruling.
  - Phase B's remaining fixes run in two loops: the main tree F4-F7 then the ship gate; worktree B (/home/alexbryan/dev/pb-par-onprem, warm target) F8-F10, minting decisions as campaign `phase-b-par` through a local, never-committed addendum line. Each tree parks the other's rows; B lands on cut by rebase and fast-forward, as at d08279f68.
  - Phase C's launch files are written (queue.toml, CHARTER.md, PROMPT.addendum.md, adapted from phase-b's). pc-pool-ready runs first, serially, in worktree C (/home/alexbryan/dev/pc-pool: detached at cut, target reflink-cloned from the main tree and every tracked file touched, the 2026-09-01 recipe), and lands after Phase B's fixes so ralph's own code does not change under the running loops.
  - pc-pool-ready widens from four items to thirteen: the launch-file census found, and the seat verified, that `pool` does not load a queue's manifest (`common(p, queue=False)`, ralph.py:2796), hard-codes ralph/heavy.txt (:1810), picks waves without held rows (:1923), names ralph/STATE.md in the lane note (:2066), lets lanes mint colliding decision ids, runs no audits, deletes a lane's evidence with its worktree (:2148), and halts the whole pool on one refused row. Without them phase-c could not run as a pool and its cut line would not hold.
  - The 14 in-scope rows that lacked `- finish`/`- trial` gained them (a refused row halts every lane).
- Because: two days is enough only if the fixes, the gate and phase-c's waves overlap where the host allows; memory (25-37 GB free with two loops) sets the overlap, a 10 GB floor watched by the seat.
