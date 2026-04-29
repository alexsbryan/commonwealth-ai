//! `sovereign atos` — the Agent Task Orchestration System CLI.
//!
//! The CLI is deliberately thin. It:
//!   1. Owns the [`corpus_engine::FeatureStore`] and
//!      [`corpus_engine::NoteStore`] paths (the same files
//!      `sovereign project serve` uses, so artifacts are shared).
//!   2. Spawns a driver subprocess (Claude Code by default, opencode
//!      behind `--driver opencode`) with `SOVEREIGN_FEATURE_ID`
//!      exported.
//!   3. Runs the feature's `stop_condition` at end-milestone and
//!      assembles a compliance report.
//!
//! ## Module layout
//!
//! Split per ARCH_PRINCIPLES.md §3 into one file per subcommand
//! family:
//!
//! - [`args`] — flag parsing shared by every subcommand
//! - [`stores`] — `.sovereign/` store openers, orchestrator factory
//! - [`provision`] — `provision`, `archive`
//! - [`milestone`] — `start-milestone`, `end-milestone`, `next`,
//!   auto-redteam spawn, driver subprocess
//! - [`feature`] — `feature approve` (Commonwealth-native approval)
//! - [`spec`] — `spec diff`, `spec accept`
//! - [`status`] — `status`, `promote`, `report`, artifact checklist
//! - [`teardown`] — `teardown`
//! - [`doctor`] — `doctor` (health check)
//! - [`plugin`] — `install-plugin`
//! - [`ab`] — `diff`, `run-ab`, `probe-driver` (A/B driver compare)
//!
//! External callers consume a single entry point:
//! [`run_atos`]. Everything else is crate-private.

// The flat namespace (`sovereign milestone`, `sovereign drift`,
// `sovereign audit`, etc.) reaches into these submodules directly,
// so they're `pub(crate)` rather than the original module-private
// `mod`. The new top-level subcommand modules call the same
// handlers without forcing every alias path through `run_atos`'s
// string-match dispatcher.
pub(crate) mod ab;
mod args;
pub(crate) mod doctor;
pub(crate) mod feature;
pub(crate) mod milestone;
pub(crate) mod plugin;
pub(crate) mod provision;
pub(crate) mod spec;
pub(crate) mod status;
mod stores;
pub(crate) mod teardown;

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run_atos(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        print_help();
        return 1;
    };

    // `sovereign atos --version` — tiny dogfood target exercised by M1.5.
    if matches!(first.as_str(), "--version" | "-V") {
        println!("atos {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    // Most leaves below moved to the flat `sovereign <leaf>`
    // namespace. Each shim prints a one-time banner and forwards to
    // the same underlying handler the new top-level arm calls, so
    // behaviour is identical. SOVEREIGN_QUIET_DEPRECATIONS=1
    // silences the banner.
    use crate::util::deprecation::announce;
    let rest = &args[1..];
    match first.as_str() {
        "provision" => {
            // Provision is no-op'd in Phase 6 (commit = founding);
            // until then it still does the original work but
            // signals the upcoming change.
            announce(
                "sovereign atos provision",
                "sovereign init + commit .sovereign/features/<id>/spec.md",
            );
            provision::cmd_provision(rest).await
        }
        "next" => milestone::cmd_next(rest).await,
        "start-milestone" => {
            announce(
                "sovereign atos start-milestone",
                "sovereign milestone <feature-id> <N>",
            );
            milestone::cmd_start_milestone(rest).await
        }
        "end-milestone" => {
            announce(
                "sovereign atos end-milestone",
                "sovereign milestone <feature-id> <N>",
            );
            milestone::cmd_end_milestone(rest).await
        }
        "archive" => {
            announce(
                "sovereign atos archive",
                "sovereign audit <feature-id> --archive",
            );
            provision::cmd_archive(rest).await
        }
        "status" => {
            announce("sovereign atos status", "sovereign status");
            status::cmd_status(rest).await
        }
        "promote" => {
            announce("sovereign atos promote", "sovereign notes promote");
            status::cmd_promote(rest).await
        }
        "diff" => ab::cmd_diff(rest).await,
        "run-ab" => ab::cmd_run_ab(rest).await,
        "probe-driver" => {
            announce("sovereign atos probe-driver", "sovereign doctor");
            ab::cmd_probe_driver(rest).await
        }
        "report" => {
            announce("sovereign atos report", "sovereign audit <feature-id>");
            status::cmd_report(rest).await
        }
        "teardown" => {
            announce(
                "sovereign atos teardown",
                "sovereign audit <feature-id> --archive",
            );
            teardown::cmd_teardown(rest).await
        }
        "feature" => feature::cmd_feature(rest).await,
        "spec" => {
            announce("sovereign atos spec", "sovereign drift");
            spec::cmd_spec(rest).await
        }
        "doctor" => {
            announce("sovereign atos doctor", "sovereign doctor");
            doctor::cmd_doctor(rest).await
        }
        "install-plugin" => {
            announce(
                "sovereign atos install-plugin",
                "sovereign doctor --fix (lands in Phase 5)",
            );
            plugin::cmd_install_plugin(rest).await
        }
        other => {
            eprintln!("atos: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!(
        "sovereign atos — Agent Task Orchestration System\n\
         \n\
         USAGE\n    sovereign atos <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS\n\
         \x20   provision <id>        --charter <path>   (structured charter: parses ## Milestones)\n\
         \x20   provision <id>        --title <t> --charter <path> [--sovereign-md <path>] [--stop-cmd <shell>]\n\
         \x20   next [<feature-id>]   [--yes] [--driver claude|opencode]\n\
         \x20   start-milestone <id>  --brief <path> [--driver claude|opencode]\n\
         \x20   end-milestone <id>    [--ordinal N]\n\
         \x20   archive <id>          --reason <text>\n\
         \x20   status [<id>]\n\
         \x20   promote <note-id>     --to feature|global [--feature-id <id>] [--content <path>]\n\
         \x20   diff <feature-id>     [--ordinal N]\n\
         \x20   run-ab <feature-id>   --brief <path> [--drivers claude,opencode]\n\
         \x20   probe-driver          [--url http://localhost:9741/v1/chat/completions]\n\
         \x20   report <feature-id>   [--section milestone|red-team|epistemic|all] [--milestone N] [--out <path>]\n\
         \x20   teardown <feature-id> [--auto] [--dry-run]\n\
         \x20   feature approve <id>  (Commonwealth-native fallback for branches where git-committer review won't apply)\n\
         \x20   spec diff <id>        Show unified diff between the approved spec and the current spec\n\
         \x20   spec accept <id>      [--reason <text>]  Accept the current spec as the new approved content\n\
         \x20   doctor                Health check: repo, .sovereign dir, DB schemas, plugin, per-feature\n\
         \x20   install-plugin        (Re)install the opencode plugin at .opencode/plugins/sovereign-atos.ts\n\
         \n\
         AUTO RED-TEAM\n\
         \x20   Opt in from the charter preamble:\n\
         \x20       **Red team:** auto\n\
         \x20   After the last milestone passes, `end-milestone` spawns a\n\
         \x20   red-team pass automatically and writes red-team.md.\n\
         \n\
         FLAGS\n\
         \x20   --version             Print atos CLI version and exit.\n\
         \x20   --help, -h            Show this message.\n"
    );
}
