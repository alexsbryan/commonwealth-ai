// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code` dispatch for the shipped binary.
//!
//! The verb itself lives in [`sovereign_cli_shared::code_index`] — the
//! workbench serves the same `code index` and the two binaries carried
//! byte-drifting copies of it from the 2026-08-06 port until 2026-08-20. What
//! stays here is only what is TRUE OF THIS BINARY: which subcommands it owns,
//! and what to say about the ones it does not.

pub use sovereign_cli_shared::code_index::rebuild_code_corpus;

/// Subcommands of `svrn code` this binary serves. Everything else under
/// `code` is workbench-only; see `refuse_workbench_subcommand`.
const IN_PROCESS: &[&str] = &["index"];

/// Returns `Some(exit_code)` when this module owns the subcommand, `None` when
/// the caller should fall through to the `sovereign-cli-dev` sibling.
pub async fn try_run(args: &[String]) -> Option<i32> {
    let sub = args.first()?;
    if !IN_PROCESS.contains(&sub.as_str()) {
        return None;
    }
    Some(sovereign_cli_shared::code_index::cmd_index(&args[1..]).await)
}

/// Refuse a `code` subcommand that still lives in the workbench, naming what
/// this build can do instead of pointing at a `cargo build` the user has no
/// checkout for. Same contract as `project_registry::refuse_workbench_subcommand`.
pub fn refuse_workbench_subcommand(sub: Option<&str>) -> i32 {
    match sub {
        Some(s) => eprintln!("svrn code {s}: not available in this build."),
        None => eprintln!("svrn code: missing subcommand."),
    }
    eprintln!();
    eprintln!("  Available here:");
    eprintln!("    svrn code index <path> [--corpus-id <id>] [--full|--incremental]");
    eprintln!();
    eprintln!("  The analysis subcommands (brief, fieldglass, arch-report, dry-report,");
    eprintln!("  suggest-seams, check-spec) are developer tooling and ship separately.");
    2
}
