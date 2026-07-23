// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code Intelligence tools.
//!
//! Five Sovereign `Tool` implementations for navigating local codebases:
//!
//! - [`SymbolLookupTool`]  — EXACT symbol-name lookup. Metadata filter
//!   pushdown; always correct.
//! - [`CodeSearchTool`]    — APPROXIMATE semantic search. Every response
//!   prefixed with "Approximate results for" — the trust label is part
//!   of the contract, not cosmetic.
//! - [`RecentChangesTool`] — Symbols modified within the last N hours.
//!   Exact, by file mtime.
//! - [`FindCalleesTool`]   — What does this function call? SCIP graph,
//!   compiler-resolved. Staleness note appended when graph isn't fresh.
//! - [`FindCallersTool`]   — What calls this function? Supports depth=2
//!   for impact radius. Same staleness model.
//!
//! The first three tools query LanceDB via tree-sitter-indexed code
//! columns and work without a SCIP export. The call graph tools
//! (`find_callees`, `find_callers`) require a SCIP export and query the
//! [`ScipGraph`](corpus_engine_scip::scip_graph::ScipGraph) SQLite database.
//!
//! None of these tools return results when no code corpora are indexed.
//! Non-code corpora (Wikipedia, SEP, etc.) are skipped explicitly by the
//! `info.kind == CorpusKind::Code` filter in [`query_all_code_indexes`]
//! and the parallel loop in `code_search`. The earlier design relied on
//! `symbol_name IS NULL` to implicitly filter prose rows, but that only
//! works when the prose schema *has* a `symbol_name` column with NULLs;
//! prose-only chunk tables don't include the typed code columns at all,
//! and Lance errors at column resolution before the predicate can run.

pub mod brief;
pub mod briefing_tool;
pub mod code_search;
pub mod recent_changes;
pub mod working_set;

#[cfg(feature = "treesitter")]
pub mod callees;
#[cfg(feature = "treesitter")]
pub mod callers;
// Capability map — derives "what the codebase does" from the SCIP graph
// (`corpus_engine_scip::capability_map`). Treesitter-gated like its
// code-intel siblings; it lives on the code-corpus surface.
#[cfg(feature = "treesitter")]
pub mod capability_map_tool;
// `symbol_lookup` reads `ScipGraphHandle` from `callees`, so it shares
// the same `treesitter` gate. Without the gate the import target
// doesn't exist and the crate fails to build for non-treesitter
// consumers (sovereign-core dev-deps among them).
#[cfg(feature = "treesitter")]
pub mod symbol_lookup;

// Shared watcher-liveness assessment for the lint/test/build status
// tools. Turns the coordinator heartbeat into an explicit, actionable
// liveness reason and demotes orphaned results away from `fresh_*`.
#[cfg(feature = "treesitter")]
pub mod watcher_health;

// Test watcher MCP tools (require treesitter for SQLite types).
#[cfg(feature = "treesitter")]
pub mod get_run_output;
#[cfg(feature = "treesitter")]
pub mod run_tests;
#[cfg(feature = "treesitter")]
pub mod test_status;

// Lint watcher MCP tools.
#[cfg(feature = "treesitter")]
pub mod get_lint_output;
#[cfg(feature = "treesitter")]
pub mod lint_status;

// Single-call build/lint view. Wraps the same `LintResultStore`
// as `LintStatusTool` but folds the agent's typical follow-up
// `get_lint_output` call into the default response. The renamed
// tool surface (Phase 2 of the CLI refactor) advertises `build`;
// the legacy `lint_status` / `get_lint_output` ids remain
// registered for backward-compatible access.
#[cfg(feature = "treesitter")]
pub mod build;

// Active-spec / architecture / charter reader. Single-call
// answer to "what's the contract I'm working under?" — replaces
// the older `project_context` + manual file-read combination on
// the agent's side.
#[cfg(feature = "treesitter")]
pub mod spec;

// Spec-drift inspection. Calls into `sovereign_atos::approval`
// so the verdict matches the daemon's approval_gate middleware
// exactly: a feature this tool calls "drifted" is the same
// feature the gate would write a deviation note for.
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod drift;

// Architectural-drift freshness gate — sibling to lint_status /
// test_status. Reads the fingerprint sidecar the drift orchestrator
// writes; reports fresh/stale/partial/never_run against the
// narrative docs. Replaces the launchd-cron trigger model.
#[cfg(feature = "treesitter")]
pub mod drift_posture;

