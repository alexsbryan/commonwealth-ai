// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replay a captured routing decision against the live scorer and the
//! live ranking policy — Phase 1 **S1** of
//! [`docs/specs/SCHEDULER_QUALITY.md`](../../../docs/specs/SCHEDULER_QUALITY.md).
//!
//! # The decomposition that makes S1 cheap
//!
//! §5 defines Tier-1 calibration as *decision-agreement between the
//! simulator and real hardware*. Read literally that seems to require
//! re-running [`crate::scheduler_core::rank`] over a capture — which
//! is impossible, because a [`RoutingDecision`] carries no manifests
//! and therefore no claims to score. It does not need to. Agreement
//! splits into two halves, and **both are computable from a
//! production capture as it exists today**:
//!
//!   - **Scorer agreement** — take each candidate's recorded
//!     [`CandidateInputs`], recorded `claim_score` and recorded
//!     locality, push them back through the *real*
//!     `score_with_adjustments`, and check the `final_score` comes
//!     out again. This asks: *does the record carry every input the
//!     score depended on?* A field the scorer reads and the record
//!     omits shows up here as a reproducible disagreement, not as a
//!     number that quietly drifts.
//!   - **Policy agreement** — take the recorded candidate scores,
//!     re-run the strictly-beats-local filter and the best-first sort
//!     ([`winners_over_local`]), and check the [`Verdict`] comes out
//!     again. This asks: *does the record carry every input the
//!     ranking depended on?*
//!
//! The two run off **independent inputs on purpose**: the policy half
//! consumes the *recorded* scores, never the recomputed ones. Chaining
//! them would make one scorer bug cascade into a policy failure and
//! you would learn strictly less from the same run.
//!
//! # Why `claim_affinity` is not needed (and not recorded)
//!
//! `score_with_adjustments` takes `claim_affinity`, which the record
//! does not carry — only the derived `observation_mult`. That looks
//! like a gap and is not one, for a reason worth stating because it is
//! load-bearing:
//!
//! ```text
//! observation_mult = effective_affinity(a, obs) / a
//!                  = (clamp(a) · (1 − w·f)) / a        [samples > 0]
//!                  = (1 − w·f)                          for a ∈ (0, 1]
//! ```
//!
//! and `a ∈ [0, 1]` **by construction** — `ScoredClaim::claim_affinity`
//! is always `CapabilityClaim::effective_affinity()`, which clamps
//! (NaN → 0). So the multiplier is independent of `a` across the whole
//! domain except `a == 0`; and `a == 0` is exactly the case
//! `claim_score == 0` (both are that same clamped affinity, and a
//! claim scoring zero on the hint gate is never scored at all). The
//! replay therefore probes with `1.0` when `claim_score > 0` and `0.0`
//! otherwise, and reproduces the breakdown exactly in both branches.
//!
//! This is the kind of thing S1 exists to settle *before* daemon time
//! is spent on a hardware capture: the answer determines whether
//! Phase 0's record schema needs another field, and it does not.
//!
//! # What a disagreement means
//!
//! Nothing here is a model of production. Every number is produced by
//! calling production's own code, so agreement below 1.000 on a
//! **simulated** capture — where the sim wrote the records by running
//! that same code moments earlier — is never a calibration finding.
//! It is a bug in the record, in the loader, or in this module. That
//! is precisely what makes the sim-generated fixture worth running as
//! a test: it has a known answer.
//!
//! On a *hardware* capture the same 1.000 is the S1 gate itself, and
//! it may only be computed from [`RecordMetrics`]-style facts — never
//! from anything a production capture cannot carry.
//!
//! [`RecordMetrics`]: crate::mesh_sim::scoreboard

use sovereign_core::oicp::{score_with_adjustments, BenchmarkResult, NodeObservations};

use crate::decision_log::{
    locality_from_label, CandidateInputs, CandidateKind, CandidateRecord, DecisionPath,
    RoutingDecision, ScoreRecord, Verdict,
};
use crate::decision_trace::SchedulerTrace;
use crate::oicp_select::ModelCandidate;
use crate::scheduler_core::{local_sentinel, winners_over_local};

/// Absolute tolerance on a reproduced score factor.
///
/// Not zero, and the reason is specific rather than defensive. The
/// affinity probe described in the module docs recomputes
/// `observation_mult` as `(1 · (1 − w·f)) / 1` where production
/// computed `(a · (1 − w·f)) / a`; those are the same real number and
/// may differ by an ulp in `f32`. Every other factor is bit-identical
/// arithmetic on bit-identical inputs, and `f32` survives the JSONL
/// round trip exactly (widened to `f64`, shortest-round-trip printed).
/// So the honest gate is "agrees to well within an ulp", and the
/// report also counts how many matched *exactly*.
pub const SCORE_TOLERANCE: f32 = 1e-6;

