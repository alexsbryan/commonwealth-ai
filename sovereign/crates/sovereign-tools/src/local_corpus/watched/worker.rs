// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Worker::run_once(corpus_id)` — one full sweep iteration.
//!
//! Sequence:
//!   1. Acquire per-corpus lock (Skipped if held).
//!   2. Load `WatchedFolderState` (or fresh).
//!   3. Skip if paused (manual or guard-tripped).
//!   4. Walk via `walker::walk_folder` (CPU-bound, on `spawn_blocking`).
//!   5. Detect tombstone revivals — reclassify revived files as adds.
//!   6. Compute diff vs. prior entries.
//!   7. Evaluate deletion guard.
//!   8. Apply via `apply::apply_watched_diff`.
//!   9. Record tombstones for this sweep's removals.
//!  10. Expire old tombstones; enforce per-corpus cap.
//!  11. Update state.entries to the new snapshot; transition status
//!      to `Idle`; persist atomically.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::error::{Error, Result};
use uuid::Uuid;

use corpus_engine::CorpusEngine;

use super::apply::apply_watched_diff;
use super::diff::compute_diff;
use super::events::{EventSink, WatchedFolderEvent};
use super::registry::WatchedFolderRegistry;
use super::soft_delete_gc;
use super::state::WatchedFolderState;
use super::status::{DiffSummary, WatchedFolderStatus};
use super::threshold::{DeletionGuard, GuardDecision};
use super::walker;
use super::workflow_trigger::WorkflowTriggerRuntime;
use crate::local_corpus::config::{
    LocalCorpusConfig, LocalCorpusSourceType, ReconcileKind, WatchedFolderConfig,
};
use crate::local_corpus::manager::LocalCorpusManager;

/// Minimum seconds between successive writeback refreshes for the
/// same obsidian-vault corpus. 5 minutes — covers the case where the
/// user is editing notes every few seconds: each sweep keeps the
/// chunk index live (cheap), but tag refresh only fires at most once
/// every five minutes. Writeback is also idempotent on unchanged
/// input (the per-note merge is a no-op when the desired tag set is
/// already present), so this debounce is purely a CPU/disk safety
/// net, not a correctness gate.
const WRITEBACK_DEBOUNCE_SECS: u64 = 300;

/// Why a sweep was skipped (no work happened).
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    AlreadyRunning,
    PausedManually,
    PausedAwaitingConfirmation,
    NotWatchedSourceType,
    NotRegistered,
    /// The prior sweep transitioned the corpus into `Errored`. We
    /// skip future sweeps until a user-initiated action (pause +
    /// resume, or remove + re-add) clears the status. Prevents
    /// the scheduler from looping on the same root cause every
    /// tick.
    Errored,
}

/// Outcome of one `run_once` invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerOutcome {
    NoChanges,
    Applied(DiffSummary),
    PausedByGuard(super::status::TrippedRule),
    Skipped(SkipReason),
}

pub struct Worker {
    engine: Arc<CorpusEngine>,
    manager: Arc<LocalCorpusManager>,
    registry: Arc<WatchedFolderRegistry>,
    sink: EventSink,
    /// Root of the engine's per-corpus index directory tree, used to
    /// locate `_watched_folder_state.json` for each corpus. Mirrors
    /// `LocalCorpusManager::engine_index_dir` since the engine doesn't
    /// expose that path publicly.
    index_dir_root: PathBuf,
    /// Living-trigger runtime. The daemon installs one (a
    /// `DaemonWorkflowRuntime` in `sovereign-cli-daemon`); tests and the
    /// desktop leave it `None`, so the trigger seam is inert. When set, a sweep
    /// that changes files and whose folder has a `run_on_changes` workflow
    /// dispatches it here (fire-and-forget). See [`WorkflowTriggerRuntime`].
    workflow_runtime: Option<Arc<dyn WorkflowTriggerRuntime>>,
}

impl Worker {
    pub fn new(
        engine: Arc<CorpusEngine>,
        manager: Arc<LocalCorpusManager>,
        registry: Arc<WatchedFolderRegistry>,
        sink: EventSink,
        index_dir_root: PathBuf,
    ) -> Self {
        Self {
            engine,
            manager,
            registry,
            sink,
            index_dir_root,
            workflow_runtime: None,
        }
    }

    /// Attach a living-trigger runtime. A builder rather than a `new` parameter so
    /// the many `Worker::new` call sites (tests especially) stay untouched — only
    /// the daemon, which has a runtime to install, chains this.
    pub fn with_workflow_runtime(
        mut self,
        runtime: Option<Arc<dyn WorkflowTriggerRuntime>>,
    ) -> Self {
        self.workflow_runtime = runtime;
        self
    }

