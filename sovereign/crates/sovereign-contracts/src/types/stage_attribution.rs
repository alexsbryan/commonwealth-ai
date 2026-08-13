// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-turn STACK ATTRIBUTION — which system spent the turn's time.
//!
//! `NATIVE_GROUNDING_ECONOMY.md` §3.4 names **G4** — "the system can tell
//! what it decided and why" — as a function no stage owns, and a standing
//! compass-#1 violation on the subsystem the whole `native-grounding`
//! initiative is named for. This is G4's wire half.
//!
//! # It is an ATTRIBUTION, not a profiler
//!
//! A profiler says `gate 121s` and still requires the reader to know what
//! belongs in a gate. Every row here names the **system that owns the
//! stage** as well as its cost, so the reader does not have to:
//!
//! ```text
//! Answered in 150.2s
//!   retrieval    32.6s   shared      chain floor — both designs pay it
//!   draft        25.4s   shared      the one irreducible generative call
//!   audit        25.6s   OLD STACK   per-claim judge ladder, 5 claims
//!   rewrite      43.2s   OLD STACK   full re-synthesis — surgical fell back
//!   re-audit     50.9s   OLD STACK   exists only because rewrite ran
//!   segments      0.1s   new         typed verdict + span resolution
//!   unattributed  2.4s   —           time no row claimed
//! ```
//!
//! # The honesty property, which is the whole point
//!
//! **Rows are appended by the code that EXECUTES the stage, at the moment
//! it executes.** Nothing in this file, and nothing that writes to it,
//! consults a flag to decide whether a stage happened. That constraint
//! exists because of two recorded failures on this same initiative:
//!
//! * `enforced = false` telemetry was technically accurate on every event
//!   and told nobody that the incumbent judge ladder was still running on
//!   the same turn. The flag reported itself honestly; the *system* was
//!   opaque.
//! * The surgical-vs-full rewrite branch is known to the code and invisible
//!   everywhere else — only a debug-gated `dbg()` records it. It fell back
//!   at 43.2s on one turn and engaged at 5.36s on another run of the same
//!   query, and nothing outside a debug build could say which.
//!
//! A strip that renders "new stack" because a flag is on would lie exactly
//! the way those two did.
//!
//! # A mechanism with no row is a DEFECT IN THE STRIP, and is detectable
//!
//! The failure mode this type is most exposed to is a mechanism that runs
//! and contributes no row — the strip then renders a clean turn that is not
//! clean. Two nested residuals make that case *visible* rather than silent,
//! and both are computed by [`TurnStageLedger::seal`] from two
//! independently produced numbers:
//!
//! * [`StageId::GateUnattributed`] — the gate's own wall clock (measured by
//!   the one funnel every gate decision passes through) minus the sum of
//!   the stage rows recorded inside that window. A gate mechanism that
//!   forgot to record itself shows up here as seconds nobody claimed.
//! * [`StageId::TurnUnattributed`] — the same arithmetic at turn scale.
//!
//! Both rows are **always present, including at zero**: "measured, found
//! nothing" and "not measured" are different facts (ARCH §18.3), and a
//! residual that is only rendered when it is large is a residual the reader
//! cannot trust when it is small.
//!
//! # One decider, one name
//!
//! [`TurnStageLedger::served_by`] is derived from the rows **here**, in the
//! runtime, and the UI reads it. Both could compute it; ARCH §10.6 says the
//! producer decides and the UI reads, so that a desktop strip and a CLI
//! footer cannot disagree about whether the old stack ran.
//!
//! # Not a decision surface
//!
//! Nothing in the runtime may branch on this type. It is written after the
//! fact by stages that have already run, and read only by renderers. That
//! is the same structural guarantee `native_grounding::segments` carries,
//! for the same reason.

use serde::{Deserialize, Serialize};