/// Cap on retained disagreement detail. A capture with thousands of
/// disagreements has one bug, not thousands; keeping every instance
/// would trade memory for no information. The totals are always
/// exact — see [`ReplayReport::scorer_disagreements_total`].
pub const MAX_RETAINED_DISAGREEMENTS: usize = 50;

// ---------------------------------------------------------------
// Outcomes of replaying one decision
// ---------------------------------------------------------------

/// Why a recorded decision is outside the domain this replay is
/// defined over. Not a failure — a decision the question does not
/// apply to, counted and reported so the denominator stays honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// A pre-scoring gate short-circuited: policy, not scheduling. No
    /// candidate was scored, so there is nothing to reproduce.
    Gated(String),
    /// `locate_named_model`, not `rank` — a different decision with a
    /// different shape and a different verdict vocabulary.
    NamedPath,
    /// The ranked path with an empty candidate set. Produced by the
    /// simulator's `Oracle` arm, which bypasses the scorer by
    /// construction, and by nothing in production.
    NoCandidates,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::Gated(g) => write!(f, "gated:{g}"),
            SkipReason::NamedPath => write!(f, "named-path"),
            SkipReason::NoCandidates => write!(f, "no-candidates"),
        }
    }
}

/// A recorded input the replay could not interpret. **This is the
/// finding case**: it names the specific field the record failed to
/// carry, so a gap reads as a schema to-do rather than as noise in an
/// agreement ratio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayGap {
    /// `CandidateRecord::locality` held a label this build does not
    /// know. Either a newer producer or a corrupted record.
    UnknownLocality { candidate: String, label: String },
}

impl std::fmt::Display for ReplayGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayGap::UnknownLocality { candidate, label } => {
                write!(f, "{candidate}: unknown locality label '{label}'")
            }
        }
    }
}

/// One candidate's recorded score against its reproduction.
#[derive(Debug, Clone)]
pub struct CandidateCheck {
    pub name: String,
    pub recorded: ScoreRecord,
    /// `Err` when a recorded input could not be interpreted at all.
    pub recomputed: Result<ScoreRecord, ReplayGap>,
    /// `|recomputed.final_score − recorded.final_score|`, `None` on a
    /// gap.
    pub final_score_delta: Option<f32>,
    /// Which factors differ beyond [`SCORE_TOLERANCE`], by field name.
    /// Naming them is what turns "the score moved" into "the
    /// throughput term moved", which points at the missing input.
    pub disagreeing_factors: Vec<&'static str>,
}

impl CandidateCheck {
    pub fn agrees(&self) -> bool {
        self.recomputed.is_ok() && self.disagreeing_factors.is_empty()
    }

    /// Whether every factor reproduced bit-for-bit. Stronger than
    /// [`Self::agrees`]; reported rather than gated on, because the
    /// affinity probe legitimately costs an ulp (see
    /// [`SCORE_TOLERANCE`]).
    pub fn exact(&self) -> bool {
        self.recomputed
            .as_ref()
            .is_ok_and(|r| score_records_identical(&self.recorded, r))
    }
}

/// One decision, replayed.
#[derive(Debug, Clone)]
pub struct DecisionReplay {
    pub decision_id: String,
    pub candidates: Vec<CandidateCheck>,
    pub recorded_verdict: Verdict,
    pub recomputed_verdict: Verdict,
}

impl DecisionReplay {
    pub fn policy_agrees(&self) -> bool {
        self.recorded_verdict == self.recomputed_verdict
    }
}

/// A scorer disagreement, kept for the report.
#[derive(Debug, Clone)]
pub struct ScorerDisagreement {
    pub decision_id: String,
    pub check: CandidateCheck,
}

/// A policy disagreement, kept for the report.
#[derive(Debug, Clone)]
pub struct PolicyDisagreement {
    pub decision_id: String,
    pub recorded: Verdict,
    pub recomputed: Verdict,
}

// ---------------------------------------------------------------
// Reconstructing the scorer's arguments from a record
// ---------------------------------------------------------------

/// Rebuild the observation snapshot the scorer was handed.
///
/// Written as an exhaustive struct literal **on purpose** — no
/// `..Default::default()`. A new field on `NodeObservations` is a new
/// scorer input, and the right response to one is a compile error
/// here, forcing whoever added it to answer "does the decision record
/// carry this?" A default-filled literal would answer that question
/// silently, and wrongly.
pub fn observations_from(inputs: &CandidateInputs) -> NodeObservations {
    NodeObservations {
        in_flight: inputs.in_flight,
        p50_latency_ms: inputs.p50_latency_ms,
        p95_latency_ms: inputs.p95_latency_ms,
        recent_failure_rate: inputs.recent_failure_rate,
        samples: inputs.samples,
        ttft_ewma_ms: inputs.ttft_ewma_ms,
        tg_tok_s_ewma: inputs.tg_tok_s_ewma,
    }
}

