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
//! [`ScipGraph`](corpus_engine::scip_graph::ScipGraph) SQLite database.
//!
//! None of these tools return results when no code corpora are indexed.
//! Non-code corpora (Wikipedia, SEP, etc.) are implicitly filtered out by
//! querying on the typed code columns — rows where `symbol_name IS NULL`
//! don't match the predicates these tools build.

pub mod code_search;
pub mod recent_changes;
pub mod symbol_lookup;

#[cfg(feature = "treesitter")]
pub mod callees;
#[cfg(feature = "treesitter")]
pub mod callers;

// Test watcher MCP tools (require treesitter for SQLite types).
#[cfg(feature = "treesitter")]
pub mod test_status;
#[cfg(feature = "treesitter")]
pub mod run_tests;
#[cfg(feature = "treesitter")]
pub mod get_run_output;

// Lint watcher MCP tools.
#[cfg(feature = "treesitter")]
pub mod lint_status;
#[cfg(feature = "treesitter")]
pub mod get_lint_output;

// Working notes tools.
#[cfg(feature = "treesitter")]
pub mod write_note;
#[cfg(feature = "treesitter")]
pub mod read_notes;
#[cfg(feature = "treesitter")]
pub mod delete_note;
#[cfg(feature = "treesitter")]
pub mod suggest_note;

// Index health reporting — used by all SCIP-dependent tools.
#[cfg(feature = "treesitter")]
pub mod index_health;

// Blast radius (transitive impact analysis).
#[cfg(feature = "treesitter")]
pub mod blast_radius;

// Project documentation search.
#[cfg(feature = "treesitter")]
pub mod project_context;

// Session reflection & feedback loop.
#[cfg(feature = "treesitter")]
pub mod session_reflection;

// Doc path validity checker.
#[cfg(feature = "treesitter")]
pub mod check_doc_paths;

// ATOS feature management.
#[cfg(feature = "treesitter")]
pub mod provision_feature;
#[cfg(feature = "treesitter")]
pub mod archive_feature;
#[cfg(feature = "treesitter")]
pub mod read_note_by_id;
#[cfg(feature = "treesitter")]
pub mod promote_note;
#[cfg(feature = "treesitter")]
pub mod read_note_digest;
#[cfg(feature = "treesitter")]
pub mod record_atos_event;
#[cfg(feature = "treesitter")]
pub mod write_redteam_finding;

// DESIGN.md structural signals — wraps corpus_engine::design_signals
// so the agent-collaborative design session (and any MCP client) can
// audit a DESIGN.md's gaps and keywords without round-tripping through
// the CLI.
#[cfg(feature = "treesitter")]
pub mod design_signals_extract;

pub use code_search::CodeSearchTool;
pub use recent_changes::RecentChangesTool;
pub use symbol_lookup::SymbolLookupTool;

#[cfg(feature = "treesitter")]
pub use callees::{FindCalleesTool, ScipGraphHandle};
#[cfg(feature = "treesitter")]
pub use callers::FindCallersTool;

#[cfg(feature = "treesitter")]
pub use test_status::TestStatusTool;
#[cfg(feature = "treesitter")]
pub use run_tests::RunTestsTool;
#[cfg(feature = "treesitter")]
pub use get_run_output::GetRunOutputTool;

#[cfg(feature = "treesitter")]
pub use lint_status::LintStatusTool;
#[cfg(feature = "treesitter")]
pub use get_lint_output::GetLintOutputTool;

#[cfg(feature = "treesitter")]
pub use write_note::WriteNoteTool;
#[cfg(feature = "treesitter")]
pub use read_notes::ReadNotesTool;
#[cfg(feature = "treesitter")]
pub use delete_note::DeleteNoteTool;
#[cfg(feature = "treesitter")]
pub use index_health::{IndexHealth, IndexHealthChecker, StalenessLevel};
#[cfg(feature = "treesitter")]
pub use blast_radius::BlastRadiusTool;
#[cfg(feature = "treesitter")]
pub use project_context::ProjectContextTool;
#[cfg(feature = "treesitter")]
pub use session_reflection::SessionReflectionTool;
#[cfg(feature = "treesitter")]
pub use check_doc_paths::CheckDocPathsTool;
#[cfg(feature = "treesitter")]
pub use provision_feature::ProvisionFeatureTool;
#[cfg(feature = "treesitter")]
pub use archive_feature::ArchiveFeatureTool;
#[cfg(feature = "treesitter")]
pub use read_note_by_id::ReadNoteByIdTool;
#[cfg(feature = "treesitter")]
pub use promote_note::PromoteNoteTool;
#[cfg(feature = "treesitter")]
pub use read_note_digest::ReadNoteDigestTool;
#[cfg(feature = "treesitter")]
pub use record_atos_event::RecordAtosEventTool;
#[cfg(feature = "treesitter")]
pub use write_redteam_finding::WriteRedteamFindingTool;
#[cfg(feature = "treesitter")]
pub use design_signals_extract::DesignSignalsExtractTool;

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{Array, Int32Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

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
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '$')
}

/// Run a filter-pushdown query against every installed corpus and collect
/// the matching rows into `CodeRow` values. Used by `SymbolLookupTool` and
/// `RecentChangesTool` — both are exact predicates on typed columns, no
/// vector search involved.
///
/// Corpora that don't have code data (Wikipedia, SEP, …) return zero rows
/// because the filter references `symbol_name IS NOT NULL` implicitly:
/// metadata rows have all code columns as Null, which don't match equality
/// or range predicates on those columns.
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
pub(crate) fn extract_code_rows_pub(
    batch: &RecordBatch,
    corpus_id: &str,
    out: &mut Vec<CodeRow>,
) {
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
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row).to_string()) })
                .unwrap_or_else(|| "unknown".into()),
            file_path: file_paths
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row).to_string()) })
                .unwrap_or_default(),
            line_start: line_starts
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row)) })
                .unwrap_or(0),
            line_end: line_ends
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row)) })
                .unwrap_or(0),
            language: languages
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row).to_string()) })
                .unwrap_or_default(),
            mtime: mtimes
                .and_then(|a| if a.is_null(row) { None } else { Some(a.value(row)) })
                .unwrap_or(0),
            content: contents
                .map(|a| a.value(row).to_string())
                .unwrap_or_default(),
            corpus_id: corpus_id.to_string(),
        });
    }
}

/// Format a set of `CodeRow` values as fenced code blocks with a header
/// comment per block. Shared by `SymbolLookupTool` and `CodeSearchTool` so
/// the output shape is consistent across the two tools.
pub(crate) fn format_code_rows(rows: &[CodeRow]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "```{lang}\n// {file}:{start}-{end}  [{kind}]  ({corpus})\n{content}\n```",
                lang = r.language,
                file = r.file_path,
                start = r.line_start + 1, // 1-indexed display
                end = r.line_end + 1,
                kind = r.symbol_kind,
                corpus = r.corpus_id,
                content = r.content,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
