<!-- ledger -->

**phase-b-85 · 2026-10-01 · ship gate (operator direction) · seat** — this commit
- Needed: Phase B's finish is structural. Boundary 0, the lifts and the census do not say whether an install, a chat turn, an ingest or a mesh query still works as it did on main. The operator pasted a review draft (origin/cut at 628b01f08, macOS peer) and directed: "own this with the most pragmatic level of verification (we don't have time for 36 hours of runs)."
- Chose:
  - ralph/PHASE_B_SHIP_GATE.md, pre-registered before its readings. About 2.5 h of runs at the final tip: Tier 0 build gate, Tier 1 static test inventory and verbs, Tier 2 behaviour on the deployed node. Screen, then confirm: each behavioural lane runs once against this host's committed baselines, and only a lane outside its band, or one with no comparable baseline, is escalated to ABAB with n=3 against main.
  - Four rows, each a split of an in-scope row:
    - pb-distribution-release-bins (F1): a release install ships three of the binaries its verbs exec, so it cannot start the daemon.
    - pb-mesh-exit-mesh-join-save (F2): cw-rails' join answers 200 on a failed persist. REVIEW_FINDINGS assigned it to exit-mesh, and its row never took it.
    - pb-serve-package-guard (F3): standalone serve's internal routes have no loopback guard.
    - pb-distribution-ship-gate: runs the gate after pb-distribution and the three fixes.
  - The review's split-deployment findings go to phase-c, as the gate file lists.
- Because:
  - F1 to F3 are regressions of Phase B's own split, each verified in the tree at b90845d20, so they are not new scope. The binary split made the release lists stale. The standalone serve added an unguarded route. The flip made cw-rails' join the only one.
  - The cheapest instrument that can fail is run first (principles 5, 7, 11). The quality lanes cost ~25 min against existing baselines. A main build and an ABAB swap cost hours, so they are bought only on alarm.
  - Measured while writing: no behavioural quality lane has run on this host during Phase B's landings. The nightly check runs are `--budget-secs 0` structural probes by design (quality/instruments.toml:1303). An early screen of the flip started at 2026-09-30 21:18 PDT.
  - Where the review is stale, the file says so:
    - the tensor-split BAR passed at ab64e8dac and was re-proved by the serve-mesh lift after the flip;
    - f26_egress_census passes on Linux since 62fa4378c;
    - the desktop already stages sovereign-stock (d1f3e1765).
- REVIEW-AFTER: pb-distribution-ship-gate. This decision is falsified if a regression a user would see passes this gate and is found later by a reading it chose not to buy. The cheap candidates are the release ABAB latency it skips, the SEP install and the macOS suite.
