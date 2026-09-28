// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a running unit is handed and what it answers when it reaches no
//! verdict — the context, [`JobError`] and [`subject_of`].
//!
//! Moved here from `commonwealth-work`'s executor seam by pb-work-donor
//! (FIVE_PROGRAMS §12 3a rung 2): the donor runs `ingest:v1` units through an
//! execute origin the svrn daemon serves, and both ends speak this vocabulary
//! on that loopback wire. The trait, the registry and the drive stay with
//! `commonwealth-work`, which re-exports every item here at its historical
//! path.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kernel_types::{Judgement, Reason, Verdict};
use serde::{Deserialize, Serialize};

use super::WorkRefusal;
use crate::{JobKind, JobUnit};

/// The tracing target a unit's progress is traced under: `commonwealth_work`'s
/// own, so `RUST_LOG=commonwealth_work=debug` still shows it after the move.
const TRACE_TARGET: &str = "commonwealth_work";

/// A sink a running unit reports progress to. The donor wires this to its own
/// `Renew` heartbeat.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

// -----------------------------------------------------------------
// The context
// -----------------------------------------------------------------

/// What a running unit is given: where to work, how to notice it should stop,
/// and where to say it is still alive.
///
/// Deliberately three fields. Anything a unit needs BEYOND these belongs in
/// its own payload, where it is hashed into the unit's identity and a peer can
/// see it; a context field is invisible to the rail and would be a way for two
/// donors to run "the same" unit differently.
pub struct JobContext {
    workdir: PathBuf,
    cancel: Arc<AtomicBool>,
    progress: Option<ProgressSink>,
}

impl JobContext {
    /// A context rooted at `workdir`, with nothing cancelling it and nobody
    /// listening for progress. Both are opt-in because a test and a `svrn job`
    /// dry run legitimately have neither.
    pub fn new(workdir: impl Into<PathBuf>) -> JobContext {
        JobContext {
            workdir: workdir.into(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }

    /// Share an existing cancellation flag — the donor holds the other end and
    /// sets it when the lease is lost or the process is shutting down.
    ///
    /// An `Arc<AtomicBool>` rather than a token type from a runtime library:
    /// this module must compile with no tokio (see the module doc), and a flag
    /// that both a sync validator and an async executor can read is the shape
    /// that works either way.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> JobContext {
        self.cancel = cancel;
        self
    }

    /// Attach a progress sink. `None` is the default and is not an error: a
    /// unit that reports nothing is a unit nobody is watching, which is
    /// different from a unit that has stalled.
    pub fn with_progress(mut self, progress: ProgressSink) -> JobContext {
        self.progress = Some(progress);
        self
    }

    /// The directory a unit's relative paths resolve against, and the only
    /// directory it is expected to touch.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The other end of the cancellation flag, for a donor that wants to stop
    /// this unit later.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Set the flag. Idempotent; a second cancel is not an error.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Has somebody asked this unit to stop?
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Report progress. Always traced at debug (ARCH §9.1) whether or not a
    /// sink is attached, so a stalled unit is visible under
    /// `RUST_LOG=commonwealth_work=debug` with no wiring at all.
    pub fn progress(&self, note: &str) {
        tracing::debug!(target: TRACE_TARGET, note, "unit progress");
        if let Some(sink) = &self.progress {
            sink(note);
        }
    }
}

impl std::fmt::Debug for JobContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobContext")
            .field("workdir", &self.workdir)
            .field("cancel_requested", &self.cancel_requested())
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

// -----------------------------------------------------------------
// The absence of a verdict
// -----------------------------------------------------------------

/// Why a unit reached no verdict.
///
/// **Every variant is a `CouldNotJudge` or a `NeverRan`, and there is no
/// variant that is a `Passed` or a `Failed`.** That is not a convention: the
/// enum has no arm [`JobError::verdict`] could map to either, which is what
/// makes the `Complete`/`Fail` boundary in the module doc structural rather
/// than remembered (ARCH §7).
///
/// The split inside the two is the one the repo's own vocabulary already
/// draws: `NeverRan` is "this unit was never started", `CouldNotJudge` is "it
/// started and we still cannot tell".
// Serde since pb-work-donor: an execute origin answers it over the wire.
#[derive(Debug, Serialize, Deserialize)]
pub enum JobError {
    /// No executor is registered for the unit's kind. The unit never started.
    NoExecutor { kind: JobKind },

    /// The predicate said no. The unit never started, and the refusal names
    /// which rule.
    Refused(WorkRefusal),

    /// The child process could not be started at all — a missing binary, a
    /// workdir that is not there. The unit never started.
    Spawn { program: String, reason: String },

