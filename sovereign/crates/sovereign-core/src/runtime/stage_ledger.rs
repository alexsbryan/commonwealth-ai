// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-turn stage ledger — G4's runtime half.
//!
//! `NATIVE_GROUNDING_ECONOMY.md` §3.4 names **G4** ("the system can tell
//! what it decided and why") as a function no stage owns. This module is
//! the recording side; [`sovereign_contracts::types::TurnStageLedger`] is
//! the wire side and carries the design rationale.
//!
//! # What this is allowed to do
//!
//! Measure and report. **Nothing here may change what the system decides.**
//! No verdict, action, threshold or prompt is a function of anything in
//! this file, and every recording site is an append after the work is
//! already done. That is checked by inspection rather than by a type, but
//! it is a short file and the property is stated so the next reader can
//! check it in one pass.
//!
//! # Why the ledger is ambient rather than threaded
//!
//! A stage-timing parameter threaded through `gate_answer` →
//! `gate_answer_inner` → `gate_longform` and its five non-streaming callers
//! is a signature change on the exact call path this order is forbidden to
//! alter the behaviour of, for a value none of those functions read. The
//! ledger is therefore installed once per turn, as a task-local, by the one
//! function that owns the turn — and every stage records into it from
//! wherever it runs.
//!
//! The precedent is in the same subsystem: `gate_answer_with_progress` is
//! described in its own doc comment as "the ONE funnel through which every
//! gate decision reaches the journal — wrapping rather than instrumenting
//! each of the inner ladder's return sites, so no exit path can forget to
//! record (ARCH §10.6)". This is the same argument applied to timing.
//!
//! **The honest limit on it, stated rather than discovered.** A task-local
//! does not cross `tokio::spawn`. Gate work does not spawn — the ladder's
//! concurrency is `futures::future::join_all`, which polls in-task — so no
//! recorded stage is lost today. If a stage is ever moved onto a spawned
//! task and forgets to carry the scope, its time does not vanish silently:
//! it lands in the [`StageId::GateUnattributed`] residual, which is exactly
//! the case the residual exists to surface.
//!
//! # Absence
//!
//! Surfaces that do not open a ledger (the non-streaming handlers, the CLI,
//! every test) record into nothing and emit no strip at all — rather than
//! an empty strip, which would claim a measurement that never ran (ARCH
//! §18.3).

use std::sync::{Arc, Mutex};

use sovereign_contracts::types::{
    StackOwner, StageCause, StageId, StageMechanism, StageRow, TurnStageLedger,
};

tokio::task_local! {
    /// The turn's ledger, installed by [`TurnLedger::scope`].
    static TURN_LEDGER: TurnLedger;
}

/// A turn's accumulating stage rows.
///
/// Cheap to clone (one `Arc`); cloning shares the same rows, which is what
/// makes "one producer" true across the turn rather than per call site.
#[derive(Clone, Default)]
pub(crate) struct TurnLedger {
    rows: Arc<Mutex<Vec<StageRow>>>,
}

impl TurnLedger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Run `fut` with this ledger installed as the ambient one.
    pub(crate) async fn scope<F: std::future::Future>(self, fut: F) -> F::Output {
        TURN_LEDGER.scope(self, fut).await
    }

    fn push(&self, row: StageRow) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.push(row);
        }
    }

    fn len(&self) -> usize {
        self.rows.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Sum of the rows recorded at or after `from`.
    fn ms_since(&self, from: usize) -> u64 {
        self.rows
            .lock()
            .map(|r| r.iter().skip(from).map(|x| x.ms).sum())
            .unwrap_or(0)
    }

    /// Close the ledger and produce the wire value.
    ///
    /// `total_ms` is measured independently of the rows — that
    /// independence is what makes the residual a detector rather than a
    /// tautology.
    pub(crate) fn seal(self, total_ms: u64) -> TurnStageLedger {
        let rows = self
            .rows
            .lock()
            .map(|r| r.clone())
            .unwrap_or_else(|_| Vec::new());
        let sealed = TurnStageLedger::seal(total_ms, rows);
        tracing::info!(
            target: "stage_attribution",
            total_ms = sealed.total_ms,
            served_by = ?sealed.served_by,
            incumbent_ms = sealed.incumbent_ms,
            rows = sealed.rows.len(),
            "turn stage attribution sealed"
        );
        sealed
    }
}

