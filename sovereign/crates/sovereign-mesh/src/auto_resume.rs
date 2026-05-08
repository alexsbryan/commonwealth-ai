//! One-shot startup hook that re-spawns any in-progress corpus
//! ingest the daemon was running before it stopped.
//!
//! Why this exists
//! ---------------
//! Wikipedia ingest is a long-running operation. The pipeline
//! checkpoints every ~60s into `_corpus_meta.json` and
//! `_source_manifest.json`, so resume from the same byte position is
//! free — but *only* if something re-spawns the ingest task. The
//! daemon doesn't do that on its own:
//!
//!   - `routes_internal::spawn_corpus_install` fires on
//!     `POST /internal/corpus/install` from the desktop or CLI.
//!   - `auto_ingest::spawn_auto_collaborate_loop` fires when peers
//!     appear or a new in-progress ingest is detected, but only
//!     dispatches *peer collaboration* — it doesn't restart a solo
//!     local task that died with the daemon.
//!
//! So a daemon restart in the middle of a Wikipedia ingest leaves the
//! on-disk state saying "in progress" while no actual work is
//! happening. The desktop's progress poller reads the on-disk shape
//! and reports `embedding`-phase + a "Resuming…" message but nothing
//! advances. The user has to re-click Install to wake it up.
//!
//! This module plugs that gap. At daemon startup, after AppState is
//! wired, we enumerate `engine.in_progress_ingestions()` once and call
//! `spawn_corpus_install` for each that has local source data. The
//! `has_local_source` guard mirrors the one in `auto_ingest::dispatch_*`
//! so we don't accidentally fire a fresh download on a peer-only
//! partition node.
//!
//! Idempotency: `spawn_corpus_install` already short-circuits when
//! the corpus is in `active_ingests`. So if the desktop also POSTs
//! Install (e.g. user clicked it just before the restart finished),
//! only one task spawns.
//!
//! Why no `has_local_source` guard
//! -------------------------------
//! Earlier versions of this hook mirrored `auto_ingest::dispatch_*`'s
//! `engine.source_manifest(id).is_some() || engine.count_jsonl_articles(id).is_ok()`
//! check. That gate exists in the auto-collaborate loop to skip
//! *peer-only* nodes that hold partitions for someone else's
//! coordinator before they've received source data. It's wrong here.
//!
//! Solo ingests write to `<corpus>-partition-<self-node-id>/`, NOT
//! to the canonical `<corpus>/` path. `source_manifest()` and
//! `count_jsonl_articles()` both read from the canonical path, so
//! they return falsy for an in-progress solo ingest — exactly the
//! case we want to resume. The Wikipedia install bug that motivated
//! this fix surfaced precisely because the guard was rejecting the
//! own partition.
//!
//! `in_progress_ingestions()` already does the right filtering:
//! it returns `<corpus>` when `<corpus>-partition-<self>/` has
//! `ingestion_in_progress=true`, and explicitly skips `<corpus>-
//! partition-<peer>` paths (`engine/mod.rs:718`). So whatever it
//! returns is by construction safe to re-spawn.

use commonwealth_api::state::AppState;

/// How recent a partition's `_corpus_meta.json` mtime must be for
/// auto-resume to consider it possibly-active and skip resuming. The
/// CLI ingest checkpoints meta every few seconds during embedding
/// and on every FTS phase boundary — 60s is comfortably longer than
/// any single inter-checkpoint gap.
const RECENT_ACTIVITY_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// True iff the partition's `_corpus_meta.json` was last modified
/// within [`RECENT_ACTIVITY_WINDOW`]. Tolerates missing files
/// (returns false) and clock skew (returns false on negative durations).
fn partition_recently_active(partition_dir: &std::path::Path) -> bool {
    let meta_path = partition_dir.join("_corpus_meta.json");
    let Ok(meta) = std::fs::metadata(&meta_path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return false;
    };
    elapsed < RECENT_ACTIVITY_WINDOW
}

