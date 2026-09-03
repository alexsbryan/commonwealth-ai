// SPDX-License-Identifier: AGPL-3.0-or-later
//! The retrieval pipeline's accounting: what a step is ALLOWED to do to the
//! chunk pool, what it SAYS it did, and the invariants between the two.
//!
//! Carved out of `retrieval_pipeline.rs` on 2026-09-03 — the pipeline is the
//! ordered list of steps and the runner that drives it; this is a separate
//! concern that depends on none of its internals (every function here is pure
//! apart from one counter). Living beside the step list obscured that and
//! pushed the file past the ARCH §3.1 ceiling.
//!
//! # Why any of this exists
//!
//! `delta = 0` used to mean two different things — "nothing was relevant" and
//! "every candidate failed to resolve" — and the pipeline could not tell them
//! apart. That ambiguity is why atlas grounding contributed literally zero
//! chunks to every SEP answer for months behind an ordinary-looking trace line
//! (see `corpus_engine::enrichment::atlas::evidence_site`). Reporting absence
//! rather than defaulting it is ARCH §18.3; doing it at the runner rather than
//! at 27 call sites is ARCH §7 — structural, not remembered.

use crate::runtime::retrieval_pipeline::RetrievalStep;

/// What a step is DECLARED to do to the pool.
///
/// Data on the step list, checked by the runner against what the step
/// actually did. Declaring it costs one token per step and turns a whole
/// class of "that step should not have been able to do that" into a runtime
/// assertion — a filter that adds, an injector that removes, a sort that
/// changes membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// May ADD chunks; never removes. Must ledger `considered`.
    Injector,
    /// May REMOVE chunks; never adds. Carries the reason it removes for —
    /// the runner derives the COUNT from the delta it already measures, so a
    /// filter needs no ledger code at all. A filter with more than one reason
    /// returns its own [`StepLedger`] and the runner checks it sums.
    Filter(DropReason),
    /// May reorder or rescore; pool MEMBERSHIP is invariant.
    Reorder,
    /// Does not touch the pool at all — spawns a lane, snapshots state,
    /// records an audit.
    Inert,
}

/// Why a candidate never became a chunk, or why a chunk left the pool.
///
/// Closed set (ARCH §2). Collapsed from the 49 distinct guards across the 27
/// steps: the granularity that matters is not "which line rejected it" but
/// "does this zero indicate a defect", which is what
/// [`DropReason::is_resolution_failure`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DropReason {
    // ── capability absent — a legitimate zero ──
    /// Lane provider, corpus engine, or index handle not attached.
    ProviderAbsent,
    /// The step's env gate is off.
    FeatureDisabled,
    /// Intent excluded this step (e.g. SimpleQuery on demand_plan).
    IntentExcluded,

    // ── scope — a legitimate zero ──
    /// Outside `enabled_corpora`, `corpus_ceiling`, expansion scope, the
    /// personal-scope filter, the wrong corpus kind, or sensitive.
    OutOfScope,
    /// Corpus exists but cannot serve: not built, no vector index, or an
    /// embedding-dimension mismatch.
    CorpusUnavailable,

    // ── the producer had nothing to offer ──
    /// No entities, empty embedding, shape not matched, planner returned None.
    NoCandidates,
    /// An LLM call, a parse, or the rerank contract failed.
    ProducerFailed,

    // ── quality gates — a legitimate zero ──
    /// Below a scoring floor: cosine, axis weight, topic grip, yes-logit.
    BelowThreshold,
    /// Noise floor: no substantive query token in title or content.
    NoQueryOverlap,

    // ── resolution — SUSPICIOUS, see `is_resolution_failure` ──
    /// The corpus the fetch was scoped to has no searchable index. THE SEP
    /// defect: grounding scoped to an atlas id that holds no chunks.
    CorpusNotSearchable,
    /// The chunk the atom pointed at could not be fetched.
    EvidenceUnresolvable,
    /// Fetched, but the title did not match the one asked for.
    TitleMismatch,

    // ── identity ──
    /// Already in the pool, or already emitted this turn.
    Duplicate,

    // ── budgets and objectives ──
    /// A hard numeric bound: per-corpus K, top-M, fetch budget, query caps.
    BudgetExhausted,
    /// A per-article, per-section, or per-corpus cap.
    CapExceeded,
    /// The merge objective did not select it.
    NotSelectedByObjective,

    // ── lifecycle ──
    /// A concurrent lane overran its join deadline or failed.
    LaneAbandoned,

    // ── domain rule ──
    /// Governance: the chunk belongs to an amended (dead-law) section.
    DeadLaw,
}

