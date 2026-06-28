// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub mod atlas_drift_report;
pub mod atlas_eval;
pub mod atlas_gaps;
pub mod atlas_phase_cmd;
pub mod atlas_query;
pub mod atlas_reconcile;
pub mod atlas_resolve;
pub mod atlas_tensions;
pub mod workflow_primitives;
pub mod atlas_tensions_classify;
pub mod build;
pub mod capability_doc;
pub mod capability_reconcile;
pub mod cascade;
pub mod classify;
pub mod code_intel;
pub mod config;
pub mod corpus_io;
pub mod delta_cmd;
pub mod diagnose;
pub mod diff;
pub mod errors;
pub mod eval;
pub mod eval_median;
pub mod exemplars;
pub mod extract;
pub mod extract_typed;
pub mod inference_client;
pub mod ingest;
pub mod init;
pub mod investigation;
pub mod paths;
pub mod phase_cmd;
pub mod pipeline_resolve;
pub mod promote;
pub mod providers;
pub mod query;
pub mod raptor;
pub mod raptor_index;
pub mod reset;
pub mod schema_review;
pub mod seed_cmd;
pub mod sep_ingest;
pub mod sheets_ingest;
pub mod show;
pub mod source_loader;
pub mod spec_intel;
pub mod spec_reconcile;
pub mod status;
pub mod templates;
pub mod triage;
pub mod validate;

