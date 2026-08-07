// SPDX-License-Identifier: AGPL-3.0-or-later
//! **§4.1 — the predicted-time objective.** Rank the feasible set on
//! estimated time-to-answer instead of on a product of dimensionless
//! multipliers.
//!
//! `SCHEDULER_QUALITY.md` §4.1 replaces only the *ranking* half of the
//! routing decision. The feasibility half is untouched and stays where
//! it already lives — `score_claim_for_request`'s hard context/output
//! gates, the hint gate, quarantine, manifest availability. By the
//! time anything here runs, every candidate in hand *could* serve the
//! request; the only question left is **which one answers it soonest**.
//!
//! ## Why the product cannot be repaired term by term
//!
//! Three findings share one root (§2). F3: the throughput term
//! evaluates to 1.0 for a 25 tok/s hub and a 120 tok/s laptop alike,
//! so heterogeneity is invisible. F5: two-choices sampling is inert
//! wherever the capability winner is unique. F7: `cold_start_weight`'s
//! 0.7 floor is load-bearing for a reason nobody intended.
//!
//! All three are shapes of one deficiency: **a product of
//! dimensionless multipliers has no unit, so it cannot represent
//! "this hop costs more than it buys."** There is no term in which a
//! 40 ms round trip and a 12-second queue are the same kind of
//! quantity, so the objective cannot decline a bad offload on the
//! merits — it can only be *prevented* from taking it by a penalty
//! large enough to dominate, which is what the cold-start floor
//! accidentally became.
//!
//! Measured, not argued (`Arm::WarmStart` / `Arm::FreshWarmStart`):
//! lifting that floor costs +235% mean latency, and **+264% when the
//! load signal is made perfect**. The extra offloads lose on their own
//! merits with or without F1.
//!
//! A predicted time has a unit. `rtt` sits inside the number, so a hop
//! that buys less than it costs loses arithmetically. **This module
//! introduces no tunable constant** — no floor, no weight, no
//! tie-margin — and that is the property to defend in review: the
//! moment a fudge factor appears here, the objective has regressed to
//! being a product with extra steps.
//!
//! ## What it is allowed to read
//!
//! Only what a decider can actually see at decision time. That
//! restriction is what separates this from `mesh_sim`'s `Arm::Oracle`
//! (that module is feature-gated, hence no link), and the difference
//! between the two is the headline measurement:
//!
//! | term | this objective | the oracle instead knows |
//! |---|---|---|
//! | prefill | `ctx / bench.pp_tok_s` | the same |
//! | decode | `out / bench.tg_tok_s` | the same |
//! | rtt | the manifest probe's round trip | the same |
//! | **queue** | `in_flight` **count** × *this* request's service time | `backlog_ms`, exactly |
//!
//! The last row is the entire information gap, and naming it is the
//! point. Gossip carries a **count**, not a queue depth in
//! milliseconds ([`crate::decision_log::CandidateInputs::in_flight`]),
//! so the only shape a decider can assume for the jobs ahead of it is
//! the shape of the job in its own hand. That over-charges a queue of
//! short requests and under-charges a queue of long ones. It is also
//! exactly *one substitution* away from the oracle's `backlog_ms`,
//! which is what makes `oracle − predicted` a measurement of that one
//! substitution rather than a vague "cost of imperfect information".
//!
//! ## Replayable from a capture, with one gap
//!
//! Every input above is already in the decision record: `in_flight`,
//! `rtt_ms`, `bench_pp_tok_s` and `bench_tg_tok_s` on
//! [`CandidateInputs`], and the token shape on
//! [`RequestFacts`][crate::decision_log::RequestFacts]. So this
//! objective can be re-run against a **production** capture with no
//! new instrumentation — see [`PredictInputs::from_candidate`].
//!
//! What a record does *not* carry is **which objective produced its
//! verdict**. [`crate::decision_replay`] therefore assumes `Product`,
//! and a capture taken under this objective will read as policy
//! disagreement. That is a missing field, not a bug, and closing it is
//! part of the §4.1 landing rather than of the arm.
//!
//! Nothing here reads a clock, allocates in the hot path, or panics:
//! [`predict`] is total over its inputs and returns `Err` rather than
//! a sentinel when a candidate cannot be predicted at all.

use sovereign_core::oicp::{InferenceRequirements, ProviderManifest};

use crate::decision_log::{CandidateInputs, RequestFacts};

/// What a candidate owes before it can begin *this* request at all:
/// nothing if the chosen model is already resident, its advertised load
/// time if it is not.
///
/// A separate concept from queueing, and the distinction is the whole
/// reason it is a separate addend. A queue is paid **per job ahead of
/// us**; a model load is paid **once**, by whichever request arrives
/// first, and everything behind it in the queue inherits a warm slot.
/// So load enters the prediction additively and is not multiplied by
/// `in_flight`.
///
/// This is the term whose absence would have been the most expensive:
/// a 21GB model paged in from disk is tens of seconds, which dwarfs
/// every other addend, and an objective that prices it at zero will
/// cheerfully send a request to a node that has to boot a model to
/// serve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadDebt {
    /// `ModelStatus::loaded` for the model the scorer picked.
    pub model_loaded: bool,
    /// `ModelStatus::estimated_load_time_sec`, in ms. Zero when the
    /// manifest advertises no estimate — see [`LoadDebt::pending_ms`]
    /// for why that is a *reported* zero and not a silent one.
    pub estimated_load_ms: u32,
}

