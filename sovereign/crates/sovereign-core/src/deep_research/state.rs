// SPDX-License-Identifier: AGPL-3.0-or-later
//! R11-thin state machine — enumerated states, one transition table.
//!
//! icd-schemas.md §13: states are a typed enum; a transition not in the
//! table is a compile error (FR-1). Abort is an input to every state's
//! transition set and lands on Rendering with truncation declared. The
//! slot deadline (`max_rounds`, F28) and budget exhaustion are terminal
//! conditions recorded distinctly. The run-scoped lock (F19) refuses a
//! second run against the same run directory.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The enumerated loop states (icd-schemas.md §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Initializing,
    Planning,
    /// STEER 2 (directive 3c5d8b53): the pre-acquisition alignment
    /// gate — after a plan is written the port decides (proceed, or
    /// redirect the question) BEFORE any acquisition spend. A redirect
    /// re-plans through the SAME PlanWritten row — one enumerated
    /// re-plan transition (FR-1), the question-stewardship sibling of
    /// the mid-run re-frame.
    Align,
    Rounding,
    Surveying,
    Auditing,
    /// GAP-4 (the re-frame, FR-1): the structural-surprise state — a
    /// spinning loop (no acquisition, gap list unchanged) lands here
    /// with the typed re-frame event; the run re-plans against the
    /// same estate with the reframed question. The ONE enumerated
    /// re-plan transition — never an ad-hoc branch, never a silently
    /// seeded new run.
    Reframing,
    Querying,
    Triage,
    Fetching,
    Enriching,
    Synthesizing,
    Rendering,
    Done,
    DonePartial,
    Aborted,
}

/// The terminal states — distinct and distinctly reported.
pub const TERMINAL_STATES: [State; 3] = [State::Done, State::DonePartial, State::Aborted];

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Initializing => "initializing",
            State::Planning => "planning",
            State::Align => "aligning",
            State::Rounding => "rounding",
            State::Surveying => "surveying",
            State::Auditing => "auditing",
            State::Reframing => "reframing",
            State::Querying => "querying",
            State::Triage => "triage",
            State::Fetching => "fetching",
            State::Enriching => "enriching",
            State::Synthesizing => "synthesizing",
            State::Rendering => "rendering",
            State::Done => "done",
            State::DonePartial => "done-partial",
            State::Aborted => "aborted",
        }
    }

    pub fn is_terminal(self) -> bool {
        TERMINAL_STATES.contains(&self)
    }

    /// The one transition table. A pair not listed here is a compile
    /// error by construction — every transition in the loop is decided
    /// through this function.
    pub fn transition(from: State, event: Event) -> Option<State> {
        use Event::*;
        use State::*;
        Some(match (from, event) {
            (Initializing, CharterWritten) => Planning,
            // STEER 2: every plan — the launch plan and every re-plan —
            // passes the alignment gate before any acquisition spend.
            // AlignProceed opens the rounds; AlignRedirect re-enters
            // Planning, where the SAME PlanWritten row gates the
            // re-plan (one enumerated re-plan transition, FR-1).
            (Planning, PlanWritten) => Align,
            (Align, AlignProceed) => Rounding,
            (Align, AlignRedirect) => Planning,
            (Rounding, RoundStarted) => Surveying,
            (Surveying, SurveyComplete) => Auditing,
            (Auditing, NoNewGaps) => Synthesizing,
            (Auditing, BudgetExhausted) => Synthesizing,
            (Auditing, GapCycle) => Querying,
            // GAP-4: the structural-surprise re-frame — Auditing lands
            // on Reframing (the reframe record is written there), and
            // ReframeWritten re-enters Planning — the SAME PlanWritten
            // row drives the re-plan (plan-2.json). One enumerated
            // re-plan transition (FR-1).
            (Auditing, ReframeRequested) => Reframing,
            (Reframing, ReframeWritten) => Planning,
            (Querying, QueriesFormed) => Triage,
            (Triage, TriageComplete) => Fetching,
            (Fetching, FetchComplete) => Enriching,
            (Enriching, EnrichComplete) => Rounding, // budget check → next round
            // The max_rounds tail lands here: rounds exhausted with
            // gaps still open, the loop at Rounding after the last
            // acquire_round. `finish()` then runs the final audit +
            // verdict set + report — the terminal chain from
            // Synthesizing. Measured missing in demo run dr-1786720828
            // ("no transition for (rounding, BudgetExhausted)").
            (Rounding, BudgetExhausted) => Synthesizing,
            (Synthesizing, DraftReady) => Rendering,
            (Rendering, ReportRendered) => Done,
            (Rendering, ReportRenderedPartial) => DonePartial,
            // Abort from EVERY state — the input is in every row.
            (Initializing, Abort) => Aborted,
            (Planning, Abort) => Aborted,
            (Align, Abort) => Aborted,
            (Rounding, Abort) => Aborted,
            (Surveying, Abort) => Aborted,
            (Auditing, Abort) => Aborted,
            (Reframing, Abort) => Aborted,
            (Querying, Abort) => Aborted,
            (Triage, Abort) => Aborted,
            (Fetching, Abort) => Aborted,
            (Enriching, Abort) => Aborted,
            (Synthesizing, Abort) => Aborted,
            (Rendering, Abort) => Aborted,
            (Aborted, Abort) => Aborted,
            // Rendering with truncation declared is the abort landing.
            (Aborted, AbortRendered) => DonePartial,
            _ => return None,
        })
    }
}