// Point-of-edit query against the latest drift findings.
// Companion to `drift_posture` (freshness gate): `findings`
// answers "what does the report say about THIS symbol or THIS
// file?" without re-running the LLM pipeline. Reads the JSON
// sidecar that the drift orchestrator mirrors to the canonical
// `~/.sovereign/drift/` path. Pre-edit, this is the narrative-
// side counterpart to `callers(name)` / `blast(name)`.
#[cfg(feature = "treesitter")]
pub mod drift_findings;
pub mod facts_tool;

// Capability-reconciliation freshness gate + findings query — siblings to
// drift_posture / drift_findings, over the `enrich capability-reconcile`
// artifact (corroborated / undocumented / drifted, derived vs the docs).
#[cfg(feature = "treesitter")]
pub mod capability_findings;
#[cfg(feature = "treesitter")]
pub mod capability_posture;

// Architecture observability (quality program, 2026-07-11): the OBSERVED
// half of dependency-direction enforcement — the xtask layer-gate checks
// Cargo-declared edges in CI; arch_report checks SCIP-observed symbol
// references against the same quality/ARCH_LAYERS.toml (shared arch-layers
// parser). arch_posture is the cheap persisted-report reader.
#[cfg(feature = "treesitter")]
pub mod arch_posture;
#[cfg(feature = "treesitter")]
pub mod arch_report;
// Advisory god-file split proposals from the SCIP call graph — the "where are
// the seams + which helpers stay behind" analysis, mechanized.
#[cfg(feature = "treesitter")]
pub mod suggest_seams;
// Semantic-duplication ("DRY") report from the per-symbol code embeddings —
// exact clones by content_hash + near clones by cosine similarity.
#[cfg(feature = "treesitter")]
pub mod dry_report;

// Working notes tools.
#[cfg(feature = "treesitter")]
pub mod delete_note;
#[cfg(feature = "treesitter")]
pub mod read_notes;
#[cfg(feature = "treesitter")]
pub mod retire_note;
#[cfg(feature = "treesitter")]
pub mod suggest_note;
#[cfg(feature = "treesitter")]
pub mod write_note;

// Index health reporting — used by all SCIP-dependent tools.
#[cfg(feature = "treesitter")]
pub mod index_health;

// Blast radius (transitive impact analysis).
#[cfg(feature = "treesitter")]
pub mod blast_radius;

// Project documentation search.
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod project_context;

// Session reflection & feedback loop.
#[cfg(feature = "treesitter")]
pub mod session_reflection;

// Doc path validity checker.
#[cfg(feature = "treesitter")]
pub mod check_doc_paths;

// ATOS feature management.
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod archive_feature;
pub mod atos_plan_emit;
pub mod atos_utils;
pub mod atos_verify;
#[cfg(feature = "treesitter")]
pub mod promote_note;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod provision_feature;
#[cfg(feature = "treesitter")]
pub mod read_note_by_id;
#[cfg(feature = "treesitter")]
pub mod read_note_digest;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod record_atos_event;
#[cfg(feature = "treesitter")]
pub mod write_redteam_finding;

// DESIGN.md structural signals — wraps corpus_engine_atos::design_signals
// so the agent-collaborative design session (and any MCP client) can
// audit a DESIGN.md's gaps and keywords without round-tripping through
// the CLI.
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub mod design_signals_extract;

pub use code_search::CodeSearchTool;
pub use recent_changes::RecentChangesTool;

#[cfg(feature = "treesitter")]
pub use callees::{FindCalleesTool, ScipGraphHandle};
#[cfg(feature = "treesitter")]
pub use callers::FindCallersTool;
#[cfg(feature = "treesitter")]
pub use capability_map_tool::CapabilityMapTool;
#[cfg(feature = "treesitter")]
pub use symbol_lookup::SymbolLookupTool;

pub use briefing_tool::{overlaps_for_working_set, BriefingTool, OverlapAccumulator};

#[cfg(feature = "treesitter")]
pub use get_run_output::GetRunOutputTool;
#[cfg(feature = "treesitter")]
pub use run_tests::RunTestsTool;
#[cfg(feature = "treesitter")]
pub use test_status::TestStatusTool;

