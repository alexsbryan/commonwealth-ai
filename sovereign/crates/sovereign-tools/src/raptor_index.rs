//! Build the derived RAPTOR summary-node ANN index (`raptor_summaries.lance`)
//! for a corpus from its `conv_raptor_nodes` SQLite rows.
//!
//! This is the sovereign-side bridge for the spec's layering: `corpus-engine`
//! owns the pure-LanceDB table mechanics (`build_raptor_index`) but cannot
//! read `conv_raptor_nodes` (it has no sovereign-store dep). This crate has
//! both handles, so it reads the rows, maps them into the engine's plain
//! [`RaptorSummaryRow`], stamps the build-version (`max(created_at)`), and
//! calls the engine builder. It mirrors how `raptor_atlas::build_raptor_atlas`
//! (the tree builder) is injected from this same crate.
//!
//! Two callers share this one builder: the auto-hook at the end of an
//! `enrich raptor` run, and the standalone `enrich raptor-index <corpus>`
//! verb.

use std::path::Path;

use corpus_engine::{build_raptor_index, RaptorSummaryRow};
use sovereign_store::sqlite::SqliteStateStore;

/// Outcome of a `build_corpus_raptor_index` run, for transparent reporting.
#[derive(Debug, Clone)]
pub enum RaptorIndexOutcome {
    /// The table + freshness sidecar were (re)built with `rows` summary nodes.
    Built { rows: usize },
    /// The corpus has no `conv_raptor_nodes` yet — run `enrich raptor` first.
    Empty,
    /// The build failed; `reason` is already human-readable.
    Failed { reason: String },
}

impl std::fmt::Display for RaptorIndexOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RaptorIndexOutcome::Built { rows } => {
                write!(f, "built raptor_summaries.lance ({rows} summary nodes)")
            }
            RaptorIndexOutcome::Empty => {
                write!(f, "no RAPTOR nodes found — run `enrich raptor` first")
            }
            RaptorIndexOutcome::Failed { reason } => write!(f, "FAILED: {reason}"),
        }
    }
}

/// Read all of `corpus_id`'s `conv_raptor_nodes`, map them to the engine's
/// [`RaptorSummaryRow`], stamp `max(created_at)` as the build-version, and
/// (re)build `<corpus_dir>/raptor_summaries.lance` + its freshness sidecar.
///
/// `corpus_dir` is the per-corpus index dir (`<indexes>/<corpus_id>`) — the
/// same value `CorpusEngine::index_dir().join(corpus_id)` produces. The build
/// is a full, idempotent rebuild (the table is a pure derivative of the
/// SQLite rows), so calling it repeatedly is safe.
pub async fn build_corpus_raptor_index(
    store: &SqliteStateStore,
    corpus_dir: &Path,
    corpus_id: &str,
) -> RaptorIndexOutcome {
    // min_level = 0 → every node (leaves through root). The query-time
    // `min_level` filter is applied caller-side over the search hits, so the
    // index must carry all levels.
    let nodes = match store.list_corpus_raptor_nodes(corpus_id, 0).await {
        Ok(n) => n,
        Err(e) => {
            return RaptorIndexOutcome::Failed {
                reason: format!("read conv_raptor_nodes({corpus_id}): {e}"),
            }
        }
    };
    if nodes.is_empty() {
        return RaptorIndexOutcome::Empty;
    }

    // Build-version stamp: the newest source row. The freshness probe compares
    // this against the live `max(created_at)` to detect a stale table.
    let source_version = nodes.iter().map(|n| n.created_at).max().unwrap_or(0);

    let rows: Vec<RaptorSummaryRow> = nodes
        .into_iter()
        .map(|n| RaptorSummaryRow {
            node_id: n.node_id,
            conv_uuid: n.conv_uuid,
            level: n.level,
            summary: n.summary,
            // The leaf embeddings RAPTOR reused — same model/space as the
            // query embedding, so cosine is meaningful. centroid_embedding and
            // the JSON columns are intentionally dropped (grounding never
            // reads them).
            embedding: n.summary_embedding,
        })
        .collect();

    match build_raptor_index(corpus_dir, &rows, source_version).await {
        Ok(0) => RaptorIndexOutcome::Empty,
        Ok(rows_written) => RaptorIndexOutcome::Built { rows: rows_written },
        Err(e) => RaptorIndexOutcome::Failed {
            reason: format!("build raptor_summaries.lance({corpus_id}): {e}"),
        },
    }
}
