# Don't chain repeated filtered test-script runs; one full-workspace run costs about the same and settles everything

When verifying changes with `scripts/sovereign-test.sh`, do NOT run it repeatedly with different `--filter`/`--package` scopes. The dominant cost is the workspace build, so each filtered run re-pays nearly the full price of an unfiltered one.

Why: The user interrupted a chain of filtered runs (2026-07-23): "it takes basically the same amount of time, we're wasting iteration time running this so often."

How to apply: Hold ALL full-suite runs until the session's work is wrapping up, then run `./scripts/sovereign-test.sh --human` ONCE (no filter, background) and read the real `cargo.exit`. Don't run the suite per-change or per-fix mid-session — the user explicitly nixed mid-session runs ("nix the test runs until we're ready to call the session quits", 2026-07-23). Reserve `--filter` for a single quick red-test iteration loop. Related: [[feedback-run-tests-at-initiative-end]].
