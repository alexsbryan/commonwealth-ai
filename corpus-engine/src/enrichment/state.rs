// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic enrichment progress + persistence shared across every
//! corpus enrichment pipeline.
//!
//! ## Why this exists
//!
//! Before this module, every enrichment dispatcher (folder watched-
//! corpus tiered build, post-install structural atlas, conversation
//! RAPTOR, future pipelines) emitted progress only via `tracing::info!`
//! and held all state in memory. A daemon restart mid-enrichment left
//! the corpus visibly indistinguishable from "still working" — UI
//! showed "starting" forever, operators had to tail the daemon log
//! to know whether anything was actually progressing, and no machine
//! could decide "this stalled; retry" without a human in the loop.
//!
//! `EnrichmentState` makes the same answer machine-readable for any
//! corpus, regardless of which pipeline ran it:
//!
//!  - The state file lives at `<index_dir>/_enrichment_state.json`
//!    — same place `_corpus_meta.json` does, so anything that already
//!    walks installed_indexes() can pick it up.
//!  - The shape is generic: phase, current/total step, last-update
//!    timestamp, optional error. Pipelines pick their own phase order.
//!  - The `EnrichmentProgressSink` trait lets callers compose write
//!    surfaces — a state-file sink for persistence, an event-channel
//!    sink for UI push, a log sink for ops — without forcing every
//!    pipeline to know about every consumer.
//!
//! ## Pipelines that should write here
//!
//!  - `FolderTieredProvider` (watched-folder RAPTOR + motif build)
//!  - Post-install structural atlas (every corpus install fires this)
//!  - Conversation tiered enrichment (per-conv RAPTOR)
//!  - Wikipedia newsworthy refresh (atom delta after each tracked
//!    article fetch — finer-grained, lower priority)
//!  - SEP / philosophy pipelines as they migrate to the v2 dispatcher
//!
//! ## Pipelines that should NOT
//!
//! Pure ingest (acquire → extract → chunk → embed → index) already
//! has its own `IngestProgress` callback wired through the desktop's
//! `corpus-progress` event channel. This module is for the ENRICHMENT
//! that runs AFTER ingest completes — the layer that produces atoms,
//! RAPTOR nodes, motifs, structural views.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Canonical filename for the per-corpus enrichment state sidecar.
/// Lives next to `_corpus_meta.json` so installed_indexes()-style
/// walks pick it up without a separate directory scan.
pub const ENRICHMENT_STATE_FILENAME: &str = "_enrichment_state.json";

/// Phase taxonomy shared across pipelines. Numeric ordering reflects
/// rough wall-time weight (Starting fast → Persisting fast, with the
/// LLM-heavy phases in the middle); UI can use this for the progress
/// bar's coarse fraction when a pipeline doesn't supply a step total.
///
/// Pipelines are NOT required to walk every phase. Folder enrichment
/// goes Starting → Scanning → RaptorLeaves → RaptorTree → Motifs →
/// Persisting → Complete. Structural atlas post-install goes
/// Starting → AtomExtraction → Persisting → Complete. Both share the
/// same file shape and the same UI rendering surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentPhase {
    Starting,
    Scanning,
    EntityExtraction,
    RaptorLeaves,
    RaptorTree,
    MotifExtraction,
    AtomExtraction,
    Persisting,
    Complete,
    Failed,
    /// Set by the daemon-start sweeper when a non-terminal state file
    /// hasn't been updated in `STALL_THRESHOLD_SECS`. Distinct from
    /// `Failed` so UI can offer "retry" affordances vs.
    /// "investigate" affordances.
    Stalled,
}