    /// State directory for one corpus — `{index_dir}/{corpus_id}/`.
    fn state_dir(&self, corpus_id: &str) -> PathBuf {
        self.index_dir_root.join(corpus_id)
    }

    /// Execute one sweep for `corpus_id`. Returns the outcome; never
    /// panics on user-facing failure paths — every error becomes a
    /// `WatchedFolderEvent::SweepErrored` first and a `Result::Err`
    /// second so the scheduler keeps the loop alive.
    pub async fn run_once(&self, corpus_id: &str) -> Result<WorkerOutcome> {
        // 1. Acquire the per-corpus lock. Held for the entire sweep.
        let _guard = match self.registry.try_acquire(corpus_id).await {
            Some(g) => g,
            None => {
                self.emit(WatchedFolderEvent::SweepSkipped {
                    corpus_id: corpus_id.to_string(),
                    reason: "already_running".into(),
                });
                return Ok(WorkerOutcome::Skipped(SkipReason::AlreadyRunning));
            }
        };

        // 2. Resolve the corpus + watched config.
        let config = match self.manager.get(corpus_id).await {
            Some(c) => c,
            None => {
                self.emit(WatchedFolderEvent::SweepSkipped {
                    corpus_id: corpus_id.to_string(),
                    reason: "not_registered".into(),
                });
                return Ok(WorkerOutcome::Skipped(SkipReason::NotRegistered));
            }
        };
        let (reconcile_kind, watched_cfg) = match reconciliation_config_for(&config) {
            Some(v) => v,
            None => {
                self.emit(WatchedFolderEvent::SweepSkipped {
                    corpus_id: corpus_id.to_string(),
                    reason: "not_reconcilable_source_type".into(),
                });
                return Ok(WorkerOutcome::Skipped(SkipReason::NotWatchedSourceType));
            }
        };

        let state_dir = self.state_dir(corpus_id);
        let mut state = WatchedFolderState::load(&state_dir)?
            .unwrap_or_else(|| WatchedFolderState::fresh(corpus_id));

        // 3. Pause check.
        if state.is_paused() {
            let reason = match &state.status {
                WatchedFolderStatus::PausedManual { .. } => "paused_manual",
                WatchedFolderStatus::PausedAwaitingConfirmation { .. } => {
                    "paused_awaiting_confirmation"
                }
                _ => "paused",
            };
            self.emit(WatchedFolderEvent::SweepSkipped {
                corpus_id: corpus_id.to_string(),
                reason: reason.into(),
            });
            return Ok(WorkerOutcome::Skipped(match &state.status {
                WatchedFolderStatus::PausedAwaitingConfirmation { .. } => {
                    SkipReason::PausedAwaitingConfirmation
                }
                _ => SkipReason::PausedManually,
            }));
        }

        // 3b. Errored check. Without this the scheduler would
        // re-fire the same broken sweep every tick (default cadence
        // ~120s), spamming logs with the same root cause. A user
        // recovers from Errored by:
        //   - Pause → fix the underlying problem → Resume
        //   - Remove + re-add the folder (clears state + reingests)
        // Both clear the Errored status; either way the scheduler
        // gets a clean state the next tick.
        if matches!(state.status, WatchedFolderStatus::Errored { .. }) {
            self.emit(WatchedFolderEvent::SweepSkipped {
                corpus_id: corpus_id.to_string(),
                reason: "errored".into(),
            });
            return Ok(WorkerOutcome::Skipped(SkipReason::Errored));
        }

        let now_unix = now_unix();
        let sweep_id = Uuid::new_v4().to_string();
        self.emit(WatchedFolderEvent::SweepStarted {
            corpus_id: corpus_id.to_string(),
            sweep_id: sweep_id.clone(),
        });
        self.registry.mark_started(corpus_id, now_unix).await;

        // Clear the on-disk Manual sync-now flag mirror. The
        // registry-side flag was already cleared on the dispatching
        // tick; clearing here too means a daemon restart between the
        // two writes can't re-dispatch the same pending request on
        // the next tick. If the sweep below errors, the flag stays
        // cleared — that's correct. The user can re-trigger
        // `/sync-now` if they want another attempt.
        state.manual_sync_pending = false;

        // Run the sweep body inside a helper so any error consistently
        // transitions state to Errored and emits SweepErrored.
        let outcome = match self
            .run_sweep_body(&config, &watched_cfg, reconcile_kind, &mut state, now_unix)
            .await
        {
            Ok(out) => out,
            Err(e) => {
                let msg = format!("{e}");
                state.status = WatchedFolderStatus::Errored {
                    message: msg.clone(),
                    errored_unix: now_unix,
                };
                state.last_updated_unix = now_unix;
                let _ = state.save(&state_dir);
                self.emit(WatchedFolderEvent::SweepErrored {
                    corpus_id: corpus_id.to_string(),
                    message: msg,
                });
                return Err(e);
            }
        };

        Ok(outcome)
    }

