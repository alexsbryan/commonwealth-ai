// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! `in_progress_ingestions()` already does the right filtering for
//! OWNERSHIP: it returns `<corpus>` when `<corpus>-partition-<self>/`
//! has `ingestion_in_progress=true`, and explicitly skips `<corpus>-
//! partition-<peer>` paths (`engine/mod.rs:718`).
//!
//! Ownership is not viability, though. An earlier version of this
//! docstring claimed whatever `in_progress_ingestions()` returns is
//! "by construction safe to re-spawn"; that is false, and the way it
//! is false costs the whole machine. `ingestion_in_progress=true` is
//! also what a *failed* ingest leaves behind, so the set includes
//! corpora that cannot complete — and re-spawning one of those
//! saturates every core on every boot, forever. Hence the third gate
//! in the loop below: see `watched_folder_errored`.

use commonwealth_api::state::AppState;
use sovereign_tools::local_corpus::watched::state::WatchedFolderState;
use sovereign_tools::local_corpus::watched::status::WatchedFolderStatus;

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

/// True iff the corpus carries a sticky `Errored` watched-folder
/// status — the same condition the sweep scheduler tests before it
/// skips a corpus with `reason=errored` (see "3b. Errored check" in
/// `sovereign-tools/src/local_corpus/watched/worker.rs`).
///
/// Why auto-resume needs the same gate, and needs it more: the
/// scheduler's copy exists to stop it re-firing a broken *sweep*
/// every ~120s, and the cost of omitting it there is log spam. The
/// unit retried HERE is the whole `embed+index` pipeline, which
/// saturates every core for as long as it runs. So a corpus that can
/// never finish re-arms itself on each daemon boot — and because the
/// pipeline only checkpoints `committed_iter_pos` on its first index
/// flush (`INDEX_FLUSH_SIZE`, 2000 chunks), a run that dies before
/// that flush resumes from source document 0 next time. It therefore
/// never converges, and every boot pays full price again.
///
/// Measured 2026-08-05 on a MacBookPro16,1 (8 physical cores,
/// CPU-only embed — the Metal path is aarch64-gated): an Obsidian
/// vault left `ingestion_in_progress=true` by a failed ingest held
/// all 8 cores at ~800% indefinitely, pinned the single embed-slot
/// mutex so `/v1/embeddings` never returned, and starved the
/// desktop's heartbeat until the supervisor killed the daemon for
/// "3 failed heartbeats" — which restarted the same doomed ingest.
/// The app never finished initializing.
///
/// Reads the CANONICAL index dir on purpose: `_watched_folder_state.json`
/// is written there, never to `<corpus>-partition-<node>/`. A missing
/// or unreadable state file is NOT errored — a corpus that was never a
/// watched folder (a Wikipedia install, say) must still auto-resume,
/// which is this hook's whole reason for existing.
fn watched_folder_errored(canonical_dir: &std::path::Path) -> bool {
    matches!(
        WatchedFolderState::load(canonical_dir),
        Ok(Some(state)) if matches!(state.status, WatchedFolderStatus::Errored { .. })
    )
}

#[cfg(test)]
mod watched_folder_gate_tests {
    use super::watched_folder_errored;