#[cfg(feature = "treesitter")]
pub use arch_posture::ArchPostureTool;
#[cfg(feature = "treesitter")]
pub use arch_report::ArchReportTool;
#[cfg(feature = "treesitter")]
pub use build::BuildTool;
#[cfg(feature = "treesitter")]
pub use capability_findings::CapabilityFindingsTool;
#[cfg(feature = "treesitter")]
pub use capability_posture::CapabilityPostureTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use drift::DriftTool;
#[cfg(feature = "treesitter")]
pub use drift_posture::{
    compute_posture, write_fingerprint, DriftFingerprint, DriftPosture, DriftPostureTool,
    PostureStatus, TopCritical, DEFAULT_NARRATIVES, FINGERPRINT_FILE,
};
#[cfg(feature = "treesitter")]
pub use get_lint_output::GetLintOutputTool;
#[cfg(feature = "treesitter")]
pub use lint_status::LintStatusTool;
#[cfg(feature = "treesitter")]
pub use spec::SpecTool;

#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use archive_feature::ArchiveFeatureTool;
#[cfg(feature = "treesitter")]
pub use atos_plan_emit::AtosPlanEmitTool;
pub use atos_verify::AtosVerifyTool;
#[cfg(feature = "treesitter")]
pub use blast_radius::BlastRadiusTool;
#[cfg(feature = "treesitter")]
pub use check_doc_paths::CheckDocPathsTool;
#[cfg(feature = "treesitter")]
pub use delete_note::DeleteNoteTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use design_signals_extract::DesignSignalsExtractTool;
#[cfg(feature = "treesitter")]
pub use index_health::{IndexHealth, IndexHealthChecker, StalenessLevel};
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use project_context::ProjectContextTool;
#[cfg(feature = "treesitter")]
pub use promote_note::PromoteNoteTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use provision_feature::ProvisionFeatureTool;
#[cfg(feature = "treesitter")]
pub use read_note_by_id::ReadNoteByIdTool;
#[cfg(feature = "treesitter")]
pub use read_note_digest::ReadNoteDigestTool;
#[cfg(feature = "treesitter")]
pub use read_notes::ReadNotesTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use record_atos_event::RecordAtosEventTool;
#[cfg(feature = "treesitter")]
pub use retire_note::RetireNoteTool;
#[cfg(feature = "treesitter")]
pub use session_reflection::SessionReflectionTool;
#[cfg(feature = "treesitter")]
pub use write_note::WriteNoteTool;
#[cfg(feature = "treesitter")]
pub use write_redteam_finding::WriteRedteamFindingTool;

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{Array, Int32Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use corpus_engine::types::CorpusKind;
use corpus_engine::{CorpusEngine, CorpusIndex, Error as CorpusError};

/// A single code chunk row read from a LanceDB query via the typed code
/// columns. This is the in-memory shape the index-based tools operate on after
/// running a metadata filter query against a `CorpusIndex`.
#[derive(Debug, Clone)]
pub struct CodeRow {
    pub symbol_name: String,
    pub symbol_kind: String,
    pub file_path: String,
    pub line_start: i32,
    pub line_end: i32,
    pub language: String,
    pub mtime: i64,
    pub content: String,
    pub corpus_id: String,
}

/// Escape single quotes so user input can't break out of a filter literal.
/// LanceDB uses SQL-like filter syntax where `''` is a literal single quote.
pub(crate) fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}

/// Valid symbol-name characters. Anything outside this set is rejected at
/// the tool boundary so a name can never carry a quote or backslash into
/// the filter string.
pub(crate) fn is_valid_symbol_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '$')
}

/// Run a filter-pushdown query against every installed *code* corpus and
/// collect the matching rows into `CodeRow` values. Used by
/// `SymbolLookupTool` and `RecentChangesTool` — both are exact predicates
/// on typed columns, no vector search involved.
///
/// Non-code corpora (Wikipedia, SEP, …) are skipped before any Lance call
/// because their chunk tables lack the typed code columns entirely; the
/// query would error at column resolution rather than return zero rows.
pub(crate) async fn query_all_code_indexes(
    engine: &Arc<CorpusEngine>,
    filter: &str,
    limit: usize,
) -> Result<Vec<CodeRow>, CorpusError> {
    let mut out = Vec::new();
    let Ok(indexes) = engine.installed_indexes().await else {
        return Ok(out);
    };

    for info in &indexes {
        if info.kind != CorpusKind::Code {
            continue;
        }
        let Ok(index) = engine.open_index(&info.path).await else {
            continue;
        };
        let batches = run_filter_query(&index, filter, limit).await?;
        for batch in &batches {
            extract_code_rows(batch, &info.corpus_id, &mut out);
        }
    }
    Ok(out)
}