/// Which system owns a stage's cost.
///
/// **Closed set, so it is an enum** (ARCH §2, smell-table row one). The
/// grounding subsystem already carries a five-place abstention string
/// namespace as its cautionary tale; this does not add a sixth.
///
/// Three members, not two, and the third is load-bearing. Labelling
/// retrieval and draft synthesis as "new" would be the flag-lie this type
/// exists to prevent: neither belongs to either stack. Retrieval (R1) and
/// generation (S1) are the chain floor — `NATIVE_GROUNDING_ECONOMY.md`
/// §3.1/§3.2 has both surviving permanently under every design considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackOwner {
    /// The native grounding stack — H1's admission verdict, the typed
    /// verdict, display segmentation, span resolution.
    Native,
    /// The incumbent judge ladder — per-claim generative audit, the rewrite
    /// pass, the re-audit, the retry machinery.
    Incumbent,
    /// Neither stack: the chain floor both designs pay. Retrieval and the
    /// draft's own generation.
    Shared,
}

impl StackOwner {
    /// The word the reader sees. Defined once, here, so the desktop strip
    /// and the CLI footer cannot drift (ARCH §10.6).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            StackOwner::Native => "new",
            StackOwner::Incumbent => "OLD STACK",
            StackOwner::Shared => "shared",
        }
    }
}

/// Which stage of the turn a row is about.
///
/// Closed set. A stage that is not in this list cannot be recorded, which
/// is deliberate: adding a mechanism to the runtime and having it appear in
/// the strip should require naming it here, in the contract, rather than
/// inventing a string at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageId {
    /// Corpus search and evidence-pool assembly (R1).
    Retrieval,
    /// H1's answerability admission (`native_grounding::admission`).
    Admission,
    /// The draft's own generation (S1).
    Draft,
    /// The first audit pass over the draft — claim extraction, the
    /// per-claim judge fan-out, the unsupported-specifics scan.
    Audit,
    /// The repair pass: surgical span edits, or a full re-synthesis.
    Rewrite,
    /// The audit pass over the repaired text. Exists only because the
    /// rewrite produced new, unaudited prose.
    ReAudit,
    /// The short path's re-synthesis after a violation-probability retry.
    Retry,
    /// The short path's two-stage `verify_grounding` critic.
    Verify,
    /// The quote-then-answer citation path
    /// (`grounding/citation.rs`) — the gate's own pre-verification
    /// rehearsal.
    Citation,
    /// Display segmentation + span resolution over the released text.
    Segments,
    /// The gate's wall clock minus the rows recorded inside it. See the
    /// module docs: this is the in-gate residual and it is the detector for
    /// a gate mechanism that recorded nothing.
    GateUnattributed,
    /// The turn's wall clock minus every row above. The turn-scale
    /// residual.
    TurnUnattributed,
}

impl StageId {
    /// The word the reader sees. One name per stage (ARCH §10.6).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            StageId::Retrieval => "retrieval",
            StageId::Admission => "admission",
            StageId::Draft => "draft",
            StageId::Audit => "audit",
            StageId::Rewrite => "rewrite",
            StageId::ReAudit => "re-audit",
            StageId::Retry => "retry",
            StageId::Verify => "verify",
            StageId::Citation => "citation",
            StageId::Segments => "segments",
            StageId::GateUnattributed => "gate — unattributed",
            StageId::TurnUnattributed => "turn — unattributed",
        }
    }

    /// Whether this row is a residual rather than a measured stage.
    ///
    /// Residuals are *arithmetic*, not observations, and the two must not
    /// be added together — a renderer that highlights them differently is
    /// reading this, not pattern-matching the label.
    #[must_use]
    pub fn is_residual(self) -> bool {
        matches!(self, StageId::GateUnattributed | StageId::TurnUnattributed)
    }
}

/// Which mechanism actually ran inside a stage that has more than one, when
/// the choice is otherwise invisible to the reader.
///
/// Closed set. `None` on a row means the stage has no branch, **not** that
/// the branch is unknown — a stage with a branch always records which arm
/// it took, at the branch site, or it is a defect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageMechanism {
    /// The repair edited only the failed spans, on the fast slot.
    /// `NATIVE_GROUNDING.md` §9 marks this component **keep**.
    SurgicalRewrite,
    /// The repair re-synthesised the whole answer on the primary — the path
    /// surgery was built to avoid. Measured at 43.2s against surgery's
    /// 5.36s on the same query (`NATIVE_GROUNDING_ECONOMY.md` §7.3).
    FullResynthesis,
    /// A generative per-claim judge decided support, claim by claim.
    PerClaimJudge,
    /// A deterministic containment / span-resolution check decided.
    Deterministic,
}

