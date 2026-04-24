//! `sovereign enrich ...` — user-facing driver for the v2 atlas
//! enrichment pipeline.
//!
//! The primary flow is `build` — one command that runs every atlas
//! phase in order against a corpus. The individual phase verbs
//! (`seed`, `extract`, `cluster`, `name`, `resolve`, `tensions`,
//! `gaps`, `configure`) stay available for debugging, partial
//! re-runs, and iterating on a single phase's prompt.
//!
//! Query + audit commands (`query`, `report`, `review`, `bridge`)
//! read the resolved atlas — no LLM in the hot path for `query`
//! or `report` so they're fast enough to run in a tight edit
//! loop.

pub mod atlas_configuration;
pub mod atlas_cross_corpus;
pub mod atlas_gaps;
pub mod atlas_phase_cmd;
pub mod atlas_query;
pub mod atlas_resolve;
pub mod atlas_tensions;
pub mod build;
pub mod cascade;
pub mod config;
pub mod corpus_io;
pub mod diff;
pub mod errors;
pub mod exemplars;
pub mod extract;
pub mod inference_client;
pub mod init;
pub mod paths;
pub mod phase_cmd;
pub mod promote;
pub mod query;
pub mod reset;
pub mod schema_review;
pub mod seed_cmd;
pub mod sep_ingest;
pub mod show;
pub mod source_loader;
pub mod status;
pub mod validate;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich",
    summary: "Build, query, and audit v2 atlas enrichments of a corpus.",
    sections: &[
        HelpSection::Usage("sovereign enrich <subcommand> [args]"),

        // Primary flow — the commands a user reaches for first.
        HelpSection::SubcommandsTitled(
            "Primary flow",
            &[
                ("init", "Scaffold a new corpus's enrichment state."),
                ("build", "One-shot: run every atlas phase for a corpus (seed → extract → cluster → name → resolve → tensions → gaps → configure → report)."),
                ("query", "Classify + traverse a natural-language question against the resolved atlas; print an assembled brief."),
                ("report", "Print the §12 schema validation report for one corpus."),
                ("review", "Compare N corpora; flag gaps present in ≥ 2 as schema-revision candidates."),
                ("bridge", "Detect Grounding edges between two resolved atlases; --explain dumps the match trace for one edge."),
            ],
        ),

        // Corpus-specific ingest helpers — scaffold a new
        // enrichment corpus from a domain-specific source (SEP
        // parquet today; add one of these per structured source
        // you want to wire end-to-end).
        HelpSection::SubcommandsTitled(
            "Ingest helpers",
            &[
                (
                    "sep-ingest",
                    "Scaffold `sep-<slug>` from one SEP article in the cached parquet.",
                ),
            ],
        ),

        // Individual phases — for debugging, partial re-runs, and
        // iterating on a single prompt.
        HelpSection::SubcommandsTitled(
            "Individual phases",
            &[
                ("seed", "Stage 1a: extract the canonical seed entity list from the first section."),
                ("extract", "Phase 1: per-section atlas extraction (subset or full)."),
                ("cluster", "Phase 2: cluster the Phase 1 sketches by facet."),
                ("name", "Phase 3: name each facet cluster."),
                ("resolve", "Phase 3a/3b: resolve atoms + edges + trajectories from the sketches."),
                ("tensions", "Phase 6 (deterministic): select tension candidates from the resolved atlas."),
                ("gaps", "Phase 7 (deterministic): detect structural gaps (missing triggers, ungrounded claims, open questions)."),
                ("configure", "Phase 8 (LLM, opt-in): 0-3 interpretive configurations over the atlas."),
            ],
        ),

        // Utilities — operator inspection + cleanup.
        HelpSection::SubcommandsTitled(
            "Utilities",
            &[
                ("status", "Per-phase cache freshness table."),
                ("show", "Formatted view of a cached phase output."),
                (
                    "errors",
                    "Aggregate structured failures across every phase. Groups by kind, \
                     prints remediation + retry command per group.",
                ),
                ("exemplars", "Report per-phase exemplar-bank counts + lint findings."),
                ("reset", "Clear phase caches + runs (full or from a phase onward)."),
            ],
        ),

        HelpSection::Notes(
            "Requires the Commonwealth daemon at localhost:9741 for LLM phases (seed, \
             extract, name, configure). Pure-Rust phases (cluster, resolve, tensions, \
             gaps, query, report, review, bridge) run offline once the atlas is \
             resolved.",
        ),
    ],
};

pub async fn run_enrich(args: &[String]) -> i32 {
    // Top-level help fires when (a) no args, or (b) the first arg
    // is itself a help token. Subcommand-level `--help` (e.g.
    // `enrich build --help`) passes through to the subcommand's
    // own help so each verb can document its flags + examples.
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    let (cmd, rest) = args.split_first().unwrap();
    match cmd.as_str() {
        // ── Primary flow ──────────────────────────────────────
        "init" => init::cmd_init(rest).await,
        "build" => build::cmd_build(rest).await,
        "sep-ingest" => sep_ingest::cmd_sep_ingest(rest).await,
        "query" | "atlas-query" => atlas_query::cmd_atlas_query(rest).await,
        "report" | "schema-report" => schema_review::cmd_schema_report(rest).await,
        "review" | "schema-review" => schema_review::cmd_schema_review(rest).await,
        "bridge" | "atlas-cross-corpus" => {
            atlas_cross_corpus::cmd_atlas_cross_corpus(rest).await
        }

        // ── Individual phases ─────────────────────────────────
        "seed" => seed_cmd::cmd_seed(rest).await,
        "extract" => extract::cmd_extract(rest).await,
        "cluster" | "cluster-atlas" => atlas_phase_cmd::cmd_cluster_atlas(rest).await,
        "name" | "name-atlas-clusters" => {
            atlas_phase_cmd::cmd_name_atlas_clusters(rest).await
        }
        "resolve" | "atlas-resolve" => atlas_resolve::cmd_atlas_resolve(rest).await,
        "tensions" | "atlas-tensions" => atlas_tensions::cmd_atlas_tensions(rest).await,
        "gaps" | "atlas-gaps" => atlas_gaps::cmd_atlas_gaps(rest).await,
        "configure" | "atlas-configuration" => {
            atlas_configuration::cmd_atlas_configuration(rest).await
        }

        // ── Utilities ─────────────────────────────────────────
        "status" => status::cmd_status(rest).await,
        "show" => show::cmd_show(rest).await,
        "errors" => errors::cmd_errors(rest).await,
        "exemplars" => exemplars::cmd_exemplars(rest).await,
        "reset" => reset::cmd_reset(rest).await,

        // ── Legacy v1 surface ── hidden from default help;
        // reachable by exact name for operators mid-flight on the
        // v1 questions/concerns/positions path. Retire once no
        // active corpus is on v1.
        "cluster-questions" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::ClusterQuestions, rest).await
        }
        "name-concerns" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::NameConcerns, rest).await
        }
        "cluster-chunks" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::ClusterChunks, rest).await
        }
        "extract-positions" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::ExtractPositions, rest).await
        }
        "detect-tensions" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::DetectTensions, rest).await
        }
        "detect-gaps" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::DetectGaps, rest).await
        }
        "cascade" => cascade::cmd_cascade(rest).await,
        "validate" => validate::cmd_validate(rest).await,
        "promote" => promote::cmd_promote(rest).await,
        "diff" => diff::cmd_diff(rest).await,
        "legacy-query" => query::cmd_query(rest).await,

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