/// State-machine inputs. The abort signal is a first-class input in
/// every state's transition set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    CharterWritten,
    PlanWritten,
    /// STEER 2: the alignment gate's decision is Proceed — the plan
    /// stands, acquisition may begin.
    AlignProceed,
    /// STEER 2: the alignment gate's decision is Redirect — the run
    /// re-plans against the same estate with the redirected question
    /// (alignment-1.json recorded).
    AlignRedirect,
    RoundStarted,
    SurveyComplete,
    NoNewGaps,
    BudgetExhausted,
    /// GAP-4: the typed re-frame event — fired by the loop when the
    /// structural-surprise trigger fires at Auditing (only when a
    /// reframe input was staged at launch).
    ReframeRequested,
    /// GAP-4: the reframe record is on disk (reframe-1.json).
    ReframeWritten,
    GapCycle,
    QueriesFormed,
    TriageComplete,
    FetchComplete,
    EnrichComplete,
    DraftReady,
    ReportRendered,
    ReportRenderedPartial,
    Abort,
    AbortRendered,
}

/// The run-scoped lock (F19): flock on `<run_dir>/lock`. A second run
/// against the same run directory refuses at acquisition. The lifecycle
/// is recorded in the manifest's `lock` record.
#[derive(Debug)]
pub struct RunLock {
    file: File,
    /// The lock file path — `File` does not expose one; keeping it lets
    /// release/Drop remove the file (the visible released state).
    path: PathBuf,
    pub id: String,
    pub acquired_at_unix: i64,
}

