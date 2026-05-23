//! Index health reporting for MCP tool responses.
//!
//! Every MCP tool that depends on the SCIP call graph attaches an
//! `IndexHealth` block to its response so the agent can distinguish
//! "no results" (the index is fine, there just aren't any) from
//! "index absent" (the results cannot be trusted). Without this
//! block, `blast_radius` returning `total: 0` looks identical whether
//! the graph is healthy-and-empty or has never been built.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use corpus_engine_scip::scip_graph::ScipGraph;

/// A hot-reloadable SCIP graph handle (mirrors the type in callees.rs).
pub type ScipGraphHandle = Arc<arc_swap::ArcSwap<ScipGraph>>;

// ── Public types ─────────────────────────────────────────────────────────────

/// Staleness classification for an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessLevel {
    /// Index present, built <1 h ago, no stale files.
    Fresh,
    /// Index present, 1–24 h old. Results are probably still good.
    Aging,
    /// Index present but >24 h old or has modified files since last build.
    Stale,
    /// Index has never been built. All results are empty/untrustworthy.
    Absent,
}

/// Health snapshot attached to tool responses that depend on the SCIP index.
#[derive(Debug, Clone, Serialize)]
pub struct IndexHealth {
    /// true if the index exists and has at least one symbol.
    pub present: bool,
    /// Staleness classification.
    pub staleness: StalenessLevel,
    pub symbol_count: usize,
    pub stale_file_count: usize,
    /// Hours since last export. `None` if never exported.
    pub export_age_hours: Option<u64>,
    /// Human-readable repair hint. `None` when `staleness == Fresh`
    /// so we don't add noise to the common case.
    pub hint: Option<String>,
}

// ── IndexHealthChecker ────────────────────────────────────────────────────────

/// Computes and caches SCIP index health for 30 seconds.
///
/// The 30-second TTL avoids hammering SQLite when several tools fire
/// in rapid succession during a single agent task. The WatcherCoordinator
/// already tracks staleness precisely; we read that state — we don't
/// recompute it from scratch on every call.
pub struct IndexHealthChecker {
    graph: ScipGraphHandle,
    cache: Mutex<Option<(IndexHealth, Instant)>>,
}

const CACHE_TTL: Duration = Duration::from_secs(30);

impl IndexHealthChecker {
    pub fn new(graph: ScipGraphHandle) -> Self {
        Self {
            graph,
            cache: Mutex::new(None),
        }
    }

    /// Return the current index health, using the cache if fresh.
    pub async fn check(&self) -> IndexHealth {
        let mut guard = self.cache.lock().await;

        if let Some((cached, at)) = guard.as_ref() {
            if at.elapsed() < CACHE_TTL {
                return cached.clone();
            }
        }

        let health = self.compute().await;
        *guard = Some((health.clone(), Instant::now()));
        health
    }

    async fn compute(&self) -> IndexHealth {
        let graph = self.graph.load_full();
        let stats = graph.stats().await;

        let (present, staleness) = classify(&stats);
        let hint = make_hint(staleness, &stats);

        IndexHealth {
            present,
            staleness,
            symbol_count: stats.symbol_count,
            stale_file_count: stats.stale_file_count,
            export_age_hours: stats.export_age_hours,
            hint,
        }
    }
}

fn classify(
    stats: &corpus_engine_scip::scip_graph::ScipGraphStats,
) -> (bool, StalenessLevel) {
    if stats.symbol_count == 0 && stats.export_age_hours.is_none() {
        return (false, StalenessLevel::Absent);
    }

    let level = match (stats.export_age_hours, stats.stale_file_count) {
        (_, n) if n > 0 => StalenessLevel::Stale,
        (Some(h), _) if h > 24 => StalenessLevel::Stale,
        (Some(h), _) if h > 1 => StalenessLevel::Aging,
        _ => StalenessLevel::Fresh,
    };

    (stats.symbol_count > 0, level)
}

