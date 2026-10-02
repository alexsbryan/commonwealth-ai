// SPDX-License-Identifier: AGPL-3.0-or-later
//! The policy arms the simulator compares. Split out of mod.rs when
//! mesh_sim moved here (pb-mesh-dissolve).

use super::*;

/// A policy arm. Arm 0 is as-implemented; everything else is a
/// candidate change that must earn its landing here first
/// (`SCHEDULER_QUALITY.md` §6: "behavioural work goes INTO the sim as
/// arms, not into production first").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// Arm 0. The production decision function on the beliefs
    /// production's dispatch path was *designed* to produce.
    ///
    /// **Read the qualifier — it was not there before F9 (2026-07-27).**
    /// This arm feeds `rank` an exact local in-flight count
    /// ([`Sim::local_view_observations`]) and lets peer observations
    /// accumulate samples through [`scheduler_core::observe_dispatch`].
    /// Production did neither on the ranked path: `record_dispatch` had
    /// no caller for the local side at all, and only the non-streaming
    /// *named* arm calls it for peers. So arm 0 was the as-**designed**
    /// baseline and [`BlindObservations`](Arm::BlindObservations) the
    /// as-**shipped** one. Every number recorded against arm 0 before
    /// 2026-07-27 — §3.1, §4.1.1, §4.1.2, §4.1.3, §4.2.1 — is a
    /// comparison against the designed system, which is the right
    /// baseline for "is this policy good?" and the wrong one for "what
    /// does this mesh do today?".
    ///
    /// **Half of that gap has since closed.** F9's local half landed in
    /// production the same day, so the exact local in-flight count this
    /// arm supplies is now what the daemon supplies too (it reads
    /// `in_flight_publisher` at the gather point). The remaining
    /// divergence is the peer half, and the arm that models today's
    /// mesh is therefore [`BlindPeerRamp`](Arm::BlindPeerRamp) — **not**
    /// `BlindObservations`, which is now a historical baseline for the
    /// pre-fix system. Whoever adds the next arm: compare against
    /// `BlindPeerRamp` when the question is "what will this do on the
    /// mesh tonight."
    ///
    /// Arm 0 keeps its name and its index because it is the denominator
    /// every recorded number already references; renaming it would
    /// invalidate the archive to fix a comment.
    AsImplemented,
    /// Arm 0 with the local candidate's in-flight count forced to zero
    /// on every decision — F9's headline half, in isolation.
    ///
    /// `load_penalty` (`scoring.rs:274`) reads `in_flight` from the
    /// local candidate's [`NodeObservations`], and the only writer of
    /// that field is `observe_dispatch`, reached for the local side
    /// exclusively through `record_dispatch(None)` — which has **zero
    /// callers in the repository**. The local candidate is therefore
    /// scored permanently idle, and the design comment at
    /// `peer_inference.rs:1198` ("so a hot local slot can lose to an
    /// idle peer on load") describes behaviour the shipped code cannot
    /// exhibit.
    ///
    /// Separate from [`BlindObservations`](Arm::BlindObservations) for
    /// the same reason [`ResponseBackpressure`](Arm::ResponseBackpressure)
    /// is separate from [`FreshSignals`](Arm::FreshSignals): the two
    /// halves of F9 bias in the same direction, so folding them into one
    /// arm would report a total without saying which half carries it.
    BlindLocalLoad,
    /// F9's peer half in isolation: peer observations frozen at zero
    /// samples, but the origin's self-observed peer in-flight count
    /// left intact.
    ///
    /// Exists to answer one question about
    /// [`BlindObservations`](Arm::BlindObservations), which changes two
    /// things about peers at once. Freezing `samples` pins
    /// `cold_start_weight` at 0.7 forever; zeroing the self-observed
    /// in-flight count makes an un-gossiped peer look idle. The first
    /// biases against offload, the second biases toward it, and a
    /// single arm carrying both cannot say which one moved the number.
    /// The delta from here to `BlindObservations` is the in-flight
    /// component alone.
    ///
    /// Not a faithful model of anything on its own — production has
    /// both — so read it as an attribution instrument, not as a policy.
    BlindPeerRamp,
    /// **Both** halves of F9 — the faithful model of what production
    /// scored up to 2026-07-27, and a historical baseline since.
    ///
    /// The local half was fixed that day, so today's mesh is
    /// [`BlindPeerRamp`](Arm::BlindPeerRamp). This arm is kept because
    /// the delta between the two *is* the value of that fix (−71% mean
    /// / −76% p95 on `isolation`, ±1% on four other fleets), and
    /// deleting it would leave the landing case unreproducible.
    ///
    /// [`BlindLocalLoad`](Arm::BlindLocalLoad), plus peer observations
    /// frozen at zero samples. On the ranked/streaming path — the path
    /// every anonymous offload takes — nothing calls
    /// `record_dispatch(Some(..))` either; the sole call site is
    /// `peer_inference.rs:2173`, the non-streaming *named* arm. So a
    /// peer's `samples` never leaves 0, which pins `cold_start_weight`
    /// at `COLD_START_MIN_WEIGHT` (0.7) forever and — the sharper
    /// consequence — makes `throughput_factor` return neutral 1.0
    /// regardless of what the peer actually achieved, because its
    /// source-of-truth gate is `samples >= THROUGHPUT_OBSERVATION_THRESHOLD`
    /// (`scoring.rs:231`). The streaming path *does* keep
    /// `tg_tok_s_ewma` current for peers
    /// (`ThroughputTarget::Peer`, `peer_inference.rs:2402`), so the
    /// measurement is taken and then refused by a gate the same path
    /// never opens.
    ///
    /// What is deliberately **not** blinded here, because production
    /// does write it: the peer in-flight count (gossip, and it overrides
    /// the self-observed value at `scheduler_core.rs:512`), the local
    /// throughput EWMA (`ThroughputTarget::Local`), and `PeerHealth`
    /// quarantine. Blinding those would model a system worse than the
    /// one that shipped.
    ///
    /// Both biases point the same way — toward local — which is why
    /// F9's arithmetic concludes the ranked path is *structurally*
    /// incapable of preferring a peer on a homogeneous fleet. This arm
    /// is how that claim gets a number instead of an argument.
    BlindObservations,
    /// Arm 0 with every candidate's advertised `BenchmarkResult`
    /// removed — F10's second half, in isolation.
    ///
    /// **This is not a hypothetical, and as of 2026-07-28 it is no
    /// longer even an accident.** No node on this mesh has ever
    /// advertised a rate card. Both producers — `run_baseline_benchmark`
    /// and `InferenceRouter::set_local_benchmark`, each with zero
    /// callers — were deleted rather than left as an invitation to wire
    /// them up, and the local `benchmark` field went with them. The
    /// gossip builder likewise hardcodes `benchmark: None`
    /// (`capabilities.rs`). Both ends of the wire are now blind by
    /// construction, so this arm measures the shipped system rather
    /// than a state it merely happens to be in.
    ///
    /// The consequence composes with F9's peer half and that is the
    /// point of the arm. `throughput_factor` (`scoring.rs:362`) has
    /// exactly two sources and production supplies neither: the
    /// observed EWMA is gated on `samples >= 5`, which the ranked path
    /// never reaches, and the benchmark estimate needs the rate card
    /// that does not exist. Its `(None, None)` branch returns **neutral
    /// 1.0**. So the term F3 catalogued as "does not discriminate under
    /// heterogeneity" does not discriminate at *all* in production — it
    /// is a constant, and every fleet is scored as though every node
    /// ran at the reference rate.
    ///
    /// Separate from [`BlindShipped`](Arm::BlindShipped) for the reason
    /// the whole `blind-*` family is factored this way: the rate card
    /// and the ramp both feed `throughput_factor`, so an arm carrying
    /// both cannot say which one holds the term shut.
    BlindRateCard,
    /// What this mesh actually does tonight, as of 2026-07-27:
    /// [`BlindPeerRamp`](Arm::BlindPeerRamp) plus
    /// [`BlindRateCard`](Arm::BlindRateCard).
    ///
    /// Successor to [`BlindObservations`](Arm::BlindObservations) as
    /// the as-shipped arm. That one was faithful until F9's local half
    /// landed and was never faithful about the rate card. The
    /// three-step correction is worth stating once, because each step
    /// was found by a different reading and the last one is F10:
    ///
    /// 1. arm 0 supplies an exact local in-flight count — production
    ///    did not, until F9's fix landed. **Closed.**
    /// 2. arm 0 lets peer `samples` accumulate — production does not,
    ///    and this was measured *protective* (§4.4), so it stays.
    /// 3. arm 0 supplies a rate card — production never has. **Open,
    ///    and this arm is what prices it.**
    ///
    /// Compare against this arm, not arm 0 and not `BlindPeerRamp`,
    /// when the question is "what will this change do on the mesh
    /// tonight."
    BlindShipped,
    /// Arm 0 with the staleness removed: the gossiped in-flight count
    /// is the peer's *true* current count. Isolates F1 — the gap
    /// between this and arm 0 is the cost of dead time, and nothing
    /// else changes.
    FreshSignals,
    /// Arm 0, but the decider samples two of the eligible peers and
    /// takes the better rather than always taking the argmax.
    /// Isolates F5 — deterministic argmax over a shared signal is a
    /// herd generator.
    TwoChoices,
    /// Both, to show whether they compose or overlap.
    FreshTwoChoices,
    /// Arm 0 with every decider's peer observations pre-seeded at
    /// `COLD_START_SAMPLES` — the counterfactual in which every
    /// decider already has history with every peer. Prices F7.
    ///
    /// **Not a single-factor isolation, and the distinction matters.**
    /// `samples` is one number feeding three scorer terms, so seeding
    /// it lifts everything the scorer withholds from a stranger:
    ///
    ///   - `cold_start_weight` — 0.7 → 1.0. This is F7 proper.
    ///   - `throughput_factor`'s *source* — a peer past
    ///     `THROUGHPUT_OBSERVATION_THRESHOLD` (5) with a warmed EWMA
    ///     is scored on observed decode rate instead of the benchmark
    ///     estimate. Identical at `t = 0` (the EWMA is still zero) and
    ///     divergent as soon as anything completes.
    ///   - `effective_affinity`'s observation weight — inert here,
    ///     because no arm produces failures, so the blend is
    ///     `1 − w·0 = 1` at any sample count.
    ///
    /// That is not a defect of the arm: `samples` is *the* "do I know
    /// this peer" signal, and a peer that is never dispatched to keeps
    /// all of these penalties at once. The counterfactual worth
    /// pricing is therefore the whole stranger penalty, not one term
    /// of it. `tests/mesh_sim_scoreboard.rs` prints the throughput
    /// source mix per arm so the two effects stay separable in the
    /// report.
    WarmStart,
    /// [`WarmStart`](Arm::WarmStart) **and** fresh signals — the arm
    /// that tells you *why* warm-start hurts.
    ///
    /// Warm-start alone is much worse than arm 0, and the obvious
    /// explanation is F1: lifting the cold-start floor unlocks
    /// offloads, and a decider cannot see the queue it is offloading
    /// into. That is a mechanism, and a mechanism asserted is not a
    /// mechanism measured. This arm discriminates. If the damage is
    /// F1's, it disappears when the signal is fresh; if warm-start is
    /// still worse here, offloading is simply unprofitable in this
    /// fleet and F1 was the wrong culprit.
    FreshWarmStart,
    /// Arm 0 with [`PublishedLoad::OutboundOnly`] — the counterfactual
    /// in which the gossiped counter misses inbound peer work.
    /// Isolates the load-attribution question before it costs two
    /// daemons.
    OutboundOnlyLoad,
    /// **§4.1.** Rank on predicted time-to-answer
    /// (`queue + prefill + decode + rtt`) instead of on the product of
    /// dimensionless multipliers. The feasibility half is untouched —
    /// same hard gates, same candidate records, same scores recorded —
    /// so the delta against arm 0 is a delta of *objective* and of
    /// nothing else. See [`sovereign_scheduler::predicted_time`].
    ///
    /// Read it against [`Oracle`](Arm::Oracle), which minimises the
    /// same quantity with perfect knowledge of every queue. The two
    /// gaps price different things, and that is the point of having
    /// this arm between them:
    ///
    ///   - `oracle − predicted` — the cost of **imperfect
    ///     information**. The only term the two disagree on is the
    ///     queue: the oracle knows `backlog_ms` exactly, a decider
    ///     knows a gossiped in-flight *count*.
    ///   - `predicted − arm 0` — the cost of a **wrong objective**,
    ///     holding information constant.
    ///
    /// Arm 0 and the oracle were both already here; this is the
    /// missing middle term, and it is the one that says which of the
    /// two problems to fix first.
    ///
    /// **Bound on what may be claimed from it.** A predicted time is
    /// far more sensitive to this module's hand-chosen service-time
    /// model than a ranking is — it consumes `pp_tok_s` / `tg_tok_s`
    /// directly, where the product objective flattens them through a
    /// clamp (F3). The qualitative result is robust: *does it decline
    /// the offloads `WarmStart` exposed?* The latency **magnitudes**
    /// are not quotable until S1 runs against a capture from real
    /// hardware.
    PredictedTime,
    /// [`PredictedTime`](Arm::PredictedTime) **under
    /// [`PublishedLoad::OutboundOnly`]** — the composition that was
    /// missing when the arm first landed, and it is the one that
    /// matters most.
    ///
    /// The two objectives consume `in_flight` very differently. The
    /// product passes it through `load_penalty`, a **bounded**
    /// multiplier: a mis-attributed count moves the score a little. The
    /// predicted time **multiplies it by a service time**, so a count
    /// that misses inbound peer work is a *first-order* error that
    /// scales with the queue. F2 should therefore hurt this objective
    /// more than it hurts arm 0, and a scheduler that is confidently
    /// wrong is worse than one that is vaguely wrong.
    ///
    /// If that holds, the two-daemon inbound-load audit is not just
    /// earned (it already was, at +126%..+584% for arm 0) but a
    /// **prerequisite** for landing §4.1 — because the objective's
    /// accuracy is exactly what it trades the product's fudge factors
    /// for.
    PredictedTimeOutboundOnly,
    /// Arm 0 **plus §4.1's tier floor** — capability filters the
    /// candidate set before the product ranks what survives.
    ///
    /// Here to separate two costs that would otherwise arrive fused.
    /// The floor is a policy, not an objective, so it applies to
    /// whatever ranks behind it; running it over the product first
    /// answers "what does requiring the top band cost a fleet, on its
    /// own?" Without this arm, any change in
    /// [`PredictedTimeTierFloor`](Arm::PredictedTimeTierFloor) could be
    /// attributed to either half.
    ///
    /// Expect it to be *slower* than arm 0 on a single-hub fleet: arm 0
    /// already prefers the hub (its affinity is the highest score in
    /// the fleet), and the floor additionally forbids the stay-local
    /// fallback, so knowledge turns that used to be answered at home
    /// now queue. That is the mechanism the household-latency price is
    /// made of, and it belongs in a separate column from §4.1's.
    TierFloor,
    /// **The §4.1 landing candidate.** [`PredictedTime`](Arm::PredictedTime)
    /// with the tier floor in front of it.
    ///
    /// `PredictedTime` cannot land as measured: it ranks on time alone,
    /// which on every fleet here prefers a small fast model, and no §5
    /// metric can see the cost because the scoreboard measures latency,
    /// fairness and waste — not answer quality. This arm is the fix and
    /// its price, together.
    ///
    /// The question it exists to answer is not "is it faster" — it will
    /// not be. It is: **how much of §4.1's win survives being made to
    /// respect capability?** If the household mean returns to arm 0's
    /// 25.7s, §4.1's 11.4s was bought by answering hard turns with a 4B
    /// and the objective is not the improvement it appeared to be. If
    /// it lands between, the gap is what a correct objective is worth
    /// *at constant quality*, which is the only version of the claim
    /// worth putting in the spec.
    ///
    /// `twin-hubs` is the fleet where the two should compose rather
    /// than fight: three identical hubs share the top band, so the
    /// floor leaves predicted time a real choice to make and should
    /// spread load across them instead of herding on one.
    PredictedTimeTierFloor,
    /// **§4.2 step 2, composed with the §4.1 landing candidate.**
    /// [`PredictedTimeTierFloor`](Arm::PredictedTimeTierFloor) with the
    /// two-choices sampler in front of the dispatch.
    ///
    /// §4.1.1 found that predicted time *herds harder* than the product
    /// once the floor makes candidates homogeneous, and named breaking
    /// the herd a prerequisite for the floor rather than a follow-on.
    /// This arm is that claim made falsifiable, and the two fleets with
    /// an unsaturated top band read it in opposite directions:
    ///
    ///   - On `twin-hubs` the band is three *identical* hubs, so the
    ///     sampler's uniform draw over the ranked list **is** a draw
    ///     over near-ties. If herding is what costs predicted time its
    ///     few percent there, this arm recovers it.
    ///   - On `mixed-hubs` the band spans 34 / 25 / 11 tok/s, and a
    ///     uniform draw throws away exactly the information the
    ///     objective exists to use. A regression here is not a failure
    ///     of the arm; it is the measurement that says §4.2 step 2's
    ///     *"among candidates whose predictions are within noise"* is
    ///     load-bearing rather than decorative.
    ///
    /// Note what makes that predicate expressible at all: predicted
    /// times are in milliseconds, so "within noise" has units. A
    /// dimensionless product has no scale on which two scores can be
    /// called close, which is a second reason §4.2 step 2 wants §4.1
    /// underneath it.
    PredictedTimeTierFloorTwoChoices,
    /// **§4.2 step 2 as actually specified** — the arm
    /// [`PredictedTimeTierFloorTwoChoices`](Arm::PredictedTimeTierFloorTwoChoices)
    /// is the blunt draft of.
    ///
    /// Same composition (predicted time + tier floor + a two-sample
    /// draw), one difference: the draw is restricted to the **tie
    /// band** — the head of the ranked list the predictor cannot
    /// resolve, sized by [`predicted_time::tie_band`] from the rate
    /// card rather than by a margin constant — and the winner of the
    /// two is the *less loaded* rather than the better-ranked, since
    /// rank order inside the band is an order on the one signal the
    /// band just declared unreadable.
    ///
    /// §4.1.2 measured the blunt draw recovering −4% on `twin-hubs`
    /// and giving back +3% on `mixed-hubs`. This arm is the claim that
    /// the *qualifier* rather than the sampling is what separates those,
    /// and §4.1.3 measured it holding on both fleets: `twin-hubs` −4%
    /// (band ≥2 on 97% of decisions, mean width 2.92 — the blunt draw
    /// there *was* a near-tie draw) and `mixed-hubs` −8% (band ≥2 on
    /// 29%, mean width 1.30 — the band collapses toward the leader and
    /// the objective keeps its win).
    ///
    /// **What the same section also measured, and it is the finding
    /// that stops this arm being a clean win:** the band reads the
    /// *advertised* rate card, so it only recognises identical hubs
    /// while they advertise identically. At ±10% rate-card error the
    /// `twin-hubs` band collapses (2.92 → 1.45) and the −4% recovery
    /// inverts to +3% against the plain argmax, while the blunt
    /// sampler — which never consults the card — keeps its whole
    /// recovery. Neither sampler dominates: this one is the only safe
    /// choice on a heterogeneous top band, and the blunt one is the
    /// robust choice on a homogeneous one.
    PredictedTimeTierFloorWithinNoise,
    /// **§4.2 step 1.** Arm 0, plus the serving node piggybacking its
    /// true load on every response it returns.
    ///
    /// This is the *implementable* half of
    /// [`FreshSignals`](Arm::FreshSignals). That arm hands every
    /// decider the truth about every peer at every instant, which no
    /// mechanism can deliver — it is an upper bound, and §3.1 priced it
    /// at −11% p95 on `household-evening-12` and −51% on `twin-hubs`.
    /// A response can only tell you about the peer that answered it, so
    /// this arm is fresh exactly where a real implementation would be:
    ///
    ///   - **Fresh** for a peer this decider has served a request
    ///     through, aged from the moment that response came back.
    ///   - **Stale gossip** for every other peer, unchanged.
    ///   - **Nothing** for a peer never heard from at all.
    ///
    /// The gap `backpressure − fresh-signals` is therefore the part of
    /// F1 that piggybacking *cannot* reach, and it is the number that
    /// decides whether the response channel is sufficient or whether
    /// the gossip interval also has to come down.
    ///
    /// **Why it should capture most of the win despite covering fewer
    /// peers.** The peer a decider is wrong about in the direction that
    /// costs latency is the peer it keeps choosing — and that is
    /// precisely the peer it keeps getting responses from. Coverage is
    /// biased toward the error that matters. If the measured recovery
    /// is small anyway, that biased coverage is the assumption to
    /// suspect first, which is why [`BackpressureTrace`] reports the
    /// coverage rate alongside the latency.
    ///
    /// **Known optimism, bounded and one-directional.** The reading is
    /// delivered at the instant service completes rather than one RTT
    /// later, so every age here is understated by up to one RTT (single
    /// -digit to ~100 ms on these fleets) against a gossip interval of
    /// 10 s. It cannot manufacture a win larger than that; it can only
    /// make this arm look at most one RTT better than the mechanism
    /// would be.
    ResponseBackpressure,
    /// [`ResponseBackpressure`](Arm::ResponseBackpressure) composed with
    /// §4.1's landing candidate
    /// ([`PredictedTimeTierFloor`](Arm::PredictedTimeTierFloor)).
    ///
    /// Here because the two objectives consume `in_flight` differently
    /// enough that a freshness result on arm 0 does not transfer. The
    /// product passes the count through `load_penalty`, a **bounded**
    /// multiplier; predicted time **multiplies it by a service time**.
    /// A stale count is a second-order error for the first and a
    /// first-order one for the second — the same asymmetry
    /// [`PredictedTimeOutboundOnly`](Arm::PredictedTimeOutboundOnly)
    /// exists to price for attribution.
    ///
    /// So the delta against `predicted-time+tier-floor` is the direct
    /// test of §4.2's own claim that step 1 is a **prerequisite** for
    /// the §4.1 landing rather than an independent improvement. If
    /// freshness is worth materially more here than it is on arm 0,
    /// that claim is measured rather than argued.
    PredictedTimeTierFloorBackpressure,
    /// Not a policy anyone could implement: assigns each request to
    /// whichever node would finish it soonest, with perfect knowledge
    /// of every queue. The denominator of the efficiency ratio.
    ///
    /// "Clairvoyant" in the online sense — it knows the present
    /// exactly but not the future, so it bounds what any
    /// current-state policy could achieve rather than being a global
    /// optimum.
    Oracle,
}

