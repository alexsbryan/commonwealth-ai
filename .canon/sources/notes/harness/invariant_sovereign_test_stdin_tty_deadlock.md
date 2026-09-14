# sovereign-test.sh must run cargo with </dev/null or a stdin-reading test deadlocks the whole workspace run silently

`scripts/sovereign-test.sh` pipes cargo's STDOUT (`| tee | adapter`) but that
does NOT redirect stdin. Run interactively, test binaries inherit the caller's
terminal as stdin. `sovereign-cli-shared::confirm` (prompts.rs) guards on
`io::stdin().is_terminal()` and takes the non-tty EOF fast-path only when stdin
is NOT a tty — but under an interactive shell it IS a tty, so `confirm` blocks
in `read_line` forever. Test `prompts::tests::confirm_returns_default_without_reading_in_non_tty`
then hangs the entire `--workspace` run indefinitely, and `--human` buffers all
output into log files so you get ZERO signal (looks like a 40-min slow build).

FIX (shipped 2026-07-21): both cargo invocations in the script now run with
`</dev/null`, forcing the non-tty path for every test. The daemon watcher never
hit this because it already runs cargo with a non-tty stdin.

Diagnosis tell: `target/sovereign-test/.runs/<pid>-*/` scratch dir never
promoted to `latest`, no `cargo.exit` file, and raw.log's last line is
`test <name> has been running for over 60 seconds`.

Workaround without rebuild: `./scripts/sovereign-test.sh --human < /dev/null`.

Related: [[reference_sovereign_test_scoping_flags]]. Build artifacts are NOT
shared with `cargo build` — different feature unification (treesitter/dev-tools)
+ cargo test compiles the cfg(test) profile, a distinct compilation unit.
