// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn audit [--recover]` — the deliverable.
//!
//! Merges two older commands under one flat name:
//!
//! - `svrn project audit` (no args) → `svrn audit`
//! - the old recovery pass          → `svrn audit --recover`
//!
//! Argument shape:
//! - `svrn audit`           → project-wide rollup
//! - `svrn audit --recover` → re-run the ToolPatternMatcher over
//!   tool_call_log sessions with no extraction-source notes yet

pub async fn run(args: &[String]) -> i32 {
    // Help passes straight through — each underlying handler owns
    // its own help text.
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // Phase 7.3 `--recover`: walk tool_call_log for sessions with
    // no extraction-source notes yet and re-run the
    // ToolPatternMatcher idempotently. Catches sessions that
    // SIGKILL'd before the in-process pattern matcher's
    // tokio::spawn finished writing.
    if args.iter().any(|a| a == "--recover") {
        return crate::dev_bin::exec("audit-recover", &[]);
    }

    // `svrn audit` (no args) → project-wide rollup.
    crate::dev_bin::exec("project-audit", args)
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn audit",
    summary: "Reviewer rollup: founding, phases, decisions, deviations, drift, milestones.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn audit                            Project-wide audit\n\
             svrn audit --recover                  Re-run tool-call pattern extraction",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces the older `svrn project audit`. The old name still works and \
             forwards here.",
        ),
    ],
};