impl StageMechanism {
    /// The phrase the reader sees beside the stage.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            StageMechanism::SurgicalRewrite => "surgical span edits",
            StageMechanism::FullResynthesis => "full re-synthesis (surgical fell back)",
            StageMechanism::PerClaimJudge => "per-claim generative judge",
            StageMechanism::Deterministic => "deterministic containment",
        }
    }
}

/// Why a stage ran at all.
///
/// The order this type serves asks the strip to say not just *what* the old
/// stack cost but *why it was there* — "re-audit exists only because rewrite
/// ran" is the difference between a profile and an attribution.
///
/// Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageCause {
    /// Runs on every turn of this shape.
    EveryTurn,
    /// The audit found claims it judged unsupported.
    AuditFoundFailures,
    /// New, unaudited prose exists because a repair pass produced it.
    RewriteProducedNewProse,
    /// The judged violation probability crossed the surface's threshold.
    ViolationOverThreshold,
}

impl StageCause {
    /// The phrase the reader sees explaining why the stage ran.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            StageCause::EveryTurn => "runs on every turn",
            StageCause::AuditFoundFailures => "the audit found unsupported claims",
            StageCause::RewriteProducedNewProse => "exists only because the rewrite ran",
            StageCause::ViolationOverThreshold => "violation probability crossed the threshold",
        }
    }
}

/// One stage of one turn, attributed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRow {
    /// Which stage of the turn this row is about.
    pub stage: StageId,
    /// Which system owns this stage's cost.
    pub owner: StackOwner,
    /// Wall time this stage spent, in milliseconds.
    pub ms: u64,
    /// Which arm a branching stage took. `None` = the stage does not branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<StageMechanism>,
    /// Why the stage ran. `None` = not recorded by this call site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<StageCause>,
    /// Model calls this stage made, when the stage counts them. `None` is
    /// "this stage does not count its calls", never "zero" (ARCH §18.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<u32>,
}

/// Which systems served the turn — derived from the rows, by the producer.
///
/// Closed set. Note there is no `Unknown`: a ledger that exists has rows,
/// and a turn with no ledger has no strip at all rather than a strip that
/// shrugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServedBy {
    /// No incumbent stage executed. The claim the initiative is trying to
    /// be able to make.
    NativeOnly,
    /// Incumbent stages executed and no native stage did.
    IncumbentOnly,
    /// Both stacks executed on this turn. This is today's normal case and
    /// establishing it took four hours of archaeology on 2026-08-12.
    BothStacks,
    /// Only chain-floor stages executed — retrieval and the draft. A turn
    /// that never reached a gate.
    ChainFloorOnly,
}

impl ServedBy {
    /// The headline phrase: which stack(s) served the turn.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ServedBy::NativeOnly => "the new stack only",
            ServedBy::IncumbentOnly => "the OLD stack only",
            ServedBy::BothStacks => "BOTH stacks",
            ServedBy::ChainFloorOnly => "no grounding stack ran",
        }
    }
}

/// The per-turn stage attribution, as it reaches the wire.
///
/// Built by `sovereign-core`'s stage ledger and serialised into the
/// message's metadata under `stage_attribution`. **Absent** on turns that
/// never opened a ledger — never an empty ledger, so "not measured" and
/// "measured, nothing to report" stay distinguishable (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnStageLedger {
    /// The turn's wall clock, measured independently of the rows.
    pub total_ms: u64,
    /// Stage rows in execution order, followed by the two residuals.
    pub rows: Vec<StageRow>,
    /// Derived here so every renderer agrees (ARCH §10.6).
    pub served_by: ServedBy,
    /// Total wall time attributed to incumbent-owned stages. Rendered as
    /// the strip's headline when it is non-zero, because it is the single
    /// number the operator asked to be able to see at a glance.
    pub incumbent_ms: u64,
}

