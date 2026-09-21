// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn milestone --project <N>` — close a project phase.
//!
//! Demo shape:
//!
//! ```text
//! sovereign milestone --project 2              # project phase 2
//! ```
//!
//! Merges the older milestone surfaces:
//!
//! - `svrn project phase pass <N>` → `--project`.
//!
//! Thin dispatcher: forwards to `project_cmd::cmd_phase_pass` in the
//! `sovereign-cli-dev` sibling.

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    let project_mode = args.iter().any(|a| a == "--project");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();

    if !project_mode {
        eprintln!(
            "  sovereign milestone requires --project.\n\
             \n\
             USAGE\n  \
               sovereign milestone --project <N>        Close project-level phase N"
        );
        return 2;
    }

    // `svrn milestone --project <N>` — single positional N.
    let Some(n) = positional.first().and_then(|s| s.parse::<u32>().ok()) else {
        eprintln!(
            "  sovereign milestone --project <N> requires N to be an integer.\n\
             \n\
             example: sovereign milestone --project 2"
        );
        return 2;
    };
    let n_str = n.to_string();
    crate::dev_bin::exec("project-phase-pass", &[n_str])
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn milestone",
    summary: "Close a project-level phase (runs its stop condition; writes the report).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn milestone --project <N>             Close project-level phase N",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces the older `svrn project phase pass`. The old name still \
             works and forwards here.",
        ),
    ],
};
