<!-- ledger -->

**phase-b-109 · 2026-10-01 · pb-distribution-ship-gate · seat, filing the gate's registry-lag finding to phase-c** — this commit
- Needed: the ship gate's release-note commit (0e216923b) names a finding "no row owns" and records it only in target/ralph/phase-b/preflight-forks.md, an untracked file: a corpus ingested while the daemon runs has no corpus_state row until the next boot, so a grounded turn searches 0 corpora with no absence named. Pre-existing on main (cli-daemon daemon_cmd/mod.rs:909 at 18f783f44).
- Chose: pc-corpus-registry-live, above phase-c's cut line with the bugs main shares; in scope.txt; it fills wave 7's free lanes.
- Because: it is a user-visible correctness bug (a fresh corpus is silently unsearched), and a finding in an untracked file is filed nowhere (the shape phase-b-97 and -104 corrected).
