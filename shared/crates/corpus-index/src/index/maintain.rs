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

use chrono::{DateTime, Utc};
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

/// How much version history a corpus keeps.
///
/// # Why two bounds and not one
///
/// Retention was expressed only as an age until 2026-08-31, and on a
/// continuously-appended corpus an age bounds nothing. Measured that day on
/// `wikipedia`: 5,972 manifest versions spanning Aug 24-31, of which **zero**
/// were older than the 7-day default — so the prune phase had no eligible
/// target while 153.9GB of superseded fragments sat under versions the window
/// could not reach (12.3GB of the 166GB `data/` directory was live). A corpus
/// writing ~850 versions a day outruns any window you can safely set.
///
/// The two bounds answer different questions and are not interchangeable:
///
/// - `min_age_days` is READER SAFETY. Lance refuses to delete a version a
///   concurrent reader may still hold; this is that guarantee.
/// - `keep_versions` is SPACE. It bounds the directory for a corpus whose
///   writes are slow enough that age alone would retain them forever.
///
/// # The limit this cannot cross
///
/// `keep_versions` can only ever delete MORE history, never YOUNGER history
/// (see [`Retention::cutoff_days`]). So the smallest reachable directory is
/// whatever the corpus writes during `min_age_days`. Bounding space below that
/// would mean deleting a version an in-flight reader may hold — a corrupted
/// query, not a smaller disk. A corpus still over budget with both bounds set
/// needs a shorter `min_age_days`, justified against the longest read that
/// holds a version open, NOT a smaller `keep_versions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Never delete a version younger than this, in days. Reader safety.
    pub min_age_days: i64,
    /// Never retain more than this many versions. `None` = no count bound.
    pub keep_versions: Option<usize>,
}

impl Retention {
    /// The prune cutoff in days: versions older than this are deleted.
    ///
    /// `timestamps` are the corpus's version timestamps, NEWEST FIRST.
    ///
    /// ONE DECIDER (ARCH §10.6). Both bounds resolve to a single number here
    /// and nowhere else, so there is no second site where "how old is old
    /// enough" can be answered differently.
    ///
    /// Pure, and takes plain timestamps rather than lance's `Version`, so the
    /// policy is testable without a dataset — the arithmetic is the part that
    /// was wrong, and it should not need 200GB on disk to exercise.
    pub fn cutoff_days(&self, timestamps: &[DateTime<Utc>], now: DateTime<Utc>) -> i64 {
        // Keep the newest `keep`; the newest version we may delete is the one
        // at index `keep`, so the cutoff can be no larger than its own age.
        let by_count = self
            .keep_versions
            .and_then(|keep| timestamps.get(keep))
            .map(|cutoff| (now - *cutoff).num_days());

        // max(), because both bounds are floors on SAFETY, never on
        // aggression: a LARGER cutoff deletes strictly less. The count bound
        // may reach FURTHER BACK than the age floor; it may never reach
        // NEARER. Swapping this for min() is how you delete a version an
        // in-flight reader is holding.
        match by_count {
            Some(d) => d.max(self.min_age_days),
            None => self.min_age_days,
        }
    }

    /// Is any superseded version actually eligible for deletion?
    ///
    /// THE RECLAMATION GATE, and it asks [`Self::cutoff_days`] rather than a
    /// question of its own. ONE DECIDER (ARCH §8): "how old is old enough" is
    /// answered in exactly one place, and the gate that decides whether to
    /// prune now resolves it the same way the prune itself will. Two sites
    /// answering that differently is the defect this replaces.
    ///
    /// # Why the gate was a version COUNT, and why that leaked
    ///
    /// Until 2026-09-11 the daemon opened this gate on `versions >
    /// keep_versions` — a count, against a shipped ceiling of 1,000.
    /// A count is not a cost. Before compaction worked, a version pinned one
    /// small append and 1,000 of them were cheap; after it worked, a version
    /// pins a whole compacted fragment and the same count means two orders of
    /// magnitude more disk. Measured on `wikipedia` this day: 926 versions —
    /// comfortably UNDER the ceiling, so the gate never opened — holding
    /// 107.4GB of superseded fragments and 31.3GB of dead index directories
    /// against 12.4GB of live data, in a 152GB dataset that had not been
    /// written since the day before.
    ///
    /// The corpus was quiet, so gate 1 (`unindexed >= floor`) was shut too.
    /// Both gates shut means `sweep: below both gates — skipping`, every cycle,
    /// forever: a corpus that stops being appended stops being reclaimed and
    /// freezes with its entire garbage backlog on disk. That is the shape any
    /// user with a shipped `wikipedia` reaches, not a property of this machine.
    ///
    /// # Why this converges instead of running every cycle
    ///
    /// `timestamps[0]` is the CURRENT version, which lance never deletes, so
    /// it is excluded: a static corpus whose single version is a year old has
    /// nothing to reclaim and must not re-open this gate hourly for a prune
    /// that can remove nothing. After a successful prune every surviving
    /// superseded version is younger than the cutoff, the gate closes, and it
    /// re-opens only when new writes age past it — at most one prune per
    /// corpus per `min_age_days`.
    ///
    /// `timestamps` are NEWEST FIRST, the same contract as
    /// [`Self::cutoff_days`]. Empty or single-version input yields `false`,
    /// which routes to "nothing to reclaim" — the conservative answer for a
    /// gate, and the same best-effort contract as
    /// [`CorpusIndex::version_timestamps`].
    pub fn reclaimable(&self, timestamps: &[DateTime<Utc>], now: DateTime<Utc>) -> bool {
        let cutoff = self.cutoff_days(timestamps, now);
        timestamps
            .get(1..)
            .is_some_and(|superseded| superseded.iter().any(|t| (now - *t).num_days() >= cutoff))
    }
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