impl RunLock {
    /// Acquire the run-scoped lock, refusing a second opener. Fail-closed:
    /// an unreadable/lockable lock file is a refused run, not a warning.
    pub fn acquire(run_dir: &Path, run_id: &str) -> Result<RunLock, String> {
        let lock_path = run_dir.join("lock");
        // flock semantics (order deep-research-t3a): create(true), then
        // File::try_lock. A LIVE second run holds the flock and refuses
        // (F19) — the refusal names "already exists". A STALE lock file
        // left by a SIGKILL'd process holds no flock and is acquirable —
        // the operator's `--resume` is the visible act that acquires it.
        // (Pre-t3a this was create_new/O_EXCL, which also refused the
        // stale file and would have blocked resume forever.)
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&lock_path)
            .map_err(|e| {
                format!(
                    "run lock refused: {lock_path:?} already exists or is unwritable ({e}); \
                     a second run against the same run dir must not proceed (F19)"
                )
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "run lock refused: {lock_path:?} already exists and is held by a live run; \
                     a second run against the same run dir must not proceed (F19). A stale lock \
                     file from a dead process is acquirable — `--resume` is the visible act."
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!(
                    "run lock refused: {lock_path:?} could not be flocked ({e}) — fail-closed (F19)"
                ));
            }
        }
        let acquired_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(RunLock {
            file,
            path: lock_path,
            id: run_id.to_string(),
            acquired_at_unix,
        })
    }

    /// Release the lock: remove the lock file. The manifest's lock
    /// record captures `released_at_unix`.
    pub fn release(&mut self) -> i64 {
        let released_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Best-effort removal; the file's absence is the visible released state.
        let _ = std::fs::remove_file(&self.path);
        released_at_unix
    }

    /// The lock file path (for the manifest record).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        // A dropped lock without an explicit release still unblocks the
        // dir — the lock is per-process; the file removal is best-effort
        // so a panic mid-run does not permanently wedge the run dir.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_table_is_exhaustive_for_abort() {
        // Every state accepts Abort (abort-from-every-state).
        for state in [
            State::Initializing,
            State::Planning,
            State::Align,
            State::Rounding,
            State::Surveying,
            State::Auditing,
            State::Reframing,
            State::Querying,
            State::Triage,
            State::Fetching,
            State::Enriching,
            State::Synthesizing,
            State::Rendering,
        ] {
            assert_eq!(
                State::transition(state, Event::Abort),
                Some(State::Aborted),
                "abort must land every state on Aborted"
            );
        }
    }

    #[test]
    fn happy_path_transitions() {
        assert_eq!(
            State::transition(State::Initializing, Event::CharterWritten),
            Some(State::Planning)
        );
        assert_eq!(
            State::transition(State::Auditing, Event::NoNewGaps),
            Some(State::Synthesizing)
        );
        assert_eq!(
            State::transition(State::Auditing, Event::BudgetExhausted),
            Some(State::Synthesizing)
        );
        // The max_rounds tail: rounds exhausted at Rounding with gaps
        // still open → the terminal chain (watched failure: demo run
        // dr-1786720828).
        assert_eq!(
            State::transition(State::Rounding, Event::BudgetExhausted),
            Some(State::Synthesizing)
        );
        assert_eq!(
            State::transition(State::Rendering, Event::ReportRendered),
            Some(State::Done)
        );
        assert_eq!(
            State::transition(State::Rendering, Event::ReportRenderedPartial),
            Some(State::DonePartial)
        );
        assert_eq!(
            State::transition(State::Aborted, Event::AbortRendered),
            Some(State::DonePartial)
        );
    }

    #[test]
    fn align_transitions_are_enumerated() {
        // STEER 2: every plan write lands on Align; Proceed opens the
        // rounds; a Redirect re-enters Planning where the SAME
        // PlanWritten row gates the re-plan (one enumerated re-plan
        // transition, FR-1).
        assert_eq!(
            State::transition(State::Planning, Event::PlanWritten),
            Some(State::Align)
        );
        assert_eq!(
            State::transition(State::Align, Event::AlignProceed),
            Some(State::Rounding)
        );
        assert_eq!(
            State::transition(State::Align, Event::AlignRedirect),
            Some(State::Planning)
        );
        assert_eq!(
            State::transition(State::Align, Event::Abort),
            Some(State::Aborted)
        );
    }

    #[test]
    fn reframe_transitions_are_enumerated() {
        // GAP-4: the structural-surprise re-frame is a typed pair —
        // Auditing → Reframing → Planning (the re-plan reuses the same
        // PlanWritten row: ONE enumerated re-plan transition, FR-1).
        assert_eq!(
            State::transition(State::Auditing, Event::ReframeRequested),
            Some(State::Reframing)
        );
        assert_eq!(
            State::transition(State::Reframing, Event::ReframeWritten),
            Some(State::Planning)
        );
        assert_eq!(
            State::transition(State::Planning, Event::PlanWritten),
            Some(State::Align),
            "the re-plan drives the SAME PlanWritten row as the first plan — \
             every plan passes the alignment gate (STEER 2)"
        );
        // The reframe is Auditing-only: it cannot fire from anywhere
        // else, and it can never fire twice in a row (a second
        // ReframeRequested from Reframing is a compile-time-none pair).
        assert_eq!(
            State::transition(State::Reframing, Event::ReframeRequested),
            None
        );
        assert_eq!(
            State::transition(State::Rounding, Event::ReframeRequested),
            None
        );
        assert_eq!(State::transition(State::Reframing, Event::GapCycle), None);
    }

    #[test]
    fn unknown_transitions_refuse() {
        assert_eq!(State::transition(State::Done, Event::Abort), None);
        assert_eq!(
            State::transition(State::Initializing, Event::DraftReady),
            None
        );
        assert_eq!(
            State::transition(State::DonePartial, Event::RoundStarted),
            None
        );
    }

    #[test]
    fn lock_stale_file_is_acquirable() {
        // A lock file left by a SIGKILL'd run (file on disk, no live
        // flock) is a STALE lock: the operator's `--resume` acquires it
        // (order deep-research-t3a). Pre-t3a the create_new acquire
        // refused it forever.
        let dir = std::env::temp_dir().join(format!("dr-lock-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap(); // the stale file
        let mut lock = RunLock::acquire(&dir, "run-stale").expect("stale lock is acquirable");
        assert_eq!(lock.id, "run-stale");
        // A live hold after acquiring the stale file still refuses a
        // second opener (F19 unchanged).
        let second = RunLock::acquire(&dir, "run-2");
        assert!(second.is_err());
        assert!(second.unwrap_err().contains("already exists"));
        lock.release();
        assert!(
            !dir.join("lock").exists(),
            "release removes the lock file (the visible released state)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_refuses_second_run() {
        let dir = std::env::temp_dir().join(format!("dr-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock = RunLock::acquire(&dir, "run-1").expect("first acquire succeeds");
        let second = RunLock::acquire(&dir, "run-2");
        assert!(
            second.is_err(),
            "second run against the same run dir must refuse (F19)"
        );
        assert!(second.unwrap_err().contains("already exists"));
        lock.release();
        assert!(
            RunLock::acquire(&dir, "run-3").is_ok(),
            "after release the dir is acquirable again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
