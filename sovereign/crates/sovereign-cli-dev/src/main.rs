// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-dev` — workbench binary: ATOS workflow + project
//! lifecycle + local code intelligence + MCP tool runner. Parent
//! `sovereign` shim execs into this binary for `atos`, `project`,
//! `code`, `tools`, plus the hidden `atos-*` / `project-*` / `audit-*`
//! / `drift-*` arms used by sovereign-cli's delegator stubs.
//!
//! Sibling crates carve off non-workbench concerns:
//!   - `sovereign-cli-daemon`: long-running host process + setup +
//!     service install + doctor (daemon, setup, install-service, doctor)
//!   - `sovereign-cli-llm`: bench / chat / eval / atlas / enrich / mesh
//!   - `sovereign-cli`: dispatcher + light delegators (notes, status, ...)
//!
//! NOTE 2026-05-22: the Cargo package is still named `sovereign-cli-atos`
//! for compatibility with the workspace-rename step that lands in
//! parallel — rename in the next commit so the bin's identity matches
//! its actual scope ("dev" / "workbench", not ATOS-specifically).

mod amend;
mod arch_report_cmd;
mod atos_cmd;
mod atos_plugin;
mod audit_extract;
mod audit_recover;
mod code_capability_graph;
mod code_fieldglass;
mod code_cmd;
mod code_index_incremental;
mod code_map;
mod design_onboarding;
mod design_session;
mod doc_fetcher;
mod drift_cmd_orchestrator;
mod dry_report_cmd;
mod found;
mod honesty;
mod observation;
mod phases;
mod plan_composer;
mod plan_enricher;
mod project_cmd;
mod project_toml;
mod suggest_seams_cmd;
mod tools_cmd;

use sovereign_cli_shared::tracing_init::init_tracing;

fn main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }

    // Rebrand back-compat (see sovereign_core::rebrand): idempotent, non-destructive.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-dev-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = raw_args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: &[String] = if raw_args.is_empty() {
        &[]
    } else {
        &raw_args[1..]
    };

    let code: i32 = match cmd {
        // ── Top-level verbs ─────────────────────────────────────────
        "atos" => atos_cmd::run_atos(rest).await,
        "project" => project_cmd::run_project(rest).await,
        "code" => code_cmd::run_code(rest).await,
        "tools" => tools_cmd::run_tools(rest).await,

        // ── Hidden arms invoked by sovereign-cli delegators ────────
        // ATOS sub-handlers (from notes/audit/drift/milestone stubs).
        "atos-status-promote" => atos_cmd::status::cmd_promote(rest).await,
        "atos-status-report" => atos_cmd::status::cmd_report(rest).await,
        "atos-teardown" => atos_cmd::teardown::cmd_teardown(rest).await,
        "atos-spec-accept" => atos_cmd::spec::cmd_spec_accept(rest).await,
        "atos-spec-diff" => atos_cmd::spec::cmd_spec_diff(rest).await,
        "atos-milestone-end" => atos_cmd::milestone::cmd_end_milestone(rest).await,

        // project_cmd sub-handlers (from status/charter/etc stubs).
        "project-status" => project_cmd::cmd_status(rest).await,
        "project-charter" => project_cmd::cmd_charter(rest).await,
        "project-amend" => project_cmd::cmd_amend(rest).await,
        "project-design" => project_cmd::cmd_design(rest).await,
        "project-plan" => project_cmd::cmd_plan(rest).await,
        "project-init" => project_cmd::cmd_init(rest).await,
        "project-refresh" => project_cmd::cmd_refresh(rest).await,
        "project-phase-pass" => project_cmd::cmd_phase_pass(rest).await,
        "project-serve" => project_cmd::cmd_serve(rest).await,
        "project-audit" => project_cmd::cmd_audit(rest).await,

        // serve_cmd uses this to detect a running daemon without
        // executing the body — bool-return becomes 0 = running, 1 = no.
        "project-daemon-is-running" => {
            if project_cmd::daemon_is_running().await {
                0
            } else {
                1
            }
        }

        // drift_cmd_orchestrator + audit_recover handlers.
        "drift-detect" => drift_cmd_orchestrator::cmd_detect(rest).await,
        "audit-recover" => audit_recover::cmd_audit_recover().await,

        "" => {
            eprintln!("sovereign-cli-dev: usage: sovereign-cli-dev <subcommand> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-dev: unknown subcommand '{other}'");
            2
        }
    };

    let _ = &init_tracing;
    std::process::exit(code);
}