impl EnrichmentPhase {
    /// True when the phase will not change without operator action.
    /// UI renders a static badge for these; non-terminal phases get a
    /// live progress bar.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Stalled)
    }

    /// True when a build left in this phase by a process that has since
    /// died (a `tauri dev` hot-reload, an app update, a crash) should be
    /// RESUMED on the next boot. `Complete` finished cleanly; `Failed` is
    /// a genuine error the operator retries deliberately (auto-retrying it
    /// every boot would loop). Everything else — any non-terminal phase a
    /// killed process left mid-run, or `Stalled` (that same corpse after
    /// the boot stall-sweep re-labelled it) — is a resumable interruption.
    /// Note this is deliberately NOT `!is_terminal()`: `Stalled` IS
    /// terminal but IS resumable.
    pub fn is_resumable_interruption(self) -> bool {
        !matches!(self, Self::Complete | Self::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Scanning => "scanning",
            Self::EntityExtraction => "entity extraction",
            Self::RaptorLeaves => "RAPTOR leaves",
            Self::RaptorTree => "RAPTOR tree",
            Self::MotifExtraction => "motif extraction",
            Self::AtomExtraction => "atom extraction",
            Self::Persisting => "persisting",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Stalled => "stalled",
        }
    }

    /// Coarse fraction-complete estimate for UI when a pipeline
    /// doesn't supply a finer per-phase step total. Roughly mirrors
    /// wall-clock weight on a 9B fast slot.
    pub fn coarse_fraction(self) -> f32 {
        match self {
            Self::Starting => 0.02,
            Self::Scanning => 0.10,
            Self::EntityExtraction => 0.25,
            Self::RaptorLeaves => 0.55,
            Self::RaptorTree => 0.75,
            Self::MotifExtraction => 0.85,
            Self::AtomExtraction => 0.85,
            Self::Persisting => 0.95,
            Self::Complete => 1.0,
            Self::Failed | Self::Stalled => 0.0,
        }
    }
}

/// Threshold beyond which an in-progress state without recent
/// `last_progress_at` updates is considered stalled. Tuned for the
/// LLM-heaviest phase (RaptorTree) on the slowest production slot
/// (35B Slow at ~4 tok/s) — a 700-chunk corpus with three tree
/// levels emits a per-level progress event every ~3-5 min.
/// 10 minutes without an update means something is genuinely wrong.
pub const STALL_THRESHOLD_SECS: i64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub corpus_id: String,
    /// Optional — pipeline name (e.g. `"folder_tiered"`,
    /// `"structural_atlas"`, `"philosophy_atlas"`). Lets the UI
    /// label which pipeline is running when several enrich the same
    /// corpus in sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    pub phase: EnrichmentPhase,
    /// Within-phase progress. `step_total = 0` means the pipeline
    /// didn't supply a denominator; UI falls back to
    /// `phase.coarse_fraction()`.
    #[serde(default)]
    pub step_current: u64,
    #[serde(default)]
    pub step_total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub started_at: i64,
    pub last_progress_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_schema_version() -> u32 {
    1
}

impl EnrichmentState {
    /// True when a non-terminal phase hasn't recorded progress within
    /// [`STALL_THRESHOLD_SECS`] — the build is wedged (its process
    /// crashed, was killed, or never actually started and left a
    /// `Starting` stamp behind). Terminal phases are never stale.
    /// Callers use this to (a) surface a stalled build to the UI so the
    /// user can retry, and (b) let a fresh enrich supersede the corpse
    /// instead of being blocked by an idempotency guard.
    pub fn is_stale(&self, now_secs: i64) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        // An error stamped on a non-terminal phase means a sweeper or
        // handler already declared this build dead. The `phase` field can
        // lag behind — e.g. the stall sweep writes the error while a
        // concurrent (doomed) writer re-stamps `Starting`, and the sweep's
        // write also bumps `last_progress_at`, hiding the age. So a
        // non-terminal phase carrying an error is stale regardless of age.
        if self.error.is_some() {
            return true;
        }
        now_secs.saturating_sub(self.last_progress_at) > STALL_THRESHOLD_SECS
    }

    pub fn new(corpus_id: impl Into<String>, pipeline_id: Option<String>) -> Self {
        let now = now_secs();
        Self {
            schema_version: 1,
            corpus_id: corpus_id.into(),
            pipeline_id,
            phase: EnrichmentPhase::Starting,
            step_current: 0,
            step_total: 0,
            message: None,
            started_at: now,
            last_progress_at: now,
            completed_at: None,
            error: None,
        }
    }
}

/// On-disk state file helpers. Pure I/O — no awareness of which
/// pipeline is running. Callers use `EnrichmentProgressSink` for the
/// write surface so multiple sinks (file, event channel, log) can
/// compose without each one needing to re-read the file.
pub struct EnrichmentStateFile;

impl EnrichmentStateFile {
    pub fn path(index_dir: &Path) -> PathBuf {
        index_dir.join(ENRICHMENT_STATE_FILENAME)
    }