/// Rebuild the gossiped benchmark, to the extent the scorer reads it.
///
/// `throughput_factor` consults exactly two fields — `tg_tok_s` and
/// `baseline_size_gb` — so the other three are reconstructed as
/// placeholders rather than guessed at. `CandidateInputs::with_benchmark`
/// writes all four benchmark fields together or none, so `tg_tok_s`
/// alone is a sound presence test.
pub fn benchmark_from(inputs: &CandidateInputs) -> Option<BenchmarkResult> {
    let tg_tok_s = inputs.bench_tg_tok_s?;
    Some(BenchmarkResult {
        // Not read by the scorer; the record carries no id and
        // inventing one would be worse than an obviously empty field.
        baseline_model_id: String::new(),
        baseline_size_gb: inputs.bench_baseline_size_gb.unwrap_or(0.0),
        pp_tok_s: inputs.bench_pp_tok_s.unwrap_or(0.0),
        tg_tok_s,
        // Not read by the scorer either — `bench_age_secs` is the
        // recorded derivative and there is no `now` to invert it
        // against.
        measured_at: 0,
    })
}

/// Recompute one candidate's whole [`ScoreRecord`] by calling the
/// production scorer on the record's own inputs.
pub fn recompute_score(record: &CandidateRecord) -> Result<ScoreRecord, ReplayGap> {
    let locality = locality_from_label(&record.locality).ok_or_else(|| {
        ReplayGap::UnknownLocality {
            candidate: record.name.clone(),
            label: record.locality.clone(),
        }
    })?;
    let obs = observations_from(&record.inputs);
    let bench = benchmark_from(&record.inputs);
    // See the module docs: any affinity in (0, 1] yields the same
    // `observation_mult`, and 0 is exactly the `claim_score == 0`
    // case. Probing rather than recording keeps Phase 0's schema
    // unchanged.
    let affinity_probe = if record.score.claim_score > 0.0 {
        1.0
    } else {
        0.0
    };
    let breakdown = score_with_adjustments(
        record.score.claim_score,
        affinity_probe,
        &obs,
        locality,
        record.size_gb.unwrap_or(0.0),
        bench.as_ref(),
        // Unclamped as received; the scorer clamps it itself, and
        // `ScoreRecord::availability` holds the clamped result.
        record.inputs.availability,
    );
    Ok(ScoreRecord::from(&breakdown))
}

/// Re-run the ranking policy over a decision's **recorded** scores.
///
/// Deliberately reads `final_score` from the record rather than from
/// [`recompute_score`], so scorer agreement and policy agreement stay
/// independent measurements.
pub fn recompute_verdict(decision: &RoutingDecision) -> Verdict {
    let as_candidate = |c: &CandidateRecord| ModelCandidate {
        score: c.score.final_score,
        size_gb: c.size_gb,
        model_id: c.model_id.clone(),
        // Neither `pick_better` nor `candidates_equal` reads affinity;
        // it exists on `ScoredClaim` for the scorer, which has already
        // run by this point in the decision.
        claim_affinity: 0.0,
    };
    let local = decision
        .candidates
        .iter()
        .find(|c| c.kind == CandidateKind::Local)
        .map(as_candidate)
        .unwrap_or_else(local_sentinel);
    // Record order is scoring order, and `winners_over_local` is
    // order-sensitive on full ties — see its doc comment.
    let peers: Vec<(String, ModelCandidate)> = decision
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Peer)
        .map(|c| (c.name.clone(), as_candidate(c)))
        .collect();
    let ranked: Vec<String> = winners_over_local(&local, peers)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    if ranked.is_empty() {
        Verdict::StayLocal
    } else {
        Verdict::Peers { ranked }
    }
}

/// Replay one decision. `Err` names why the decision is outside the
/// replay's domain.
pub fn replay_decision(decision: &RoutingDecision) -> Result<DecisionReplay, SkipReason> {
    if let Verdict::Gated { gate } = &decision.verdict {
        return Err(SkipReason::Gated(gate.clone()));
    }
    if decision.path != DecisionPath::RankedOicp {
        return Err(SkipReason::NamedPath);
    }
    if decision.candidates.is_empty() {
        return Err(SkipReason::NoCandidates);
    }
    let candidates = decision
        .candidates
        .iter()
        .map(|c| {
            let recomputed = recompute_score(c);
            let (delta, factors) = match &recomputed {
                Ok(r) => (
                    Some((r.final_score - c.score.final_score).abs()),
                    disagreeing_factors(&c.score, r),
                ),
                Err(_) => (None, Vec::new()),
            };
            CandidateCheck {
                name: c.name.clone(),
                recorded: c.score.clone(),
                recomputed,
                final_score_delta: delta,
                disagreeing_factors: factors,
            }
        })
        .collect();
    Ok(DecisionReplay {
        decision_id: decision.decision_id.clone(),
        candidates,
        recorded_verdict: decision.verdict.clone(),
        recomputed_verdict: recompute_verdict(decision),
    })
}

// ---------------------------------------------------------------
// Aggregate report
// ---------------------------------------------------------------

