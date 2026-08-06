//! Dataset maintenance — compaction, index optimization, version pruning.
//!
//! # Why this module exists
//!
//! Until 2026-08-05 this workspace **never** performed Lance maintenance:
//! `optimize_indices`, `compact_files` and `cleanup_old_versions` appeared
//! nowhere in `corpus-engine` or any `sovereign` crate. Corpora are ingested
//! incrementally — watched folders, mesh pulls, peer-assisted ingest — so every
//! append committed a new manifest version and left new fragments outside the
//! existing indexes. Nothing ever merged them back.
//!
//! What that costs, measured 2026-08-05 on the `bench sep/summarize --synth`
//! lane:
//!
//! | corpus                  | fragments | manifest versions | indices | per-search |
//! |-------------------------|-----------|-------------------|---------|------------|
//! | `wikipedia`             | 5,957     | 3,955             | 24      | **2218ms** |
//! | `sep`                   | 2,937     | 1                 | 3       | 100ms      |
//! | `conversations-anthropic` | 9       | 13                | 3       | 64ms       |
//!
//! `sep` carries HALF of wikipedia's fragments and answers 22x faster, so raw
//! fragment count is not the driver — the difference is 3,955 append-commits
//! whose fragments were never folded into the index. Lance answers a query over
//! such a dataset by searching indexed fragments via the index, **flat-scanning
//! the unindexed ones**, and merging. A flat scan is invariant to every ANN
//! parameter, which is exactly what we measured: `nprobes` 50→10, `refine` 30→
//! off and coverage overfetch 16→4 all left the query at 5.03-5.11s.
//!
//! For scale: DiskANN serves a *billion* points at <3ms on a 64GB workstation.
//! At 1.95M rows this index is ~700x off what the hardware allows. That is a
//! maintenance gap, not a reason to shard.
//!
//! # What this is not
//!
//! Not a substitute for `build_indexes` — [`CorpusIndex::optimize`] folds
//! unindexed fragments into indexes that already exist; it does not create a
//! missing one.

use lancedb::table::{CompactionOptions, OptimizeAction};

use super::CorpusIndex;
use crate::{Error, Result};

/// What one maintenance pass actually did. Every field is reported rather than
/// summarised to a boolean: an operator running this against a 15GB corpus
/// needs to see which of the three phases moved, because they fail and pay off
/// independently (ARCH §0.1).
#[derive(Debug, Clone, Default)]
pub struct MaintenanceStats {
    /// Fragments removed by compaction (small files merged into larger ones).
    pub fragments_removed: usize,
    /// Fragments written by compaction.
    pub fragments_added: usize,
    /// Files removed by version pruning.
    pub old_versions_removed: u64,
    /// Bytes reclaimed by version pruning.
    pub bytes_removed: u64,
    /// Whether index optimization ran without error.
    pub indexes_optimized: bool,
    /// Rows sitting OUTSIDE the indexes before this pass — summed across every
    /// index on the table. This is the number that explains a slow search:
    /// lancedb answers a query by running the index over indexed data AND a
    /// FLAT SCAN over everything else, then merging (`Table::optimize` docs).
    /// A flat scan ignores every ANN parameter, which is why `nprobes` /
    /// `refine_factor` / overfetch ablations all came back flat at 5.03-5.11s
    /// on wikipedia before this landed.
    pub unindexed_rows_before: usize,
    /// True when the pass declined to touch the indexes because there was
    /// nothing outside them and compaction moved nothing.
    pub skipped_as_clean: bool,
}

impl CorpusIndex {
    /// Rows outside the indexes, summed over every index on the table.
    ///
    /// This is the health signal for an appended corpus: lancedb serves a query
    /// by running the index over indexed data AND flat-scanning everything
    /// else, so this number IS the size of the flat scan every search pays.
    /// Cheap — a metadata read, no data scan — which is what lets the daemon's
    /// maintenance sweep ask it of every corpus on every cycle.
    ///
    /// Best-effort by design: a table with no indexes, or a stats call that
    /// fails, yields 0 — which routes to "nothing to fold in", the
    /// conservative answer for a maintenance gate. It is never used to claim an
    /// index IS healthy, only to decline unnecessary work.
    pub async fn unindexed_rows_estimate(&self) -> usize {
        let Ok(indices) = self.table.list_indices().await else {
            return 0;
        };
        let mut total = 0usize;
        for idx in indices {
            if let Ok(Some(s)) = self.table.index_stats(&idx.name).await {
                total = total.saturating_add(s.num_unindexed_rows);
            }
        }
        total
    }

