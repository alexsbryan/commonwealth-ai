<!-- ledger -->

**phase-b-66 · 2026-09-30 · pb-cli-llm-ingest-move in parallel · seat (operator: "Go ahead", 2026-09-30)** — this commit
- Needed: the operator asked to shorten the remaining ~20 hours. Measured over the last 24 h: 35 sessions; 11 stops cost ~80 minutes in total (5%), so removing stops buys little. The time is in the work itself.
- Chose: one parallel slice, not a second loop over the whole queue.
  - pb-meshapp-rest moves ahead of pb-ingest-dial-daemon in file order, so the main loop runs those two next.
  - pb-cli-llm-ingest-move is held in this queue's ctl/parked. Once dial-daemon lands, it runs in worktree B (/home/alexbryan/dev/pb-par-ingest, branched from that commit) while the main loop runs the serve chain.
  - B's target is a reflinked copy of the main target with every tracked file touched, per the worktree-private-target memory; sharing one target across worktrees is unsafe.
  - B shares the main cargo lock (`toolbox run` drops SVRN_CARGO_LOCK_DIR in any case). The two workers edit in parallel and build one at a time, so memory pressure on the deployed daemon stays where it is today.
  - The seat directs B, rebases B's code commits onto `cut` (never its ralph state commits), reconciles Cargo.lock, ARCH_LAYERS.toml and the size baselines, re-runs the row's gates on `cut`, marks the row [x] and unparks its dependents.
- Because:
  - The ingest move is the largest single item (~44k lines). It touches cli-llm, a new ingest CLI crate and the dispatcher, and barely touches sovereign-daemon.
  - The serve chain's rows rewrite sovereign-daemon (bootstrap, boot, state, daemon.rs), the same files pb-ingest-dial-daemon rewrites. Parallelizing those would mean large refactor merges (principle 2).
  - The operator waived AGENTS.md's one-cargo-worker rule for this slice ("Go ahead"). The shared lock keeps its purpose, OOM safety, intact.
  - Expected saving: ~3-5 h. Unmeasured; reported after.
  - This commit changes no Rust.

<!-- appendix -->