/// What a replay over a whole capture found.
///
/// Both agreement ratios return `0.0` on an empty denominator rather
/// than a vacuous `1.0` — same rule as
/// [`SchedulerTrace::join_rate`](crate::decision_trace::SchedulerTrace::join_rate),
/// and for the same reason: a gate that passes because nothing was
/// measured is the failure mode hardest to notice.
#[derive(Debug, Clone, Default)]
pub struct ReplayReport {
    pub decisions_seen: usize,
    pub replayed: usize,
    /// Counts by reason, for the decisions the replay is not defined
    /// over. Sorted for a stable render.
    pub skipped: Vec<(SkipReason, usize)>,
    pub candidates_checked: usize,
    pub candidates_agreed: usize,
    /// Of the agreeing candidates, how many reproduced bit-for-bit.
    pub candidates_exact: usize,
    pub max_final_score_delta: f32,
    pub policy_agreed: usize,
    pub scorer_disagreements: Vec<ScorerDisagreement>,
    pub scorer_disagreements_total: usize,
    pub policy_disagreements: Vec<PolicyDisagreement>,
    pub policy_disagreements_total: usize,
    /// Records that could not be interpreted at all, deduplicated by
    /// display form so one bad producer does not print a thousand
    /// lines.
    pub gaps: Vec<ReplayGap>,
}

impl ReplayReport {
    /// Fraction of scored candidates whose `final_score` and every
    /// factor reproduced from the record alone.
    pub fn scorer_agreement(&self) -> f64 {
        if self.candidates_checked == 0 {
            return 0.0;
        }
        self.candidates_agreed as f64 / self.candidates_checked as f64
    }

    /// Fraction of replayed decisions whose verdict reproduced from
    /// the recorded scores alone.
    pub fn policy_agreement(&self) -> f64 {
        if self.replayed == 0 {
            return 0.0;
        }
        self.policy_agreed as f64 / self.replayed as f64
    }

    fn note_skip(&mut self, reason: SkipReason) {
        if let Some(slot) = self.skipped.iter_mut().find(|(r, _)| *r == reason) {
            slot.1 += 1;
        } else {
            self.skipped.push((reason, 1));
        }
    }

    fn note_gap(&mut self, gap: ReplayGap) {
        if !self.gaps.contains(&gap) {
            self.gaps.push(gap);
        }
    }
}

/// Replay every decision in an iterator and aggregate.
pub fn replay_decisions<'a, I>(decisions: I) -> ReplayReport
where
    I: IntoIterator<Item = &'a RoutingDecision>,
{
    let mut report = ReplayReport::default();
    for decision in decisions {
        report.decisions_seen += 1;
        let replay = match replay_decision(decision) {
            Ok(r) => r,
            Err(reason) => {
                report.note_skip(reason);
                continue;
            }
        };
        report.replayed += 1;
        if replay.policy_agrees() {
            report.policy_agreed += 1;
        } else {
            report.policy_disagreements_total += 1;
            if report.policy_disagreements.len() < MAX_RETAINED_DISAGREEMENTS {
                report.policy_disagreements.push(PolicyDisagreement {
                    decision_id: replay.decision_id.clone(),
                    recorded: replay.recorded_verdict.clone(),
                    recomputed: replay.recomputed_verdict.clone(),
                });
            }
        }
        for check in replay.candidates {
            report.candidates_checked += 1;
            if let Some(d) = check.final_score_delta {
                if d > report.max_final_score_delta {
                    report.max_final_score_delta = d;
                }
            }
            if let Err(gap) = &check.recomputed {
                report.note_gap(gap.clone());
            }
            if check.agrees() {
                report.candidates_agreed += 1;
                if check.exact() {
                    report.candidates_exact += 1;
                }
                continue;
            }
            report.scorer_disagreements_total += 1;
            if report.scorer_disagreements.len() < MAX_RETAINED_DISAGREEMENTS {
                report.scorer_disagreements.push(ScorerDisagreement {
                    decision_id: replay.decision_id.clone(),
                    check,
                });
            }
        }
    }
    report.skipped.sort_by_key(|(r, _)| r.to_string());
    report
}

/// Replay a loaded trace. The convenience entry point for both the
/// sim-generated fixture and a hardware capture.
pub fn replay_trace(trace: &SchedulerTrace) -> ReplayReport {
    replay_decisions(trace.episodes.iter().map(|e| &e.decision))
}

// ---------------------------------------------------------------
// Comparison + rendering
// ---------------------------------------------------------------

fn close(a: f32, b: f32) -> bool {
    // NaN never reproduces silently: two NaNs are not "equal enough".
    (a - b).abs() <= SCORE_TOLERANCE
}