    /// Compact fragments, fold unindexed fragments into existing indexes, and
    /// prune superseded versions.
    ///
    /// Ordering is load-bearing and is the reason this is one method rather
    /// than three: compaction rewrites fragments, so indexes are optimized
    /// AFTER it (optimizing first would index fragments that compaction then
    /// replaces), and pruning runs LAST so it reclaims the versions both
    /// earlier phases superseded.
    ///
    /// `prune_older_than_days` guards the destructive phase. Lance's own
    /// cleanup refuses to delete versions newer than the threshold because a
    /// concurrent reader may hold them; passing a small value on a live dataset
    /// is how you break an in-flight query. `None` skips pruning entirely —
    /// compaction and index optimization alone are non-destructive (they add a
    /// new version; the old ones remain readable).
    ///
    /// Each phase is attempted independently and a failure is returned rather
    /// than swallowed — a partially-maintained dataset is a legitimate outcome
    /// and the caller must be able to say which phase stopped (ARCH §18.3: an
    /// `Err` is never collapsed into a success-shaped value).
    pub async fn optimize(&self, prune_older_than_days: Option<i64>) -> Result<MaintenanceStats> {
        let mut stats = MaintenanceStats::default();

        // 1. Compaction — merge the many small fragments incremental ingest
        //    leaves behind.
        let compaction = self
            .table
            .optimize(OptimizeAction::Compact {
                options: CompactionOptions::default(),
                remap_options: None,
            })
            .await
            .map_err(|e| Error::Database(format!("compact {}: {e}", self.corpus_id)))?;
        if let Some(c) = compaction.compaction {
            stats.fragments_removed = c.fragments_removed;
            stats.fragments_added = c.fragments_added;
        }

        // 2. Index optimization — fold fragments that postdate the index into
        //    it. This is the phase that stops the flat-scan-and-merge path
        //    documented above.
        //
        //    GATED, because this call is NOT idempotent: every invocation
        //    writes new index versions and removes none. Measured 2026-08-05 —
        //    four unconditional passes took wikipedia's `_indices` from 24 to
        //    36 entries / 2.4GB, and one pass took the already-healthy `sep`
        //    from 1 version / 3 indices to 4 / 9 for zero benefit. Since this
        //    is meant to run on a CADENCE against continuously-appended
        //    corpora, an ungated version would compound that every cycle —
        //    maintenance that degrades what it maintains.
        stats.unindexed_rows_before = self.unindexed_rows_estimate().await;
        let worth_indexing = stats.unindexed_rows_before > 0 || stats.fragments_removed > 0;
        if worth_indexing {
            self.table
                .optimize(OptimizeAction::Index(Default::default()))
                .await
                .map_err(|e| Error::Database(format!("optimize indexes {}: {e}", self.corpus_id)))?;
            stats.indexes_optimized = true;
        } else {
            stats.skipped_as_clean = true;
        }

        // 3. Pruning — DESTRUCTIVE and therefore opt-in. Reclaims the storage
        //    held by superseded manifests (wikipedia carried 3,955 of them).
        if let Some(days) = prune_older_than_days {
            let removal = self
                .table
                .optimize(OptimizeAction::Prune {
                    older_than: Some(lancedb::table::Duration::days(days)),
                    delete_unverified: Some(false),
                    error_if_tagged_old_versions: Some(false),
                })
                .await
                .map_err(|e| Error::Database(format!("prune {}: {e}", self.corpus_id)))?;
            if let Some(p) = removal.prune {
                stats.old_versions_removed = p.old_versions;
                stats.bytes_removed = p.bytes_removed;
            }
        }

        // The cached search gate holds (row_count, ivf_built, fts_built) for
        // one dataset version. Every phase above commits a new one, and phase 2
        // can flip `fts_built`/`ivf_built` coverage — a stale gate here would
        // silently skip a leg of the next search (see the `gate_cache` doc).
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }

        Ok(stats)
    }
}
