// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self-healing corpus maintenance.
//!
//! # Why this is a daemon task and not a CLI verb
//!
//! `svrn corpus optimize` is an operator tool. The person who most needs a
//! healthy corpus is a desktop user who will never open a terminal, and the
//! corpus that degrades fastest is the one they never touch by hand:
//! `wikipedia`, fed continuously by the `wikipedia-newsworthy` freshness
//! daemon. A product whose search quietly halves in speed over months of
//! background appends is broken regardless of how good the CLI is.
//!
//! # What decays, and why it is invisible
//!
//! lancedb answers a query by running the index over indexed data AND a FLAT
//! SCAN over anything appended since the index was built, then merging the two
//! (`Table::optimize` docs, lancedb-0.27.2 `table.rs:667-672`). Nothing about
//! that fails: results stay correct, no error is logged, and every ANN tuning
//! knob keeps reporting healthy values. It only gets slower. Measured on this
//! workspace 2026-08-05, before any maintenance existed:
//!
//! | corpus                    | versions | per-search |
//! |---------------------------|----------|------------|
//! | `wikipedia` (appended)    | 3,955    | **2218ms** |
//! | `sep` (static)            | 1        | 100ms      |
//!
//! One maintenance pass took wikipedia's search from 5.33s to 2.96s end-to-end
//! (-43%). It had never been run because nothing in the workspace called
//! `compact_files` or `optimize_indices` at all.
//!
//! # Shape
//!
//! Cheap-check, rare-act. Every cycle asks each corpus how many rows sit
//! outside its indexes — a metadata read — and does real work only when that
//! number crosses a floor. On an idle corpus a cycle costs a few milliseconds
//! and writes nothing, which matters because the index phase is NOT idempotent
//! (see `corpus_engine::index::maintain`): an unconditional pass adds index
//! versions forever and turns the healer into a leak.

use std::sync::Arc;
use std::time::Duration;

use corpus_engine::{CorpusEngine, Retention};