/// One recorded stage. Built at the call site, appended to the ambient
/// ledger if there is one.
///
/// Constructed with the stage's OWNER at the site, deliberately: which
/// system owns a stage is a property of the code that runs it, and reading
/// it off a flag somewhere else is precisely the failure this instrument
/// exists to prevent (see the wire type's module docs).
pub(crate) struct Stage {
    stage: StageId,
    owner: StackOwner,
    mechanism: Option<StageMechanism>,
    cause: Option<StageCause>,
    calls: Option<u32>,
}

impl Stage {
    pub(crate) fn new(stage: StageId, owner: StackOwner) -> Self {
        Self {
            stage,
            owner,
            mechanism: None,
            cause: None,
            calls: None,
        }
    }

    /// The arm a branching stage actually took. Set at the branch.
    pub(crate) fn mechanism(mut self, m: StageMechanism) -> Self {
        self.mechanism = Some(m);
        self
    }

    /// Why the stage ran.
    pub(crate) fn cause(mut self, c: StageCause) -> Self {
        self.cause = Some(c);
        self
    }

    /// Model calls the stage made. Omit rather than pass 0 when the stage
    /// does not count them.
    pub(crate) fn calls(mut self, n: u32) -> Self {
        self.calls = Some(n);
        self
    }

    fn row(self, ms: u64) -> StageRow {
        let row = StageRow {
            stage: self.stage,
            owner: self.owner,
            ms,
            mechanism: self.mechanism,
            cause: self.cause,
            calls: self.calls,
        };
        tracing::debug!(
            target: "stage_attribution",
            stage = row.stage.label(),
            owner = ?row.owner,
            ms = row.ms,
            mechanism = ?row.mechanism,
            cause = ?row.cause,
            "stage attributed"
        );
        row
    }

    /// Append this stage to the ambient ledger.
    ///
    /// A no-op when no ledger is installed — that is the honest state for
    /// every surface that does not render a strip, not an error.
    pub(crate) fn record(self, ms: u64) {
        let row = self.row(ms);
        let _ = TURN_LEDGER.try_with(|l| l.push(row));
    }

    /// Append to a ledger held by hand rather than the ambient one.
    ///
    /// Needed by the two stages a turn measures from OUTSIDE its own
    /// scope: retrieval is timed by the caller that scopes it, so by the
    /// time the duration is known the scope has already closed. Using
    /// [`Self::record`] there silently dropped the row and pushed
    /// retrieval into the residual — which is exactly the "mechanism ran,
    /// no row" case, caught by the residual on the first live turn.
    pub(crate) fn record_into(self, ledger: &TurnLedger, ms: u64) {
        ledger.push(self.row(ms));
    }
}

/// Open a gate window: remember how many rows existed before the gate ran.
///
/// `None` when no ledger is installed.
pub(crate) fn gate_open() -> Option<usize> {
    TURN_LEDGER.try_with(|l| l.len()).ok()
}

/// Close a gate window, appending the in-gate residual.
///
/// `gate_ms` is the gate's own wall clock, measured by the funnel that
/// journals every gate decision — a number produced independently of the
/// stage rows. The difference between it and the rows recorded inside the
/// window is time a gate mechanism spent without recording itself, and it
/// is appended as a row rather than dropped: **a mechanism that fires with
/// no row is a defect in the strip**, and this is how that defect becomes
/// visible without a debug build.
///
/// The row is appended even at zero. "Measured, found nothing" and "not
/// measured" are different facts (ARCH §18.3), and a residual rendered only
/// when it is large is one the reader cannot trust when it is small.
pub(crate) fn gate_close(open_at: Option<usize>, gate_ms: u64) {
    let Some(open_at) = open_at else { return };
    let _ = TURN_LEDGER.try_with(|l| {
        let attributed = l.ms_since(open_at);
        let residual = gate_ms.saturating_sub(attributed);
        if residual > 0 {
            tracing::debug!(
                target: "stage_attribution",
                gate_ms,
                attributed_ms = attributed,
                residual_ms = residual,
                "gate time not claimed by any stage row — a mechanism ran without recording, or the gate has stages this build does not name"
            );
        }
        l.push(StageRow {
            stage: StageId::GateUnattributed,
            owner: StackOwner::Incumbent,
            ms: residual,
            mechanism: None,
            cause: None,
            calls: None,
        });
    });
}