/// Which factors of a reproduced score fall outside tolerance, by
/// field name.
pub fn disagreeing_factors(recorded: &ScoreRecord, recomputed: &ScoreRecord) -> Vec<&'static str> {
    let mut out = Vec::new();
    let pairs: [(&'static str, f32, f32); 7] = [
        ("claim_score", recorded.claim_score, recomputed.claim_score),
        (
            "observation_mult",
            recorded.observation_mult,
            recomputed.observation_mult,
        ),
        ("load_penalty", recorded.load_penalty, recomputed.load_penalty),
        (
            "locality_bonus",
            recorded.locality_bonus,
            recomputed.locality_bonus,
        ),
        (
            "cold_start_weight",
            recorded.cold_start_weight,
            recomputed.cold_start_weight,
        ),
        (
            "throughput_factor",
            recorded.throughput_factor,
            recomputed.throughput_factor,
        ),
        ("availability", recorded.availability, recomputed.availability),
    ];
    for (name, a, b) in pairs {
        if !close(a, b) {
            out.push(name);
        }
    }
    if recorded.throughput_source != recomputed.throughput_source {
        out.push("throughput_source");
    }
    if !close(recorded.final_score, recomputed.final_score) {
        out.push("final_score");
    }
    out
}

fn score_records_identical(a: &ScoreRecord, b: &ScoreRecord) -> bool {
    a == b
}

