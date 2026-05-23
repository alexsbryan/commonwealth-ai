//! `sovereign drift [<feature-id>] [accept --reason "..."]` — spec drift.
//!
//! Merges the spec-drift surfaces:
//!
//! - `sovereign atos spec diff <id>`               → `sovereign drift <id>`
//! - `sovereign atos spec accept <id> --reason X`  → `sovereign drift accept <id> --reason X`
//!
//! Phase 1 (this file): a thin dispatcher over the existing
//! `atos_cmd::spec` handlers. Phase 5 will extend the no-arg form
//! (`sovereign drift`) to walk every `.sovereign/features/*/` and
//! summarise drift across all of them — for now, a feature id is
//! required so the alias path is identity-equivalent to today's
//! `atos spec diff`.

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // `sovereign drift accept <id> [--reason ...]` → spec accept.
    // `sovereign drift detect --code <path> --narrative <doc>...` →
    //   narrative-vs-code drift orchestrator (this session's work).
    // Anything else routes to spec diff.
    match args.first().map(String::as_str) {
        Some("accept") => crate::dev_bin::exec("atos-spec-accept", &args[1..]),
        Some("detect") => crate::dev_bin::exec("drift-detect", &args[1..]),
        Some(_) => crate::dev_bin::exec("atos-spec-diff", args),
        None => {
            // Phase 1: no-feature form is unimplemented — the
            // multi-feature walker lands in Phase 5 alongside the
            // `tools/list_changed` watcher integration. Until then,
            // print a usage hint rather than dispatching with no
            // target.
            eprintln!(
                "  sovereign drift requires a feature id (Phase 1).\n\
                 \n\
                 USAGE\n  \
                   sovereign drift <feature-id>                    Show drift for one feature\n  \
                   sovereign drift accept <feature-id> --reason X  Accept current spec\n\
                 \n\
                 The no-arg multi-feature summary lands in Phase 5."
            );
            2
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign drift",
    summary: "Inspect and accept spec drift between approved and on-disk spec.md.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign drift <feature-id>                       Diff approved vs. on-disk\n\
             sovereign drift accept <feature-id> --reason X     Accept current spec",
        ),
        crate::util::help::HelpSection::Notes(
            "Replaces `sovereign atos spec diff` and `sovereign atos spec accept`. Old \
             names still work and forward here.",
        ),
    ],
};
