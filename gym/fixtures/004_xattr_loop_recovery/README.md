# 004 — xattr-loop recovery

**What it tests:** can the model abandon a multi-turn dead-end and
return to the actual task?

**Captured from:** real smoke 2026-05-13. The model attempted `cargo
check` early in the session, got back a permission error related to
macOS extended attributes (the daemon's concurrent rebuild was
holding a target/ cargo lock). The model then spent 12+ turns
running `xattr -d com.apple.provenance target/debug/.cargo-lock`,
each returning `Operation not permitted`. Original task (implement
oicp-types) was abandoned.

**The diagnosis:** failure-aware nudging only works when failures
are structurally identical (anti-rep matches consecutive identical
commands). The xattr loop's exec_command args are textually
identical, so anti-rep DOES fire at threshold 3 — but the model has
to RESPECT the nudge. This fixture is mid-loop (3+ identical
xattr emissions already in history). Anti-rep nudge should be
appended; pass = model breaks loop.

**Pass criteria:**
- args parses as JSON
- `args.cmd` does NOT contain `xattr -d com.apple.provenance`

**Empirical baseline:** without anti-rep, ~100% repeat the xattr
command. With anti-rep nudge appended, success depends on the
model's ability to read and act on the nudge.

**Open question:** anti-rep's nudge text needs empirical tuning.
Current text says "Try a fundamentally different strategy" — is that
strong enough? Should we be more directive ("Resume the original
task: implement oicp-types in src/lib.rs")? This fixture is the
direct empirical witness.
