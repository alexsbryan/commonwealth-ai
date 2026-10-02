// SPDX-License-Identifier: AGPL-3.0-or-later
//! The merged SCIP graph as the code tools hold it: loaded from
//! `<indexes>/*/scip_graph.db` by the one loader
//! (`corpus_engine_scip::merged_graph`) when a tool first READS it, never at
//! registry build (phase-b pb-code-freshness). Every `svrn tools` verb paid
//! the load (~39 s on this repo's 857 MB graph, pb-atlas-kv) whether or not it
//! touched the graph.
//!
//! A handle a host already loaded converts in as-is (`From<ScipGraphHandle>`),
//! so an eager host keeps its behaviour.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use corpus_engine_scip::ScipGraph;

use crate::ScipGraphHandle;

/// A [`ScipGraphHandle`] that loads itself on first read. Cheap to clone;
/// clones share one load.
#[derive(Clone)]
pub struct LazyScipGraph(Arc<Inner>);

struct Inner {
    handle: ScipGraphHandle,
    /// `None` for a handle that arrived loaded.
    indexes_dir: Option<PathBuf>,
    loaded: tokio::sync::OnceCell<()>,
}

impl LazyScipGraph {
    /// An empty graph that merges `indexes_dir` on its first read.
    pub fn deferred(indexes_dir: PathBuf) -> Self {
        let empty = ScipGraph::open_in_memory("merged").expect("in-memory ScipGraph");
        Self(Arc::new(Inner {
            handle: Arc::new(ArcSwap::from_pointee(empty)),
            indexes_dir: Some(indexes_dir),
            loaded: tokio::sync::OnceCell::new(),
        }))
    }

    /// Load the graph if no read has yet. The one place a deferred graph
    /// is loaded; concurrent first reads share it.
    pub async fn ensure_loaded(&self) {
        let Some(dir) = &self.0.indexes_dir else {
            return;
        };
        self.0
            .loaded
            .get_or_init(|| async {
                let started = std::time::Instant::now();
                let (graph, summary) =
                    corpus_engine_scip::merged_graph::load_merged_graph(dir, false).await;
                self.0.handle.store(Arc::new(graph));
                tracing::info!(
                    indexes = %dir.display(),
                    graphs = summary.graphs_found,
                    symbols = summary.total_symbols,
                    edges = summary.total_refs,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "scip graph: loaded on first read"
                );
            })
            .await;
    }

    /// Whether the graph is in memory: always for a handle that arrived
    /// loaded, and for a deferred one once a read has loaded it.
    pub fn is_loaded(&self) -> bool {
        self.0.indexes_dir.is_none() || self.0.loaded.initialized()
    }

    /// The current graph, loading it first if this is the first read.
    pub async fn load_full(&self) -> Arc<ScipGraph> {
        self.ensure_loaded().await;
        self.0.handle.load_full()
    }

    /// The swappable handle underneath, for a writer that updates the graph
    /// in place (the Reindexer). Reading through it does NOT load.
    pub fn handle(&self) -> ScipGraphHandle {
        Arc::clone(&self.0.handle)
    }
}

impl From<ScipGraphHandle> for LazyScipGraph {
    /// A graph the host already loaded: reads never load.
    fn from(handle: ScipGraphHandle) -> Self {
        Self(Arc::new(Inner {
            handle,
            indexes_dir: None,
            loaded: tokio::sync::OnceCell::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_deferred_graph_loads_on_first_read_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let corpus = dir.path().join("fixture");
        std::fs::create_dir_all(&corpus).unwrap();
        let on_disk = ScipGraph::open(&corpus.join("scip_graph.db"), "fixture").unwrap();
        on_disk
            .ingest_symbols_and_refs(
                vec![corpus_engine_scip::scip_graph::ScipSymbolRecord {
                    name: "lazy_target".into(),
                    qualified_name: "fixture src/lib.rs/lazy_target().".into(),
                    kind: "function".into(),
                    file_path: "src/lib.rs".into(),
                    line_start: 0,
                    line_end: 1,
                    language: "rust".into(),
                }],
                vec![],
            )
            .await
            .unwrap();

        let lazy = LazyScipGraph::deferred(dir.path().to_path_buf());
        assert!(!lazy.is_loaded(), "building the handle must not load it");
        let before = lazy.handle().load_full().stats().await.symbol_count;
        assert_eq!(before, 0, "the handle is empty until a read");

        let graph = lazy.load_full().await;
        assert!(lazy.is_loaded());
        assert_eq!(graph.stats().await.symbol_count, 1, "the first read loads");
    }

    #[tokio::test]
    async fn a_loaded_handle_never_loads() {
        let handle: ScipGraphHandle = Arc::new(ArcSwap::from_pointee(
            ScipGraph::open_in_memory("x").unwrap(),
        ));
        let hosts = handle.load_full();
        let lazy = LazyScipGraph::from(Arc::clone(&handle));
        assert!(lazy.is_loaded());
        let read = lazy.load_full().await;
        assert!(
            Arc::ptr_eq(&hosts, &read),
            "a read returns the host's graph; it never swaps a load in over it"
        );
    }
}