    /// This corpus's manifest version timestamps, NEWEST FIRST.
    ///
    /// The health signal for RECLAMATION, and deliberately a different reading
    /// from [`Self::unindexed_rows_estimate`], which is the health signal for
    /// SEARCH SPEED. Conflating them is what leaked 154GB: see [`Self::prune`].
    ///
    /// Timestamps rather than a count, because a count cannot answer the gate's
    /// question — see [`Retention::reclaimable`] for the 138.7GB that ruling on
    /// the count alone retained. `.len()` still gives the count, for telemetry.
    ///
    /// Cheap — a metadata listing, no data scan — the same contract that lets
    /// the daemon sweep ask `unindexed_rows_estimate` of every corpus every
    /// cycle. Best-effort by design: a listing that fails yields an empty vec,
    /// which routes to "nothing to reclaim". Never used to claim a corpus IS
    /// healthy, only to decline unnecessary work.
    pub async fn version_timestamps(&self) -> Vec<DateTime<Utc>> {
        match self.table.list_versions().await {
            Ok(v) => {
                let mut ts: Vec<DateTime<Utc>> = v.iter().map(|x| x.timestamp).collect();
                ts.sort_unstable_by(|a, b| b.cmp(a));
                ts
            }
            Err(e) => {
                // The 0 is the documented best-effort contract above, but it
                // must not be SILENT. 0 routes to "nothing to reclaim", so a
                // corpus whose metadata listing keeps failing is a corpus the
                // reclaimer keeps declining — the same shape as the leak this
                // sweep exists to close, and previously invisible because the
                // only other signal is a `debug!` the shipped daemon does not
                // emit (§9.1, §18.3).
                tracing::warn!(
                    error = %e,
                    "version_timestamps: listing versions failed — reporting none, so \
                     this corpus will not be reclaimed this cycle"
                );
                Vec::new()
            }
        }
    }