use sovereign_cli_shared::help::{self, Help, HelpSection};

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
                ("delta", "Incrementally enrich a chapter subset and merge the resulting atoms+edges into the EXISTING atlas (additive; no full rebuild)."),
                ("delta-manifest", "Mint sec_NNNNN chapter ids for freshly-appended chunks (by --source-prefix) and append them to chapters.json."),
                ("ingest", "Run an AtlasIngestion strategy end-to-end (today: structure_first deterministic Wikipedia parser)."),
                ("raptor", "Retrofit an installed corpus with a per-document RAPTOR tier-3 summary tree (additive to any existing atom-graph atlas) — powers whole-document summarization."),
                ("raptor-index", "(Re)build the RAPTOR summary-node ANN index (raptor_summaries.lance) from conv_raptor_nodes — the query-time fast path; 'enrich raptor' builds it automatically at the end of a run."),
                ("code-intel", "Summarize every function in a CODE corpus (plain-English intent + the questions it answers) and index them as searchable chunks — the conceptual->code retrieval bridge."),
                ("spec-intel", "Extract conditioned claims from a spec .md (split on `## ` headers) — validated findings (contract) + planned behavior (proposal), grammar-constrained, resumable per section."),
                ("spec-reconcile", "Reconcile a spec's conditioned claims (from `enrich spec-intel`) against what the corpus code actually does: corroborated / todo / drift / gap / unverifiable, per-condition adjudicated."),
                ("capability-doc", "Narrate every derived capability into a grounded, file:line-cited architecture document (from `code capability-map` + `enrich code-intel`)."),
                ("capability-reconcile", "Reconcile derived capabilities against the architecture docs: corroborated / undocumented (LLM-verified) / drifted (doc claim vs code)."),
                ("triage-candidates", "Rank atlas entities by inbound link degree to pick Tier-1.5 / Tier-2 enrichment candidates."),
                ("atlas-eval", "Score the structural atlas against a question bank by tokenized title-overlap retrieval."),
                ("eval", "Score the resolved atlas against a golden-set TOML; reports per-phase precision/recall/F1."),
                (
                    "eval-median",
                    "Run reset → build → eval N times against an initialised corpus; reports min/median/max F1 per phase to separate signal from variance.",
                ),
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
                ("classify", "Phase 0: per-section type classification (fiction / argumentative / journal / ...). Drives routed Phase 1 dispatch."),
                ("extract", "Phase 1: per-section atlas extraction (subset or full)."),
                ("extract-typed", "Routed-Phase-1 v1: per-type typed-extension extraction over already-extracted sections (ArgumentativeEssay only in v1)."),
                ("cluster", "Phase 2: cluster the Phase 1 sketches by facet."),
                ("name", "Phase 3: name each facet cluster."),
                ("resolve", "Phase 3a/3b: resolve atoms + edges + trajectories from the sketches."),
                ("tensions", "Phase 6 (deterministic): select tension candidates from the resolved atlas."),
                ("tensions-classify", "Phase 6 (LLM): classify tension candidates and merge accepted ones into edges.json as Tension edges."),
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
                    "diagnose",
                    "Read-only inspection of the resolved philosophy atlas — atom counts, positions, fault lines, gaps, configurations.",
                ),
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
        "delta" => delta_cmd::cmd_delta(rest).await,
        "delta-manifest" => delta_cmd::cmd_delta_manifest(rest).await,
        "ingest" => ingest::cmd_ingest(rest).await,
        "triage-candidates" | "triage" => triage::cmd_triage(rest).await,
        "atlas-eval" => atlas_eval::cmd_atlas_eval(rest).await,
        "sep-ingest" => sep_ingest::cmd_sep_ingest(rest).await,
        "eval" => eval::cmd_eval(rest).await,
        "eval-median" => eval_median::cmd_eval_median(rest).await,
        "query" | "atlas-query" => atlas_query::cmd_atlas_query(rest).await,
        "report" | "schema-report" => schema_review::cmd_schema_report(rest).await,
        "review" | "schema-review" => schema_review::cmd_schema_review(rest).await,
        "atlas-drift-report" => atlas_drift_report::cmd_atlas_drift_report(rest).await,
        "bridge" | "atlas-cross-corpus" => atlas_cross_corpus::cmd_atlas_cross_corpus(rest).await,

        // ── Individual phases ─────────────────────────────────
        "seed" => seed_cmd::cmd_seed(rest).await,
        "classify" => classify::cmd_classify(rest).await,
        "extract" => extract::cmd_extract(rest).await,
        "extract-typed" => extract_typed::cmd_extract_typed(rest).await,
        "sheets-ingest" => sheets_ingest::cmd_sheets_ingest(rest).await,
        "cluster" | "cluster-atlas" => atlas_phase_cmd::cmd_cluster_atlas(rest).await,
        "name" | "name-atlas-clusters" => atlas_phase_cmd::cmd_name_atlas_clusters(rest).await,
        "resolve" | "atlas-resolve" => atlas_resolve::cmd_atlas_resolve(rest).await,
        "reconcile" | "atlas-reconcile" => atlas_reconcile::cmd_atlas_reconcile(rest).await,
        "tensions" | "atlas-tensions" => atlas_tensions::cmd_atlas_tensions(rest).await,
        "tensions-classify" | "atlas-tensions-classify" => {
            atlas_tensions_classify::cmd_atlas_tensions_classify(rest).await
        }
        "gaps" | "atlas-gaps" => atlas_gaps::cmd_atlas_gaps(rest).await,
        "configure" | "atlas-configuration" => {
            atlas_configuration::cmd_atlas_configuration(rest).await
        }

        // ── Investigation pipeline (typed-relationship graph + pattern detectors) ──
        "investigation" => investigation::cmd_investigation(rest).await,
        "raptor" => raptor::cmd_raptor(rest).await,
        "raptor-index" => raptor_index::cmd_raptor_index(rest).await,

        // ── Code intelligence (per-symbol intent summaries -> chunks) ──
        "code-intel" => code_intel::cmd_code_intel(rest).await,
        "spec-intel" => spec_intel::cmd_spec_intel(rest).await,
        "spec-reconcile" => spec_reconcile::cmd_spec_reconcile(rest).await,
        "capability-doc" => capability_doc::cmd_capability_doc(rest).await,
        "capability-reconcile" => capability_reconcile::cmd_capability_reconcile(rest).await,

        // ── Utilities ─────────────────────────────────────────
        "status" => status::cmd_status(rest).await,
        "show" => show::cmd_show(rest).await,
        "diagnose" => diagnose::cmd_diagnose(rest).await,
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
        "name-concerns" => phase_cmd::run_phase(phase_cmd::PhaseOp::NameConcerns, rest).await,
        "cluster-chunks" => phase_cmd::run_phase(phase_cmd::PhaseOp::ClusterChunks, rest).await,
        "extract-positions" => {
            phase_cmd::run_phase(phase_cmd::PhaseOp::ExtractPositions, rest).await
        }
        "detect-tensions" => phase_cmd::run_phase(phase_cmd::PhaseOp::DetectTensions, rest).await,
        "detect-gaps" => phase_cmd::run_phase(phase_cmd::PhaseOp::DetectGaps, rest).await,
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
        let guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        HomeGuard { dir, _guard: guard }
    }
}
