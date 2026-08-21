// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-dev` — the workbench: ATOS workflow + project lifecycle +
//! local code intelligence + MCP tool runner.
//!
//! ## Why this crate has a `[lib]` target (2026-08-21)
//!
//! It did not, until this commit, and that was measurable damage rather than a
//! stylistic choice. `nc-reach` scored this crate at **135 types, 76% private,
//! 0% exported** — and the 0% was structurally forced: nothing in Rust can
//! import a type out of a binary-only crate. Four sibling crates read the same
//! 0% for the same reason.
//!
//! The cost was already being paid daily. `sovereign-cli` is a thin dispatcher
//! that `exec`s into this binary for `atos` / `project` / `code` / `tools`,
//! which is why `AGENTS.md` carries the trap *"rebuild the sibling that owns
//! the verb you changed, or your change won't run"* and why the dispatcher had
//! to grow a stale-sibling warning on 2026-07-26. That process boundary exists
//! BECAUSE this crate could not be linked.
//!
//! So: the modules move here, [`main`](../main.rs) becomes a shim over
//! [`bin_main`], and `sovereign-cli` LINKS this crate to serve
//! [`InProcessCodeVerb`] arms in its own process — no exec, no sibling, no
//! staleness.
//!
//! ## The `workbench` feature, and why the dispatcher turns it off
//!
//! The workbench's dependency tree is enormous: `sovereign-mesh` (llama.cpp),
//! `sovereign-tools` (arrow + parquet + the enrichment catalog), `axum`,
//! `corpus-engine` with the tree-sitter grammars. Linking THAT into the
//! dispatcher would be the "absorb a large dependency to move a percentage"
//! failure, so every heavy dependency is optional and every module that needs
//! one is `#[cfg(feature = "workbench")]`.
//!
//! With `default-features = false` this crate is two workspace dependencies
//! wide — `corpus-engine-scip` and `sovereign-cli-shared` — which the
//! dispatcher already carries. `workbench` is a DEFAULT feature, so building
//! the binary is unchanged; only a consumer that explicitly opts out gets the
//! thin surface.
//!
//! Sibling crates carve off non-workbench concerns:
//!   - `sovereign-cli-daemon`: long-running host process + setup +
//!     service install + doctor (daemon, setup, install-service, doctor)
//!   - `sovereign-cli-llm`: bench / chat / eval / atlas / enrich / mesh
//!   - `sovereign-cli`: dispatcher + light delegators (notes, status, ...)

// ── Always available: the thin, linkable surface ────────────────────
// Nothing below may reference a `workbench`-gated dependency. That rule is
// what keeps `sovereign-cli` free of the workbench's dependency tree, and the
// build breaks loudly if it is broken.
mod converge_cmd;

// ── The workbench proper — `workbench` feature ──────────────────────
#[cfg(feature = "workbench")]
mod amend;
#[cfg(feature = "workbench")]
mod arch_report_cmd;
#[cfg(feature = "workbench")]
mod atlas_identity;
#[cfg(feature = "workbench")]
mod atos_cmd;
#[cfg(feature = "workbench")]
mod atos_plugin;
#[cfg(feature = "workbench")]
mod audit_extract;
#[cfg(feature = "workbench")]
mod audit_recover;
#[cfg(feature = "workbench")]
mod code_capability_graph;
#[cfg(feature = "workbench")]
mod code_cmd;
#[cfg(feature = "workbench")]
mod code_fieldglass;
#[cfg(feature = "workbench")]
mod code_map;
#[cfg(feature = "workbench")]
mod design_onboarding;
#[cfg(feature = "workbench")]
mod design_session;
#[cfg(feature = "workbench")]
mod doc_fetcher;
#[cfg(feature = "workbench")]
mod drift_cmd_orchestrator;
#[cfg(feature = "workbench")]
mod dry_report_cmd;
#[cfg(feature = "workbench")]
mod found;
#[cfg(feature = "workbench")]
mod honesty;
#[cfg(feature = "workbench")]
mod phases;
#[cfg(feature = "workbench")]
mod plan_composer;
#[cfg(feature = "workbench")]
mod plan_enricher;
#[cfg(feature = "workbench")]
mod project_cmd;
#[cfg(feature = "workbench")]
mod redirect_cmd;
#[cfg(feature = "workbench")]
mod suggest_seams_cmd;
#[cfg(feature = "workbench")]
mod tools_cmd;

// The project model moved to `sovereign-cli-shared` (2026-08-07) when
// `project init` shipped in the dispatcher: init writes
// `.sovereign/project.toml`, the workbench's `found` / `phase` / `audit` /
// `charter amend` read it. Re-exported at the old crate-root paths so every
// `crate::observation::…` / `crate::project_toml::…` call site is unchanged.
#[cfg(feature = "workbench")]
pub(crate) use sovereign_cli_shared::{observation, project_toml};