    /// Delete superseded manifest versions and the fragments they alone hold.
    ///
    /// DESTRUCTIVE, and split out of [`Self::optimize`] deliberately: the two
    /// phases answer to different signals. Folding an index is expensive and
    /// is earned by rows sitting outside it; pruning is a metadata delete and
    /// is earned by accumulated versions. Gating both on the FIRST number is
    /// what let `wikipedia` reach 5,972 versions and 207GB on disk.
    ///
    /// The failure is worth stating because neither subsystem is wrong alone.
    /// `newsworthy_watcher` folds every tick's writes into the index
    /// immediately, on purpose, so search never degrades — which holds
    /// unindexed rows permanently BELOW the sweep's floor. The sweep then
    /// never fires, so nothing prunes. Measured 2026-08-31, cycle after cycle:
    /// `max_unindexed=3153 floor=5000 acted=0`, while `data/` held 153.9GB of
    /// fragments no live version referenced. The producer of versions was
    /// suppressing the exact signal the reclaimer waited on.
    pub async fn prune(&self, retention: &Retention) -> Result<MaintenanceStats> {
        let mut stats = MaintenanceStats::default();
        let versions = self
            .table
            .list_versions()
            .await
            .map_err(|e| Error::Database(format!("list versions {}: {e}", self.corpus_id)))?;

        let mut timestamps: Vec<DateTime<Utc>> = versions.iter().map(|v| v.timestamp).collect();
        timestamps.sort_unstable_by(|a, b| b.cmp(a));
        let days = retention.cutoff_days(&timestamps, Utc::now());

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
        Ok(stats)
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
    /// `retention` guards the destructive phase — see [`Retention`] for why it
    /// carries two bounds rather than an age alone. `None` skips pruning
    /// entirely: compaction and index optimization are non-destructive on
    /// their own (they add a new version; the old ones stay readable). A
    /// caller that folds on a cadence and leaves reclamation to the sweep
    /// passes `None` deliberately — `newsworthy_watcher` does.
    ///
    /// A caller that wants ONLY reclamation calls [`Self::prune`] directly
    /// rather than passing a retention here, because this method's first two
    /// phases are the expensive ones and are earned by a different signal.
    ///
    /// Each phase is attempted independently and a failure is returned rather
    /// than swallowed — a partially-maintained dataset is a legitimate outcome
    /// and the caller must be able to say which phase stopped (ARCH §18.3: an
    /// `Err` is never collapsed into a success-shaped value).
    pub async fn optimize(&self, retention: Option<Retention>) -> Result<MaintenanceStats> {
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
                .map_err(|e| {
                    Error::Database(format!("optimize indexes {}: {e}", self.corpus_id))
                })?;
            stats.indexes_optimized = true;
        } else {
            stats.skipped_as_clean = true;
        }

        // 3. Pruning — DESTRUCTIVE and therefore opt-in. Reclaims the storage
        //    held by superseded manifests (wikipedia carried 5,972 of them).
        //    Delegated so that the sweep can reclaim WITHOUT paying phases 1
        //    and 2, which is the split that stops the leak (see `prune`).
        if let Some(r) = retention {
            let pruned = self.prune(&r).await?;
            stats.old_versions_removed = pruned.old_versions_removed;
            stats.bytes_removed = pruned.bytes_removed;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    /// Version timestamps, newest first, one per hour going back.
    fn hourly(now: DateTime<Utc>, count: usize) -> Vec<DateTime<Utc>> {
        (0..count)
            .map(|i| now - ChronoDuration::hours(i as i64))
            .collect()
    }

    /// THE REGRESSION. `wikipedia` on 2026-08-31: 5,972 versions, all written
    /// within the last seven days, against the shipped `min_age_days = 7`.
    /// Zero were eligible, so `--prune-days 7` reported success having deleted
    /// nothing while the directory held 153.9GB of unreferenced fragments.
    #[test]
    fn an_age_alone_cannot_bound_a_corpus_that_outruns_the_window() {
        let now = Utc::now();
        // The measured shape, and the whole point: 5,972 versions compressed
        // into SEVEN DAYS, ~101 seconds apart. Spreading the same count over
        // months would make the age window work fine — it is the write RATE
        // that defeats it.
        let versions: Vec<_> = (0..5972)
            .map(|i| now - ChronoDuration::seconds(i * 101))
            .collect();
        let age_only = Retention {
            min_age_days: 7,
            keep_versions: None,
        };
        assert_eq!(
            age_only.cutoff_days(&versions, now),
            7,
            "an age-only policy can only ever return its own age"
        );
        // Every version is younger than the cutoff, so nothing is eligible —
        // which is exactly the no-op that shipped.
        assert!(
            versions.iter().all(|t| (now - *t).num_days() < 7),
            "the whole retained history is younger than the window meant to bound it"
        );
    }

    /// The count bound reaches back further than the age floor when the corpus
    /// has outrun it — this is the fix.
    #[test]
    fn the_count_bound_reaches_past_the_age_floor_when_versions_pile_up() {
        let now = Utc::now();
        // 200 versions/day for 10 days.
        let versions: Vec<_> = (0..2000)
            .map(|i| now - ChronoDuration::minutes(i * 7))
            .collect();
        let r = Retention {
            min_age_days: 1,
            keep_versions: Some(500),
        };
        // The 500th-newest is ~2.4 days back, so the cutoff moves out to 2
        // days rather than sitting at the 1-day floor.
        assert_eq!(r.cutoff_days(&versions, now), 2);
    }

    /// max(), not min(). A count bound must never delete a version YOUNGER
    /// than the reader-safety floor, however aggressive the count.
    #[test]
    fn the_count_bound_never_deletes_younger_than_the_safety_floor() {
        let now = Utc::now();
        let versions = hourly(now, 1000);
        let r = Retention {
            min_age_days: 7,
            // Aggressive enough that the 10th-newest is ten hours old.
            keep_versions: Some(10),
        };
        assert_eq!(
            r.cutoff_days(&versions, now),
            7,
            "reader safety wins: a ten-hour-old version is not deletable under a 7-day floor"
        );
    }

    /// A corpus holding fewer versions than the bound has no count-derived
    /// cutoff at all, and falls through to the age floor.
    #[test]
    fn a_corpus_under_the_count_bound_falls_through_to_the_age_floor() {
        let now = Utc::now();
        let versions = hourly(now, 5);
        let r = Retention {
            min_age_days: 3,
            keep_versions: Some(500),
        };
        assert_eq!(r.cutoff_days(&versions, now), 3);
    }

    /// An empty history is not a licence to delete: no versions means no
    /// count-derived cutoff, and the floor stands.
    #[test]
    fn an_empty_version_list_yields_the_age_floor() {
        let now = Utc::now();
        let r = Retention {
            min_age_days: 2,
            keep_versions: Some(1),
        };
        assert_eq!(r.cutoff_days(&[], now), 2);
    }

    /// The shipped daemon policy, so these cases are about the real gate and
    /// not a shape chosen to pass. `prune_days()` = 1, `keep_versions()` = 1000.
    fn shipped() -> Retention {
        Retention {
            min_age_days: 1,
            keep_versions: Some(1_000),
        }
    }

    /// THE REGRESSION. `wikipedia` on 2026-09-11: 926 manifest versions
    /// spanning Sep 8 13:40 - Sep 10 23:14 (57.6h), against the shipped
    /// ceiling of 1,000. The count gate asked `versions > keep_versions`, got
    /// `926 > 1000` = false, and never opened — while the dataset held 107.4GB
    /// of superseded fragments and 31.3GB of dead index directories against
    /// 12.4GB of live data. Gate 1 was shut too (nothing appended since the
    /// day before), so the sweep skipped the corpus every cycle, forever.
    #[test]
    fn a_quiet_corpus_under_the_count_ceiling_is_still_reclaimable() {
        let now = Utc::now();
        // The measured shape: 926 versions over 57.6 hours.
        let versions: Vec<_> = (0..926)
            .map(|i| now - ChronoDuration::minutes(i * 3736 / 926))
            .collect();
        let r = shipped();
        assert!(
            !(versions.len() > r.keep_versions.unwrap()),
            "the count gate that shipped: 926 versions is UNDER the 1,000 ceiling"
        );
        assert!(
            r.reclaimable(&versions, now),
            "and yet two days of superseded versions are past the 1-day cutoff"
        );
    }

    /// The reason the gate cannot simply be "always prune": `timestamps[0]` is
    /// the current version and lance never deletes it, so a static corpus whose
    /// one version is ancient would re-open the gate every cycle for a prune
    /// that can remove nothing.
    #[test]
    fn a_static_corpus_with_one_ancient_version_never_reopens_the_gate() {
        let now = Utc::now();
        let versions = vec![now - ChronoDuration::days(365)];
        assert!(!shipped().reclaimable(&versions, now));
        assert!(
            !shipped().reclaimable(&[], now),
            "nor does an empty listing"
        );
    }

    /// Reader safety, restated as the gate sees it: superseded versions exist,
    /// but all of them are inside the `min_age_days` window, so there is
    /// nothing this policy may delete and the gate stays shut.
    #[test]
    fn superseded_versions_inside_the_safety_window_do_not_open_the_gate() {
        let now = Utc::now();
        let versions = hourly(now, 12); // twelve versions, all < 1 day old
        assert!(!shipped().reclaimable(&versions, now));
    }

    /// CONVERGENCE. After a prune, everything left is younger than the cutoff,
    /// so the gate closes and stays closed until new writes age past it — at
    /// most one prune per corpus per `min_age_days`, not one per sweep cycle.
    #[test]
    fn the_gate_closes_after_a_prune_and_reopens_only_on_age() {
        let now = Utc::now();
        let after_prune = hourly(now, 20); // all within the last day
        assert!(!shipped().reclaimable(&after_prune, now));
        // A day later, the same history has aged past the floor.
        let later = now + ChronoDuration::days(1) + ChronoDuration::hours(1);
        assert!(shipped().reclaimable(&after_prune, later));
    }

    /// The gate and the prune answer to ONE decider (ARCH §8): whenever
    /// `reclaimable` opens, some superseded version is at or past the very
    /// cutoff `prune` will pass to lance — the gate can never open on a
    /// question the prune would answer differently.
    #[test]
    fn the_gate_agrees_with_the_cutoff_the_prune_will_use() {
        let now = Utc::now();
        for count in [2usize, 50, 926, 2000] {
            for min_age_days in [1i64, 7] {
                let versions: Vec<_> = (0..count)
                    .map(|i| now - ChronoDuration::minutes(i as i64 * 4))
                    .collect();
                let r = Retention {
                    min_age_days,
                    keep_versions: Some(1_000),
                };
                let cutoff = r.cutoff_days(&versions, now);
                let eligible = versions[1..]
                    .iter()
                    .any(|t| (now - *t).num_days() >= cutoff);
                assert_eq!(
                    r.reclaimable(&versions, now),
                    eligible,
                    "count={count} min_age_days={min_age_days} cutoff={cutoff}"
                );
            }
        }
    }
}