    async fn run_sweep_body(
        &self,
        config: &LocalCorpusConfig,
        watched_cfg: &WatchedFolderConfig,
        reconcile_kind: ReconcileKind,
        state: &mut WatchedFolderState,
        now_unix: u64,
    ) -> Result<WorkerOutcome> {
        let corpus_id = config.id.clone();

        // 4. Walk.
        let prior_entries = state.entries.clone();
        let walk_cfg = config.clone();
        let exclude = watched_cfg.exclude_globs.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            walker::walk_folder(&walk_cfg, &prior_entries, &exclude)
        })
        .await
        .map_err(|e| Error::Execution(format!("watched_folder walk task: {e}")))?
        .map_err(|e| Error::Execution(format!("watched_folder walk: {e}")))?;
        self.emit(WatchedFolderEvent::Walked {
            corpus_id: corpus_id.clone(),
            visited: outcome.visited,
        });

        // Refresh skipped-by-extension + failed-file detail every
        // sweep. Files no longer present drop out; first_seen_unix
        // is preserved for files we've seen before so the UI can
        // surface "this PDF has been broken since 3 weeks ago".
        // OCR availability is checked here (cheap RwLock read) so
        // scanned PDFs flip in and out of `failed_files`
        // appropriately when the desktop installs an OcrCtx mid-run.
        let prior_failed: std::collections::HashMap<String, u64> = state
            .failed_files
            .iter()
            .map(|f| (f.doc_id.clone(), f.first_seen_unix))
            .collect();
        let ocr_available = self.manager.ocr_available().await;
        state.skipped_by_extension = outcome.raw.skipped_by_extension.clone();
        state.failed_files = collect_failed_files(
            &outcome.raw,
            &config.root_path,
            &prior_failed,
            now_unix,
            watched_cfg,
            ocr_available,
        );

        let mut snapshot = outcome.snapshot;

        // Filter out docs already classified as failed-extraction
        // (scanned PDFs without OCR, corrupt files, etc.). Without
        // this, a folder whose only docs ALL fail extraction would
        // (a) show the docs as `added` in every diff,
        // (b) trigger `apply_update` which would fail when no docs
        //     successfully extract,
        // (c) leave the index without `_corpus_meta.json` because no
        //     successful ingest ever wrote it,
        // (d) trap the worker in `Errored` because the next sweep's
        //     precondition guard at the bottom of this function would
        //     fire on the missing meta.
        // The failed-files list already surfaces unreadable docs to
        // the UI; dropping them from the apply path is purely
        // defensive — it doesn't hide the problem, just stops the
        // loop. When OCR becomes available later (mid-run install
        // via `set_ocr_ctx`), `collect_failed_files` above re-classifies
        // the doc out of `failed_files`, this filter no-ops on it,
        // and it lands in the next diff naturally.
        let failed_ids: std::collections::HashSet<String> = state
            .failed_files
            .iter()
            .map(|f| f.doc_id.clone())
            .collect();
        if !failed_ids.is_empty() {
            let before = snapshot.len();
            snapshot.retain(|doc_id, _| !failed_ids.contains(doc_id));
            tracing::debug!(
                corpus_id = %corpus_id,
                dropped = before.saturating_sub(snapshot.len()),
                remaining = snapshot.len(),
                "watched_folder:snapshot_filtered_for_failed_files"
            );
        }

        // 5. Tombstone revivals — must run BEFORE diff, because a
        // revived doc should be reclassified from `unchanged` (which
        // happens if prior entries still hold it) or `added` (if
        // prior entries do not). Since the chunks were physically
        // deleted at apply time, revivals always need re-extraction
        // → mark them as adds.
        let revived = soft_delete_gc::detect_revivals(
            state,
            &snapshot,
            watched_cfg.soft_delete_grace_secs,
            now_unix,
        );
        for doc_id in &revived {
            self.emit(WatchedFolderEvent::RevivalDetected {
                corpus_id: corpus_id.clone(),
                doc_id: doc_id.clone(),
            });
        }
        // For revived docs we want them to appear in the diff as
        // `added` even if prior_hashes already contains the same
        // hash (which it would if state.entries weren't pruned at
        // tombstone time — see step 11 below). To guarantee this,
        // we strip revived entries from the prior hash map before
        // diffing. The chunks need to be physically re-inserted
        // because they were deleted at apply time.
        let mut prior_hashes = state.prior_hashes();
        for doc_id in &revived {
            prior_hashes.remove(doc_id);
        }

        // 6. Diff.
        let diff = compute_diff(&prior_hashes, &snapshot);
        let live_before = prior_hashes.len();
        let summary = diff.summary(live_before);
        self.emit(WatchedFolderEvent::DiffComputed {
            corpus_id: corpus_id.clone(),
            summary: summary.clone(),
        });

        if diff.is_empty() {
            state.status = WatchedFolderStatus::Idle {
                last_sweep_unix: now_unix,
                live_docs: snapshot.len(),
                tombstones: state.tombstones.len(),
            };
            state.last_updated_unix = now_unix;
            // Persist refreshed entries (mtime/size cache may have
            // been updated even with no content changes — fast-path
            // depends on this).
            state.entries = snapshot;
            state.save(&self.state_dir(&corpus_id))?;
            self.emit(WatchedFolderEvent::SweepCompleted {
                corpus_id: corpus_id.clone(),
                applied: summary,
                duration_secs: now_unix.saturating_sub(now_unix), // 0; future: instrument
            });
            return Ok(WorkerOutcome::NoChanges);
        }

        // 7. Deletion guard. The user can pre-acknowledge a tripped
        // guard by calling `confirm-deletion`, which sets a one-shot
        // bypass flag — consume it here regardless of whether the
        // guard would have tripped (so a confirm followed by a
        // benign sweep doesn't silently leave the bypass armed for
        // a later wipe).
        let bypass_active = state.bypass_guard_next_sweep;
        if bypass_active {
            state.bypass_guard_next_sweep = false;
            tracing::info!(
                corpus_id = %corpus_id,
                "watched_folder:guard_bypass_consumed"
            );
        }
        if !bypass_active {
            match DeletionGuard::evaluate(
                diff.removed.len(),
                live_before,
                &watched_cfg.deletion_guard,
            ) {
                GuardDecision::Allow => {}
                GuardDecision::Pause(rule) => {
                    state.status = WatchedFolderStatus::PausedAwaitingConfirmation {
                        diff_summary: summary.clone(),
                        tripped_rule: rule.clone(),
                        sweep_started_unix: now_unix,
                    };
                    state.last_updated_unix = now_unix;
                    // We do NOT update state.entries — the next sweep
                    // (after confirm) re-walks fresh.
                    state.save(&self.state_dir(&corpus_id))?;
                    self.emit(WatchedFolderEvent::GuardTripped {
                        corpus_id: corpus_id.clone(),
                        rule: rule.clone(),
                    });
                    return Ok(WorkerOutcome::PausedByGuard(rule));
                }
            }
        }

        // 8. Apply through the engine's three-phase updater.
        // The diff's `added` list is what the updater treats as new
        // documents — revived docs are correctly in this list because
        // we stripped them from prior_hashes before diffing.
        // OCR context is fetched per-sweep so a runtime install via
        // `LocalCorpusManager::set_ocr_ctx` (the desktop's boot path
        // does this once Tesseract is resolved) takes effect on the
        // very next sweep without a daemon restart. `None` when
        // either OCR is off for this corpus OR no context is
        // installed — `apply_watched_diff` no-ops the OCR branch.
        let ocr_ctx = if config.ocr_pdfs {
            self.manager.ocr_ctx_clone().await
        } else {
            None
        };
        // Precondition guard. `apply_update` opens the corpus's
        // existing index — when the index dir was wiped out-of-band
        // (manual `rm`, a prior aborted ingest that never wrote
        // `_corpus_meta.json`, etc.) the call returns "Missing
        // metadata" and we'd loop on every scheduler tick spamming
        // the same error. Transition straight to Errored with a
        // self-recovery hint instead of trying the apply.
        let index_dir = self.index_dir_root.join(&corpus_id);
        let meta_path = index_dir.join("_corpus_meta.json");
        if !meta_path.is_file() {
            return Err(Error::Execution(format!(
                "watched_folder: index for '{corpus_id}' is missing \
                 `_corpus_meta.json` at {} — initial ingest never \
                 completed or the index was wiped out-of-band. \
                 Re-register the folder (Settings → Local Knowledge \
                 → remove + re-add) to rebuild from scratch.",
                meta_path.display()
            )));
        }
        let started = std::time::Instant::now();
        apply_watched_diff(
            self.engine.clone(),
            config,
            &diff,
            &snapshot,
            ocr_ctx,
            &self.sink,
            now_unix,
        )
        .await?;
        let duration_secs = started.elapsed().as_secs();

        // 9. Record tombstones for the docs we just deleted. The
        // prior `state.entries` is the source of truth for the
        // tombstone payload — the docs are gone from disk now, so we
        // can't re-read them.
        let prior_snapshot = state.entries.clone();
        let recorded =
            soft_delete_gc::record_tombstones(state, &diff.removed, &prior_snapshot, now_unix);
        if recorded != diff.removed.len() {
            tracing::warn!(
                corpus_id = %corpus_id,
                expected = diff.removed.len(),
                recorded,
                "watched_folder:tombstone_record_mismatch — some removed docs were absent from prior snapshot"
            );
        }

        // 10. GC + cap.
        let expired = soft_delete_gc::expire(state, watched_cfg.soft_delete_grace_secs, now_unix);
        for doc_id in &expired {
            self.emit(WatchedFolderEvent::TombstoneExpired {
                corpus_id: corpus_id.clone(),
                doc_id: doc_id.clone(),
            });
        }
        let evicted = soft_delete_gc::enforce_cap(state);
        if evicted > 0 {
            tracing::warn!(
                corpus_id = %corpus_id,
                evicted_count = evicted,
                cap = soft_delete_gc::TOMBSTONE_CAP,
                "watched_folder:tombstone_evicted — per-corpus cap exceeded; oldest dropped"
            );
            self.emit(WatchedFolderEvent::TombstoneEvicted {
                corpus_id: corpus_id.clone(),
                evicted_count: evicted,
            });
        }

        // 11. Update entries cache; status → Idle.
        // Note: revivals are already absent from `state.tombstones`
        // (detect_revivals removed them) and present in `snapshot`
        // (the walk found them). Removed docs are absent from
        // `snapshot` so they correctly drop out of `state.entries`.
        let live_docs = snapshot.len();
        std::mem::swap(&mut state.entries, &mut snapshot);
        state.status = WatchedFolderStatus::Idle {
            last_sweep_unix: now_unix,
            live_docs,
            tombstones: state.tombstones.len(),
        };
        state.last_updated_unix = now_unix;

        // 11b. For obsidian vaults: best-effort refresh per-note tag
        // writes against the cached cluster preview. The chunk index
        // is already up-to-date via the apply step above; this step
        // only keeps the *frontmatter tags* on existing notes in sync
        // with the most recent clustering result. Skipped when:
        //   - corpus is a watched folder (no writeback at all)
        //   - the user hasn't run the initial clustering pipeline yet
        //     (no cached preview → benign no-op)
        //   - the writeback was refreshed within the debounce window
        //     (5 minutes — see WRITEBACK_DEBOUNCE_SECS below)
        // touched_user_notes from the result is patched onto
        // state.entries so the next sweep's fast-path treats writeback's
        // own mtime bumps as "no change" — preventing the
        // writeback ↔ walker feedback loop.
        if reconcile_kind == ReconcileKind::ObsidianVault {
            let debounce_ok = state
                .last_writeback_unix
                .map(|prev| now_unix.saturating_sub(prev) >= WRITEBACK_DEBOUNCE_SECS)
                .unwrap_or(true);
            if debounce_ok {
                match self
                    .manager
                    .refresh_writeback_if_clustered(&corpus_id)
                    .await
                {
                    Ok(Some(wb_result)) => {
                        for touched in &wb_result.touched_user_notes {
                            if let Some(entry) = state.entries.get_mut(&touched.relative_path) {
                                entry.mtime_unix = touched.mtime_unix;
                                entry.size_bytes = touched.size_bytes;
                                entry.content_hash = touched.content_hash.clone();
                            }
                        }
                        state.last_writeback_unix = Some(now_unix);
                        tracing::info!(
                            corpus_id = %corpus_id,
                            tagged = wb_result.files_tagged,
                            touched = wb_result.touched_user_notes.len(),
                            skipped = wb_result.files_skipped.len(),
                            index_notes = wb_result.index_notes_created,
                            "obsidian_vault:writeback_refreshed"
                        );
                    }
                    Ok(None) => {
                        // No cached preview yet — benign. User has not
                        // run the initial clustering. The chunk index is
                        // still being kept live by the sweep itself.
                        tracing::debug!(
                            corpus_id = %corpus_id,
                            "obsidian_vault:writeback_skipped — no cached cluster preview"
                        );
                    }
                    Err(e) => {
                        // Writeback failure is non-fatal: the chunk
                        // index is already updated; tag refresh can
                        // retry on the next sweep. Log loudly but
                        // don't fail the sweep.
                        tracing::warn!(
                            corpus_id = %corpus_id,
                            error = %e,
                            "obsidian_vault:writeback_failed (non-fatal)"
                        );
                    }
                }
            } else {
                tracing::debug!(
                    corpus_id = %corpus_id,
                    last_writeback_unix = ?state.last_writeback_unix,
                    "obsidian_vault:writeback_debounced"
                );
            }
        }

        // 11c. Persist the final state. Done after writeback so
        // state.last_writeback_unix and the patched entries land in
        // the same write — no race where a daemon crash between the
        // two writes leaves entries pointing at pre-writeback hashes
        // for the same `last_writeback_unix`.
        state.save(&self.state_dir(&corpus_id))?;

        // 11d. Tiered incremental re-enrichment. For vault + watched-
        // folder corpora that route through `FolderTieredProvider`
        // (recipe emits `[display] category = "vault"` or
        // `"watched_folder"`), re-run GLiNER over new/changed chunks
        // and re-run per-source RAPTOR for only the touched notes.
        // Always fires for these source kinds — the engine's
        // `reindex_changed_sources_tiered` no-ops cleanly when the
        // extractor / provider isn't installed, so a daemon without
        // tiered wiring stays unaffected.
        //
        // Skipped for `DocumentFolder` (one-shot drag-drop) — those
        // never enter the worker anyway, but defensive in case the
        // reconcile-kind logic ever extends.
        if matches!(
            reconcile_kind,
            ReconcileKind::ObsidianVault | ReconcileKind::WatchedFolder
        ) {
            let touched_basenames = diff
                .added
                .iter()
                .chain(diff.modified.iter())
                .filter_map(|rel| {
                    // source_doc_id is set to the file basename by
                    // `extract_stage::source_id_for` — convert each
                    // diff entry's relative path to its basename so
                    // the per-source RAPTOR rows match.
                    std::path::Path::new(rel)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                })
                .collect::<Vec<_>>();
            if !touched_basenames.is_empty() {
                tracing::info!(
                    corpus_id = %corpus_id,
                    touched = touched_basenames.len(),
                    "watched_folder:tiered_incremental_start"
                );
                self.engine
                    .reindex_changed_sources_tiered(&corpus_id, &touched_basenames)
                    .await;
            }
        }

        // 11e. Living trigger. If the user attached a workflow to this folder
        // (`run_on_changes`) AND the daemon installed a runtime, dispatch it on the
        // changed files. Fire-and-forget — the runtime spawns + debounces, so this
        // never blocks the sweep or holds the per-corpus lock. Gated on a non-empty
        // diff (an unchanged sweep is silent). A folder without a runtime or without
        // `run_on_changes` behaves exactly as before.
        if !diff.is_empty() {
            if let (Some(rt), Some(workflow)) = (
                &self.workflow_runtime,
                watched_cfg.run_on_changes.as_deref(),
            ) {
                self.emit(WatchedFolderEvent::WorkflowTriggered {
                    corpus_id: corpus_id.clone(),
                    workflow: workflow.to_string(),
                });
                rt.dispatch(&corpus_id, config, watched_cfg, &diff).await;
            }
        }

        self.emit(WatchedFolderEvent::SweepCompleted {
            corpus_id: corpus_id.clone(),
            applied: summary.clone(),
            duration_secs,
        });
        tracing::info!(
            corpus_id = %corpus_id,
            added = summary.added,
            modified = summary.modified,
            removed = summary.removed,
            tombstones = state.tombstones.len(),
            duration_secs,
            "watched_folder:sweep_completed"
        );
        Ok(WorkerOutcome::Applied(summary))
    }

    fn emit(&self, event: WatchedFolderEvent) {
        // Fire the sink AND a tracing event in lockstep — sinks can
        // come and go (tests install Vec sinks; the daemon installs a
        // tracing-bridge sink); tracing events go to the operator's
        // log either way.
        match &event {
            WatchedFolderEvent::SweepStarted {
                corpus_id,
                sweep_id,
            } => {
                tracing::debug!(corpus_id = %corpus_id, sweep_id = %sweep_id, "watched_folder:sweep_started");
            }
            WatchedFolderEvent::Walked { corpus_id, visited } => {
                tracing::debug!(corpus_id = %corpus_id, visited, "watched_folder:walked");
            }
            WatchedFolderEvent::DiffComputed { corpus_id, summary } => {
                tracing::debug!(
                    corpus_id = %corpus_id,
                    added = summary.added,
                    modified = summary.modified,
                    removed = summary.removed,
                    live_before = summary.live_before,
                    "watched_folder:diff_computed"
                );
            }
            WatchedFolderEvent::GuardTripped { corpus_id, rule } => {
                tracing::warn!(
                    corpus_id = %corpus_id,
                    rule = ?rule,
                    "watched_folder:guard_tripped — deletion phase blocked, awaiting user confirmation"
                );
            }
            WatchedFolderEvent::SweepErrored { corpus_id, message } => {
                tracing::error!(
                    corpus_id = %corpus_id,
                    error = %message,
                    "watched_folder:sweep_errored"
                );
            }
            WatchedFolderEvent::RevivalDetected { corpus_id, doc_id } => {
                tracing::info!(
                    corpus_id = %corpus_id,
                    doc_id = %doc_id,
                    "watched_folder:revival_detected"
                );
            }
            WatchedFolderEvent::TombstoneExpired { corpus_id, doc_id } => {
                tracing::debug!(corpus_id = %corpus_id, doc_id = %doc_id, "watched_folder:tombstone_expired");
            }
            WatchedFolderEvent::SweepSkipped { corpus_id, reason } => {
                tracing::debug!(corpus_id = %corpus_id, reason = %reason, "watched_folder:sweep_skipped");
            }
            // PhaseProgress, SweepCompleted, TombstoneEvicted are
            // emitted with their own dedicated tracing in the worker
            // body (PhaseProgress is high-volume so it stays sink-only).
            _ => {}
        }
        (self.sink)(event);
    }
}