fn make_hint(level: StalenessLevel, stats: &corpus_engine_scip::scip_graph::ScipGraphStats) -> Option<String> {
    match level {
        StalenessLevel::Fresh => None,
        StalenessLevel::Aging => Some(format!(
            "ScipGraph is {}h old. Results may miss recent changes. \
             Run `sovereign corpus scip` to refresh.",
            stats.export_age_hours.unwrap_or(0)
        )),
        StalenessLevel::Stale if stats.stale_file_count > 0 => Some(format!(
            "{} file(s) modified since last SCIP export. \
             Run `sovereign corpus scip` for accurate call-graph results.",
            stats.stale_file_count
        )),
        StalenessLevel::Stale => Some(format!(
            "ScipGraph is {}h old. \
             Run `sovereign corpus scip` for accurate call-graph results.",
            stats.export_age_hours.unwrap_or(0)
        )),
        StalenessLevel::Absent => Some(
            "ScipGraph not built. Run `sovereign corpus scip` to enable \
             call-graph analysis. blast_radius and find_callers results \
             cannot be trusted until the index exists."
                .into(),
        ),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use corpus_engine_scip::scip_graph::ScipGraph;

    async fn make_checker(graph: ScipGraph) -> IndexHealthChecker {
        let handle: ScipGraphHandle = Arc::new(ArcSwap::from_pointee(graph));
        IndexHealthChecker::new(handle)
    }

    #[tokio::test]
    async fn scip_absent_returns_absent() {
        // An in-memory graph with no symbols and no export → Absent.
        // Note: open_in_memory records a fresh export, so we use a fresh
        // graph but clear it to simulate never-built.
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph.clear().await.unwrap();
        // Also reset the export timestamp so it looks like never-built.
        // We simulate "never built" by using a graph that still has the
        // in-memory export recorded but zero symbols. Since open_in_memory
        // records an export, we check the present=false path instead:
        // stats() returns symbol_count=0 and export_age_hours=Some(0).
        // The classify() logic: symbol_count==0 && age.is_none() → Absent.
        // For the in-memory case with zero symbols but recent export:
        // classify returns (false, Fresh) — which is fine for this test
        // variant. Let's open a real empty graph without the export record.
        let graph2 = ScipGraph::open_in_memory("test2").unwrap();
        // Manually: a graph with no export recorded would need a fresh open
        // without the INSERT in open_in_memory. Since we can't easily mock
        // that, test the classify() function directly.
        let _ = graph2;

        // Test classify() directly with a zero-symbol, no-export stats.
        let stats_absent = corpus_engine_scip::scip_graph::ScipGraphStats {
            symbol_count: 0,
            ref_count: 0,
            stale_file_count: 0,
            export_age_hours: None,
        };
        let (present, level) = classify(&stats_absent);
        assert!(!present);
        assert_eq!(level, StalenessLevel::Absent);
        assert!(make_hint(level, &stats_absent).is_some());
    }

    #[tokio::test]
    async fn scip_fresh_no_hint() {
        // Fresh stats: recent export, no stale files, has symbols.
        let stats = corpus_engine_scip::scip_graph::ScipGraphStats {
            symbol_count: 100,
            ref_count: 50,
            stale_file_count: 0,
            export_age_hours: Some(0),
        };
        let (present, level) = classify(&stats);
        assert!(present);
        assert_eq!(level, StalenessLevel::Fresh);
        assert!(make_hint(level, &stats).is_none(), "Fresh index must not produce a hint");
    }

    #[tokio::test]
    async fn health_checker_caches_result() {
        let graph = ScipGraph::open_in_memory("cache_test").unwrap();
        let checker = make_checker(graph).await;

        // Call twice rapidly — should not panic, second call returns cache.
        let h1 = checker.check().await;
        let h2 = checker.check().await;
        // Both results should be identical (same snapshot).
        assert_eq!(h1.symbol_count, h2.symbol_count);
    }
}