impl std::fmt::Display for ReplayReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "scheduler replay — {} decisions seen, {} replayed",
            self.decisions_seen, self.replayed
        )?;
        if !self.skipped.is_empty() {
            let parts: Vec<String> = self
                .skipped
                .iter()
                .map(|(r, n)| format!("{r} {n}"))
                .collect();
            writeln!(f, "  skipped            {}", parts.join(", "))?;
        }
        writeln!(
            f,
            "  scorer agreement   {:.3}   ({}/{} candidates, {} bit-exact, max Δ {:.3e})",
            self.scorer_agreement(),
            self.candidates_agreed,
            self.candidates_checked,
            self.candidates_exact,
            self.max_final_score_delta
        )?;
        writeln!(
            f,
            "  policy agreement   {:.3}   ({}/{} decisions)",
            self.policy_agreement(),
            self.policy_agreed,
            self.replayed
        )?;
        for gap in &self.gaps {
            writeln!(f, "  GAP                {gap}")?;
        }
        for d in &self.scorer_disagreements {
            writeln!(
                f,
                "  SCORE  {} {}: factors [{}] recorded {:?}",
                d.decision_id,
                d.check.name,
                d.check.disagreeing_factors.join(", "),
                d.check.recorded
            )?;
            match &d.check.recomputed {
                Ok(r) => writeln!(f, "         recomputed {r:?}")?,
                Err(gap) => writeln!(f, "         gap {gap}")?,
            }
        }
        if self.scorer_disagreements_total > self.scorer_disagreements.len() {
            writeln!(
                f,
                "  ... {} further scorer disagreements not retained (cap {})",
                self.scorer_disagreements_total - self.scorer_disagreements.len(),
                MAX_RETAINED_DISAGREEMENTS
            )?;
        }
        for d in &self.policy_disagreements {
            writeln!(
                f,
                "  POLICY {}: recorded {:?} recomputed {:?}",
                d.decision_id, d.recorded, d.recomputed
            )?;
        }
        if self.policy_disagreements_total > self.policy_disagreements.len() {
            writeln!(
                f,
                "  ... {} further policy disagreements not retained (cap {})",
                self.policy_disagreements_total - self.policy_disagreements.len(),
                MAX_RETAINED_DISAGREEMENTS
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::{
        DecisionBuilder, LoadSource, RequestFacts, DECISION_LOG_SCHEMA,
    };
    use sovereign_core::oicp::{effective_affinity, NodeLocality};

    fn facts() -> RequestFacts {
        RequestFacts {
            capability_hint: "general".into(),
            latency_class: "Extended".into(),
            sharding: "MeshAllowed".into(),
            context_tokens: Some(1500),
            max_output_tokens: Some(250),
            preferred_speed: "Slow".into(),
            explicit_model_id: None,
        }
    }

    /// A candidate built the way `scheduler_core::rank` builds one:
    /// score the observations for real, then record the breakdown. So
    /// these fixtures are not hand-written numbers — they are the
    /// scorer's own output, which is what makes "the replay
    /// reproduces it" meaningful.
    fn scored_candidate(
        name: &str,
        claim_score: f32,
        claim_affinity: f32,
        obs: NodeObservations,
        locality: NodeLocality,
        size_gb: Option<f32>,
        availability: Option<f32>,
        bench: Option<BenchmarkResult>,
    ) -> CandidateRecord {
        let breakdown = score_with_adjustments(
            claim_score,
            claim_affinity,
            &obs,
            locality,
            size_gb.unwrap_or(0.0),
            bench.as_ref(),
            availability,
        );
        let source = if name == "local" {
            LoadSource::Local
        } else {
            LoadSource::Gossip
        };
        let mut inputs = CandidateInputs::from_observations(&obs, source).with_benchmark(
            bench.as_ref(),
            1_000,
        );
        inputs.availability = availability;
        CandidateRecord {
            kind: if name == "local" {
                CandidateKind::Local
            } else {
                CandidateKind::Peer
            },
            name: name.into(),
            node_id: None,
            model_id: format!("{name}-model"),
            size_gb,
            locality: crate::decision_log::locality_label(locality).to_string(),
            rank: None,
            selected: false,
            score: ScoreRecord::from(&breakdown),
            inputs,
            tier_band: None,
        }
    }

    fn busy_peer_obs() -> NodeObservations {
        NodeObservations {
            in_flight: 7,
            p50_latency_ms: 900,
            p95_latency_ms: 2400,
            recent_failure_rate: 0.2,
            samples: 31,
            ttft_ewma_ms: 410.0,
            tg_tok_s_ewma: 12.5,
        }
    }

    fn bench() -> BenchmarkResult {
        BenchmarkResult {
            baseline_model_id: "baseline".into(),
            baseline_size_gb: 4.0,
            pp_tok_s: 380.0,
            tg_tok_s: 26.0,
            measured_at: 400,
        }
    }

    /// Build a decision the way production does: push candidates in
    /// scoring order, then finish with the ranked list `rank` derived.
    fn decision(candidates: Vec<CandidateRecord>) -> RoutingDecision {
        let mut b = DecisionBuilder::new("req", DecisionPath::RankedOicp, facts());
        for c in &candidates {
            b.push_candidate(c.clone());
        }
        // Derive the verdict through the shared policy, exactly as
        // `rank` does — so the fixture's verdict is production's, not
        // the test author's opinion of it.
        let local = candidates
            .iter()
            .find(|c| c.kind == CandidateKind::Local)
            .map(|c| ModelCandidate {
                score: c.score.final_score,
                size_gb: c.size_gb,
                model_id: c.model_id.clone(),
                claim_affinity: 0.0,
            })
            .unwrap_or_else(local_sentinel);
        let peers: Vec<(String, ModelCandidate)> = candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Peer)
            .map(|c| {
                (
                    c.name.clone(),
                    ModelCandidate {
                        score: c.score.final_score,
                        size_gb: c.size_gb,
                        model_id: c.model_id.clone(),
                        claim_affinity: 0.0,
                    },
                )
            })
            .collect();
        let ranked: Vec<String> = winners_over_local(&local, peers)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let verdict = if ranked.is_empty() {
            Verdict::StayLocal
        } else {
            Verdict::Peers {
                ranked: ranked.clone(),
            }
        };
        b.finish_at(verdict, &ranked, 1_000_000)
    }

    /// Three candidates chosen to exercise every branch of the
    /// scorer at once, and to leave **two** peers strictly beating
    /// local so the ranking has an order to get wrong:
    ///
    /// | | locality | load | failure | cold-start | throughput src |
    /// |---|---|---|---|---|---|
    /// | local  | Local | 2 in flight | none | warm | neutral |
    /// | hub    | Near  | 7 in flight | 20% | warm | observed |
    /// | laptop | Far   | idle | none | ramping | benchmark estimate |
    ///
    /// Local's claim is weak (0.18 after the hint and latency gates)
    /// — the point of the fixture is a routed decision, not a
    /// realistic household.
    fn mixed_fleet() -> RoutingDecision {
        decision(vec![
            scored_candidate(
                "local",
                0.18,
                0.60,
                NodeObservations {
                    in_flight: 2,
                    samples: 40,
                    ..Default::default()
                },
                NodeLocality::Local,
                Some(9.0),
                None,
                None,
            ),
            scored_candidate(
                "hub",
                0.95,
                0.95,
                busy_peer_obs(),
                NodeLocality::Near,
                Some(24.0),
                Some(0.8),
                Some(bench()),
            ),
            scored_candidate(
                "laptop",
                0.80,
                0.80,
                NodeObservations {
                    samples: 3,
                    ..Default::default()
                },
                NodeLocality::Far,
                Some(14.0),
                None,
                Some(bench()),
            ),
        ])
    }

    // ── the lemma the whole design rests on ──────────────────────

    /// `observation_mult` is invariant across every affinity a claim
    /// can legally carry — which is why the record does not need
    /// `claim_affinity` and Phase 0's schema stays put. Checked over
    /// the real domain (`CapabilityClaim::effective_affinity` clamps
    /// to `[0, 1]`) rather than argued in a comment.
    #[test]
    fn observation_mult_is_independent_of_claim_affinity_over_its_whole_domain() {
        for obs in [
            busy_peer_obs(),
            NodeObservations::default(),
            NodeObservations {
                samples: 200,
                recent_failure_rate: 1.0,
                ..Default::default()
            },
            NodeObservations {
                samples: 7,
                recent_failure_rate: 0.35,
                ..Default::default()
            },
        ] {
            let probe = effective_affinity(1.0, &obs) / 1.0;
            for step in 1..=100u32 {
                let a = step as f32 / 100.0;
                let mult = effective_affinity(a, &obs) / a;
                assert!(
                    (mult - probe).abs() <= SCORE_TOLERANCE,
                    "affinity {a} gave {mult}, probe gave {probe} (samples {}, failure {})",
                    obs.samples,
                    obs.recent_failure_rate
                );
            }
        }
    }

    /// The one affinity where the identity breaks is exactly the one
    /// that cannot reach the scorer with a non-zero claim score:
    /// `claim_score` and `claim_affinity` are the same clamped number
    /// upstream, so `a == 0` implies `claim_score == 0` and the whole
    /// product collapses to zero either way.
    #[test]
    fn zero_affinity_is_the_zero_claim_score_case_and_still_reproduces() {
        let obs = busy_peer_obs();
        let breakdown = score_with_adjustments(0.0, 0.0, &obs, NodeLocality::Far, 9.0, None, None);
        assert_eq!(breakdown.observation_mult, 1.0);
        assert_eq!(breakdown.final_score, 0.0);

        let mut c = scored_candidate(
            "dead",
            0.0,
            0.0,
            obs,
            NodeLocality::Far,
            Some(9.0),
            None,
            None,
        );
        c.score = ScoreRecord::from(&breakdown);
        let back = recompute_score(&c).expect("no gap");
        assert_eq!(back, c.score);
    }

    // ── scorer agreement ─────────────────────────────────────────

    #[test]
    fn a_recorded_candidate_reproduces_bit_for_bit() {
        for c in &mixed_fleet().candidates {
            let back = recompute_score(c).expect("no gap");
            assert_eq!(
                back, c.score,
                "{} did not reproduce exactly: {:?} vs {:?}",
                c.name, back, c.score
            );
        }
    }

    #[test]
    fn every_scorer_input_survives_the_jsonl_round_trip() {
        let d = mixed_fleet();
        let line = serde_json::to_string(&crate::decision_log::DecisionEvent::Decision(Box::new(
            d.clone(),
        )))
        .unwrap();
        let trace = SchedulerTrace::from_jsonl(line.as_bytes()).unwrap();
        let report = replay_trace(&trace);
        assert_eq!(report.scorer_agreement(), 1.0, "{report}");
        assert_eq!(
            report.candidates_exact, report.candidates_checked,
            "f32 must survive JSONL exactly:\n{report}"
        );
    }

    /// The replay must be *able* to fail, or a passing run proves
    /// nothing. Perturb one factor and check the report names it.
    #[test]
    fn a_tampered_score_is_caught_and_the_offending_factor_named() {
        let mut d = mixed_fleet();
        let hub = d
            .candidates
            .iter_mut()
            .find(|c| c.name == "hub")
            .expect("hub");
        hub.score.load_penalty *= 1.5;
        hub.score.final_score *= 1.5;
        let report = replay_decisions([&d]);
        assert!(report.scorer_agreement() < 1.0);
        assert_eq!(report.scorer_disagreements_total, 1);
        let factors = &report.scorer_disagreements[0].check.disagreeing_factors;
        assert!(
            factors.contains(&"load_penalty") && factors.contains(&"final_score"),
            "expected load_penalty + final_score, got {factors:?}"
        );
    }

    /// A recorded *input* that no longer matches its recorded score is
    /// the failure mode that matters in the field — an input the
    /// scorer read and the record stamped inconsistently.
    #[test]
    fn a_tampered_input_is_caught_even_though_the_score_is_untouched() {
        let mut d = mixed_fleet();
        let hub = d
            .candidates
            .iter_mut()
            .find(|c| c.name == "hub")
            .expect("hub");
        hub.inputs.in_flight += 5;
        let report = replay_decisions([&d]);
        assert_eq!(report.scorer_disagreements_total, 1);
        let factors = &report.scorer_disagreements[0].check.disagreeing_factors;
        assert!(factors.contains(&"load_penalty"), "got {factors:?}");
    }

    #[test]
    fn an_unreadable_locality_is_reported_as_a_named_gap_not_a_neutral_bonus() {
        let mut d = mixed_fleet();
        d.candidates[1].locality = "orbital".into();
        let report = replay_decisions([&d]);
        assert_eq!(report.gaps.len(), 1);
        assert!(
            matches!(&report.gaps[0], ReplayGap::UnknownLocality { label, .. } if label == "orbital")
        );
        assert!(report.scorer_agreement() < 1.0, "a gap must not read as agreement");
    }

    // ── policy agreement ─────────────────────────────────────────

    #[test]
    fn a_recorded_verdict_reproduces_from_the_recorded_scores_alone() {
        let d = mixed_fleet();
        assert_eq!(recompute_verdict(&d), d.verdict);
        let report = replay_decisions([&d]);
        assert_eq!(report.policy_agreement(), 1.0, "{report}");
    }

    #[test]
    fn a_tampered_verdict_is_caught() {
        let mut d = mixed_fleet();
        d.verdict = Verdict::StayLocal;
        let report = replay_decisions([&d]);
        assert!(report.policy_agreement() < 1.0);
        assert_eq!(report.policy_disagreements_total, 1);
    }

    /// Reordering the ranked list is a different decision, and the
    /// policy check has to see it — otherwise "agreement" would only
    /// mean "the same set", and cascade order is what the caller acts
    /// on.
    #[test]
    fn a_reordered_ranking_is_a_policy_disagreement() {
        let mut d = mixed_fleet();
        let Verdict::Peers { ranked } = &mut d.verdict else {
            panic!("fixture must route to peers, got {:?}", d.verdict);
        };
        assert!(ranked.len() >= 2, "fixture needs two winners: {ranked:?}");
        ranked.reverse();
        let report = replay_decisions([&d]);
        assert_eq!(report.policy_disagreements_total, 1);
    }

    /// With no local candidate at all, the sentinel must let any
    /// scoring peer through — the `<local-insufficient>` path.
    #[test]
    fn a_decision_with_no_local_candidate_still_ranks_its_peers() {
        let peers: Vec<CandidateRecord> = mixed_fleet()
            .candidates
            .into_iter()
            .filter(|c| c.kind == CandidateKind::Peer)
            .collect();
        let d = decision(peers);
        let Verdict::Peers { ranked } = recompute_verdict(&d) else {
            panic!("every peer strictly beats an absent local");
        };
        assert_eq!(ranked.len(), 2);
        assert_eq!(replay_decisions([&d]).policy_agreement(), 1.0);
    }

    // ── domain boundaries ────────────────────────────────────────

    #[test]
    fn gated_and_named_decisions_are_skipped_with_their_reason_not_scored() {
        let gated = DecisionBuilder::new("g", DecisionPath::RankedOicp, facts()).finish_at(
            Verdict::Gated {
                gate: "not_offload_eligible".into(),
            },
            &[],
            1,
        );
        let named = DecisionBuilder::new("n", DecisionPath::NamedModel, facts()).finish_at(
            Verdict::NamedLocal {
                model_id: "m".into(),
            },
            &[],
            2,
        );
        let empty = DecisionBuilder::new("e", DecisionPath::RankedOicp, facts())
            .finish_at(Verdict::StayLocal, &[], 3);

        assert_eq!(
            replay_decision(&gated).err(),
            Some(SkipReason::Gated("not_offload_eligible".into()))
        );
        assert_eq!(replay_decision(&named).err(), Some(SkipReason::NamedPath));
        assert_eq!(replay_decision(&empty).err(), Some(SkipReason::NoCandidates));

        let good = mixed_fleet();
        let report = replay_decisions([&gated, &named, &empty, &good]);
        assert_eq!(report.decisions_seen, 4);
        assert_eq!(report.replayed, 1);
        assert_eq!(report.skipped.iter().map(|(_, n)| n).sum::<usize>(), 3);
        // Skips must not dilute agreement — they are outside the
        // question, not failures of it.
        assert_eq!(report.scorer_agreement(), 1.0, "{report}");
        assert_eq!(report.policy_agreement(), 1.0, "{report}");
    }

    /// An empty capture must never read as perfect agreement. Same
    /// rule as `SchedulerTrace::join_rate`.
    #[test]
    fn an_empty_replay_reports_zero_agreement_not_one() {
        let report = replay_decisions([]);
        assert_eq!(report.scorer_agreement(), 0.0);
        assert_eq!(report.policy_agreement(), 0.0);
    }

    #[test]
    fn a_benchmark_free_candidate_reconstructs_as_none() {
        let obs = NodeObservations::default();
        let inputs = CandidateInputs::from_observations(&obs, LoadSource::SelfObserved);
        assert!(benchmark_from(&inputs).is_none());
        let with = inputs.with_benchmark(Some(&bench()), 1_000);
        let back = benchmark_from(&with).expect("present");
        // Only the two fields the scorer reads are asserted; the rest
        // are documented placeholders.
        assert_eq!(back.tg_tok_s, bench().tg_tok_s);
        assert_eq!(back.baseline_size_gb, bench().baseline_size_gb);
    }

    #[test]
    fn the_report_renders_its_totals_and_names_the_failing_candidate() {
        let mut d = mixed_fleet();
        d.candidates[1].score.final_score *= 2.0;
        let report = replay_decisions([&d]);
        let text = report.to_string();
        assert!(text.contains("scorer agreement"), "{text}");
        assert!(text.contains("policy agreement"), "{text}");
        assert!(text.contains("SCORE"), "{text}");
        assert!(text.contains("hub"), "{text}");
    }

    /// This module reads `CandidateInputs` field by field. A schema
    /// bump means the record shape moved and those reads need
    /// re-checking against it — fail loudly here rather than
    /// reproducing scores from a format nobody re-audited.
    #[test]
    fn the_replay_is_pinned_to_the_record_schema_it_was_written_against() {
        assert_eq!(DECISION_LOG_SCHEMA, "oicp-decision/v1");
    }
}
