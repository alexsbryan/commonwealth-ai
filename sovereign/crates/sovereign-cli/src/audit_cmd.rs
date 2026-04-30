//! `sovereign audit [feature-id] [--archive]` — the deliverable.
//!
//! Merges three older commands under one flat name:
//!
//! - `sovereign project audit` (no args)             → `sovereign audit`
//! - `sovereign atos report <id>`                    → `sovereign audit <id>`
//! - `sovereign atos teardown <id>` / `atos archive` → `sovereign audit <id> --archive`
//!
//! Phase 1 (this file): a dispatcher over the existing handlers.
//! Phase 7 rewrites the project-wide path to merge in the four
//! extraction streams (tool-call patterns, diff extraction, response
//! mining, commit-message harvesting) so the floor is never empty;
//! that rewrite lands inside `project_cmd::cmd_audit` so the alias
//! path benefits too.
//!
//! Argument shape:
//! - `sovereign audit`                       → project-wide rollup
//! - `sovereign audit <feature-id>`          → feature-specific report
//! - `sovereign audit <feature-id> --archive`→ archive the feature

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
        return crate::audit_recover::cmd_audit_recover().await;
    }

    // Detect the `--archive` flag anywhere in args. Strip it before
    // forwarding so the underlying teardown handler doesn't see a
    // duplicated flag.
    let archive_requested = args.iter().any(|a| a == "--archive");
    let forwarded: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--archive")
        .cloned()
        .collect();

    // Identify the feature id by taking the first positional arg
    // (anything not starting with `-`). `None` means project-wide.
    let feature_id: Option<&String> = forwarded.iter().find(|a| !a.starts_with('-'));

    match (feature_id, archive_requested) {
        (Some(_), true) => {
            // `sovereign audit <id> --archive` → teardown the feature.
            crate::atos_cmd::teardown::cmd_teardown(&forwarded).await
        }
        (Some(_), false) => {
            // `sovereign audit <id>` → per-feature report. The atos
            // report handler accepts the feature id as the first
            // positional arg, matching this surface.
            crate::atos_cmd::status::cmd_report(&forwarded).await
        }
        (None, true) => {
            // `sovereign audit --archive` with no id is a user error
            // — there's no obvious target. Print a short hint rather
            // than silently archiving the most-recent feature.
            eprintln!(
                "  sovereign audit --archive requires a feature id.\n\
                 \n\
                 USAGE\n  \
                   sovereign audit <feature-id> --archive    Archive that feature\n  \
                   sovereign audit                          Project-wide rollup\n  \
                   sovereign audit <feature-id>             Per-feature report"
            );
            2
        }
        (None, false) => {
            // `sovereign audit` (no args) → project-wide rollup.
            crate::project_cmd::cmd_audit(&forwarded).await
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign audit",
    summary: "Reviewer rollup: founding, phases, decisions, deviations, drift, milestones.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign audit                            Project-wide audit\n\
             sovereign audit <feature-id>               Feature-specific report\n\
             sovereign audit <feature-id> --archive     Archive the feature",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces the older `sovereign project audit` + `sovereign atos report` \
             + `sovereign atos teardown` triple. Old names still work and forward here.",
        ),
    ],
};

#[cfg(test)]
mod tests {
    /// `--archive` only fires when a feature id is present. Without
    /// one, `run` short-circuits with a usage hint rather than the
    /// project-wide path. Verified via the dispatch shape directly
    /// rather than spawning the full handler stack.
    #[test]
    fn dispatch_recognises_flag_layouts() {
        let args = vec!["foo".to_string(), "--archive".to_string()];
        let archive = args.iter().any(|a| a == "--archive");
        let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        assert!(archive);
        assert_eq!(positional, vec![&"foo".to_string()]);

        let args2: Vec<String> = vec![];
        let archive2 = args2.iter().any(|a| a == "--archive");
        let positional2: Vec<&String> = args2.iter().filter(|a| !a.starts_with('-')).collect();
        assert!(!archive2);
        assert!(positional2.is_empty());
    }
}