impl TurnStageLedger {
    /// Close a ledger: append the turn-scale residual and derive
    /// [`ServedBy`] and [`Self::incumbent_ms`] from the rows.
    ///
    /// `rows` must already contain any in-gate residual — the gate's own
    /// funnel appends that, because only the funnel holds the gate's
    /// independently measured wall clock.
    pub fn seal(total_ms: u64, mut rows: Vec<StageRow>) -> Self {
        let attributed: u64 = rows.iter().map(|r| r.ms).sum();
        rows.push(StageRow {
            stage: StageId::TurnUnattributed,
            owner: StackOwner::Shared,
            ms: total_ms.saturating_sub(attributed),
            mechanism: None,
            cause: None,
            calls: None,
        });

        // Derived from what EXECUTED.
        //
        // A residual at ZERO is pure arithmetic and never votes — otherwise
        // every turn would read `BothStacks` by construction, which is the
        // flag-lie in a new costume. But a NON-ZERO
        // [`StageId::GateUnattributed`] is not arithmetic: the gate window
        // only exists if the gate ran, so seconds inside it that no row
        // claimed are positive evidence that incumbent code executed, by
        // some mechanism this build does not yet name.
        //
        // This distinction was NOT in the first draft, and a live turn
        // found it: an uninstrumented citation path put 11.08s in the
        // residual while the turn rendered "no grounding stack ran"
        // (2026-08-12). Under-reporting the old stack is the one direction
        // this strip must never fail in.
        let mut native = false;
        let mut incumbent = false;
        let mut incumbent_ms = 0u64;
        for r in rows.iter() {
            let counts =
                !r.stage.is_residual() || (r.stage == StageId::GateUnattributed && r.ms > 0);
            if !counts {
                continue;
            }
            match r.owner {
                StackOwner::Native => native = true,
                StackOwner::Incumbent => {
                    incumbent = true;
                    incumbent_ms += r.ms;
                }
                StackOwner::Shared => {}
            }
        }
        let served_by = match (native, incumbent) {
            (true, true) => ServedBy::BothStacks,
            (true, false) => ServedBy::NativeOnly,
            (false, true) => ServedBy::IncumbentOnly,
            (false, false) => ServedBy::ChainFloorOnly,
        };
        Self {
            total_ms,
            rows,
            served_by,
            incumbent_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(stage: StageId, owner: StackOwner, ms: u64) -> StageRow {
        StageRow {
            stage,
            owner,
            ms,
            mechanism: None,
            cause: None,
            calls: None,
        }
    }

    #[test]
    fn seal_names_both_stacks_when_both_executed() {
        let l = TurnStageLedger::seal(
            100_000,
            vec![
                row(StageId::Retrieval, StackOwner::Shared, 32_000),
                row(StageId::Admission, StackOwner::Native, 40),
                row(StageId::Draft, StackOwner::Shared, 25_000),
                row(StageId::Audit, StackOwner::Incumbent, 25_600),
            ],
        );
        assert_eq!(l.served_by, ServedBy::BothStacks);
        assert_eq!(l.incumbent_ms, 25_600);
    }

    #[test]
    fn seal_names_native_only_when_no_incumbent_stage_ran() {
        let l = TurnStageLedger::seal(
            60_000,
            vec![
                row(StageId::Retrieval, StackOwner::Shared, 32_000),
                row(StageId::Admission, StackOwner::Native, 40),
                row(StageId::Segments, StackOwner::Native, 60),
            ],
        );
        assert_eq!(l.served_by, ServedBy::NativeOnly);
        assert_eq!(l.incumbent_ms, 0);
    }

    #[test]
    fn seal_names_chain_floor_only_when_no_grounding_stage_ran() {
        let l = TurnStageLedger::seal(
            40_000,
            vec![
                row(StageId::Retrieval, StackOwner::Shared, 32_000),
                row(StageId::Draft, StackOwner::Shared, 7_000),
            ],
        );
        assert_eq!(l.served_by, ServedBy::ChainFloorOnly);
    }

    /// The residual is the detector for a mechanism that ran and recorded
    /// nothing, so it must be arithmetic on the two independently measured
    /// numbers — never a fill so the rows add up.
    #[test]
    fn unaccounted_time_becomes_a_residual_row() {
        let l = TurnStageLedger::seal(
            100_000,
            vec![row(StageId::Retrieval, StackOwner::Shared, 30_000)],
        );
        let resid = l.rows.last().unwrap();
        assert_eq!(resid.stage, StageId::TurnUnattributed);
        assert_eq!(resid.ms, 70_000);
    }

    /// "Measured, found nothing" is a fact and is rendered as one.
    #[test]
    fn residual_row_is_present_even_at_zero() {
        let l = TurnStageLedger::seal(
            30_000,
            vec![row(StageId::Retrieval, StackOwner::Shared, 30_000)],
        );
        assert_eq!(l.rows.last().unwrap().stage, StageId::TurnUnattributed);
        assert_eq!(l.rows.last().unwrap().ms, 0);
    }

    /// A clock that runs backwards must not underflow into a 500-million-
    /// second row.
    #[test]
    fn residual_saturates_rather_than_underflowing() {
        let l = TurnStageLedger::seal(
            1_000,
            vec![row(StageId::Retrieval, StackOwner::Shared, 30_000)],
        );
        assert_eq!(l.rows.last().unwrap().ms, 0);
    }

    /// A ZERO residual is arithmetic, not an observation. If it voted, a
    /// turn with only shared stages would read as though a stack had run.
    #[test]
    fn zero_residuals_do_not_vote_on_served_by() {
        let l = TurnStageLedger::seal(
            100_000,
            vec![
                row(StageId::Retrieval, StackOwner::Shared, 90_000),
                row(StageId::GateUnattributed, StackOwner::Incumbent, 0),
            ],
        );
        assert_eq!(l.served_by, ServedBy::ChainFloorOnly);
        assert_eq!(l.incumbent_ms, 0);
    }

    /// The regression a live turn found: an uninstrumented gate mechanism
    /// burned 11.08s, no row claimed it, and the turn rendered as
    /// "no grounding stack ran". A NON-ZERO gate residual is evidence the
    /// gate ran, so it votes and it is counted.
    #[test]
    fn a_nonzero_gate_residual_is_evidence_the_old_stack_ran() {
        let l = TurnStageLedger::seal(
            31_387,
            vec![
                row(StageId::Retrieval, StackOwner::Shared, 8_470),
                row(StageId::Draft, StackOwner::Shared, 11_743),
                row(StageId::GateUnattributed, StackOwner::Incumbent, 11_080),
            ],
        );
        assert_eq!(l.served_by, ServedBy::IncumbentOnly);
        assert_eq!(l.incumbent_ms, 11_080);
    }

    /// The turn-scale residual still never votes: unlike the gate window,
    /// it exists on every turn and owns no stack.
    #[test]
    fn the_turn_residual_never_votes() {
        let l = TurnStageLedger::seal(
            100_000,
            vec![row(StageId::Retrieval, StackOwner::Shared, 10_000)],
        );
        assert_eq!(l.rows.last().unwrap().ms, 90_000);
        assert_eq!(l.served_by, ServedBy::ChainFloorOnly);
    }

    /// The wire spelling is a contract with `answerProvenance.ts`. A rename
    /// here without one there silently blanks the strip.
    #[test]
    fn wire_spellings_are_pinned() {
        let l = TurnStageLedger::seal(
            10_000,
            vec![StageRow {
                stage: StageId::Rewrite,
                owner: StackOwner::Incumbent,
                ms: 43_200,
                mechanism: Some(StageMechanism::FullResynthesis),
                cause: Some(StageCause::AuditFoundFailures),
                calls: Some(1),
            }],
        );
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["served_by"], "incumbent_only");
        assert_eq!(v["rows"][0]["stage"], "rewrite");
        assert_eq!(v["rows"][0]["owner"], "incumbent");
        assert_eq!(v["rows"][0]["mechanism"], "full_resynthesis");
        assert_eq!(v["rows"][0]["cause"], "audit_found_failures");
        assert_eq!(v["rows"][1]["stage"], "turn_unattributed");
    }

    /// `None` mechanism must be ABSENT on the wire, not `null`: the reader
    /// distinguishes "this stage does not branch" from "we did not look".
    #[test]
    fn absent_optionals_are_omitted_not_nulled() {
        let l = TurnStageLedger::seal(
            10_000,
            vec![row(StageId::Retrieval, StackOwner::Shared, 9_000)],
        );
        let v = serde_json::to_value(&l).unwrap();
        assert!(v["rows"][0].get("mechanism").is_none());
        assert!(v["rows"][0].get("calls").is_none());
    }

    #[test]
    fn round_trips() {
        let l = TurnStageLedger::seal(
            10_000,
            vec![row(StageId::Audit, StackOwner::Incumbent, 9_000)],
        );
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<TurnStageLedger>(&s).unwrap(), l);
    }
}