    /// Writes the on-disk shape `WatchedFolderState::load` actually
    /// parses. `entries`, `tombstones` and `last_updated_unix` carry
    /// no serde default, so all three must be present even though only
    /// `status` is under test.
    ///
    /// The load assertion at the end is load-bearing, not belt-and-
    /// braces: `watched_folder_errored` fails OPEN, so a fixture that
    /// doesn't parse returns `false` — the same answer the negative
    /// tests below expect. Without this guard a typo'd fixture would
    /// make `idle_status_still_resumes` and `absent_state_still_resumes`
    /// pass while proving nothing. (It did, on first run: the fixture
    /// was missing `last_updated_unix` and only the positive test
    /// caught it.)
    fn write_state(dir: &std::path::Path, status_json: &str) {
        std::fs::write(
            dir.join("_watched_folder_state.json"),
            format!(
                r#"{{"corpus_id":"c","schema_version":1,"status":{status_json},
                     "entries":{{}},"tombstones":[],"last_updated_unix":1785167365}}"#
            ),
        )
        .unwrap();
        assert!(
            matches!(
                super::WatchedFolderState::load(dir),
                Ok(Some(_))
            ),
            "fixture must parse, else the negative assertions below are vacuous"
        );
    }

    /// The failing input this gate exists for, copied from the state
    /// file that livelocked a real install on 2026-08-05.
    #[test]
    fn errored_status_is_gated() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(
            tmp.path(),
            r#"{"kind":"errored","message":"index for 'x' is missing _corpus_meta.json",
                "errored_unix":1785167365}"#,
        );
        assert!(
            watched_folder_errored(tmp.path()),
            "an Errored watched folder must not be auto-resumed"
        );
    }

    /// The gate must be narrow: a healthy watched folder still resumes.
    #[test]
    fn idle_status_still_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(
            tmp.path(),
            r#"{"kind":"idle","last_sweep_unix":0,"live_docs":3,"tombstones":0}"#,
        );
        assert!(!watched_folder_errored(tmp.path()));
    }

    /// The regression that would matter most if this gate over-reached:
    /// a corpus that was never a watched folder (a Wikipedia install)
    /// has no state file at all, and resuming those is the entire
    /// reason this hook exists.
    #[test]
    fn absent_state_still_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!watched_folder_errored(tmp.path()));
    }

    /// Fail OPEN on a corrupt sidecar: we cannot prove the corpus is
    /// doomed, and refusing to resume every ingest behind one bad JSON
    /// file would be a worse failure than the one this gate prevents.
    #[test]
    fn unparseable_state_still_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("_watched_folder_state.json"), "{not json").unwrap();
        assert!(!watched_folder_errored(tmp.path()));
    }
}

/// Fire the resume scan in the background. Returns immediately;
/// per-corpus work is offloaded to `spawn_corpus_install`'s own
/// internal `tokio::spawn`. Logged at `info` so the operator can
/// confirm via `journalctl --user -u sovereign` (Linux) or
/// `~/.svrnmesh/logs/daemon.log` (macOS) that resume actually fired.
pub fn spawn_resume_in_progress_ingests(state: AppState) {
    tokio::spawn(async move {
        resume_in_progress_ingests(state).await;
    });
}

fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// The actual scan. Pulled out so a future test can drive it
/// against a fixture engine without depending on `tokio::spawn`
/// timing.
async fn resume_in_progress_ingests(state: AppState) {
    // Operator-facing opt-out — used by `sovereign agent-bench` to
    // stop a half-finished corpus ingest from competing with the
    // bench's chat slot. Default behaviour (env unset / "0" / "false")
    // is unchanged.
    if env_truthy("SOVEREIGN_DISABLE_AUTO_RESUME") {
        tracing::info!("auto_resume: SOVEREIGN_DISABLE_AUTO_RESUME set — skipping ingest resume");
        return;
    }
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

        // Failure gate. A sticky `Errored` watched-folder status means
        // the sweeper has already judged this corpus unworkable and is
        // skipping it every tick. Resuming its ingest here anyway is
        // the two subsystems disagreeing about the same corpus from
        // the same on-disk state — and the disagreement is expensive,
        // not cosmetic (see `watched_folder_errored`). Recovery is
        // user-driven and already documented on the status itself:
        // Pause → fix → Resume, or remove + re-add the folder, both of
        // which clear `Errored` and let this hook resume normally.
        if watched_folder_errored(&engine.canonical_path(&corpus_id)) {
            tracing::warn!(
                corpus = %corpus_id,
                "auto_resume: skipping corpus with a sticky Errored watched-folder \
                 status — the sweep scheduler already skips it (reason=errored), and \
                 resuming its embed+index pipeline would re-saturate every core on \
                 each boot without ever converging. Clear it from Settings → Local \
                 Knowledge (remove + re-add), or via reset_enrichment_state."
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