    pub fn read(index_dir: &Path) -> Result<Option<EnrichmentState>> {
        let path = Self::path(index_dir);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(Error::Io)?;
        let state: EnrichmentState = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Extraction(format!("enrichment_state: parse {}: {e}", path.display()))
        })?;
        Ok(Some(state))
    }

    pub fn write(index_dir: &Path, state: &EnrichmentState) -> Result<()> {
        let path = Self::path(index_dir);
        let json = serde_json::to_vec_pretty(state)
            .map_err(|e| Error::Extraction(format!("enrichment_state: serialize: {e}")))?;
        // Write through a sibling tempfile + rename so a concurrent
        // reader never sees a truncated file. Cheap on local FS.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(Error::Io)?;
        std::fs::rename(&tmp, &path).map_err(Error::Io)?;
        Ok(())
    }

    /// Idempotent: stamp the state file in `index_dir` with a new
    /// phase and optional message/step counts. Reads the existing
    /// state (creating a default if absent) so the started_at +
    /// pipeline_id fields survive across phase transitions.
    pub fn stamp(
        index_dir: &Path,
        corpus_id: &str,
        pipeline_id: Option<&str>,
        phase: EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    ) -> Result<EnrichmentState> {
        let mut state = Self::read(index_dir)?
            .unwrap_or_else(|| EnrichmentState::new(corpus_id, pipeline_id.map(String::from)));
        state.phase = phase;
        state.step_current = step_current;
        state.step_total = step_total;
        state.message = message.map(String::from);
        state.last_progress_at = now_secs();
        if matches!(phase, EnrichmentPhase::Complete) {
            state.completed_at = Some(state.last_progress_at);
            state.error = None;
        } else {
            // Any non-`Complete` stamp means a run is in flight (or a
            // fresh run has superseded a prior completed one).
            // `completed_at` must describe *this* run — carrying a stale
            // value forward from an earlier Complete leaves the file
            // self-contradictory: a non-terminal `phase` with
            // `completed_at` set and `last_progress_at > completed_at`.
            // That contradiction is exactly what tripped the folder
            // pipeline's per-note-Complete-then-continue bug. Clear it so
            // `completed_at.is_some()` reliably means "this run finished".
            state.completed_at = None;
        }
        Self::write(index_dir, &state)?;
        Ok(state)
    }

    /// Mark the state file as failed, capturing the error message.
    /// Idempotent — call on every error path so the UI sees the
    /// failure reason instead of silently retaining the last
    /// successful phase.
    pub fn fail(index_dir: &Path, corpus_id: &str, error: &str) -> Result<EnrichmentState> {
        let mut state =
            Self::read(index_dir)?.unwrap_or_else(|| EnrichmentState::new(corpus_id, None));
        state.phase = EnrichmentPhase::Failed;
        state.last_progress_at = now_secs();
        state.error = Some(error.to_string());
        // A failed run did not complete — a stale `completed_at` from an
        // earlier successful run would falsely read as "finished OK".
        state.completed_at = None;
        Self::write(index_dir, &state)?;
        Ok(state)
    }

    /// Bump `last_progress_at` to now on a *live* state — one that is
    /// non-terminal and carries no error — while preserving its phase,
    /// step counters, and message. No-op when the state file is absent,
    /// already terminal, or already errored (a build a sweeper or
    /// handler has declared dead must never be silently resurrected by a
    /// heartbeat tick — [`EnrichmentState::is_stale`] treats a stamped
    /// error as death regardless of clock).
    ///
    /// This is the liveness floor for phases that do real work without
    /// emitting their own `stamp`s. The pre-RAPTOR ingest embed pass and
    /// the CPU-bound GliNER NER pass each run for minutes with no phase
    /// transition in between; without a heartbeat `is_stale` trips at
    /// [`STALL_THRESHOLD_SECS`] and the UI false-reports a wedge on a
    /// build that is very much alive. Idempotent; cheap (one read + one
    /// atomic write).
    pub fn heartbeat(index_dir: &Path) -> Result<()> {
        let Some(mut state) = Self::read(index_dir)? else {
            return Ok(());
        };
        if state.phase.is_terminal() || state.error.is_some() {
            return Ok(());
        }
        state.last_progress_at = now_secs();
        Self::write(index_dir, &state)
    }
}