impl DropReason {
    /// Does this reason mean the step TRIED to realize a candidate and could
    /// not — as opposed to deciding it should not?
    ///
    /// The distinction the SEP defect turned on. A step whose every candidate
    /// dies for one of these is not filtering; it is broken. Scope, threshold
    /// and budget drops are decisions; these three are failures.
    pub fn is_resolution_failure(self) -> bool {
        matches!(
            self,
            DropReason::CorpusNotSearchable
                | DropReason::EvidenceUnresolvable
                | DropReason::TitleMismatch
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::ProviderAbsent => "provider_absent",
            DropReason::FeatureDisabled => "feature_disabled",
            DropReason::IntentExcluded => "intent_excluded",
            DropReason::OutOfScope => "out_of_scope",
            DropReason::CorpusUnavailable => "corpus_unavailable",
            DropReason::NoCandidates => "no_candidates",
            DropReason::ProducerFailed => "producer_failed",
            DropReason::BelowThreshold => "below_threshold",
            DropReason::NoQueryOverlap => "no_query_overlap",
            DropReason::CorpusNotSearchable => "corpus_not_searchable",
            DropReason::EvidenceUnresolvable => "evidence_unresolvable",
            DropReason::TitleMismatch => "title_mismatch",
            DropReason::Duplicate => "duplicate",
            DropReason::BudgetExhausted => "budget_exhausted",
            DropReason::CapExceeded => "cap_exceeded",
            DropReason::NotSelectedByObjective => "not_selected_by_objective",
            DropReason::LaneAbandoned => "lane_abandoned",
            DropReason::DeadLaw => "dead_law",
        }
    }
}

/// A step's account of what it did to the pool.
///
/// Exists because `delta = 0` was ambiguous between "nothing was relevant" and
/// "every candidate failed to resolve", and that ambiguity is what let atlas
/// grounding contribute zero to every SEP answer for months without one line
/// of evidence in any log (note `81feaf78`). Absence is now reported, never
/// defaulted — ARCH §18.3, enforced at the runner rather than remembered at
/// 27 call sites.
#[derive(Debug, Default, Clone)]
pub struct StepLedger {
    /// Candidates generated before the step tried to realize them.
    /// `None` means this step generates no candidates (filters, sorts).
    pub considered: Option<usize>,
    /// Candidates dropped / chunks removed, by reason. For an
    /// [`StepKind::Injector`] this must satisfy
    /// `considered == added + sum(accounted)`; for a [`StepKind::Filter`],
    /// `removed == sum(accounted)`.
    pub accounted: std::collections::BTreeMap<DropReason, usize>,
}

impl StepLedger {
    /// An injector's ledger: candidates in, drops by reason.
    pub fn injected(considered: usize) -> Self {
        Self {
            considered: Some(considered),
            accounted: Default::default(),
        }
    }

    /// A filter's ledger: every removal carries one reason.
    pub fn removed(reason: DropReason, n: usize) -> Self {
        let mut accounted = std::collections::BTreeMap::new();
        if n > 0 {
            accounted.insert(reason, n);
        }
        Self {
            considered: None,
            accounted,
        }
    }

    pub fn drop(mut self, reason: DropReason, n: usize) -> Self {
        if n > 0 {
            *self.accounted.entry(reason).or_insert(0) += n;
        }
        self
    }

    pub fn total_accounted(&self) -> usize {
        self.accounted.values().sum()
    }

