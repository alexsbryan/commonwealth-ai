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

mod ab;
mod args;
mod doctor;
mod feature;
mod milestone;
mod plugin;
mod provision;
mod spec;
mod status;
mod stores;
mod teardown;

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

    let rest = &args[1..];
    match first.as_str() {
        "provision" => provision::cmd_provision(rest).await,
        "next" => milestone::cmd_next(rest).await,
        "start-milestone" => milestone::cmd_start_milestone(rest).await,
        "end-milestone" => milestone::cmd_end_milestone(rest).await,
        "archive" => provision::cmd_archive(rest).await,
        "status" => status::cmd_status(rest).await,
        "promote" => status::cmd_promote(rest).await,
        "diff" => ab::cmd_diff(rest).await,
        "run-ab" => ab::cmd_run_ab(rest).await,
        "probe-driver" => ab::cmd_probe_driver(rest).await,
        "report" => status::cmd_report(rest).await,
        "teardown" => teardown::cmd_teardown(rest).await,
        "feature" => feature::cmd_feature(rest).await,
        "spec" => spec::cmd_spec(rest).await,
        "doctor" => doctor::cmd_doctor(rest).await,
        "install-plugin" => plugin::cmd_install_plugin(rest).await,
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