    /// The wall cap fired and the process group was killed. It started; it
    /// reached no verdict.
    Timeout { secs: u64 },

    /// Somebody set the cancellation flag: the lease was lost to another
    /// donor, or this node is shutting down.
    Cancelled,

    /// The unit exited with a code its executor DECLARES means could-not-judge
    /// (`JobExecutorDescriptor::could_not_judge_exits`) rather than failed.
    ///
    /// The two that matter here are the ones `scripts/sovereign-test.sh`
    /// already defines: **4** is a run that resolved zero tests, and **5** is a
    /// run whose results could not be attributed to it. Both are exit-non-zero
    /// and neither is a failure of the code under test; reporting either as
    /// `Failed` is the false red this variant exists to prevent.
    DeclaredCouldNotJudge { exit_code: i32, tail: String },

    /// The unit ran and produced something, and that something is not a
    /// verdict: killed by a signal with no exit code at all, stdout that was
    /// supposed to be JSON and is not, a verdict line that is missing.
    NoVerdict { reason: String },
}

// Written out rather than derived: this leaf takes no `thiserror` (the
// `WorkRefusal` precedent). The sentences are the ones the derive carried in
// `commonwealth-work` before pb-work-donor moved the type here.
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::NoExecutor { kind } => write!(
                f,
                "no executor is registered for `{kind}` — this donor offered a kind it cannot run, or the unit reached a node that never registered one"
            ),
            JobError::Refused(refusal) => {
                write!(f, "the unit was refused before it ran: {refusal}")
            }
            JobError::Spawn { program, reason } => {
                write!(f, "`{program}` could not be started: {reason}")
            }
            JobError::Timeout { secs } => write!(
                f,
                "the unit was still running after {secs}s and its process group was killed — no verdict was reached"
            ),
            JobError::Cancelled => write!(
                f,
                "the unit was cancelled before it reached a verdict — the lease was lost, or the donor is stopping"
            ),
            JobError::DeclaredCouldNotJudge { exit_code, tail } => write!(
                f,
                "the unit exited {exit_code}, an exit code this executor declares means could-not-judge rather than failed — {tail}"
            ),
            JobError::NoVerdict { reason } => {
                write!(f, "the unit ran but no verdict could be read from it: {reason}")
            }
        }
    }
}

impl std::error::Error for JobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JobError::Refused(refusal) => Some(refusal),
            _ => None,
        }
    }
}

impl From<WorkRefusal> for JobError {
    fn from(refusal: WorkRefusal) -> Self {
        JobError::Refused(refusal)
    }
}

impl JobError {
    /// The verdict this absence maps to. Total, and never `Passed`/`Failed`.
    pub fn verdict(&self) -> Verdict {
        match self {
            // Nothing was started, so nothing can be said about the work.
            JobError::NoExecutor { .. } | JobError::Refused(_) => Verdict::NeverRan,
            // It started (or the attempt to start it is itself the failure to
            // reach a verdict) and we still cannot tell.
            JobError::Spawn { .. }
            | JobError::Timeout { .. }
            | JobError::Cancelled
            | JobError::DeclaredCouldNotJudge { .. }
            | JobError::NoVerdict { .. } => Verdict::CouldNotJudge,
        }
    }

    /// Render this as the [`Judgement`] a `Fail` act carries.
    ///
    /// `subject` is the caller's — usually [`subject_of`] — because a
    /// `JobError` does not know which unit it is about, and inventing a
    /// subject here would put two spellings of one unit on the rail.
    pub fn judgement(&self, subject: impl Into<String>) -> Judgement {
        let text = self.to_string();
        let reason = Reason::new(text).unwrap_or_else(|| {
            // `Reason::new` refuses placeholder text ("unknown", "n/a", …).
            // Every message above is a sentence, so this arm is unreachable
            // today — and it is written as a NAMED substitution rather than an
            // `expect`, because the alternative is a panic on the failure path
            // (ARCH §18.3).
            Reason::literal(
                "the executor's own refusal text was a placeholder — that is a bug in \
                 commonwealth-work, not in the unit",
            )
        });
        match self.verdict() {
            Verdict::NeverRan => Judgement::never_ran(subject, reason),
            _ => Judgement::could_not_judge(subject, reason),
        }
    }
}

/// The one spelling of what a judgement about a unit is *about*.
///
/// `<kind> <unit_hash>`: the kind so a reader can tell a `process:v1` row from
/// an `ingest:v1` row without a join, the hash because that is the unit's
/// identity. Written once here so a `Complete` and a `Fail` about the same
/// unit carry the same subject (ARCH §10.6).
pub fn subject_of(unit: &JobUnit) -> String {
    format!("{} {}", unit.kind, unit.unit_hash)
}