/// A `svrn code` subverb this crate serves as a **linked library call** rather
/// than a sibling-process `exec`.
///
/// This is the seam the `[lib]` target exists for. `sovereign-cli` has no way
/// to know which workbench verbs it may run in its own process, and until this
/// type existed the answer was "none of them" — every `svrn code …` paid a
/// 414 MB `exec` into a binary that might be stale. The enum is the ONE list of
/// in-process verbs: the dispatcher matches on it, and this crate's own `code`
/// router matches on it too, so a verb cannot be linked in one place and
/// forgotten in the other (ARCH §2 — closed sets are enums; §10.6 — one
/// decider, one name).
///
/// **Invariant for new arms:** everything an arm reaches must compile with the
/// `workbench` feature OFF. An arm that needs `sovereign-tools`,
/// `sovereign-mesh` or `corpus-engine`'s grammars belongs in the sibling
/// binary, not here — dragging those into the dispatcher is the cost this
/// split exists to refuse. `cargo build -p sovereign-cli-dev
/// --no-default-features` is the check, and it is what enforces the rule
/// rather than this paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InProcessCodeVerb {
    /// `svrn code converge <census|roles|noun|status>` — duplicated concept
    /// identity over the SCIP graph. Read-only: no daemon, no model, no build,
    /// and `corpus-engine-scip` is its whole dependency surface. It is also the
    /// verb `AGENTS.md` requires before minting any new type, so it runs more
    /// often than anything else in this crate.
    Converge,
}

impl InProcessCodeVerb {
    /// Every verb served in-process. `parse` is derived from this list rather
    /// than from a second `match`, so a variant that is not listed here is not
    /// dispatchable from EITHER router — it cannot work in one and quietly
    /// miss in the other (ARCH §10.6).
    pub const ALL: &'static [Self] = &[Self::Converge];

    /// The `code` subcommand token this verb answers to, or `None` when the
    /// subcommand still belongs to the sibling binary.
    pub fn parse(sub: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == sub)
    }

    /// The subcommand token, for tracing and help text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Converge => "converge",
        }
    }

    /// Run the verb in the calling process. `args` are the tokens AFTER the
    /// subcommand (`svrn code converge noun Foo` → `["noun", "Foo"]`).
    pub async fn run(self, args: &[String]) -> i32 {
        match self {
            Self::Converge => converge_cmd::run(args).await,
        }
    }
}

#[cfg(test)]
mod in_process_verb_tests {
    use super::InProcessCodeVerb;

    /// The token the `sovereign-cli` dispatcher matches on is the token this
    /// crate's own `code` router matches on. Round-tripping every arm of
    /// `ALL` is what keeps the two routers from drifting apart.
    #[test]
    fn every_in_process_verb_round_trips_through_its_token() {
        assert!(
            !InProcessCodeVerb::ALL.is_empty(),
            "the lib target exists to serve at least one verb"
        );
        for verb in InProcessCodeVerb::ALL {
            assert_eq!(
                InProcessCodeVerb::parse(verb.as_str()),
                Some(*verb),
                "`{}` does not parse back to its own variant",
                verb.as_str()
            );
        }
    }

    /// A `code` subcommand whose implementation is behind `workbench` must NOT
    /// be claimed here. Claiming one would send the dispatcher off to run code
    /// whose dependencies it deliberately never linked — the whole reason the
    /// thin/`workbench` split exists.
    #[test]
    fn workbench_only_subcommands_are_left_to_the_sibling() {
        for sub in [
            "index",
            "brief",
            "fieldglass",
            "arch-report",
            "dry-report",
            "suggest-seams",
            "capability-graph",
            "capability-map",
            "map",
            "redirect",
            "check-spec",
        ] {
            assert!(
                InProcessCodeVerb::parse(sub).is_none(),
                "`code {sub}` needs the workbench's dependency tree and must stay in the sibling"
            );
        }
    }
}

/// The workbench binary's entry point. `src/main.rs` is a shim over this so
/// the crate has exactly one implementation of its verb table.
#[cfg(feature = "workbench")]
pub fn bin_main() -> ! {
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
    let code = runtime.block_on(async_main());
    std::process::exit(code);
}

#[cfg(feature = "workbench")]
async fn async_main() -> i32 {
    use sovereign_cli_shared::tracing_init::init_tracing;

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
        // `project-init` is gone (2026-08-07): `svrn init` used to spawn this
        // sibling to reach `cmd_init`. `cmd_init` now lives in the dispatcher
        // itself, which calls it in-process — no spawn, and `svrn init --help`
        // no longer needs a 240 MB binary to be built.
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
    code
}
