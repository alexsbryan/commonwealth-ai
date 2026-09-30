<!-- ledger -->

**phase-b-79 · 2026-09-30 · pb-meshapp-solve, pb-distribution-setup (worktree B) · seat (operator autonomy)** — this commit
- Needed: the main loop's remaining path is serial and long: rehome-daemon → rehome → exit-transport-claims → the flip (a series of sessions, ~2,000 lines new and ~2,400 deleted) → exit-mesh → dissolve (~10,400 deleted) → serve-package → svrn-lift → distribution. Two open rows depend only on rows already `[x]`, and neither touches the mesh chain: pb-meshapp-solve (depends on pb-code-server, pb-code-daemon-exit, pb-stock-binary) and pb-distribution-setup (pb-stock-binary, pb-serve-placement, pb-serve-distributes). In file order, the main loop would reach them only after the flip.
- Chose:
  - Both run in worktree B (/home/alexbryan/dev/pb-par-B, detached from df515cf23), one after the other, while the main loop on `cut` carries the mesh chain. The mechanics are phase-b-75's: B's target is a reflinked copy of the main target with every tracked file touched; B shares the main cargo lock, so the two workers edit in parallel and build one at a time.
  - The main loop holds both rows in ctl/parked. B holds every other open row in its own ctl/parked.
  - The seat directs B. It rebases B's code commits onto `cut` (never B's ralph state commits) and lands them only when the main tree has no uncommitted code. It re-runs each row's gates on `cut`, then marks the row `[x]` and deletes the main loop's parked file.
  - B's launch hit a ralph bug: `ensure_excludes` assumed `.git` is a directory. It is fixed in a5b218c21, which asks git for the exclude path; the test was watched red.
- Because:
  - Neither row's proof or edge touches the flip's. pb-meshapp-solve closes `sovereign-daemon → sovereign-tdd`, and pb-distribution-setup closes `sovereign-cli-daemon → sovereign-inference`. The files they share with the mesh chain are the daemon's boot.rs, daemon_services.rs and lib.rs wiring, plus ARCH_LAYERS.toml, and the rebase reconciles those.
  - The operator asked for this host's runs to finish as fast as possible, and phase-b-75's B landed cleanly.
- REVIEW-AFTER: B's first landing. This decision is falsified if B's builds stall the main loop's gates for longer than B saves. The measure is minutes spent waiting on the cargo lock in the main log.