/// How often [`EnrichmentHeartbeat`] touches the state file. An order of
/// magnitude below [`STALL_THRESHOLD_SECS`] so that even a couple of
/// missed ticks (a GC pause, a busy runtime) still leave generous margin
/// before `is_stale` could fire.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// RAII liveness heartbeat for a long-running enrichment phase.
///
/// While this guard is alive, a background task calls
/// [`EnrichmentStateFile::heartbeat`] on `index_dir` every
/// [`HEARTBEAT_INTERVAL_SECS`], keeping `last_progress_at` fresh so a
/// phase that legitimately runs for many minutes without emitting its
/// own progress events (ingest embed, GliNER NER, one long RAPTOR
/// document) is not mistaken for a stall. Dropping the guard aborts the
/// task.
///
/// The heartbeat only ever *touches the timestamp* of a non-terminal,
/// error-free state — it never changes the phase and never resurrects a
/// terminal/errored one — so the pipeline's own terminal `Complete` /
/// `Failed` stamp always wins the race, and a genuinely abandoned build
/// (owning process gone, so no heartbeat) still goes stale and is swept
/// on the next daemon start. Liveness is thereby tied to the *owning
/// process*, not to how chatty a given phase happens to be.
#[must_use = "the heartbeat stops the moment the guard is dropped"]
pub struct EnrichmentHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl EnrichmentHeartbeat {
    /// Spawn a heartbeat over `index_dir` at the default interval.
    /// Must be called from within a Tokio runtime.
    pub fn spawn(index_dir: impl Into<PathBuf>) -> Self {
        Self::spawn_every(
            index_dir,
            std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
        )
    }

    /// Spawn a heartbeat with an explicit interval (tests pass a short
    /// one). Must be called from within a Tokio runtime.
    pub fn spawn_every(index_dir: impl Into<PathBuf>, interval: std::time::Duration) -> Self {
        let index_dir = index_dir.into();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first `tick()` completes immediately; skip it so the
            // guard doesn't re-stamp the timestamp the caller just wrote.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(e) = EnrichmentStateFile::heartbeat(&index_dir) {
                    tracing::warn!(
                        index_dir = %index_dir.display(),
                        error = %e,
                        "enrichment heartbeat: touch failed (state may read stale if this persists)"
                    );
                }
            }
        });
        Self { handle }
    }
}

impl Drop for EnrichmentHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Trait every enrichment pipeline accepts on entry. Default impl is
/// `StateFileSink` (writes the on-disk file); a desktop daemon may
/// stack a Tauri-event sink on top via `CompositeSink`. Keeping it as
/// a trait lets pipelines stay free of an axum/tauri dependency.
#[async_trait::async_trait]
pub trait EnrichmentProgressSink: Send + Sync {
    async fn report(
        &self,
        phase: EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    );
    async fn complete(&self);
    async fn fail(&self, error: &str);
}

/// State-file sink — the persistence half. Tied to a specific corpus
/// index dir on construction so callers don't have to thread the path
/// through every progress call.
pub struct StateFileSink {
    index_dir: PathBuf,
    corpus_id: String,
    pipeline_id: Option<String>,
}

impl StateFileSink {
    pub fn new(
        index_dir: impl Into<PathBuf>,
        corpus_id: impl Into<String>,
        pipeline_id: Option<String>,
    ) -> Self {
        Self {
            index_dir: index_dir.into(),
            corpus_id: corpus_id.into(),
            pipeline_id,
        }
    }
}

#[async_trait::async_trait]
impl EnrichmentProgressSink for StateFileSink {
    async fn report(
        &self,
        phase: EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    ) {
        if let Err(e) = EnrichmentStateFile::stamp(
            &self.index_dir,
            &self.corpus_id,
            self.pipeline_id.as_deref(),
            phase,
            step_current,
            step_total,
            message,
        ) {
            tracing::warn!(
                corpus = %self.corpus_id,
                phase = phase.label(),
                error = %e,
                "enrichment_state: stamp failed; UI may lag this transition"
            );
        }
    }

    async fn complete(&self) {
        self.report(EnrichmentPhase::Complete, 0, 0, None).await;
    }

    async fn fail(&self, error: &str) {
        if let Err(e) = EnrichmentStateFile::fail(&self.index_dir, &self.corpus_id, error) {
            tracing::warn!(
                corpus = %self.corpus_id,
                error = %e,
                "enrichment_state: fail-stamp failed; state file will lag actual failure"
            );
        }
    }
}

