// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn drift detect --code <path> --narrative <doc>...` —
//! narrative-vs-code drift detection.

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // `svrn drift detect --code <path> --narrative <doc>...` →
    //   narrative-vs-code drift orchestrator.
    match args.first().map(String::as_str) {
        Some("detect") => crate::dev_bin::exec("drift-detect", &args[1..]),
        _ => {
            eprintln!(
                "  svrn drift requires a subcommand.\n\
                 \n\
                 USAGE\n  \
                   svrn drift detect --code <path> --narrative <doc>...   Narrative-vs-code drift report"
            );
            2
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn drift",
    summary: "Narrative-vs-code drift detection.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn drift detect --code <path> --narrative <doc>...   Narrative-vs-code drift report",
        ),
        crate::util::help::HelpSection::Notes(
            "`detect` runs the narrative-vs-code pipeline (code index → \
             structural atlas → enrich → drift report); read the results \
             cheaply afterwards via the `drift_posture` / `drift_findings` \
             tools.",
        ),
    ],
};