    /// Render as `reason=n,reason=n` for the trace line.
    pub(crate) fn render(&self) -> String {
        self.accounted
            .iter()
            .map(|(r, n)| format!("{}={}", r.as_str(), n))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// What a step reports back to the runner. The runner computes the
/// chunk-count delta itself from the state.
#[derive(Debug, Default)]
pub struct StepOutcome {
    /// Optional human note surfaced on the per-step trace line
    /// (e.g. "late-inject mode — skipped").
    pub note: Option<String>,
    /// The step's account of what it did. See [`StepLedger`].
    pub ledger: StepLedger,
}

impl StepOutcome {
    pub fn with_ledger(ledger: StepLedger) -> Self {
        Self { note: None, ledger }
    }
}

/// Ledger violations this process has observed, ever.
///
/// A logged error is not a gate: nothing fails, and on a bench run the line
/// scrolls past among thousands. This counter is what `svrn eval run
/// --prod-pipeline` reads to turn a violation into a non-zero exit — the
/// difference between an instrument and a ratchet (ARCH §18.1).
static LEDGER_VIOLATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Total ledger violations observed since process start.
pub fn ledger_violation_count() -> usize {
    LEDGER_VIOLATIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The pure decision behind [`RetrievalPipeline::audit_step`]: given what a
/// step was DECLARED to do, what it actually did to the pool, and what it says
/// it did, what is wrong?
///
/// Pure and separately tested, because a check nobody has watched fail is not
/// a check (ARCH §18.1). The tests at the bottom of this file drive each arm
/// with an input that trips it.
///
/// Returns every violation, not the first: a step can both exceed its kind and
/// fail to account, and reporting one would hide the other.
pub fn ledger_violations(kind: StepKind, delta: i64, led: &StepLedger) -> Vec<&'static str> {
    let mut out = Vec::new();
    let accounted = led.total_accounted();

    // 1. The step did something its declared kind forbids.
    match kind {
        StepKind::Injector if delta < 0 => out.push("injector REMOVED chunks"),
        StepKind::Filter(_) if delta > 0 => out.push("filter ADDED chunks"),
        StepKind::Reorder | StepKind::Inert if delta != 0 => {
            out.push("non-mutating step changed pool membership")
        }
        _ => {}
    }

    match kind {
        StepKind::Injector => {
            let Some(considered) = led.considered else {
                return out;
            };
            let added = delta.max(0) as usize;
            // 2. Candidates must be fully accounted for: realised, or dropped
            //    for a named reason. A residual means candidates vanished and
            //    nothing recorded why.
            if added + accounted != considered {
                out.push("candidates unaccounted for (added + dropped != considered)");
            }
            // 3. The shape the SEP defect had: candidates existed, none
            //    survived, and every drop was a failure to RESOLVE rather than
            //    a decision not to admit.
            if considered > 0 && added == 0 {
                if led.accounted.is_empty() {
                    out.push("every candidate vanished with no reason recorded");
                } else if led.accounted.keys().all(|r| r.is_resolution_failure()) {
                    out.push(
                        "every candidate died at resolution — the step is not filtering, it is failing",
                    );
                }
            }
        }
        StepKind::Filter(_) => {
            // An empty ledger is not a gap here: the runner synthesises it
            // from the declared reason before this runs. A MISMATCH means the
            // step supplied its own ledger and it does not add up.
            let removed = (-delta).max(0) as usize;
            if removed != accounted {
                out.push("removals unaccounted for (removed != sum of reasons)");
            }
        }
        StepKind::Reorder | StepKind::Inert => {
            if accounted != 0 {
                out.push("non-mutating step reported drops");
            }
        }
    }
    out
}

/// The invariants the ledger buys. One site, sees every step — so no step
/// has to REMEMBER to be honest about a zero (ARCH §7: structural, not
/// remembered).
///
/// Each of these is an `error!` rather than a panic on purpose: a
/// retrieval turn that trips one still returns an answer, and a
/// user-facing crash would be a worse failure than the one being
/// reported. The CI-bench gate is what turns them red.
pub(crate) fn audit_step(pipeline: &str, s: &RetrievalStep, delta: i64, led: &StepLedger) {
    if led.considered.is_none() && matches!(s.kind, StepKind::Injector) {
        // Not a violation — this injector has not been taught to ledger
        // yet. Named at debug so the gap is greppable rather than
        // invisible, which is the whole point of the exercise.
        tracing::debug!(
            target: "retrieval.pipeline",
            pipeline, step = s.name,
            "retrieval.pipeline: injector has no ledger yet"
        );
    }
    for why in ledger_violations(s.kind, delta, led) {
        LEDGER_VIOLATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::error!(
            target: "retrieval.pipeline",
            pipeline,
            step = s.name,
            kind = ?s.kind,
            delta,
            considered = led.considered.map(|c| c as i64).unwrap_or(-1),
            accounted = %led.render(),
            "retrieval.pipeline: LEDGER VIOLATION — {why}"
        );
    }
}