/// Compose two sinks into one — the file sink for durability and the
/// event sink for UI push, for example. `report`/`complete`/`fail`
/// fan out to both halves; either failing is logged inside the half
/// and does NOT short-circuit the other.
pub struct CompositeSink {
    pub left: std::sync::Arc<dyn EnrichmentProgressSink>,
    pub right: std::sync::Arc<dyn EnrichmentProgressSink>,
}

impl CompositeSink {
    pub fn new(
        left: std::sync::Arc<dyn EnrichmentProgressSink>,
        right: std::sync::Arc<dyn EnrichmentProgressSink>,
    ) -> Self {
        Self { left, right }
    }
}

#[async_trait::async_trait]
impl EnrichmentProgressSink for CompositeSink {
    async fn report(
        &self,
        phase: EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    ) {
        self.left
            .report(phase, step_current, step_total, message)
            .await;
        self.right
            .report(phase, step_current, step_total, message)
            .await;
    }
    async fn complete(&self) {
        self.left.complete().await;
        self.right.complete().await;
    }
    async fn fail(&self, error: &str) {
        self.left.fail(error).await;
        self.right.fail(error).await;
    }
}

/// Scan `indexes_root` for state files whose phase is non-terminal
/// and whose `last_progress_at` is older than `STALL_THRESHOLD_SECS`,
/// rewriting them as `Stalled` with `error = "daemon restart"`.
/// Returns the corpus IDs that were transitioned so callers can
/// emit a single rollup log line.
///
/// Called from the daemon's startup sequence — guarantees that a
/// crash, kill, or normal restart never leaves a corpus's UI claiming
/// "RAPTOR leaves" forever. The next observation of the state file
/// shows `Stalled` which carries `retry` semantics in the UI.
pub fn sweep_stalled_states(indexes_root: &Path) -> Result<Vec<String>> {
    let mut transitioned = Vec::new();
    if !indexes_root.is_dir() {
        return Ok(transitioned);
    }
    for entry in std::fs::read_dir(indexes_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let state = match EnrichmentStateFile::read(&path) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "sweep_stalled_states: read failed; skipping"
                );
                continue;
            }
        };
        if state.phase.is_terminal() {
            continue;
        }
        let now = now_secs();
        if now - state.last_progress_at < STALL_THRESHOLD_SECS {
            continue;
        }
        let corpus_id = state.corpus_id.clone();
        let elapsed = now - state.last_progress_at;
        if let Err(e) = EnrichmentStateFile::fail(
            &path,
            &corpus_id,
            &format!("stalled — no progress for {elapsed}s (daemon likely restarted mid-pipeline)"),
        ) {
            tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "sweep_stalled_states: stall-mark failed"
            );
            continue;
        }
        // The fail() helper writes phase=Failed; bump to Stalled so
        // UI can distinguish operator-facing retry vs. real
        // application error. A second write is cheap given the
        // already-rare path.
        if let Err(e) = EnrichmentStateFile::stamp(
            &path,
            &corpus_id,
            state.pipeline_id.as_deref(),
            EnrichmentPhase::Stalled,
            0,
            0,
            Some("interrupted by daemon restart"),
        ) {
            tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "sweep_stalled_states: stall-stamp failed"
            );
            continue;
        }
        transitioned.push(corpus_id);
    }
    Ok(transitioned)
}