/// Minutes between sweeps. `0` disables the sweep entirely.
fn interval_mins() -> u64 {
    std::env::var("SOVEREIGN_CORPUS_MAINTENANCE_INTERVAL_MINS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

/// Rows-outside-the-index needed before a corpus is worth rewriting.
///
/// Not zero: folding an index costs seconds-to-minutes on a large corpus, and
/// a handful of stragglers is not worth it — the flat scan over a few hundred
/// rows is genuinely cheap. The cost only becomes visible in the thousands.
fn unindexed_floor() -> usize {
    std::env::var("SOVEREIGN_CORPUS_MAINTENANCE_UNINDEXED_FLOOR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5_000)
}

/// Reader-safety floor: no version younger than this is ever deleted.
///
/// Was 7 days until 2026-08-31, defended as "far outside any live query and
/// still bounds the growth". The first clause was true and the second was not,
/// and the second is the one the default was carrying. `wikipedia` writes ~850
/// manifest versions a day; measured that day it held 5,972 of them and NOT
/// ONE was older than seven days, so the prune phase had zero eligible targets
/// while 153.9GB of superseded fragments sat on disk. An age cannot bound a
/// store whose write rate outruns it — that is now `keep_versions`' job, and
/// this knob went back to being only what it can actually guarantee.
///
/// One day is still ~10^5 times the longest read that holds a version open (a
/// query is sub-second; the longest observed index-holding job is an ~11h
/// enrichment pass). `0` disables pruning and retains every version.
fn prune_days() -> i64 {
    std::env::var("SOVEREIGN_CORPUS_MAINTENANCE_PRUNE_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

/// Manifest versions a corpus may retain before the sweep reclaims, regardless
/// of age.
///
/// The bound that `prune_days` alone could not provide. Deliberately generous:
/// this is a ceiling that catches unbounded accumulation, not a target to
/// hold corpora at. Reclamation still never deletes anything younger than
/// `prune_days` (see `corpus_engine::index::maintain::Retention`), so the
/// directory settles at whatever the corpus writes in that window — for
/// wikipedia, ~850 versions rather than ~6,000. `0` disables the count bound
/// and leaves age as the only limit, which is the behaviour that leaked.
fn keep_versions() -> Option<usize> {
    match std::env::var("SOVEREIGN_CORPUS_MAINTENANCE_KEEP_VERSIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(1_000),
    }
}

/// Spawn the supervised self-healing sweep.
///
/// Supervised because a panic here is silent by construction: the failure mode
/// it prevents produces no error, only gradual slowness, so a dead sweep looks
/// exactly like a healthy one (DAEMON_RESILIENCE.md P0.4). Each cycle is
/// independent and idempotent, so a restart loses nothing.
pub(crate) fn spawn(engine: Arc<CorpusEngine>) {
    crate::supervise::spawn_supervised("corpus_maintenance_sweep", move || {
        let engine = Arc::clone(&engine);
        async move {
            let mins = interval_mins();
            if mins == 0 {
                tracing::info!(
                    target: "corpus_maintenance",
                    "corpus maintenance sweep DISABLED (interval=0) — appended corpora will \
                     accumulate unindexed rows and searches will slow over time"
                );
                return;
            }
            let floor = unindexed_floor();
            let prune = prune_days();
            let keep = keep_versions();
            // Two bounds, logged separately, because they gate two different
            // phases and a reader of this line needs to know which one is
            // slack. `prune_days=7 keep_versions=none` — the shipped pair
            // until 2026-08-31 — reclaims nothing on an appended corpus.
            tracing::info!(
                target: "corpus_maintenance",
                interval_mins = mins,
                unindexed_floor = floor,
                prune_days = prune,
                keep_versions = ?keep,
                "corpus maintenance sweep armed"
            );

            // Let ingest, index loading and model residency settle first — the
            // sweep is never urgent and must not compete with a cold start.
            tokio::time::sleep(Duration::from_secs(120)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(mins * 60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                sweep_once(&engine, floor, prune, keep).await;
            }
        }
    });
}

async fn sweep_once(
    engine: &Arc<CorpusEngine>,
    floor: usize,
    prune: i64,
    keep: Option<usize>,
) {
    // `None` only when pruning is disabled outright; otherwise every corpus
    // gets the same policy and the per-corpus decision below is purely about
    // whether it has earned each phase.
    let retention = (prune > 0).then_some(Retention {
        min_age_days: prune,
        keep_versions: keep,
    });
    let cycle_started = std::time::Instant::now();
    let indexes = match engine.installed_indexes().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "corpus_maintenance", error = %e, "sweep: list indexes failed");
            return;
        }
    };
    let checked = indexes.len();
    let mut acted = 0usize;
    let mut failed = 0usize;
    // The largest backlog seen this cycle, so the INFO summary below can show
    // the approach to the floor rather than only the crossing of it.
    let mut max_unindexed = 0usize;
    // The reclamation counterpart, for the same reason: an operator watching
    // this climb between cycles can see the version bound approaching before
    // it is crossed.
    let mut max_versions = 0usize;
    for info in indexes {
        // TRANSIENT open, deliberately. `open_index` populates a
        // never-evicted query-path cache; this loop visits EVERY
        // installed corpus every cycle, so going through it made one
        // sweep tick enough to pin every LanceDB handle on the box
        // resident for the life of the process — regardless of whether
        // anything ever queried that corpus. A cache hit is still
        // served from the cache (free); only the insert is suppressed.
        // See `CorpusEngine::open_index_transient` and
        // `MESH_SCALE_100_USERS_1000_CORPORA.md` §7.4 item 7.
        let idx = match engine.open_index_transient(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    target: "corpus_maintenance",
                    corpus = %info.corpus_id,
                    error = %e,
                    "sweep: open_index failed"
                );
                continue;
            }
        };

        // The cheap question, asked every cycle. `optimize` re-checks this
        // itself and will decline the index phase — asking here too keeps the
        // no-op path free of even a compaction attempt, and gives the operator
        // a number to watch climb between sweeps.
        let unindexed = idx.unindexed_rows_estimate().await;
        max_unindexed = max_unindexed.max(unindexed);

        // GATE 1 — the EXPENSIVE phases (compact + index fold). Unchanged:
        // folding costs seconds-to-minutes on a large corpus and is earned by
        // rows sitting outside the index.
        let fold = unindexed >= floor;

        // GATE 2 — the CHEAP phase (prune), and INDEPENDENT of gate 1. This
        // separation is the fix, and the bug it closes is not that the floor
        // was too high; it is that gate 1 was held shut BY the very subsystem
        // producing the versions. `newsworthy_watcher` folds each tick's
        // writes into the index immediately so search never degrades, which
        // pins `unindexed` permanently below the floor — measured
        // 2026-08-31: `max_unindexed=3153 floor=5000 acted=0`, cycle after
        // cycle, while wikipedia's directory reached 207GB holding 12.3GB of
        // live data. Pruning is a metadata delete earned by ACCUMULATED
        // VERSIONS, which is a number gate 1 says nothing about, so it now
        // asks its own question. That question is one metadata listing — the
        // same budget as the row estimate above (ARCH §10.6: two decisions,
        // two deciders, rather than one threshold standing in for both).
        let versions = if retention.is_some() {
            idx.version_count().await
        } else {
            0
        };
        max_versions = max_versions.max(versions);
        let reclaim = retention
            .and_then(|r| r.keep_versions)
            .is_some_and(|k| versions > k);

        if !fold && !reclaim {
            tracing::debug!(
                target: "corpus_maintenance",
                corpus = %info.corpus_id,
                unindexed,
                floor,
                versions,
                "sweep: below both gates — skipping"
            );
            continue;
        }

        tracing::info!(
            target: "corpus_maintenance",
            corpus = %info.corpus_id,
            unindexed,
            floor,
            versions,
            fold,
            reclaim,
            "sweep: corpus has earned maintenance"
        );
        let started = std::time::Instant::now();
        let outcome = match (fold, retention) {
            // Full pass. The compact -> index -> prune ordering is
            // load-bearing and stays inside `optimize`.
            (true, r) => idx.optimize(r).await,
            // Reclaim only — skips the two expensive phases this corpus has
            // not earned. The whole point of the split.
            (false, Some(r)) => idx.prune(&r).await,
            // Unreachable: `reclaim` cannot be true without a policy, and we
            // continued above when neither gate opened. A branch rather than
            // an `expect` so a future edit to the gates degrades into a
            // skipped corpus, never a panicking daemon.
            (false, None) => continue,
        };
        match outcome {
            Ok(stats) => {
                acted += 1;
                tracing::info!(
                    target: "corpus_maintenance",
                    corpus = %info.corpus_id,
                    unindexed_before = stats.unindexed_rows_before,
                    fragments_removed = stats.fragments_removed,
                    fragments_added = stats.fragments_added,
                    indexes_optimized = stats.indexes_optimized,
                    versions_pruned = stats.old_versions_removed,
                    bytes_reclaimed = stats.bytes_removed,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "sweep: maintenance complete"
                );
            }
            Err(e) => {
                failed += 1;
                // Never fatal: a corpus that fails maintenance is slow, not
                // broken, and the next cycle retries it.
                tracing::warn!(
                    target: "corpus_maintenance",
                    corpus = %info.corpus_id,
                    error = %e,
                    "sweep: maintenance failed — corpus stays queryable but unoptimized"
                );
            }
        }
    }
    // UNCONDITIONAL, and at INFO. Do not re-gate this on `acted > 0`.
    //
    // The healthy case for this sweep is doing nothing, and the per-corpus
    // "below floor" line above is `debug!` — which the shipped daemon never
    // emits, because the launchd plist sets no `RUST_LOG`. Gated on `acted`,
    // the entire subsystem was therefore invisible in production between the
    // one-time "armed" line and the first corpus that crossed the floor
    // (potentially never). A wedged loop, a sweep that silently stopped
    // ticking, and a perfectly healthy one all looked identical — which is
    // the exact failure `spawn_supervised` is here to prevent, reintroduced
    // one layer up. `max_unindexed` is the number to watch climb toward
    // `floor` between cycles. One line per corpus per hour is not a budget.
    tracing::info!(
        target: "corpus_maintenance",
        checked,
        acted,
        failed,
        max_unindexed,
        floor,
        max_versions,
        elapsed_ms = cycle_started.elapsed().as_millis() as u64,
        "sweep: cycle complete"
    );
}
