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

use corpus_engine::CorpusEngine;

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

/// Age below which superseded versions are KEPT.
///
/// Deliberately generous. Compaction is non-destructive — superseded fragments
/// remain readable under old manifests — so without pruning the directory grows
/// forever, which on a desktop is its own product failure. But a threshold near
/// zero can delete a version an in-flight reader still holds. Seven days is far
/// outside any live query and still bounds the growth. `0` disables pruning and
/// keeps every version.
fn prune_days() -> i64 {
    std::env::var("SOVEREIGN_CORPUS_MAINTENANCE_PRUNE_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7)
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
            tracing::info!(
                target: "corpus_maintenance",
                interval_mins = mins,
                unindexed_floor = floor,
                prune_days = prune,
                "corpus maintenance sweep armed"
            );

            // Let ingest, index loading and model residency settle first — the
            // sweep is never urgent and must not compete with a cold start.
            tokio::time::sleep(Duration::from_secs(120)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(mins * 60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                sweep_once(&engine, floor, prune).await;
            }
        }
    });
}

async fn sweep_once(engine: &Arc<CorpusEngine>, floor: usize, prune: i64) {
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
    for info in indexes {
        let idx = match engine.open_index(&info.path).await {
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
        if unindexed < floor {
            tracing::debug!(
                target: "corpus_maintenance",
                corpus = %info.corpus_id,
                unindexed,
                floor,
                "sweep: below floor — skipping"
            );
            continue;
        }

        tracing::info!(
            target: "corpus_maintenance",
            corpus = %info.corpus_id,
            unindexed,
            floor,
            "sweep: corpus has drifted past the floor — maintaining"
        );
        let started = std::time::Instant::now();
        match idx
            .optimize(if prune > 0 { Some(prune) } else { None })
            .await
        {
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
        elapsed_ms = cycle_started.elapsed().as_millis() as u64,
        "sweep: cycle complete"
    );
}