use corpus_engine_yield::time::unix_now as now_secs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        state.phase = EnrichmentPhase::RaptorLeaves;
        state.step_current = 17;
        state.step_total = 45;
        state.message = Some("summarising leaf 17 / 45".into());
        EnrichmentStateFile::write(tmp.path(), &state).unwrap();
        let read = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(read.corpus_id, "c-1");
        assert_eq!(read.phase, EnrichmentPhase::RaptorLeaves);
        assert_eq!(read.step_current, 17);
        assert_eq!(read.step_total, 45);
    }

    #[test]
    fn stamp_preserves_started_at() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = EnrichmentState::new("c-1", None);
        EnrichmentStateFile::write(tmp.path(), &initial).unwrap();
        // Force a delay so any naive `last_progress_at = started_at`
        // bug surfaces in the test.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let after = EnrichmentStateFile::stamp(
            tmp.path(),
            "c-1",
            None,
            EnrichmentPhase::RaptorTree,
            2,
            5,
            Some("tree level 2 / 5"),
        )
        .unwrap();
        assert_eq!(after.started_at, initial.started_at);
        assert!(after.last_progress_at >= after.started_at);
    }

    #[test]
    fn resumable_interruption_covers_stalled_and_nonterminal_but_not_done_or_failed() {
        // Resume the corpses a killed process leaves behind...
        assert!(EnrichmentPhase::Starting.is_resumable_interruption());
        assert!(EnrichmentPhase::RaptorLeaves.is_resumable_interruption());
        assert!(EnrichmentPhase::Persisting.is_resumable_interruption());
        // ...including Stalled (terminal, but the boot stall-sweep just
        // re-labelled an interrupted non-terminal build).
        assert!(EnrichmentPhase::Stalled.is_resumable_interruption());
        assert!(EnrichmentPhase::Stalled.is_terminal());
        // ...but never a clean finish or a genuine failure (would loop).
        assert!(!EnrichmentPhase::Complete.is_resumable_interruption());
        assert!(!EnrichmentPhase::Failed.is_resumable_interruption());
    }

    #[test]
    fn stamp_clears_stale_completed_at_when_a_new_run_supersedes() {
        // The folder-pipeline zombie: a prior run stamped Complete
        // (setting completed_at), then a fresh run re-enters a
        // non-terminal phase. The file must NOT keep the old
        // completed_at — that leaves `phase=non-terminal` +
        // `completed_at=set`, the exact self-contradiction that made
        // the desktop chip read "done" while work was still in flight.
        let tmp = tempfile::tempdir().unwrap();
        let mut done = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        done.phase = EnrichmentPhase::Complete;
        done.completed_at = Some(done.last_progress_at);
        EnrichmentStateFile::write(tmp.path(), &done).unwrap();

        // A fresh run re-enters RaptorLeaves.
        let after = EnrichmentStateFile::stamp(
            tmp.path(),
            "c-1",
            Some("folder_tiered"),
            EnrichmentPhase::RaptorLeaves,
            0,
            8,
            Some("building RAPTOR tree"),
        )
        .unwrap();
        assert_eq!(after.phase, EnrichmentPhase::RaptorLeaves);
        assert!(
            after.completed_at.is_none(),
            "a non-terminal stamp must clear the prior run's completed_at"
        );
        assert!(!after.phase.is_terminal());
    }

    #[test]
    fn is_stale_only_for_old_non_terminal_states() {
        let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        let now = s.last_progress_at;
        // Fresh non-terminal build → not stale.
        assert!(!s.is_stale(now));
        // Just under the threshold → still live.
        assert!(!s.is_stale(now + STALL_THRESHOLD_SECS));
        // Past the threshold → wedged.
        assert!(s.is_stale(now + STALL_THRESHOLD_SECS + 1));
        // A terminal phase is never stale, however old.
        s.phase = EnrichmentPhase::Complete;
        assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
        s.phase = EnrichmentPhase::Failed;
        assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
        // Stalled is already terminal — not double-counted as "stale".
        s.phase = EnrichmentPhase::Stalled;
        assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
    }

    #[test]
    fn is_stale_when_error_stamped_on_nonterminal_phase() {
        // The real-world zombie: a stall sweep stamps an error but a
        // concurrent writer leaves phase=Starting and bumps
        // last_progress_at, so the age looks fresh. The error must still
        // win.
        let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        let now = s.last_progress_at;
        s.error = Some("stalled — no progress for 3371s".into());
        assert!(s.phase == EnrichmentPhase::Starting);
        assert!(s.is_stale(now)); // fresh clock, but error present → stale
        // A terminal phase with an error is a normal Failed — not "stale".
        s.phase = EnrichmentPhase::Failed;
        assert!(!s.is_stale(now));
    }

    #[test]
    fn heartbeat_bumps_live_state_without_changing_phase() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        s.phase = EnrichmentPhase::EntityExtraction;
        s.step_current = 3;
        s.step_total = 10;
        s.message = Some("Finding people, places, and ideas".into());
        // Backdate so a live build looks stale before the heartbeat.
        let stale_ts = now_secs() - STALL_THRESHOLD_SECS - 60;
        s.last_progress_at = stale_ts;
        EnrichmentStateFile::write(tmp.path(), &s).unwrap();
        assert!(s.is_stale(now_secs()), "precondition: backdated build is stale");

        EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

        let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert!(
            after.last_progress_at > stale_ts,
            "heartbeat must advance last_progress_at"
        );
        assert!(!after.is_stale(now_secs()), "heartbeat must clear staleness");
        // Phase / step / message are preserved — only the clock moves.
        assert_eq!(after.phase, EnrichmentPhase::EntityExtraction);
        assert_eq!(after.step_current, 3);
        assert_eq!(after.step_total, 10);
        assert_eq!(after.message.as_deref(), Some("Finding people, places, and ideas"));
    }

    #[test]
    fn heartbeat_is_noop_on_terminal_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = EnrichmentState::new("c-1", None);
        s.phase = EnrichmentPhase::Complete;
        let ts = now_secs() - 500;
        s.last_progress_at = ts;
        s.completed_at = Some(ts);
        EnrichmentStateFile::write(tmp.path(), &s).unwrap();

        EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

        let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(after.phase, EnrichmentPhase::Complete);
        assert_eq!(
            after.last_progress_at, ts,
            "heartbeat must not touch a terminal state"
        );
    }

    #[test]
    fn heartbeat_is_noop_on_errored_nonterminal_state() {
        // A build a sweeper declared dead (error stamped on a still
        // non-terminal phase) must not be resurrected by a heartbeat.
        let tmp = tempfile::tempdir().unwrap();
        let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        s.phase = EnrichmentPhase::Starting;
        s.error = Some("stalled — no progress for 3371s".into());
        let ts = now_secs() - 500;
        s.last_progress_at = ts;
        EnrichmentStateFile::write(tmp.path(), &s).unwrap();

        EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

        let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(after.last_progress_at, ts, "errored state must stay untouched");
        assert!(after.is_stale(now_secs()), "errored state stays stale after heartbeat");
    }

    #[test]
    fn heartbeat_is_noop_when_state_absent() {
        let tmp = tempfile::tempdir().unwrap();
        // No state file yet — heartbeat is a clean no-op, does not create one.
        EnrichmentStateFile::heartbeat(tmp.path()).unwrap();
        assert!(EnrichmentStateFile::read(tmp.path()).unwrap().is_none());
    }

    #[tokio::test]
    async fn heartbeat_guard_keeps_a_live_build_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
        s.phase = EnrichmentPhase::EntityExtraction;
        s.last_progress_at = now_secs() - STALL_THRESHOLD_SECS - 60;
        EnrichmentStateFile::write(tmp.path(), &s).unwrap();

        let guard = EnrichmentHeartbeat::spawn_every(
            tmp.path().to_path_buf(),
            std::time::Duration::from_millis(20),
        );
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert!(
            !after.is_stale(now_secs()),
            "guard should have bumped last_progress_at at least once"
        );
        assert_eq!(after.phase, EnrichmentPhase::EntityExtraction);
        drop(guard);
    }

    #[test]
    fn sweep_marks_old_non_terminal_as_stalled() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus_dir = tmp.path().join("corpus-a");
        std::fs::create_dir_all(&corpus_dir).unwrap();
        let mut stale = EnrichmentState::new("corpus-a", Some("folder_tiered".into()));
        stale.phase = EnrichmentPhase::RaptorLeaves;
        stale.last_progress_at = now_secs() - STALL_THRESHOLD_SECS - 60;
        EnrichmentStateFile::write(&corpus_dir, &stale).unwrap();

        let transitioned = sweep_stalled_states(tmp.path()).unwrap();
        assert_eq!(transitioned, vec!["corpus-a".to_string()]);
        let after = EnrichmentStateFile::read(&corpus_dir).unwrap().unwrap();
        assert_eq!(after.phase, EnrichmentPhase::Stalled);
        assert!(after.error.is_some());
    }

    #[test]
    fn sweep_leaves_recent_progress_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus_dir = tmp.path().join("corpus-b");
        std::fs::create_dir_all(&corpus_dir).unwrap();
        let mut fresh = EnrichmentState::new("corpus-b", None);
        fresh.phase = EnrichmentPhase::RaptorTree;
        fresh.last_progress_at = now_secs() - 60; // 1 min ago
        EnrichmentStateFile::write(&corpus_dir, &fresh).unwrap();

        let transitioned = sweep_stalled_states(tmp.path()).unwrap();
        assert!(transitioned.is_empty());
        let after = EnrichmentStateFile::read(&corpus_dir).unwrap().unwrap();
        assert_eq!(after.phase, EnrichmentPhase::RaptorTree);
    }
}
