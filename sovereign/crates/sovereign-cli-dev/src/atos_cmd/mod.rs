// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atos` — the Agent Task Orchestration System CLI.
//!
//! The CLI is deliberately thin. It:
//!   1. Owns the [`corpus_engine_atos::FeatureStore`] and
//!      [`corpus_engine_notes::NoteStore`] paths (the same files
//!      `svrn project serve` uses, so artifacts are shared).
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

// The flat namespace (`svrn milestone`, `svrn drift`,
// `svrn audit`, etc.) reaches into these submodules directly,
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
pub(crate) mod replay;
pub(crate) mod run;
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

    // `svrn atos --version` — tiny dogfood target exercised by M1.5.
    if matches!(first.as_str(), "--version" | "-V") {
        println!("atos {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    // Most leaves below moved to the flat `svrn <leaf>`
    // namespace. Each shim prints a one-time banner and forwards to
    // the same underlying handler the new top-level arm calls, so
    // behaviour is identical. SOVEREIGN_QUIET_DEPRECATIONS=1
    // silences the banner.
    use sovereign_cli_shared::deprecation::announce;
    let rest = &args[1..];
    match first.as_str() {
        "provision" => {
            // Provision is no-op'd in Phase 6 (commit = founding);
            // until then it still does the original work but
            // signals the upcoming change.
            announce(
                "svrn atos provision",
                "svrn init + commit .sovereign/features/<id>/spec.md",
            );
            provision::cmd_provision(rest).await
        }
        "next" => milestone::cmd_next(rest).await,
        "run" => run::cmd_run(rest).await,
        "replay" => replay::cmd_replay(rest).await,
        "start-milestone" => {
            announce(
                "svrn atos start-milestone",
                "svrn milestone <feature-id> <N>",
            );
            milestone::cmd_start_milestone(rest).await
        }
        "end-milestone" => {
            announce("svrn atos end-milestone", "svrn milestone <feature-id> <N>");
            milestone::cmd_end_milestone(rest).await
        }
        "archive" => {
            announce("svrn atos archive", "svrn audit <feature-id> --archive");
            provision::cmd_archive(rest).await
        }
        "status" => {
            announce("svrn atos status", "svrn status");
            status::cmd_status(rest).await
        }
        "promote" => {
            announce("svrn atos promote", "svrn notes promote");
            status::cmd_promote(rest).await
        }
        "diff" => ab::cmd_diff(rest).await,
        "run-ab" => ab::cmd_run_ab(rest).await,
        "probe-driver" => {
            announce("svrn atos probe-driver", "svrn doctor");
            ab::cmd_probe_driver(rest).await
        }
        "report" => {
            announce("svrn atos report", "svrn audit <feature-id>");
            status::cmd_report(rest).await
        }
        "teardown" => {
            announce("svrn atos teardown", "svrn audit <feature-id> --archive");
            teardown::cmd_teardown(rest).await
        }
        "feature" => feature::cmd_feature(rest).await,
        "spec" => {
            announce("svrn atos spec", "svrn drift");
            spec::cmd_spec(rest).await
        }
        "doctor" => {
            announce("svrn atos doctor", "svrn doctor");
            doctor::cmd_doctor(rest).await
        }
        "install-plugin" => {
            announce(
                "svrn atos install-plugin",
                "svrn doctor --fix (lands in Phase 5)",
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
        "svrn atos — Agent Task Orchestration System\n\
         \n\
         USAGE\n    sovereign atos <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS\n\
         \x20   provision <id>        --charter <path>   (structured charter: parses ## Milestones)\n\
         \x20   provision <id>        --title <t> --charter <path> [--sovereign-md <path>] [--stop-cmd <shell>]\n\
         \x20   next [<feature-id>]   [--yes] [--driver claude|opencode]\n\
         \x20   run                   --workdir <path> [--design <p>] [--charter <p>] [--plan <p>]\n\
         \x20                         [--feature-id <id>] [--driver opencode|claude]\n\
         \x20                         [--max-iters N] [--reviewer-model <id>] [--dry-run]\n\
         \x20                         Ralph-wiggum-style loop: spawn driver, wait for DONE.md,\n\
         \x20                         have a reviewer judge it against the charter, repeat.\n\
         \x20                         See sovereign/docs/ATOS_RUNNER.md.\n\
         \x20   replay                --commit <sha> --workdir <repo> [--driver opencode|claude]\n\
         \x20                         Reconstruct a historical commit as a Runner task. Synthesizes\n\
         \x20                         DESIGN.md + CHARTER.md from the commit's diff via the Fast\n\
         \x20                         slot, then delegates to `atos run`. See ATOS_RUNNER.md.\n\
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
