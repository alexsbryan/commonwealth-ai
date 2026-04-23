//! `sovereign enrich ...` — admin harness for the v2 enrichment pipeline.
//!
//! This module is explicitly a **temporary dev tool**: it exists to
//! iterate on the v2 pipeline + exemplar banks while v1 is still
//! serving production enrichment via `FieldModelEngine`. When v2 is
//! promoted to production the CLI surface here either retires or
//! demotes to a diagnostic helper (§12 Architecture Roadmap).
//!
//! Landing 2 ships:
//!   - `init`      — scaffold + pin config
//!   - `extract`   — phase 1 (per-chapter questions) on subset or full
//!   - `show`      — formatted view of cached phase 1 output
//!   - `exemplars` — bank counts + lint
//!   - `status`    — per-phase fresh/stale/never-run table
//!
//! Subsequent landings add the remaining phases (cluster-questions,
//! name-concerns, cluster-chunks, extract-positions, detect-tensions,
//! detect-gaps, cascade), the validation battery (query, validate),
//! and the dev-UX helpers (promote, diff).

pub mod cascade;
pub mod config;
pub mod corpus_io;
pub mod diff;
pub mod exemplars;
pub mod extract;
pub mod inference_client;
pub mod init;
pub mod paths;
pub mod phase_cmd;
pub mod promote;
pub mod query;
pub mod reset;
pub mod show;
pub mod source_loader;
pub mod status;
pub mod validate;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich",
    summary: "Admin harness for iterating on the v2 enrichment pipeline.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich <subcommand> [args]",
        ),
        HelpSection::Subcommands(&[
            ("init", "Scaffold a new corpus's enrichment state."),
            ("extract", "Phase 1: per-chapter questions (subset or full)."),
            ("cluster-questions", "Phase 2: cluster questions by embedding."),
            ("name-concerns", "Phase 3: name the canonical concern per cluster."),
            ("cluster-chunks", "Phase 4: cluster paragraph chunks by embedding."),
            ("extract-positions", "Phase 5: grounded position extraction."),
            ("detect-tensions", "Phase 6: pairwise tension detection."),
            ("detect-gaps", "Phase 7: gap identification."),
            ("cascade", "Rerun from a phase + every downstream dependent."),
            ("query", "Traverse the atlas with a one-off query."),
            ("validate", "Run a QueryBattery against the atlas; print score table."),
            ("promote", "Lift a run finding into the per-phase exemplar bank."),
            ("diff", "Side-by-side compare two phase 1 run outputs."),
            ("reset", "Clear phase caches + runs (full or from a phase onward)."),
            ("show", "Inspect cached phase output."),
            ("exemplars", "Report exemplar-bank counts + lint findings."),
            ("status", "Per-phase cache freshness table."),
        ]),
        HelpSection::Notes(
            "This is a provisional admin surface that retires once v2 enrichment is promoted \
             to production. See `sovereign/docs/...` for the broader plan.",
        ),
    ],
};

pub async fn run_enrich(args: &[String]) -> i32 {
    if args.is_empty() || help::wants_help(args) {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    let (cmd, rest) = args.split_first().unwrap();
    match cmd.as_str() {
        "init" => init::cmd_init(rest).await,
        "extract" => extract::cmd_extract(rest).await,
        "cluster-questions" => phase_cmd::run_phase(phase_cmd::PhaseOp::ClusterQuestions, rest).await,
        "name-concerns" => phase_cmd::run_phase(phase_cmd::PhaseOp::NameConcerns, rest).await,
        "cluster-chunks" => phase_cmd::run_phase(phase_cmd::PhaseOp::ClusterChunks, rest).await,
        "extract-positions" => phase_cmd::run_phase(phase_cmd::PhaseOp::ExtractPositions, rest).await,
        "detect-tensions" => phase_cmd::run_phase(phase_cmd::PhaseOp::DetectTensions, rest).await,
        "detect-gaps" => phase_cmd::run_phase(phase_cmd::PhaseOp::DetectGaps, rest).await,
        "cascade" => cascade::cmd_cascade(rest).await,
        "query" => query::cmd_query(rest).await,
        "validate" => validate::cmd_validate(rest).await,
        "promote" => promote::cmd_promote(rest).await,
        "diff" => diff::cmd_diff(rest).await,
        "reset" => reset::cmd_reset(rest).await,
        "show" => show::cmd_show(rest).await,
        "exemplars" => exemplars::cmd_exemplars(rest).await,
        "status" => status::cmd_status(rest).await,
        other => {
            eprintln!("error: unknown subcommand '{other}'");
            eprintln!();
            help::print(&HELP);
            2
        }
    }
}

#[cfg(test)]
mod integration_tests;

/// Shared test helpers across `enrich_cmd` test modules.
///
/// `std::env::set_var("HOME", …)` is process-wide state; tests that
/// scope `HOME` to a tempdir must acquire this lock before doing so
/// to avoid racing each other.
#[cfg(test)]
pub(super) mod test_env {
    use std::sync::{Mutex, MutexGuard};

    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Handle holding both the tempdir and the `HOME` lock. Drop to
    /// release.
    pub struct HomeGuard {
        dir: tempfile::TempDir,
        _guard: MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        pub fn path(&self) -> &std::path::Path {
            self.dir.path()
        }
    }

    /// Acquire the `HOME` lock and point `HOME` at a fresh tempdir.
    pub fn scoped_home() -> HomeGuard {
        let guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        HomeGuard { dir, _guard: guard }
    }
}