/// Take the ambient ledger, if one is installed, and seal it.
pub(crate) fn seal_ambient(total_ms: u64) -> Option<TurnStageLedger> {
    TURN_LEDGER.try_with(|l| l.clone().seal(total_ms)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::ServedBy;

    #[tokio::test]
    async fn records_only_inside_a_scope() {
        // Outside: a no-op, and nothing panics.
        Stage::new(StageId::Draft, StackOwner::Shared).record(10);
        assert!(seal_ambient(100).is_none(), "no ledger => no strip at all");

        let l = TurnLedger::new();
        let sealed = l
            .clone()
            .scope(async {
                Stage::new(StageId::Draft, StackOwner::Shared).record(25_000);
                Stage::new(StageId::Audit, StackOwner::Incumbent)
                    .mechanism(StageMechanism::PerClaimJudge)
                    .cause(StageCause::EveryTurn)
                    .record(25_600);
                seal_ambient(60_000)
            })
            .await
            .expect("a ledger is installed");
        assert_eq!(sealed.served_by, ServedBy::IncumbentOnly);
        assert_eq!(sealed.incumbent_ms, 25_600);
    }

    /// The whole point of the in-gate residual: a mechanism that burned
    /// gate time and recorded nothing must show up as time nobody claimed.
    #[tokio::test]
    async fn unrecorded_gate_work_lands_in_the_gate_residual() {
        let sealed = TurnLedger::new()
            .scope(async {
                let open = gate_open();
                Stage::new(StageId::Audit, StackOwner::Incumbent).record(20_000);
                // ...and 30s of some mechanism that recorded nothing.
                gate_close(open, 50_000);
                seal_ambient(50_000)
            })
            .await
            .unwrap();
        let resid = sealed
            .rows
            .iter()
            .find(|r| r.stage == StageId::GateUnattributed)
            .expect("the gate residual row is always appended");
        assert_eq!(resid.ms, 30_000);
    }

    #[tokio::test]
    async fn gate_residual_is_appended_even_at_zero() {
        let sealed = TurnLedger::new()
            .scope(async {
                let open = gate_open();
                Stage::new(StageId::Audit, StackOwner::Incumbent).record(20_000);
                gate_close(open, 20_000);
                seal_ambient(20_000)
            })
            .await
            .unwrap();
        assert!(sealed
            .rows
            .iter()
            .any(|r| r.stage == StageId::GateUnattributed && r.ms == 0));
    }

    /// Rows recorded BEFORE the gate opened must not be counted against the
    /// gate's own clock — otherwise retrieval's 32s would mask 32s of
    /// unrecorded gate work.
    #[tokio::test]
    async fn gate_residual_ignores_rows_recorded_before_the_gate_opened() {
        let sealed = TurnLedger::new()
            .scope(async {
                Stage::new(StageId::Retrieval, StackOwner::Shared).record(32_000);
                let open = gate_open();
                gate_close(open, 40_000);
                seal_ambient(72_000)
            })
            .await
            .unwrap();
        let resid = sealed
            .rows
            .iter()
            .find(|r| r.stage == StageId::GateUnattributed)
            .unwrap();
        assert_eq!(resid.ms, 40_000, "the whole gate window was unrecorded");
    }

    /// The bug the residual caught on the first live turn: retrieval is
    /// timed by the caller that scoped it, so the duration is only known
    /// AFTER the scope closed. `record` there is a silent no-op.
    #[tokio::test]
    async fn record_into_lands_a_row_measured_outside_the_scope() {
        let l = TurnLedger::new();
        l.clone().scope(async {}).await;
        // Ambient path: nothing to record into — the row is lost.
        Stage::new(StageId::Retrieval, StackOwner::Shared).record(8_000);
        assert_eq!(l.len(), 0);
        // Explicit path: the row lands.
        Stage::new(StageId::Retrieval, StackOwner::Shared).record_into(&l, 8_000);
        assert_eq!(l.len(), 1);
        let sealed = l.seal(10_000);
        assert_eq!(sealed.rows[0].stage, StageId::Retrieval);
        assert_eq!(sealed.rows.last().unwrap().ms, 2_000);
    }

    /// The ledger is shared by clone, not copied — a stage recorded through
    /// one handle is visible through another.
    #[tokio::test]
    async fn clones_share_one_row_list() {
        let l = TurnLedger::new();
        l.clone()
            .scope(async {
                Stage::new(StageId::Draft, StackOwner::Shared).record(1_000);
            })
            .await;
        assert_eq!(l.len(), 1);
    }
}