/// Resolve the per-corpus reconciliation knobs for a sweep, plus the
/// `ReconcileKind` so the worker can branch where needed (e.g., the
/// post-apply writeback step that only fires for obsidian vaults).
///
/// For `WatchedFolder` corpora the knobs come straight off the
/// persisted `WatchedFolderConfig`. For `ObsidianVault` corpora we
/// synthesise an equivalent on the fly: the worker body wants
/// `exclude_globs`, `deletion_guard`, `soft_delete_grace_secs`,
/// `with_ocr`, `additional_roots`, etc., and obsidian vaults can
/// reuse sensible watched-folder defaults for every field that
/// matters here (vault excludes are hardcoded below; OCR is always
/// off for a markdown-only corpus).
///
/// `DocumentFolder` is one-shot and should never reach the worker;
/// returning `None` makes the dispatch loop emit a benign
/// `not_reconcilable_source_type` skip event.
fn reconciliation_config_for(
    c: &LocalCorpusConfig,
) -> Option<(ReconcileKind, WatchedFolderConfig)> {
    match &c.source_type {
        LocalCorpusSourceType::WatchedFolder(cfg) => {
            Some((ReconcileKind::WatchedFolder, cfg.clone()))
        }
        LocalCorpusSourceType::ObsidianVault { .. } => {
            let mut wf = WatchedFolderConfig::default();
            // Sovereign's own writeback output and Obsidian's app
            // config folder must never be walked. The writeback
            // sentinel-frontmatter check (Phase A2) catches edits to
            // managed per-note tag files, but `_sovereign-index/**`
            // is fully sovereign-owned and warrants a hard exclude
            // — no point hashing files we authored ourselves.
            wf.exclude_globs = vec![
                "_sovereign-index/**".to_string(),
                ".obsidian/**".to_string(),
                ".trash/**".to_string(),
            ];
            // Markdown-only vault, no OCR path needed regardless of
            // the OcrCtx the daemon may or may not have installed.
            wf.with_ocr = false;
            Some((ReconcileKind::ObsidianVault, wf))
        }
        LocalCorpusSourceType::DocumentFolder => None,
    }
}

