<!-- ledger -->

**phase-c-5 · 2026-10-02 · pc-pool-ready's owed dry wave · seat, reading it live and moving the lanes out of the tree** — this commit
- Needed: pc-pool-ready landed with its two-lane dry wave never-ran, owed to the seat. Read live on phase-c's waves 1 and 2: the merge landed with no conflict, the clashing decision was renumbered (868dc0b3a, phase-c-2 → phase-c-4) and `ralph-decisions.py --check` stayed current, and each lane's jobs line shows its share (4 of 14). The claim the row rests on did not hold: a lane's first lint recompiled crates.io dependencies (pc-cmnwlth-lift-flake: proc-macro2, quote, unicode-ident and 196 more in its first 1,534 log lines).
- Chose: lanes live beside the main tree, `<workdir>-lanes/<unit>` (`lane_root_for`, one accessor for every lane path), not under `.ralph/wt/`. The lanes already running finish where they are; any left at the next pool start are moved with `git worktree move`.
- Because: cargo reads every ancestor's `.cargo/config.toml` and concatenates arrays, so a lane under the main tree ran `target.x86_64-unknown-linux-gnu.rustflags` twice (`cargo config get` from a lane prints the mold/`-L native` pair twice; from the main tree and from a sibling directory, once). Rustflags are part of every unit's identity, so the lane's fingerprints (proc-macro2 6179e4c45d78eccd and three more) are hashes the cloned target never held. The 2026-09-01 recipe the row cited used worktrees beside the repo. A RUSTFLAGS override would be a second copy of the config's list (principle 8).

<!-- appendix -->

## phase-c-5 · 2026-10-02 — pool lanes live beside the main tree, so cargo reads one config

<details><summary>reasoning, evidence, package</summary>

Fingerprint comparison, lane `.ralph/wt/pc-cmnwlth-lift-flake/target/debug/.fingerprint/proc-macro2-6179e4c45d78eccd/lib-proc_macro2.json` against main's `proc-macro2-954eb3cf8ea8a8d7`: rustc, features, target, profile, path and config hashes are equal (config 9185878174080762935 on both); rustflags differ, the lane's list being main's twice.

The `.ralph/` code-watcher skip (b0004ed0e) and the `.gitignore` line now guard a path the pool no longer writes once the running lanes are moved; they are left as they are, being harmless, and the watcher skip would cost a Rust rebuild and a daemon restart to remove mid-campaign.

</details>
