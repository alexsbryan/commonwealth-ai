//! Worker-emitted events plus the `EventSink` indirection.
//!
//! Per ARCH §9 (glassbox): every non-obvious decision in the sweep
//! loop emits one of these events. The CLI tail consumes them; in
//! Phase 2 the desktop streams them onto a Tauri channel.

use std::sync::Arc;

use serde::Serialize;

use super::status::{DiffSummary, SweepPhase, TrippedRule};

/// One observable thing the worker did during a sweep. Names follow
/// the `watched_folder:short_action` convention so log greps line up.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WatchedFolderEvent {
    SweepStarted {
        corpus_id: String,
        sweep_id: String,
    },
    Walked {
        corpus_id: String,
        visited: usize,
    },
    DiffComputed {
        corpus_id: String,
        summary: DiffSummary,
    },
    GuardTripped {
        corpus_id: String,
        rule: TrippedRule,
    },
    PhaseProgress {
        corpus_id: String,
        phase: SweepPhase,
        done: usize,
        total: usize,
    },
    SweepCompleted {
        corpus_id: String,
        applied: DiffSummary,
        duration_secs: u64,
    },
    SweepErrored {
        corpus_id: String,
        message: String,
    },
    /// A previously-deleted file with the same content hash reappeared
    /// within the soft-delete grace window. The tombstone is dropped
    /// and the file is re-extracted as an `Added` doc (because the
    /// chunks were physically deleted at apply time — option (c) from
    /// the plan).
    RevivalDetected {
        corpus_id: String,
        doc_id: String,
    },
    /// Tombstone exceeded the grace window and is unrecoverable.
    TombstoneExpired {
        corpus_id: String,
        doc_id: String,
    },
    /// Tombstone count exceeded the per-corpus cap (default 100k).
    /// Oldest entries evicted; emitted at `warn!` level so an operator
    /// notices the cap is being hit before grace expiry would have
    /// drained the list naturally.
    TombstoneEvicted {
        corpus_id: String,
        evicted_count: usize,
    },
    /// Sweep skipped — already running, paused, daemon shutting down.
    SweepSkipped {
        corpus_id: String,
        reason: String,
    },
}

impl WatchedFolderEvent {
    /// The corpus this event pertains to. All variants carry it; this
    /// helper saves callers from a five-arm match when they just want
    /// to route by corpus.
    pub fn corpus_id(&self) -> &str {
        match self {
            WatchedFolderEvent::SweepStarted { corpus_id, .. }
            | WatchedFolderEvent::Walked { corpus_id, .. }
            | WatchedFolderEvent::DiffComputed { corpus_id, .. }
            | WatchedFolderEvent::GuardTripped { corpus_id, .. }
            | WatchedFolderEvent::PhaseProgress { corpus_id, .. }
            | WatchedFolderEvent::SweepCompleted { corpus_id, .. }
            | WatchedFolderEvent::SweepErrored { corpus_id, .. }
            | WatchedFolderEvent::RevivalDetected { corpus_id, .. }
            | WatchedFolderEvent::TombstoneExpired { corpus_id, .. }
            | WatchedFolderEvent::TombstoneEvicted { corpus_id, .. }
            | WatchedFolderEvent::SweepSkipped { corpus_id, .. } => corpus_id,
        }
    }
}

/// Sink for `WatchedFolderEvent`s. The daemon installs a sink that
/// fans events into `tracing::info!`/`warn!` (per ARCH §9.2);
/// integration tests install one that pushes into a `Vec` for
/// assertion. Single-method closure type alias — no trait widening
/// per ARCH §5.3.
pub type EventSink = Arc<dyn Fn(WatchedFolderEvent) + Send + Sync>;

/// A no-op sink — useful for unit tests and CLI commands that don't
/// care about progress events.
pub fn noop_sink() -> EventSink {
    Arc::new(|_| {})
}