/// Build the post-sweep `failed_files` list from the pre-scan
/// classification buckets. Preserves `first_seen_unix` for files
/// already on the prior list (so an old failure shows its true age,
/// not the latest sweep's clock).
pub(crate) fn collect_failed_files(
    raw: &crate::local_corpus::pre_scanner::PreScanResult,
    root: &std::path::Path,
    prior_first_seen: &std::collections::HashMap<String, u64>,
    now_unix: u64,
    watched_cfg: &WatchedFolderConfig,
    ocr_available: bool,
) -> Vec<crate::local_corpus::watched::state::FailedFile> {
    use crate::local_corpus::watched::state::FailedFile;
    let mut out = Vec::new();
    let push = |out: &mut Vec<FailedFile>,
                kind: &str,
                reason: &str,
                meta: &crate::local_corpus::pre_scanner::FileMeta| {
        let Some(doc_id) = walker::doc_id_for(root, &meta.path) else {
            return;
        };
        let first_seen = prior_first_seen.get(&doc_id).copied().unwrap_or(now_unix);
        out.push(FailedFile {
            doc_id,
            absolute_path: meta.path.clone(),
            kind: kind.into(),
            reason: reason.into(),
            first_seen_unix: first_seen,
        });
    };
    for f in &raw.corrupt_files {
        push(
            &mut out,
            "corrupt",
            "pdf-extract failed to parse the document",
            f,
        );
    }
    for f in &raw.protected_pdfs {
        push(
            &mut out,
            "password_protected",
            "PDF is encrypted; OCR cannot help",
            f,
        );
    }
    // Scanned PDFs only land in `failed_files` when the OCR path
    // can't pick them up — i.e., OCR is disabled OR the daemon
    // hasn't installed an `OcrCtx`. When OCR is wired and enabled,
    // scanned PDFs flow through `apply_watched_diff`'s OCR fallback
    // and contribute real chunks to the index, so surfacing them as
    // "failed" would be misleading.
    let ocr_active = watched_cfg.with_ocr && ocr_available;
    if !ocr_active {
        let reason = if watched_cfg.with_ocr {
            "PDF has no text layer — OCR is enabled but the daemon's OcrCtx isn't installed"
        } else {
            "PDF has no text layer — turn on OCR to read it"
        };
        for f in &raw.scanned_pdfs {
            push(&mut out, "scanned_no_text", reason, f);
        }
    }
    out
}