impl LoadDebt {
    /// Read the debt off a manifest for one specific model id — the
    /// model the scorer actually chose, not the manifest's first entry.
    ///
    /// `None` when the manifest has no such model, which the caller
    /// should treat the way it treats any other missing input rather
    /// than as zero debt.
    pub fn from_manifest(manifest: &ProviderManifest, model_id: &str) -> Option<Self> {
        let model = manifest.models.iter().find(|m| m.id == model_id)?;
        Some(Self {
            model_loaded: model.status.loaded,
            estimated_load_ms: model
                .status
                .estimated_load_time_sec
                .map(|s| s.saturating_mul(1_000))
                .unwrap_or(0),
        })
    }

    /// Milliseconds of load this request would have to wait through.
    ///
    /// A cold model with **no advertised estimate** yields zero, and
    /// that is a deliberate under-charge rather than an oversight: the
    /// alternative is inventing a load time, which is the same
    /// fabrication [`Unpredictable::NoThroughput`] refuses for decode
    /// rates. The honest fix is for manifests to advertise the field —
    /// `record it, do not guess it` — and until they do, this objective
    /// is optimistic about cold peers in exactly one measurable way.
    pub fn pending_ms(&self) -> u32 {
        if self.model_loaded {
            0
        } else {
            self.estimated_load_ms
        }
    }
}

/// The size of the job, in the two token counts that set it.
///
/// Both are `u32` rather than `Option<u32>`: a request whose shape is
/// unknown has no predicted time at all, and that case is
/// [`Unpredictable::NoRequestShape`] rather than a zero here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestShape {
    pub context_tokens: u32,
    pub output_tokens: u32,
}

impl RequestShape {
    /// From the live OICP envelope — the production decision path.
    ///
    /// `None` when the envelope omitted either count. Both are already
    /// hard feasibility gates in the scorer (`scoring.rs:590`), so a
    /// request that reaches ranking without them is one the gates
    /// could not bind on either.
    pub fn from_requirements(req: &InferenceRequirements) -> Option<Self> {
        Some(Self {
            context_tokens: req.context_tokens?,
            output_tokens: req.max_output_tokens?,
        })
    }

    /// From a recorded decision — the capture-replay path. Same two
    /// fields, read back off the record instead of the envelope.
    pub fn from_facts(facts: &RequestFacts) -> Option<Self> {
        Some(Self {
            context_tokens: facts.context_tokens?,
            output_tokens: facts.max_output_tokens?,
        })
    }
}

/// What a decider can see about one candidate, and nothing more.
///
/// Deliberately *not* a view onto a peer's true state: `in_flight` is
/// whatever the scorer was handed (a gossiped count, possibly
/// seconds stale), which is the same number the product objective
/// reads. Swapping this type for ground truth is how an arm isolates
/// F1, not something this objective may do for itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PredictInputs {
    /// Requests already committed to this candidate, as a count.
    pub in_flight: u32,
    /// Round trip to it. Zero for the local candidate.
    pub rtt_ms: u32,
    /// Prompt-processing rate from the advertised benchmark.
    pub pp_tok_s: Option<f32>,
    /// Token-generation rate from the advertised benchmark.
    pub tg_tok_s: Option<f32>,
    /// Model-load debt, already resolved to milliseconds by
    /// [`LoadDebt::pending_ms`]. Zero for a resident model.
    pub pending_load_ms: u32,
}

impl PredictInputs {
    /// Recover the inputs from a recorded candidate — the whole reason
    /// this objective can be scored against a production capture
    /// without new instrumentation.
    ///
    /// `rtt_ms` is `None` on the local candidate (there is no probe to
    /// itself), which reads correctly as zero wire cost.
    pub fn from_candidate(inputs: &CandidateInputs) -> Self {
        Self {
            in_flight: inputs.in_flight,
            rtt_ms: inputs.rtt_ms.unwrap_or(0),
            pp_tok_s: inputs.bench_pp_tok_s,
            tg_tok_s: inputs.bench_tg_tok_s,
            // A record from before the load-debt fields existed reads
            // as "already resident", which is the pre-existing
            // behaviour rather than a new guess.
            pending_load_ms: LoadDebt {
                model_loaded: inputs.model_loaded.unwrap_or(true),
                estimated_load_ms: inputs.estimated_load_ms.unwrap_or(0),
            }
            .pending_ms(),
        }
    }
}

/// Why a candidate has no predicted time.
///
/// A closed set, and reported rather than collapsed into a sentinel:
/// "the hub advertises no benchmark" and "the hub is predicted slow"
/// are different facts, and a scheduler that silently treats the first
/// as the second is the failure this whole section is trying to leave
/// behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unpredictable {
    /// The candidate advertises no `BenchmarkResult`, so there is no
    /// rate to divide by. Note what this is *not*: an excuse to
    /// substitute a default rate. A guessed rate would put a number
    /// with a unit on a candidate nobody measured, which is worse than
    /// having none — it would look like knowledge in the trace.
    NoThroughput,
    /// The request carried no context/output token counts, so the job
    /// has no size. Affects every candidate equally.
    NoRequestShape,
    /// A benchmark advertising a rate at or below zero. Division would
    /// produce an infinity that sorts first and wins every comparison.
    NonPositiveRate,
}