impl Arm {
    pub fn label(&self) -> &'static str {
        match self {
            Arm::AsImplemented => "as-implemented",
            Arm::BlindLocalLoad => "blind-local-load",
            Arm::BlindPeerRamp => "blind-peer-ramp",
            Arm::BlindObservations => "blind-observations",
            Arm::BlindRateCard => "blind-rate-card",
            Arm::BlindShipped => "blind-shipped",
            Arm::FreshSignals => "fresh-signals",
            Arm::TwoChoices => "two-choices",
            Arm::FreshTwoChoices => "fresh+two-choices",
            Arm::WarmStart => "warm-start",
            Arm::FreshWarmStart => "fresh+warm-start",
            Arm::OutboundOnlyLoad => "outbound-only-load",
            Arm::PredictedTime => "predicted-time",
            Arm::PredictedTimeOutboundOnly => "predicted-time+outbound-only",
            Arm::TierFloor => "tier-floor",
            Arm::PredictedTimeTierFloor => "predicted-time+tier-floor",
            Arm::PredictedTimeTierFloorTwoChoices => "predicted-time+tier-floor+two-choices",
            Arm::PredictedTimeTierFloorWithinNoise => "predicted-time+tier-floor+within-noise",
            Arm::ResponseBackpressure => "response-backpressure",
            Arm::PredictedTimeTierFloorBackpressure => "predicted-time+tier-floor+backpressure",
            Arm::Oracle => "oracle",
        }
    }

    /// Which ranking objective this arm hands to
    /// `scheduler_core::rank`. Everything but the predicted-time arms
    /// ranks the way production does today.
    pub(crate) fn objective(&self) -> RankObjective {
        match self {
            Arm::PredictedTime
            | Arm::PredictedTimeOutboundOnly
            | Arm::PredictedTimeTierFloor
            | Arm::PredictedTimeTierFloorTwoChoices
            | Arm::PredictedTimeTierFloorWithinNoise
            | Arm::PredictedTimeTierFloorBackpressure => RankObjective::PredictedTime,
            _ => RankObjective::Product,
        }
    }

    /// Whether this arm applies §4.1's capability filter before
    /// ranking. A *policy* dimension, orthogonal to
    /// [`objective`](Arm::objective) — which is exactly why it is a
    /// separate method and not another `RankObjective` variant.
    pub(crate) fn tier_floor(&self, req: &InferenceRequirements) -> TierFloor {
        match self {
            Arm::TierFloor
            | Arm::PredictedTimeTierFloor
            | Arm::PredictedTimeTierFloorTwoChoices
            | Arm::PredictedTimeTierFloorWithinNoise
            | Arm::PredictedTimeTierFloorBackpressure => TierFloor::from_requirements(req),
            _ => TierFloor::None,
        }
    }

    pub(super) fn fresh_signals(&self) -> bool {
        matches!(
            self,
            Arm::FreshSignals | Arm::FreshTwoChoices | Arm::FreshWarmStart
        )
    }

    /// Whether the serving node piggybacks its true load on the
    /// responses it returns (§4.2 step 1). Deliberately **not** folded
    /// into [`fresh_signals`](Arm::fresh_signals): that one is an
    /// oracle and this one is a mechanism, and the whole value of the
    /// arm is the distance between them.
    pub(super) fn response_backpressure(&self) -> bool {
        matches!(
            self,
            Arm::ResponseBackpressure | Arm::PredictedTimeTierFloorBackpressure
        )
    }

    /// Whether the local candidate is scored with an in-flight count of
    /// zero no matter what this node is actually running — F9's local
    /// half. See [`BlindLocalLoad`](Arm::BlindLocalLoad).
    pub(super) fn blind_local_load(&self) -> bool {
        matches!(self, Arm::BlindLocalLoad | Arm::BlindObservations)
    }

    /// Whether peer observations stay frozen at zero samples — F9's
    /// peer half. Kept distinct from
    /// [`warm_start`](Arm::warm_start), which moves the same field in
    /// the opposite direction: `warm_start` asks "what if every decider
    /// had already finished the ramp?", this asks "what if no decider
    /// can ever start it?". Both are needed because F7's answer to the
    /// first turned out to be counter-intuitive.
    pub(super) fn blind_peer_samples(&self) -> bool {
        matches!(
            self,
            Arm::BlindPeerRamp | Arm::BlindObservations | Arm::BlindShipped
        )
    }

    /// Whether every candidate — local and peer alike — is scored with
    /// `benchmark: None`, which is what production does because nothing
    /// ever measures or gossips one. See
    /// [`BlindRateCard`](Arm::BlindRateCard).
    ///
    /// Applied at both injection sites rather than at the node, because
    /// the blindness is not a property of the advertiser: the local
    /// side is missing for a different reason (no producer — the
    /// setter and the field were deleted) than the peer side
    /// (`capabilities.rs` hardcodes `None`), and a change could
    /// plausibly land one without the other.
    pub(super) fn blind_rate_card(&self) -> bool {
        matches!(self, Arm::BlindRateCard | Arm::BlindShipped)
    }

    /// Whether the origin's *self-observed* in-flight count for a peer
    /// is frozen at zero — the second thing production's ranked path
    /// never writes. Narrow by construction: `rank` overrides this
    /// field with the gossiped count whenever a gossip record exists
    /// (`scheduler_core.rs:512`), so it only reaches the score for a
    /// peer never heard from. Separate from
    /// [`blind_peer_samples`](Arm::blind_peer_samples) because the two
    /// bias in opposite directions.
    pub(super) fn blind_peer_inflight(&self) -> bool {
        matches!(self, Arm::BlindObservations)
    }

    pub(super) fn two_choices(&self) -> bool {
        matches!(
            self,
            Arm::TwoChoices | Arm::FreshTwoChoices | Arm::PredictedTimeTierFloorTwoChoices
        )
    }

    /// Whether the two-sample draw is restricted to the tie band
    /// (§4.2 step 2 as written) rather than taken over the whole ranked
    /// list. Checked *before* [`two_choices`](Arm::two_choices) at the
    /// dispatch site, and kept a separate predicate rather than a flag
    /// on that one, because the two samplers differ in their winner
    /// rule as well as their draw set — they are two policies, not one
    /// policy with a switch.
    pub(super) fn within_noise_sampling(&self) -> bool {
        matches!(self, Arm::PredictedTimeTierFloorWithinNoise)
    }

    /// Whether deciders start already believing they have completed
    /// the cold-start ramp for every peer.
    pub fn warm_start(&self) -> bool {
        matches!(self, Arm::WarmStart | Arm::FreshWarmStart)
    }

    /// What a node counts when it gossips its load.
    pub fn published_load(&self) -> PublishedLoad {
        match self {
            Arm::OutboundOnlyLoad | Arm::PredictedTimeOutboundOnly => PublishedLoad::OutboundOnly,
            _ => PublishedLoad::Total,
        }
    }
}