async fn run_filter_query(
    index: &CorpusIndex,
    filter: &str,
    limit: usize,
) -> Result<Vec<RecordBatch>, CorpusError> {
    index
        .table()
        .query()
        .only_if(filter)
        .limit(limit)
        .execute()
        .await
        .map_err(|e| CorpusError::Database(format!("code filter query: {e}")))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| CorpusError::Database(format!("code filter collect: {e}")))
}

/// Public alias used by `code_search` which runs its own queries via
/// `Table::nearest_to` / `Table::full_text_search` and then converts
/// the result batches into `CodeRow`s. Same logic as the private
/// helper — renamed so `code_search.rs` can call it without going
/// through the `query_all_code_indexes` entry point.
pub(crate) fn extract_code_rows_pub(batch: &RecordBatch, corpus_id: &str, out: &mut Vec<CodeRow>) {
    extract_code_rows(batch, corpus_id, out);
}

fn extract_code_rows(batch: &RecordBatch, corpus_id: &str, out: &mut Vec<CodeRow>) {
    let symbol_names = batch
        .column_by_name("symbol_name")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>());
    let symbol_kinds = batch
        .column_by_name("symbol_kind")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>());
    let file_paths = batch
        .column_by_name("file_path")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>());
    let line_starts = batch
        .column_by_name("line_start")
        .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
    let line_ends = batch
        .column_by_name("line_end")
        .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
    let languages = batch
        .column_by_name("language")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>());
    let mtimes = batch
        .column_by_name("mtime")
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
    let contents = batch
        .column_by_name("content")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>());

    for row in 0..batch.num_rows() {
        // Skip rows whose symbol_name is Null — non-code corpora leave
        // every code column Null, and those rows aren't meaningful here.
        let name = match symbol_names {
            Some(a) if !a.is_null(row) => a.value(row).to_string(),
            _ => continue,
        };
        out.push(CodeRow {
            symbol_name: name,
            symbol_kind: symbol_kinds
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row).to_string())
                    }
                })
                .unwrap_or_else(|| "unknown".into()),
            file_path: file_paths
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row).to_string())
                    }
                })
                .unwrap_or_default(),
            line_start: line_starts
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row))
                    }
                })
                .unwrap_or(0),
            line_end: line_ends
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row))
                    }
                })
                .unwrap_or(0),
            language: languages
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row).to_string())
                    }
                })
                .unwrap_or_default(),
            mtime: mtimes
                .and_then(|a| {
                    if a.is_null(row) {
                        None
                    } else {
                        Some(a.value(row))
                    }
                })
                .unwrap_or(0),
            content: contents
                .map(|a| a.value(row).to_string())
                .unwrap_or_default(),
            corpus_id: corpus_id.to_string(),
        });
    }
}

/// Group code rows by `file_path` preserving insertion order. Used by
/// `RecentChangesTool` to print `file → symbols` buckets.
pub(crate) fn group_by_file(rows: &[CodeRow]) -> Vec<(String, Vec<&CodeRow>)> {
    let mut ordered: Vec<String> = Vec::new();
    let mut buckets: HashMap<String, Vec<&CodeRow>> = HashMap::new();
    for r in rows {
        if !buckets.contains_key(&r.file_path) {
            ordered.push(r.file_path.clone());
        }
        buckets.entry(r.file_path.clone()).or_default().push(r);
    }
    ordered
        .into_iter()
        .map(|file| {
            let bucket = buckets.remove(&file).unwrap_or_default();
            (file, bucket)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_name_rejects_quotes() {
        assert!(!is_valid_symbol_name("foo'bar"));
        assert!(!is_valid_symbol_name("foo\""));
        assert!(!is_valid_symbol_name("foo;"));
        assert!(!is_valid_symbol_name("foo\\"));
        assert!(!is_valid_symbol_name(""));
    }

    #[test]
    fn symbol_name_accepts_valid_identifiers() {
        assert!(is_valid_symbol_name("foo"));
        assert!(is_valid_symbol_name("Foo_Bar"));
        assert!(is_valid_symbol_name("Module::sub"));
        assert!(is_valid_symbol_name("_internal"));
        assert!(is_valid_symbol_name("$special"));
    }

    #[test]
    fn escape_sql_doubles_quotes() {
        assert_eq!(escape_sql("O'Brien"), "O''Brien");
        assert_eq!(escape_sql("clean"), "clean");
    }
}
