<!-- ledger -->

**phase-b-103 · 2026-10-01 · phase-c · operator, structuring phase-c for throughput** — this commit
- Needed: the operator: "structure Phase C to maximize throughput -- I want as many tackled in parallel as possible." ralph's pool runs ready rows in parallel git worktrees with serial merges, a per-queue conflicts.txt (pairs that may not share a wave) and heavy.txt (at most one per wave). Four limits were read in the tree at f0ecba1b6: a new lane builds from an empty target; lanes size cargo jobs from MemAvailable when each starts, so lanes starting together over-commit; the serial merge halts on any conflict and every lane regenerates ralph/DECISIONS.md; two rows' quality readings need a quiet host.
- Chose:
  - pc-pool-ready, first and a dependency of every in-scope row: reflink-cloned lane targets, a per-lane jobs share from one budget, a memory floor before a wave starts, DECISIONS.md regenerated once per merge (lanes write only their decision file), and a census of whether lane commits trigger the deployed daemon's code reindex (a 13.8 GB rust-analyzer was measured during one phase-b row).
  - Every bug row is independent after it (`depends [pc-pool-ready]`).
  - conflicts.txt pairs the rows that edit the same files (sovereign-core grounding; the daemon's routes; setup_config's env reads; serve's peer dialing).
  - pc-partial-decline-verdict and pc-gk-rescue-fabrication split into a code row (parallel) and a `-measure` row that conflicts with every other row, so each quality reading runs alone (principle 7).
  - heavy.txt holds the rows that load models or a sandbox daemon: one per wave.
  - 3 lanes. Simulated with ralph's own conflict and heavy rules: 8 waves at 3 lanes (the floor, given two solo readings), 7 at 4. Not 4: 25 GB was available during one row today, and 4 lanes would build at about 4 jobs each.
- Because: the binding constraint on this host is memory, not rows; the structure spends it on parallel code and keeps the two readings honest.