impl Unpredictable {
    /// Short label for a glassbox trace or a scoreboard cell.
    pub fn label(&self) -> &'static str {
        match self {
            Unpredictable::NoThroughput => "no-benchmark",
            Unpredictable::NoRequestShape => "no-request-shape",
            Unpredictable::NonPositiveRate => "non-positive-rate",
        }
    }
}

impl std::fmt::Display for Unpredictable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A predicted time to answer, kept as its four addends.
///
/// The breakdown is not decoration: §4.1's legibility claim is that a
/// trace can say "node B, predicted 4.2s; local 6.8s" *and* say which
/// term dominated. A single total cannot answer "was that queue or
/// wire?", and that question is the first one anyone asks of a
/// surprising route.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    /// Model-load debt — paid once by whoever arrives first, so it is
    /// added rather than multiplied by the queue depth.
    pub load_ms: f64,
    /// Time behind the jobs already committed to this candidate.
    pub queue_ms: f64,
    /// Prompt processing for this request.
    pub prefill_ms: f64,
    /// Token generation for this request.
    pub decode_ms: f64,
    /// Wire cost. Zero locally.
    pub rtt_ms: f64,
    /// The sum, and the only field the ranking compares.
    pub total_ms: f64,
}

impl Prediction {
    /// Service time for this request alone — what it would cost on an
    /// idle candidate, wire included.
    pub fn service_ms(&self) -> f64 {
        self.prefill_ms + self.decode_ms
    }

    /// The prediction with the guessed term taken out: what this
    /// candidate would cost with nobody ahead of it.
    ///
    /// The split matters because the four terms are not equally
    /// trustworthy. `load`, `prefill`, `decode` and `rtt` come from the
    /// advertised rate card, the load estimate and the RTT probe;
    /// `queue` is `in_flight × service`, and `in_flight` is a gossiped
    /// count that can be seconds stale — F1, the defect this whole
    /// document is organised around. So this is the part of the
    /// prediction a decider is *not* guessing at, and comparing two
    /// candidates on it asks the capability question free of the
    /// congestion one.
    pub fn uncontended_ms(&self) -> f64 {
        self.total_ms - self.queue_ms
    }
}

/// Predicted time to answer, from what a decider can see.
///
/// ```text
/// service = ctx / pp_tok_s + out / tg_tok_s
/// queue   = in_flight × service        // per job ahead of us
/// load    = 0 if resident, else advertised load time   // paid ONCE
/// total   = load + queue + service + rtt
/// ```
///
/// The `queue` line is the estimator's one real assumption: each job
/// ahead of us is assumed to look like the job in our hand, because a
/// count is all gossip carries. Every other term is measured or
/// advertised.
pub fn predict(
    inputs: &PredictInputs,
    shape: Option<RequestShape>,
) -> Result<Prediction, Unpredictable> {
    let shape = shape.ok_or(Unpredictable::NoRequestShape)?;
    let (pp, tg) = match (inputs.pp_tok_s, inputs.tg_tok_s) {
        (Some(pp), Some(tg)) => (pp, tg),
        _ => return Err(Unpredictable::NoThroughput),
    };
    if !(pp > 0.0) || !(tg > 0.0) || !pp.is_finite() || !tg.is_finite() {
        return Err(Unpredictable::NonPositiveRate);
    }

    let prefill_ms = shape.context_tokens as f64 / pp as f64 * 1000.0;
    let decode_ms = shape.output_tokens as f64 / tg as f64 * 1000.0;
    let service_ms = prefill_ms + decode_ms;
    // A count, not a duration — the substitution the module docs name.
    let queue_ms = inputs.in_flight as f64 * service_ms;
    let rtt_ms = inputs.rtt_ms as f64;
    // Additive, NOT multiplied by the queue: one load warms the slot
    // for everything behind it.
    let load_ms = inputs.pending_load_ms as f64;
    Ok(Prediction {
        load_ms,
        queue_ms,
        prefill_ms,
        decode_ms,
        rtt_ms,
        total_ms: load_ms + queue_ms + service_ms + rtt_ms,
    })
}

/// What staying local is worth, in the three states that are
/// genuinely different.
///
/// Collapsing these into one `Option` or one infinity is the mistake
/// this enum exists to prevent: "we cannot compare" and "there is
/// nothing to compare against" point in **opposite** directions, and
/// getting them backwards either strands work on a node that cannot
/// serve it or ships every request over the wire on a missing
/// benchmark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocalOption {
    /// Local can serve the request, and here is what it would cost.
    Predicted(Prediction),
    /// Local can serve it, but there is no prediction for it — so
    /// there is no comparison, and no hop.
    Unpredictable(Unpredictable),
    /// No loaded local model's claims can serve this request at all.
    /// Staying home is not on the menu, so any feasible peer wins.
    /// The exact analogue of `scheduler_core`'s `local_sentinel`
    /// (negative infinity, which every peer beats).
    Infeasible,
}