/// Fire the resume scan in the background. Returns immediately;
/// per-corpus work is offloaded to `spawn_corpus_install`'s own
/// internal `tokio::spawn`. Logged at `info` so the operator can
/// confirm via `journalctl --user -u sovereign` (Linux) or
/// `~/.sovereign/logs/daemon.log` (macOS) that resume actually fired.
pub fn spawn_resume_in_progress_ingests(state: AppState) {
    tokio::spawn(async move {
        resume_in_progress_ingests(state).await;
    });
}

/// The actual scan. Pulled out so a future test can drive it
/// against a fixture engine without depending on `tokio::spawn`
/// timing.
async fn resume_in_progress_ingests(state: AppState) {
    let Some(engine) = state.inner.corpus_engine.clone() else {
        // No engine = nothing to resume. Standalone Commonwealth
        // configurations that disable the corpus surface land here.
        tracing::debug!("auto_resume: no corpus engine — skipping");
        return;
    };

    let in_progress = engine.in_progress_ingestions();
    if in_progress.is_empty() {
        tracing::debug!("auto_resume: no in-progress ingests on disk");
        return;
    }

    tracing::info!(
        count = in_progress.len(),
        corpora = ?in_progress,
        "auto_resume: scanning in-progress ingests for daemon-restart resume"
    );

    for corpus_id in in_progress {
        // Provenance gate. `in_progress_ingestions` already filters
        // out peer partitions for OTHER nodes; what's left is either:
        //   (a) a self-initiated install on this machine — auto-resume.
        //   (b) a `<corpus>-partition-<self>/` we wrote because a
        //       coordinator on another node handed us a unit. Skipping
        //       this case is the entire point of the provenance field:
        //       the coordinator re-issues the handoff if it still wants
        //       the work, and pulling on every restart competes with
        //       foreground inference and undoes pause-from-another-node.
        //
        // Probe both the canonical and partition-of-self meta files;
        // PeerPulled on EITHER is enough to skip. (Solo runs flip the
        // partition's flag because that's the active write target;
        // a peer-pulled coordinator role would be on canonical.)
        let canonical_provenance =
            corpus_engine::read_provenance(&engine.canonical_path(&corpus_id));
        let partition_provenance =
            corpus_engine::read_provenance(&engine.partition_path(&corpus_id));
        if canonical_provenance == corpus_engine::CorpusProvenance::PeerPulled
            || partition_provenance == corpus_engine::CorpusProvenance::PeerPulled
        {
            tracing::info!(
                corpus = %corpus_id,
                "auto_resume: skipping peer-pulled partition — coordinator owns the schedule"
            );
            continue;
        }

        // Activity gate. If the partition's `_corpus_meta.json` was
        // mtime'd within `RECENT_ACTIVITY_WINDOW`, an in-process CLI
        // ingest (e.g. `sovereign code index`) is plausibly still
        // writing to it — and resuming via the daemon would race the
        // CLI's chunks.lance writes and meta updates, leaving the
        // canonical with a half-built meta state (gap A from the
        // 2026-05-07 stress test). The CLI ingest checkpoints every
        // few seconds, so a partition genuinely abandoned > 60s ago
        // is safe to resume; one being actively written is not.
        if partition_recently_active(&engine.partition_path(&corpus_id)) {
            tracing::info!(
                corpus = %corpus_id,
                "auto_resume: skipping recently-active partition — \
                 in-process CLI ingest likely still writing"
            );
            continue;
        }

        // `spawn_corpus_install` is itself idempotent — it
        // short-circuits when the corpus_id is already in
        // `active_ingests`. So even if the desktop races us with its
        // own install POST, exactly one task spawns.
        let spawned = commonwealth_api::routes_internal::spawn_corpus_install(
            state.clone(),
            corpus_id.clone(),
        )
        .await;
        if spawned {
            tracing::info!(
                corpus = %corpus_id,
                "auto_resume: resumed self-initiated ingest after daemon restart"
            );
        } else {
            tracing::info!(
                corpus = %corpus_id,
                "auto_resume: ingest already active — no resume needed"
            );
        }
    }
}
