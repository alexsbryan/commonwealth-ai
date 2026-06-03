//! `sovereign milestone <feature-id> <N> [--project]` — close a milestone.
//!
//! Demo shape:
//!
//! ```text
//! sovereign milestone p0-payments 1            # feature milestone 1
//! sovereign milestone --project 2              # project phase 2
//! ```
//!
//! Merges the older milestone surfaces:
//!
//! - `sovereign atos start-milestone` + `end-milestone` → unified
//!   here. The new flow runs the stop condition once; explicit
//!   start/end is no longer required for the common case (the demo
//!   never invokes `start-milestone` separately).
//! - `sovereign project phase pass <N>`                → `--project`.
//!
//! Phase 1 (this file): a thin dispatcher. Feature path forwards to
//! [`crate::atos_cmd::milestone::cmd_end_milestone`] with `--ordinal
//! N` translated from the second positional. Project path forwards to
//! [`crate::project_cmd::cmd_phase_pass`].

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    let project_mode = args.iter().any(|a| a == "--project");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();

    if project_mode {
        // `sovereign milestone --project <N>` — single positional N.
        let Some(n) = positional.first().and_then(|s| s.parse::<u32>().ok()) else {
            eprintln!(
                "  sovereign milestone --project <N> requires N to be an integer.\n\
                 \n\
                 example: sovereign milestone --project 2"
            );
            return 2;
        };
        let n_str = n.to_string();
        return crate::dev_bin::exec("project-phase-pass", &[n_str]);
    }

    // Feature path: `sovereign milestone <feature-id> <N>`.
    if positional.len() < 2 {
        eprintln!(
            "  sovereign milestone <feature-id> <N> requires both args.\n\
             \n\
             USAGE\n  \
               sovereign milestone <feature-id> <N>          Close milestone N for the feature\n  \
               sovereign milestone --project <N>             Close project-level phase N"
        );
        return 2;
    }
    let feature_id = positional[0].clone();
    let Ok(ordinal) = positional[1].parse::<i64>() else {
        eprintln!(
            "  sovereign milestone: <N> must be an integer (got {:?})",
            positional[1]
        );
        return 2;
    };

    // Forward as: <feature-id> --ordinal <N>. Pass through any other
    // flags the user supplied (e.g. --driver) so power users keep
    // their existing knobs.
    let mut forwarded: Vec<String> = vec![feature_id, "--ordinal".to_string(), ordinal.to_string()];
    for a in args.iter() {
        if a.starts_with('-') && a != "--project" {
            forwarded.push(a.clone());
        }
    }

    crate::dev_bin::exec("atos-milestone-end", &forwarded)
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign milestone",
    summary: "Close a feature milestone (runs its stop condition; writes the report).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign milestone <feature-id> <N>          Close milestone N for the feature\n\
             sovereign milestone --project <N>             Close project-level phase N",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces the older `sovereign atos start-milestone` / `end-milestone` and \
             `sovereign project phase pass` triple. Old names still work and forward here.",
        ),
    ],
};