/// A *feasible* local candidate's prediction, whether or not it
/// succeeded. Infeasibility is not representable in a `Result`, so it
/// is not representable here either — the caller has to say
/// [`LocalOption::Infeasible`] out loud.
impl From<Result<Prediction, Unpredictable>> for LocalOption {
    fn from(p: Result<Prediction, Unpredictable>) -> Self {
        match p {
            Ok(p) => LocalOption::Predicted(p),
            Err(e) => LocalOption::Unpredictable(e),
        }
    }
}

/// The time-objective analogue of `scheduler_core::winners_over_local`
/// (crate-private, hence no link): keep the peers that beat local,
/// ordered best-first.
///
/// Three rules, each load-bearing:
///
/// 1. **Local unpredictable ⇒ no hop, whatever the peers say.** There
///    is no comparison to make, and the conservative direction is the
///    one that treats missing information as a decline rather than as
///    a discount. (The product objective does the opposite: it prices
///    a stranger at 0.7 and lets it compete anyway.) `Infeasible` is
///    the one case that inverts this, and it inverts it because local
///    is then not an option rather than an unmeasured one.
/// 2. **A peer without a prediction is dropped**, not defaulted. Same
///    reason as [`Unpredictable::NoThroughput`] — a guessed rate is a
///    fabricated fact with a unit attached.
/// 3. **Strictly faster, and no margin constant.** Local wins ties, as
///    everywhere else in this scheduler (no round trip, no attribution
///    churn). No tie-margin is needed *because* `rtt` is already
///    inside the peer's number: the hop pays for itself in the
///    comparison, so there is nothing left for a fudge factor to
///    protect against.
///
/// Order-sensitive on exact ties, deliberately and for the same
/// reason `winners_over_local` is: the sort is stable, so equal
/// predictions rank in the order the caller supplied, which is
/// scoring order, which is the order the candidate records were
/// pushed. Reordering the caller's pushes reorders these winners.
pub fn faster_than_local<T>(
    local: LocalOption,
    scored: Vec<(T, Result<Prediction, Unpredictable>)>,
) -> Vec<(T, Prediction)> {
    let ceiling = match local {
        LocalOption::Predicted(p) => p.total_ms,
        LocalOption::Unpredictable(_) => return Vec::new(),
        LocalOption::Infeasible => f64::INFINITY,
    };
    let mut winners: Vec<(T, Prediction)> = scored
        .into_iter()
        .filter_map(|(tag, pred)| pred.ok().map(|p| (tag, p)))
        .filter(|(_, p)| p.total_ms < ceiling)
        .collect();
    // Stable, and `total_cmp` rather than `partial_cmp().unwrap()` so
    // a NaN could never panic the scheduler. (`predict` cannot emit
    // one — the rate gate above rules it out — which is exactly why
    // this is cheap insurance rather than dead defence.)
    winners.sort_by(|(_, a), (_, b)| a.total_ms.total_cmp(&b.total_ms));
    winners
}

/// How many candidates at the head of a ranking are separated from the
/// best one by nothing but a number the decider is guessing at — §4.2
/// step 2's *"candidates whose predictions are within noise"*.
///
/// **The noise is named, not chosen, and there is no constant here**
/// (invariant 1). Of `predict`'s four terms exactly one is built from a
/// gossiped count that can be seconds stale — `queue = in_flight ×
/// service` — and [`Prediction::uncontended_ms`] is the rest. Two
/// candidates are therefore within noise of each other when their
/// uncontended predictions do not separate them: whatever order the
/// queue term then imposes is an order on the one signal known to be
/// wrong, and F1 is precisely the finding that it *is* wrong, together,
/// for every decider at once.
///
/// §4.1.2 measured why the width has to come from the rate card rather
/// than from a margin:
///
///   - `twin-hubs` band 0 is three *identical* hubs on a uniform LAN,
///     so every uncontended prediction is equal, the whole band is in
///     play, and this behaves like the blunt sampler that recovered
///     −4% there.
///   - `mixed-hubs` band 0 spans 34 / 25 / 11 tok/s, so the uncontended
///     predictions separate strictly, the band collapses to the leader,
///     and the objective's −8% win is not sampled away. That is the
///     +3% regression the blunt arm paid, declined.
///
/// Returned as a **prefix length**, and it is a prefix by construction
/// because the run stops at the first candidate that fails the test —
/// deliberately, and this is the one place the definition is narrower
/// than its own principle. A genuinely capable candidate sitting far
/// down the list behind a large queue would pass the uncontended test,
/// but its `total_ms` says a *lot* of work is already committed to it,
/// and a count of five is not plausibly a stale zero. The run keeps the
/// cases where the queue signal is doing the separating and drops the
/// ones where it is merely large.
///
/// **Known limit, and it is sharper than "conservative" — §4.1.3
/// measured it and the first draft of this paragraph was wrong.** The
/// band decides "are these the same machine?" from the *advertised*
/// rate card, which is a number the candidate states about itself and
/// this module deliberately never corrects (invariant 3: no defaulting,
/// no substituted rates — where the product objective does have an
/// error-correcting path in `throughput_factor`'s observed EWMA). So
/// two hubs that are identical in fact but differ in what they claim
/// separate strictly, and the band collapses. On `twin-hubs` a mere
/// ±10% rate-card error takes the mean band from 2.92 to 1.45 — a
/// cliff, not a decay.
///
/// That is *not* a safe failure: the collapsed band still opens on ~25%
/// of decisions, on whichever pairs happen to have near-equal
/// *perturbed* times, so the sampler keeps firing on noise while no
/// longer firing on the real ties. Measured cost is ~+3% against the
/// plain argmax, where the blunt uniform sampler — which never consults
/// the rate card at all — holds its full recovery. Fixing this means
/// giving the band an observed rate when one exists, not a tolerance
/// constant.
///
/// Zero for an empty ranking; never zero otherwise, since the leader is
/// trivially within noise of itself.
pub fn tie_band<T>(ranked: &[(T, Prediction)]) -> usize {
    let Some((_, leader)) = ranked.first() else {
        return 0;
    };
    let ceiling = leader.uncontended_ms();
    ranked
        .iter()
        .take_while(|(_, p)| p.uncontended_ms() <= ceiling)
        .count()
}