/// Every arm worth reporting side by side. `PredictedTime` sits last
/// before `Oracle` because that is the order the §4.1 reading wants:
/// arm 0 → predicted → perfect information.
pub const ALL_ARMS: [Arm; 21] = [
    Arm::AsImplemented,
    // The F9 and F10 arms sit immediately after arm 0 because that is
    // the order the reading wants: as-designed → as-shipped → candidate
    // changes. They must not displace index 0, which the scoreboard
    // dereferences directly as the baseline.
    Arm::BlindLocalLoad,
    Arm::BlindPeerRamp,
    Arm::BlindObservations,
    Arm::BlindRateCard,
    Arm::BlindShipped,
    Arm::FreshSignals,
    Arm::TwoChoices,
    Arm::FreshTwoChoices,
    Arm::WarmStart,
    Arm::FreshWarmStart,
    Arm::OutboundOnlyLoad,
    Arm::PredictedTime,
    Arm::PredictedTimeOutboundOnly,
    Arm::TierFloor,
    Arm::PredictedTimeTierFloor,
    Arm::PredictedTimeTierFloorTwoChoices,
    Arm::PredictedTimeTierFloorWithinNoise,
    Arm::ResponseBackpressure,
    Arm::PredictedTimeTierFloorBackpressure,
    Arm::Oracle,
];