use sovereign_core::time::unix_now_u64 as now_unix;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_corpus::pre_scanner::{FileMeta, PreScanResult};
    use std::path::Path;

    #[test]
    fn skip_reason_variants_are_distinguishable() {
        // Smoke check that the four variants are indeed distinct —
        // the scheduler matches on these to decide whether to log a
        // benign skip vs. retry vs. drop the corpus.
        assert_ne!(SkipReason::AlreadyRunning, SkipReason::PausedManually);
        assert_ne!(
            SkipReason::PausedAwaitingConfirmation,
            SkipReason::NotRegistered
        );
        assert_ne!(SkipReason::NotWatchedSourceType, SkipReason::NotRegistered);
    }

    fn fake_meta(name: &str) -> FileMeta {
        FileMeta {
            path: PathBuf::from(format!("/root/{name}")),
            size_bytes: 1024,
            display_name: name.into(),
        }
    }

    fn pre_scan_with_one_scanned_pdf() -> PreScanResult {
        PreScanResult {
            readable: vec![],
            scanned_pdfs: vec![fake_meta("scan.pdf")],
            protected_pdfs: vec![],
            corrupt_files: vec![],
            large_files: vec![],
            ignored_types: 0,
            skipped_by_extension: Default::default(),
            total_visited: 1,
        }
    }

    #[test]
    fn scanned_pdf_surfaces_when_ocr_off() {
        let raw = pre_scan_with_one_scanned_pdf();
        let mut wf = WatchedFolderConfig::default();
        wf.with_ocr = false;
        let out = collect_failed_files(
            &raw,
            Path::new("/root"),
            &Default::default(),
            100,
            &wf,
            /* ocr_available = */ true, // irrelevant when with_ocr=false
        );
        assert_eq!(
            out.len(),
            1,
            "scanned PDF should surface as failure when OCR is off"
        );
        assert_eq!(out[0].kind, "scanned_no_text");
        assert!(out[0].reason.contains("turn on OCR"));
    }

    #[test]
    fn scanned_pdf_surfaces_when_ocr_on_but_runtime_missing() {
        // Spec: when the user opts into OCR but the daemon hasn't
        // installed an OcrCtx (CLI without the bundle, missing
        // tesseract sidecar), surface the scanned PDF as a failure
        // with a reason that explains *why* the OCR path didn't
        // pick it up.
        let raw = pre_scan_with_one_scanned_pdf();
        let mut wf = WatchedFolderConfig::default();
        wf.with_ocr = true;
        let out = collect_failed_files(
            &raw,
            Path::new("/root"),
            &Default::default(),
            100,
            &wf,
            /* ocr_available = */ false,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "scanned_no_text");
        assert!(out[0].reason.contains("OcrCtx isn't installed"));
    }

    #[test]
    fn scanned_pdf_does_not_surface_when_ocr_active() {
        // OCR on AND runtime installed → the scanned PDF flows
        // through the apply-time OCR fallback and contributes real
        // chunks. Surfacing it as "failed" would be misleading.
        let raw = pre_scan_with_one_scanned_pdf();
        let mut wf = WatchedFolderConfig::default();
        wf.with_ocr = true;
        let out = collect_failed_files(
            &raw,
            Path::new("/root"),
            &Default::default(),
            100,
            &wf,
            /* ocr_available = */ true,
        );
        assert!(
            out.is_empty(),
            "scanned PDF should NOT surface as failure when OCR is active; got {out:?}"
        );
    }

    #[test]
    fn corrupt_and_protected_always_surface() {
        // OCR can't help these — they always show up regardless of
        // OCR state.
        let raw = PreScanResult {
            readable: vec![],
            scanned_pdfs: vec![],
            protected_pdfs: vec![fake_meta("locked.pdf")],
            corrupt_files: vec![fake_meta("broken.pdf")],
            large_files: vec![],
            ignored_types: 0,
            skipped_by_extension: Default::default(),
            total_visited: 2,
        };
        let mut wf = WatchedFolderConfig::default();
        wf.with_ocr = true;
        let out = collect_failed_files(
            &raw,
            Path::new("/root"),
            &Default::default(),
            100,
            &wf,
            true,
        );
        assert_eq!(out.len(), 2);
        let kinds: Vec<&str> = out.iter().map(|f| f.kind.as_str()).collect();
        assert!(kinds.contains(&"corrupt"));
        assert!(kinds.contains(&"password_protected"));
    }
}