/// Of two candidates the prediction cannot tell apart, the one to take
/// — §4.2 step 2's *"take the less loaded"*. `true` keeps `a`.
///
/// "Loaded" is `queue_ms` rather than the raw in-flight count, because
/// a count has no scale: three jobs on a 34 tok/s hub is less work
/// ahead of you than two on an 11 tok/s one, and the count cannot say
/// so. Preferring the smaller queue also picks the candidate whose
/// prediction leans least on the stale number — with less committed
/// ahead of you there is less for a missed gossip round to be wrong
/// about.
///
/// Ties keep `a`, so a caller that passes the better-ranked candidate
/// first gets the better-ranked one back on an exact tie — invariant 5's
/// rule applied one layer up.
pub fn prefer_less_loaded(a: &Prediction, b: &Prediction) -> bool {
    a.queue_ms <= b.queue_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Option<RequestShape> {
        Some(RequestShape {
            context_tokens: 4_000,
            output_tokens: 500,
        })
    }

    fn inputs(in_flight: u32, rtt_ms: u32, pp: f32, tg: f32) -> PredictInputs {
        PredictInputs {
            in_flight,
            rtt_ms,
            pp_tok_s: Some(pp),
            tg_tok_s: Some(tg),
            pending_load_ms: 0,
        }
    }

    /// The same candidate, but its model has to be paged in first.
    fn cold(mut i: PredictInputs, load_ms: u32) -> PredictInputs {
        i.pending_load_ms = load_ms;
        i
    }

    /// Load is paid ONCE, so it is added rather than multiplied by the
    /// queue depth — the property that distinguishes it from queueing.
    #[test]
    fn load_debt_is_additive_and_not_multiplied_by_the_queue() {
        let warm = predict(&inputs(3, 8, 1_000.0, 100.0), shape()).unwrap();
        let cold = predict(&cold(inputs(3, 8, 1_000.0, 100.0), 42_000), shape()).unwrap();
        assert_eq!(cold.load_ms, 42_000.0);
        assert_eq!(
            cold.queue_ms, warm.queue_ms,
            "load must not scale the queue"
        );
        assert_eq!(cold.total_ms, warm.total_ms + 42_000.0);
    }

    /// The term that would have hurt most by its absence: a big cold
    /// model loses to a small warm one even though it is faster once
    /// running.
    #[test]
    fn a_cold_big_model_loses_to_a_warm_small_one_on_load_alone() {
        let local = predict(&inputs(0, 0, 800.0, 40.0), shape());
        // Faster hardware, but 42s of paging in first.
        let cold_hub = predict(&cold(inputs(0, 8, 2_000.0, 60.0), 42_000), shape());
        assert!(
            cold_hub.as_ref().unwrap().total_ms > local.as_ref().unwrap().total_ms,
            "the load debt has to dominate here or the test proves nothing"
        );
        assert!(faster_than_local(local.into(), vec![("hub", cold_hub)]).is_empty());
    }

    /// `LoadDebt` reads the model the scorer PICKED, not the manifest's
    /// first entry, and a resident model owes nothing whatever it
    /// advertises.
    #[test]
    fn load_debt_is_read_per_model_and_a_resident_model_owes_nothing() {
        use sovereign_core::oicp::{
            CapabilityClaim, CapabilityHint, LatencyClass, ModelStatus, ProviderModel, OICP_VERSION,
        };
        let model = |id: &str, loaded: bool, load_sec: Option<u32>| ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: load_sec,
            },
            size_gb: Some(21.0),
            claims: vec![CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Extended,
                32_768,
                4_000,
                0.9,
            )],
            fingerprint: None,
        };
        let manifest = ProviderManifest {
            oicp_version: OICP_VERSION.into(),
            provider: None,
            models: vec![
                model("warm", true, Some(42)),
                model("cold", false, Some(42)),
                model("cold-unadvertised", false, None),
            ],
            knowledge: None,
            federation: None,
            features: vec![],
        };
        assert_eq!(
            LoadDebt::from_manifest(&manifest, "warm")
                .unwrap()
                .pending_ms(),
            0,
            "a resident model owes nothing no matter what it advertises"
        );
        assert_eq!(
            LoadDebt::from_manifest(&manifest, "cold")
                .unwrap()
                .pending_ms(),
            42_000
        );
        // Documented under-charge: no estimate means no invented one.
        assert_eq!(
            LoadDebt::from_manifest(&manifest, "cold-unadvertised")
                .unwrap()
                .pending_ms(),
            0
        );
        assert!(LoadDebt::from_manifest(&manifest, "absent").is_none());
    }

    /// A record written before the load-debt fields existed must read as
    /// "resident", i.e. reproduce the old prediction exactly rather than
    /// acquiring a new guess.
    #[test]
    fn a_record_without_load_fields_reads_as_resident() {
        use crate::decision_log::LoadSource;
        let obs = sovereign_core::oicp::NodeObservations::default();
        let ci = CandidateInputs::from_observations(&obs, LoadSource::Local);
        assert_eq!(ci.model_loaded, None);
        assert_eq!(PredictInputs::from_candidate(&ci).pending_load_ms, 0);
    }

    #[test]
    fn a_prediction_is_the_sum_of_its_named_parts() {
        // 4000 tok at 2000 tok/s = 2000ms prefill;
        // 500 tok at 25 tok/s = 20000ms decode; idle, so no queue.
        let p = predict(&inputs(0, 8, 2_000.0, 25.0), shape()).expect("predictable");
        assert_eq!(p.prefill_ms, 2_000.0);
        assert_eq!(p.decode_ms, 20_000.0);
        assert_eq!(p.queue_ms, 0.0);
        assert_eq!(p.rtt_ms, 8.0);
        assert_eq!(p.total_ms, 22_008.0);
        assert_eq!(p.service_ms(), 22_000.0);
    }

    /// The queue term is a count times a service time, so two requests
    /// ahead of us cost two service times — the assumption the module
    /// docs name, asserted so a change to it is deliberate.
    #[test]
    fn the_queue_term_charges_one_service_time_per_request_ahead() {
        let idle = predict(&inputs(0, 0, 1_000.0, 100.0), shape()).unwrap();
        let busy = predict(&inputs(2, 0, 1_000.0, 100.0), shape()).unwrap();
        assert_eq!(busy.queue_ms, 2.0 * idle.service_ms());
        assert_eq!(busy.total_ms, 3.0 * idle.total_ms);
    }

    #[test]
    fn a_candidate_with_no_benchmark_has_no_prediction_rather_than_a_default() {
        let mut i = inputs(0, 8, 1_000.0, 50.0);
        i.tg_tok_s = None;
        assert_eq!(predict(&i, shape()), Err(Unpredictable::NoThroughput));
        i.tg_tok_s = Some(50.0);
        i.pp_tok_s = None;
        assert_eq!(predict(&i, shape()), Err(Unpredictable::NoThroughput));
    }

    #[test]
    fn a_request_with_no_token_shape_has_no_prediction() {
        assert_eq!(
            predict(&inputs(0, 8, 1_000.0, 50.0), None),
            Err(Unpredictable::NoRequestShape)
        );
    }

    /// A zero rate would divide to infinity, and an infinity sorts
    /// first — it would win every comparison it entered.
    #[test]
    fn a_non_positive_rate_is_rejected_rather_than_dividing_to_infinity() {
        assert_eq!(
            predict(&inputs(0, 0, 0.0, 50.0), shape()),
            Err(Unpredictable::NonPositiveRate)
        );
        assert_eq!(
            predict(&inputs(0, 0, 1_000.0, -1.0), shape()),
            Err(Unpredictable::NonPositiveRate)
        );
    }

    #[test]
    fn the_faster_peer_ranks_first_and_the_slower_one_still_ranks() {
        let local = predict(&inputs(0, 0, 100.0, 10.0), shape()); // slow
        let fast = predict(&inputs(0, 8, 4_000.0, 200.0), shape());
        let mid = predict(&inputs(0, 8, 2_000.0, 100.0), shape());
        let winners = faster_than_local(local.into(), vec![("mid", mid), ("fast", fast)]);
        assert_eq!(
            winners.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec!["fast", "mid"]
        );
    }

    /// `Infeasible` inverts rule 1: local is not an unmeasured option,
    /// it is not an option, so every predictable peer wins.
    #[test]
    fn an_infeasible_local_lets_any_predictable_peer_win() {
        let slow = predict(&inputs(9, 40, 100.0, 5.0), shape());
        assert!(slow.is_ok(), "even a dreadful peer must win here");
        let winners = faster_than_local(
            LocalOption::Infeasible,
            vec![
                ("slow", slow),
                ("mystery", Err(Unpredictable::NoThroughput)),
            ],
        );
        // The unpredictable peer is still dropped — infeasible local
        // relaxes the ceiling, not rule 2.
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].0, "slow");
    }

    /// The §4.1 property the product objective cannot express: a hop
    /// that costs more than it buys loses *arithmetically*, with no
    /// floor or margin doing the work.
    #[test]
    fn a_hop_that_costs_more_than_it_buys_is_declined_on_the_arithmetic() {
        // Same hardware both sides, but the peer has two jobs queued
        // and is a round trip away. Nothing about it can win.
        let local = predict(&inputs(0, 0, 1_000.0, 50.0), shape());
        let peer = predict(&inputs(2, 40, 1_000.0, 50.0), shape());
        assert!(faster_than_local(local.into(), vec![("peer", peer)]).is_empty());
    }

    /// Rule 1: no comparison is possible, so no hop — even against a
    /// peer that would have been predicted fast.
    #[test]
    fn an_unpredictable_local_declines_every_hop() {
        let fast = predict(&inputs(0, 1, 8_000.0, 400.0), shape());
        assert!(fast.is_ok());
        let winners = faster_than_local(
            LocalOption::Unpredictable(Unpredictable::NoThroughput),
            vec![("fast", fast)],
        );
        assert!(winners.is_empty());
    }

    /// Rule 3: local is the incumbent, so an exactly-equal peer does
    /// not earn a round trip.
    #[test]
    fn an_exactly_equal_peer_does_not_beat_local() {
        let l = predict(&inputs(0, 0, 1_000.0, 50.0), shape());
        let same = predict(&inputs(0, 0, 1_000.0, 50.0), shape());
        assert_eq!(
            l.as_ref().unwrap().total_ms,
            same.as_ref().unwrap().total_ms
        );
        assert!(faster_than_local(l.into(), vec![("twin", same)]).is_empty());
    }

    /// Ties among *peers* keep caller order — the same order-sensitivity
    /// `winners_over_local` documents, and for the same reason.
    #[test]
    fn tied_peers_keep_the_order_the_caller_supplied() {
        let local = predict(&inputs(0, 0, 100.0, 10.0), shape());
        let a = predict(&inputs(0, 8, 2_000.0, 100.0), shape());
        let b = predict(&inputs(0, 8, 2_000.0, 100.0), shape());
        assert_eq!(a.as_ref().unwrap().total_ms, b.as_ref().unwrap().total_ms);
        let winners = faster_than_local(local.into(), vec![("first", a), ("second", b)]);
        assert_eq!(
            winners.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn an_unpredictable_peer_is_dropped_not_defaulted() {
        let local = predict(&inputs(0, 0, 100.0, 10.0), shape());
        let winners = faster_than_local(
            local.into(),
            vec![
                ("mystery", Err(Unpredictable::NoThroughput)),
                ("known", predict(&inputs(0, 8, 4_000.0, 200.0), shape())),
            ],
        );
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].0, "known");
    }

    /// The replay path reads the same four numbers off a record that
    /// the live path reads off the envelope and the manifest.
    #[test]
    fn inputs_round_trip_through_a_recorded_candidate() {
        use crate::decision_log::LoadSource;
        use sovereign_core::oicp::{BenchmarkResult, NodeObservations};

        let obs = NodeObservations {
            in_flight: 3,
            samples: 40,
            ..Default::default()
        };
        let bench = BenchmarkResult {
            baseline_model_id: "m".into(),
            baseline_size_gb: 21.0,
            pp_tok_s: 2_000.0,
            tg_tok_s: 25.0,
            measured_at: 0,
        };
        let mut ci = CandidateInputs::from_observations(&obs, LoadSource::Gossip)
            .with_benchmark(Some(&bench), 100);
        ci.rtt_ms = Some(12);

        let recovered = PredictInputs::from_candidate(&ci);
        assert_eq!(recovered, inputs(3, 12, 2_000.0, 25.0));
        // And it predicts, which is the claim that matters: a capture
        // carries everything this objective needs.
        assert!(predict(&recovered, shape()).is_ok());
    }

    /// The local candidate records no RTT (there is no probe to
    /// itself), and that must read as zero wire cost rather than as a
    /// missing input.
    #[test]
    fn a_local_candidate_record_with_no_rtt_predicts_zero_wire_cost() {
        use crate::decision_log::LoadSource;
        let obs = sovereign_core::oicp::NodeObservations::default();
        let ci = CandidateInputs::from_observations(&obs, LoadSource::Local);
        assert_eq!(PredictInputs::from_candidate(&ci).rtt_ms, 0);
    }

    // ── §4.2 step 2: the tie band ───────────────────────────────
    // These are the two fleets of §4.1.2 reduced to their arithmetic.
    // If the band admits `mixed-hubs`' second hub, the sampler built on
    // it gives back the objective's whole win; if it refuses
    // `twin-hubs`' identical ones, there is no herd-breaking left to do.

    /// Rank three candidates by `total_ms`, the way
    /// [`faster_than_local`] hands them over.
    fn band_of(cands: Vec<(&'static str, PredictInputs)>) -> (usize, Vec<&'static str>) {
        let scored: Vec<_> = cands
            .into_iter()
            .map(|(n, i)| (n, predict(&i, shape())))
            .collect();
        let ranked = faster_than_local(LocalOption::Infeasible, scored);
        let order = ranked.iter().map(|(n, _)| *n).collect();
        (tie_band(&ranked), order)
    }

    /// `twin-hubs`: same rate card, different queues. The queue is the
    /// stale term, so it must not separate them — the whole band is in
    /// play however deep the (believed) queues are.
    #[test]
    fn identical_rate_cards_are_all_within_noise_however_their_queues_differ() {
        let (band, order) = band_of(vec![
            ("busy", inputs(2, 8, 1_000.0, 100.0)),
            ("idle", inputs(0, 8, 1_000.0, 100.0)),
            ("mid", inputs(1, 8, 1_000.0, 100.0)),
        ]);
        assert_eq!(order, vec!["idle", "mid", "busy"]);
        assert_eq!(
            band, 3,
            "identical hardware must not be separated by a gossiped count"
        );
    }

    /// `mixed-hubs`: same model, different machines. The rate card is
    /// the trustworthy term, so it *must* separate them — this is the
    /// −8% the blunt sampler gave back.
    #[test]
    fn a_slower_machine_is_not_within_noise_of_a_faster_one() {
        let (band, order) = band_of(vec![
            ("hub-fast", inputs(0, 8, 1_000.0, 100.0)),
            ("hub-mid", inputs(0, 8, 800.0, 74.0)),
            ("hub-slow", inputs(0, 8, 400.0, 33.0)),
        ]);
        assert_eq!(order, vec!["hub-fast", "hub-mid", "hub-slow"]);
        assert_eq!(
            band, 1,
            "a real throughput difference is signal, not noise — sampling it away is the regression"
        );
    }

    /// A candidate that is genuinely more capable but *looks* worse
    /// because of its queue is exactly the case the band exists for:
    /// admitted, and then the less-loaded rule decides.
    #[test]
    fn a_faster_machine_hiding_behind_its_queue_is_still_within_noise() {
        let (band, order) = band_of(vec![
            // Nearly the same predicted total, reached two ways.
            ("slow-idle", inputs(0, 8, 500.0, 52.0)),
            ("fast-queued", inputs(1, 8, 1_000.0, 100.0)),
        ]);
        assert_eq!(order, vec!["slow-idle", "fast-queued"]);
        assert_eq!(band, 2);
    }

    /// The one place the definition is deliberately narrower than its
    /// own principle: a candidate whose uncontended time qualifies but
    /// which sits far down the list behind a large queue is NOT
    /// admitted, because a count of two is not plausibly a stale zero.
    /// The run stops at the first candidate it cannot admit.
    #[test]
    fn the_band_is_a_prefix_and_stops_at_the_first_candidate_it_refuses() {
        let (band, order) = band_of(vec![
            ("fast-idle", inputs(0, 8, 1_000.0, 100.0)),
            ("slow-idle", inputs(0, 8, 500.0, 50.0)),
            ("fast-swamped", inputs(2, 8, 1_000.0, 100.0)),
        ]);
        assert_eq!(order, vec!["fast-idle", "slow-idle", "fast-swamped"]);
        assert_eq!(
            band, 1,
            "'slow-idle' closes the run, so 'fast-swamped' is never reached despite qualifying"
        );
    }

    #[test]
    fn an_empty_ranking_has_no_band() {
        let empty: Vec<((), Prediction)> = Vec::new();
        assert_eq!(tie_band(&empty), 0);
    }

    /// "Less loaded" is queue *time*, not a count: three jobs on a fast
    /// machine is less work ahead of you than two on a slow one, and a
    /// count cannot say so.
    #[test]
    fn prefer_less_loaded_reads_queue_time_and_not_the_job_count() {
        let three_on_fast = predict(&inputs(3, 8, 1_000.0, 100.0), shape()).unwrap();
        let two_on_slow = predict(&inputs(2, 8, 500.0, 50.0), shape()).unwrap();
        assert!(three_on_fast.queue_ms < two_on_slow.queue_ms);
        assert!(prefer_less_loaded(&three_on_fast, &two_on_slow));
    }

    /// Ties keep the first argument, so a caller that passes the
    /// better-ranked candidate first gets it back — invariant 5's rule
    /// one layer up.
    #[test]
    fn prefer_less_loaded_keeps_the_first_argument_on_an_exact_tie() {
        let a = predict(&inputs(1, 8, 1_000.0, 100.0), shape()).unwrap();
        let b = predict(&inputs(1, 8, 1_000.0, 100.0), shape()).unwrap();
        assert!(prefer_less_loaded(&a, &b));
        assert!(prefer_less_loaded(&b, &a));
    }

    /// `uncontended_ms` is the prediction minus the one term built from
    /// a gossiped count — stated as an identity so a refactor of
    /// `predict`'s addends cannot silently change what the band means.
    #[test]
    fn uncontended_is_the_prediction_minus_the_guessed_term() {
        let p = predict(&cold(inputs(4, 8, 1_000.0, 100.0), 42_000), shape()).unwrap();
        assert_eq!(p.uncontended_ms(), p.total_ms - p.queue_ms);
        assert_eq!(
            p.uncontended_ms(),
            p.load_ms + p.prefill_ms + p.decode_ms + p.rtt_ms
        );
    }
}
