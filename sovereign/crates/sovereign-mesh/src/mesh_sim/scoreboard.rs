// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scoreboard — what makes this a quality loop and not a test
//! suite (`SCHEDULER_QUALITY.md` §5).
//!
//! Two families of metric, kept deliberately apart:
//!
//!   - [`RecordMetrics`] is computed from the **record stream alone**
//!     (`RoutingDecision` + `RoutingOutcome`). Every number in it is
//!     therefore computable from a production capture too, which is
//!     the precondition for the calibration contract: sim and
//!     hardware must be measurable by the same ruler.
//!   - [`TruthMetrics`] needs ground truth only a simulation has —
//!     the counterfactual "what would local have cost", the true age
//!     of a signal. These are the sharper numbers, and they are
//!     exactly the ones a Tier-2 run cannot check. Keeping them in a
//!     separate type stops that distinction from eroding.

use std::collections::{BTreeMap, HashMap};

use crate::decision_log::{DecisionEvent, RoutingDecision, RoutingOutcome, ServedBy, Verdict};

use super::scenario::RequestClass;
use super::{Arm, RunReport};

/// Percentile by nearest-rank over a pre-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Coefficient of variation — σ/μ. Scale-free, so a 12-node fleet and
/// a 3-node fleet are comparable.
fn coefficient_of_variation(xs: &[f64]) -> f64 {
    let m = mean(xs);
    if m == 0.0 {
        return 0.0;
    }
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64;
    var.sqrt() / m
}

/// Jain's fairness index over a set of shares: 1.0 is perfectly
/// equal, 1/n is maximally concentrated.
fn jains_index(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 1.0;
    }
    let sum: f64 = xs.iter().sum();
    let sum_sq: f64 = xs.iter().map(|x| x * x).sum();
    if sum_sq == 0.0 {
        return 1.0;
    }
    (sum * sum) / (xs.len() as f64 * sum_sq)
}

/// Metrics derivable from the record stream — sim or production.
#[derive(Debug, Clone, Default)]
pub struct RecordMetrics {
    pub decisions: usize,
    pub outcomes: usize,
    /// Fraction of decisions whose outcome carries the same
    /// `decision_id`. The calibration contract has nothing to compare
    /// below 1.0.
    pub join_rate: f64,
    pub offloaded: usize,
    pub gated: usize,
    pub stayed_local: usize,
    pub p50_total_ms: f64,
    pub p95_total_ms: f64,
    pub p50_ttft_ms: f64,
    pub p95_ttft_ms: f64,
    /// Share of *served* requests taken by the busiest server.
    pub top_server_share: f64,
    /// CoV of dispatch counts across servers, per gossip window —
    /// the herding metric. High means everyone piled onto the same
    /// node in the same window. `None` when the records name no
    /// scored peers (the oracle arm records a verdict but no
    /// candidate set), which is *not* the same as zero herding.
    pub herding_cov: Option<f64>,
    /// Fraction of consecutive same-origin decisions that flipped
    /// their chosen target. Low *and* concentrated is the herd
    /// signature; high is churn.
    pub route_flip_rate: f64,
    /// Per-origin p95, worst minus best — §5 tail fairness.
    pub tail_fairness_spread_ms: f64,
    /// Jain's index over per-origin service shares.
    pub origin_share_jain: f64,
    /// Requests that failed over at least once.
    pub failovers: usize,
    pub shed: usize,
    /// Served-request counts keyed by server. Local service is keyed
    /// per origin (`<local:N>`), never pooled.
    pub served_by: BTreeMap<String, usize>,
}

/// Metrics that need simulator ground truth.
#[derive(Debug, Clone, Default)]
pub struct TruthMetrics {
    /// Mean total latency, ms.
    pub mean_total_ms: f64,
    /// Offloads that took longer than staying local would have.
    ///
    /// This is **not** the same as waste. The mesh routes on
    /// capability: a laptop sending a knowledge turn to a bigger,
    /// slower model is buying a better answer with latency, and it
    /// would count here every time. §5's "offloads where round-trip
    /// exceeded local service" measures that trade, not an error.
    pub slower_than_local: usize,
    /// Offloads that were slower than local **because the peer was
    /// backed up** — the peer would have won had it been idle. That
    /// is scheduler error rather than a deliberate trade, and it is
    /// the number a fix to F1 or F5 should move.
    pub wasted_offloads: usize,
    pub offloads: usize,
    /// Mean size of the eligible set (peers that strictly beat
    /// local). Two-choices sampling is inert wherever this is 1.
    pub mean_eligible_peers: f64,
    /// Offload decisions whose eligible set held exactly one peer.
    pub singleton_choices: usize,
    /// Median true age of the load signal a peer decision used.
    pub median_true_signal_age_ms: f64,
    /// Median age the *record* claimed for that same signal. The gap
    /// between the two is how much staleness the glassbox understates.
    pub median_recorded_signal_age_ms: f64,
    /// p95 total latency for the origin with the worst p95.
    pub worst_origin_p95_ms: f64,
    /// Per-class mean latency, so a Fast request being dragged behind
    /// a knowledge turn is visible.
    pub mean_by_class: BTreeMap<&'static str, f64>,
}

/// The column §5 was missing — where capability went, per decision.
///
/// §5's metric table is latency, fairness and waste. None of those can
/// see the failure `Arm::PredictedTime` exposed: it ranks on time
/// alone, so it prefers small fast models, and a scoreboard made of
/// latency reads that as an *improvement*. The sim cannot judge an
/// answer, so it cannot supply the quality gate directly. It can
/// supply the **mechanism by which quality is lost**, which is where
/// the turn was served relative to what was available, and that is
/// what this counts.
///
/// Lives in the record-metric family deliberately: every number here
/// comes from `RoutingDecision.candidates[].tier_band` plus the
/// outcome's `served_by`, so **the identical function scores a
/// production capture.** A quality regression on real hardware is
/// therefore countable by the same ruler as a simulated one, which is
/// the only way this becomes a landing gate rather than a sim artifact.
#[derive(Debug, Clone, Default)]
pub struct TierMetrics {
    /// Decisions with at least one banded candidate — the denominator
    /// for every rate below.
    pub banded_decisions: usize,
    /// Decisions short-circuited before any candidate was scored
    /// (`LocalOnly` privacy, not offload-eligible). Outside the tier
    /// question entirely; counted so they cannot quietly inflate or
    /// deflate a rate.
    pub gated_decisions: usize,
    /// Decisions where no candidate advertised a size at all, so no
    /// band could be assigned. Reported rather than dropped: a zero
    /// here is what makes the rates below trustworthy, and a non-zero
    /// one says the fleet is under-advertising rather than that the
    /// scheduler behaved.
    pub unbanded_decisions: usize,
    /// **Served by a weaker band than the origin's own local model.**
    /// A real regression: the user would have had a better answer by
    /// not offloading at all. This is the one a hard invariant asserts.
    pub downgrades: usize,
    /// **A stronger band was feasible and was passed over**, while the
    /// served candidate was no worse than staying home. Not a
    /// regression against local — and precisely what a household hub
    /// exists to prevent. Disjoint from `downgrades` by construction,
    /// so the two sum to "served below the best available".
    pub declined_upgrades: usize,
    /// Mean band actually served (0 = the most capable models visible).
    pub mean_served_band: f64,
    /// Mean of the best band that was available to be served from. The
    /// gap between this and `mean_served_band` is the magnitude behind
    /// the two counts.
    pub mean_best_band: f64,
    /// How many turns each band served.
    pub served_band_share: BTreeMap<u32, usize>,
}

impl TierMetrics {
    /// Share of banded decisions served below the origin's own local
    /// option. The invariant a tier-floor arm must hold at zero.
    pub fn downgrade_rate(&self) -> f64 {
        if self.banded_decisions == 0 {
            return 0.0;
        }
        self.downgrades as f64 / self.banded_decisions as f64
    }

    /// Share of banded decisions that passed over a strictly more
    /// capable feasible candidate without going below local.
    pub fn declined_upgrade_rate(&self) -> f64 {
        if self.banded_decisions == 0 {
            return 0.0;
        }
        self.declined_upgrades as f64 / self.banded_decisions as f64
    }

    /// Everything served below the best band that was available,
    /// however it got there.
    pub fn below_best_rate(&self) -> f64 {
        self.downgrade_rate() + self.declined_upgrade_rate()
    }
}

/// One arm's full result.
#[derive(Debug, Clone)]
pub struct ArmScore {
    pub arm: Arm,
    pub records: RecordMetrics,
    pub truth: TruthMetrics,
    /// Where capability went — §5's missing column.
    pub tier: TierMetrics,
    /// Mean latency of the oracle ÷ mean latency of this arm. 1.0
    /// means "as good as perfect information"; 0.5 means twice the
    /// oracle's mean. `None` until an oracle run is supplied.
    pub efficiency_ratio: Option<f64>,
}

fn class_label(c: RequestClass) -> &'static str {
    match c {
        RequestClass::Knowledge => "knowledge",
        RequestClass::Fast => "fast",
        RequestClass::Private => "private",
    }
}

/// Compute [`TierMetrics`] from the record stream. Takes a slice for
/// the same reason [`record_metrics`] does: a production capture must
/// be scorable by this exact code, or the landing gate measures the
/// sim rather than the mesh.
///
/// Where a turn was *actually* served comes from the outcome's
/// `served_by` when one joins, and falls back to the decision's
/// selected candidate otherwise. That order matters: the cascade can
/// fail over to a different node than the decision ranked first, and
/// the user's answer came from whoever actually produced it.
pub fn tier_metrics(records: &[DecisionEvent]) -> TierMetrics {
    let mut decisions: Vec<&RoutingDecision> = Vec::new();
    let mut served: HashMap<&str, &ServedBy> = HashMap::new();
    for ev in records {
        match ev {
            DecisionEvent::Decision(d) => decisions.push(d),
            DecisionEvent::Outcome(o) => {
                served.insert(o.decision_id.as_str(), &o.served_by);
            }
            _ => {}
        }
    }

    let mut m = TierMetrics::default();
    let mut served_bands: Vec<f64> = Vec::new();
    let mut best_bands: Vec<f64> = Vec::new();

    for d in &decisions {
        if matches!(d.verdict, Verdict::Gated { .. }) {
            m.gated_decisions += 1;
            continue;
        }
        let Some(best) = d.candidates.iter().filter_map(|c| c.tier_band).min() else {
            // Candidates existed but none advertised a size. Not a
            // scheduling event — a fleet-advertisement one.
            m.unbanded_decisions += 1;
            continue;
        };

        // Who actually answered. `LocalFallback` is still local, and
        // the distinction between it and `Local` is about how the
        // cascade got there, not about which model ran.
        let served_name: Option<&str> = match served.get(d.decision_id.as_str()) {
            Some(ServedBy::Local { .. }) | Some(ServedBy::LocalFallback { .. }) => {
                Some(crate::scheduler_core::LOCAL_CANDIDATE_NAME)
            }
            Some(ServedBy::Peer { name, .. }) => Some(name.as_str()),
            // A request every cascade step failed has no served tier
            // to attribute; it is `RecordMetrics`' failure to count.
            Some(ServedBy::Failed) => None,
            None => d
                .candidates
                .iter()
                .find(|c| c.selected)
                .map(|c| c.name.as_str())
                .or(Some(crate::scheduler_core::LOCAL_CANDIDATE_NAME)),
        };
        let Some(served_name) = served_name else {
            continue;
        };
        let Some(served_band) = d
            .candidates
            .iter()
            .find(|c| c.name == served_name)
            .and_then(|c| c.tier_band)
        else {
            continue;
        };
        let local_band = d
            .candidates
            .iter()
            .find(|c| c.name == crate::scheduler_core::LOCAL_CANDIDATE_NAME)
            .and_then(|c| c.tier_band);

        m.banded_decisions += 1;
        *m.served_band_share.entry(served_band).or_insert(0) += 1;
        served_bands.push(served_band as f64);
        best_bands.push(best as f64);

        // A larger band index is a weaker model. The two counts are
        // kept disjoint so they can be added: a downgrade is already
        // the worst version of passing over something better, and
        // counting it twice would make the totals unreadable.
        let downgraded = local_band.is_some_and(|lb| served_band > lb);
        if downgraded {
            m.downgrades += 1;
        } else if served_band > best {
            m.declined_upgrades += 1;
        }
    }

    m.mean_served_band = mean(&served_bands);
    m.mean_best_band = mean(&best_bands);
    m
}

/// Compute the record-stream half. Takes a slice so a production
/// capture (`SchedulerTrace`) can be scored by exactly this code.
pub fn record_metrics(records: &[DecisionEvent], gossip_window_ms: u64) -> RecordMetrics {
    let mut decisions: Vec<&RoutingDecision> = Vec::new();
    let mut outcomes: Vec<&RoutingOutcome> = Vec::new();
    for ev in records {
        match ev {
            DecisionEvent::Decision(d) => decisions.push(d),
            DecisionEvent::Outcome(o) => outcomes.push(o),
            _ => {}
        }
    }
    let mut m = RecordMetrics {
        decisions: decisions.len(),
        outcomes: outcomes.len(),
        ..Default::default()
    };
    if decisions.is_empty() {
        return m;
    }

    let outcome_ids: std::collections::HashSet<&str> =
        outcomes.iter().map(|o| o.decision_id.as_str()).collect();
    let joined = decisions
        .iter()
        .filter(|d| outcome_ids.contains(d.decision_id.as_str()))
        .count();
    m.join_rate = joined as f64 / decisions.len() as f64;

    for d in &decisions {
        match &d.verdict {
            Verdict::Gated { .. } => m.gated += 1,
            Verdict::StayLocal => m.stayed_local += 1,
            Verdict::Peers { .. } => m.offloaded += 1,
            _ => {}
        }
    }

    // Latency distributions.
    let mut totals: Vec<f64> = outcomes.iter().filter_map(|o| o.total_ms).collect();
    let mut ttfts: Vec<f64> = outcomes.iter().filter_map(|o| o.ttft_ms).collect();
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ttfts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    m.p50_total_ms = percentile(&totals, 50.0);
    m.p95_total_ms = percentile(&totals, 95.0);
    m.p50_ttft_ms = percentile(&ttfts, 50.0);
    m.p95_ttft_ms = percentile(&ttfts, 95.0);

    m.failovers = outcomes.iter().filter(|o| o.attempt_index > 0).count();
    m.shed = outcomes.iter().filter(|o| o.shed).count();

    // Who served what. Local service is keyed by *which* node stayed
    // home, not lumped into one `<local>` bucket — otherwise a policy
    // that spreads work across twelve nodes' own hardware reads as
    // more concentrated than one that funnels everything to a single
    // hub, which is backwards.
    let mut decision_origin: HashMap<&str, &str> = HashMap::new();
    for d in &decisions {
        decision_origin.insert(d.decision_id.as_str(), origin_of(d));
    }
    let mut per_server: HashMap<String, usize> = HashMap::new();
    for o in &outcomes {
        let key = match &o.served_by {
            ServedBy::Peer { name, .. } => name.clone(),
            ServedBy::Local { .. } | ServedBy::LocalFallback { .. } => format!(
                "<local:{}>",
                decision_origin
                    .get(o.decision_id.as_str())
                    .copied()
                    .unwrap_or("?")
            ),
            ServedBy::Failed => continue,
        };
        *per_server.entry(key).or_default() += 1;
    }
    let served_total: usize = per_server.values().sum();
    if served_total > 0 {
        m.top_server_share =
            per_server.values().copied().max().unwrap_or(0) as f64 / served_total as f64;
    }
    m.served_by = per_server.into_iter().collect();

    // Herding: per gossip window, how unevenly were *offload*
    // dispatches spread across the peers that were available to take
    // them? Local service is excluded — staying home is not a herd.
    //
    // The denominator is every peer that was ever *scored*, not every
    // peer that was ever *chosen*. Using the latter is the degenerate
    // trap: a fleet where all twelve deciders pile onto one hub has
    // exactly one chosen target, and the CoV of a one-element vector
    // is zero — perfect herding would score as perfect spread.
    let mut windows: BTreeMap<u64, HashMap<String, usize>> = BTreeMap::new();
    for d in &decisions {
        if let Verdict::Peers { ranked } = &d.verdict {
            if let Some(target) = ranked.first() {
                let w = d.ts_unix_ms / gossip_window_ms.max(1);
                *windows
                    .entry(w)
                    .or_default()
                    .entry(target.clone())
                    .or_default() += 1;
            }
        }
    }
    // The denominator is every peer that was ever *eligible* — that
    // appeared somewhere in a ranked list — not every peer that was
    // ever chosen, and not every peer that was merely scored.
    //
    //   - chosen-only is degenerate: twelve deciders piling onto one
    //     hub yields a one-element vector, whose CoV is zero, so
    //     perfect herding would score as perfect spread.
    //   - scored-only is insensitive: eight laptops that lose every
    //     comparison sit at zero in every window and swamp the
    //     variance among the peers that were actually in contention.
    //
    // Eligible is the set a policy could have spread across, which is
    // the only set "did it spread?" is a question about.
    let target_names: Vec<String> = decisions
        .iter()
        .filter_map(|d| match &d.verdict {
            Verdict::Peers { ranked } => Some(ranked.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    // With fewer than two eligible peers there is nothing to spread
    // across, so "did it spread?" has no answer — report `None`
    // rather than a 0.00 that reads like perfect balance.
    if target_names.len() > 1 {
        let per_window: Vec<f64> = windows
            .values()
            .map(|w| {
                let counts: Vec<f64> = target_names
                    .iter()
                    .map(|n| *w.get(n).unwrap_or(&0) as f64)
                    .collect();
                coefficient_of_variation(&counts)
            })
            .collect();
        m.herding_cov = Some(mean(&per_window));
    }

    // Route flips: consecutive decisions from the same origin that
    // chose a different target.
    let mut last_target: HashMap<&str, String> = HashMap::new();
    let (mut flips, mut pairs) = (0usize, 0usize);
    for d in &decisions {
        let origin = origin_of(d);
        let target = match &d.verdict {
            Verdict::Peers { ranked } => ranked.first().cloned().unwrap_or_default(),
            Verdict::StayLocal => "<local>".to_string(),
            _ => continue,
        };
        if let Some(prev) = last_target.get(origin) {
            pairs += 1;
            if *prev != target {
                flips += 1;
            }
        }
        last_target.insert(origin, target);
    }
    if pairs > 0 {
        m.route_flip_rate = flips as f64 / pairs as f64;
    }

    // Tail fairness: per-origin p95 spread, and Jain over shares.
    let mut per_origin_lat: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in &outcomes {
        if let (Some(origin), Some(total)) = (
            decision_origin.get(o.decision_id.as_str()).copied(),
            o.total_ms,
        ) {
            per_origin_lat.entry(origin).or_default().push(total);
        }
    }
    let mut p95s: Vec<f64> = Vec::new();
    let mut shares: Vec<f64> = Vec::new();
    for (_origin, mut lats) in per_origin_lat {
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        p95s.push(percentile(&lats, 95.0));
        shares.push(lats.len() as f64);
    }
    if !p95s.is_empty() {
        let hi = p95s.iter().cloned().fold(f64::MIN, f64::max);
        let lo = p95s.iter().cloned().fold(f64::MAX, f64::min);
        m.tail_fairness_spread_ms = hi - lo;
        m.origin_share_jain = jains_index(&shares);
    }
    m
}

/// The origin of a decision, recovered from its `oicp_request_id`.
///
/// The sim stamps `sim-<origin>-<seq>`. A production capture carries
/// the workload resolver's own tag, which has no origin field — every
/// record in a single-node capture belongs to that node. Returning
/// the whole id in that case keeps per-origin metrics meaningful in
/// both worlds without inventing a field.
fn origin_of(d: &RoutingDecision) -> &str {
    let id = d.oicp_request_id.as_str();
    if let Some(rest) = id.strip_prefix("sim-") {
        if let Some((origin, _)) = rest.split_once('-') {
            return origin;
        }
    }
    id
}

/// Compute the ground-truth half from a completed run.
pub fn truth_metrics(report: &RunReport) -> TruthMetrics {
    let mut m = TruthMetrics::default();
    if report.truth.is_empty() {
        return m;
    }
    let totals: Vec<f64> = report.truth.iter().map(|f| f.total_ms as f64).collect();
    m.mean_total_ms = mean(&totals);

    let mut eligible: Vec<f64> = Vec::new();
    for fact in &report.truth {
        if fact.origin != fact.server {
            m.offloads += 1;
            eligible.push(fact.eligible_peers as f64);
            if fact.eligible_peers == 1 {
                m.singleton_choices += 1;
            }
            if fact.total_ms > fact.local_alternative_ms {
                m.slower_than_local += 1;
                // Would this peer have beaten local had it been idle?
                // If yes, the loss is queueing — a scheduling error.
                // If no, the decider knowingly bought capability with
                // latency and the scheduler is not at fault.
                if fact.total_ms.saturating_sub(fact.queue_wait_ms) <= fact.local_alternative_ms {
                    m.wasted_offloads += 1;
                }
            }
        }
    }
    m.mean_eligible_peers = mean(&eligible);

    let mut true_ages: Vec<f64> = report
        .truth
        .iter()
        .filter_map(|f| f.true_signal_age_ms)
        .map(|a| a as f64)
        .collect();
    let mut recorded_ages: Vec<f64> = report
        .truth
        .iter()
        .filter_map(|f| f.recorded_signal_age_ms)
        .map(|a| a as f64)
        .collect();
    true_ages.sort_by(|a, b| a.partial_cmp(b).unwrap());
    recorded_ages.sort_by(|a, b| a.partial_cmp(b).unwrap());
    m.median_true_signal_age_ms = percentile(&true_ages, 50.0);
    m.median_recorded_signal_age_ms = percentile(&recorded_ages, 50.0);

    let mut per_origin: HashMap<usize, Vec<f64>> = HashMap::new();
    let mut per_class: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    for fact in &report.truth {
        per_origin
            .entry(fact.origin)
            .or_default()
            .push(fact.total_ms as f64);
        per_class
            .entry(class_label(fact.class))
            .or_default()
            .push(fact.total_ms as f64);
    }
    for (_origin, mut lats) in per_origin {
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        m.worst_origin_p95_ms = m.worst_origin_p95_ms.max(percentile(&lats, 95.0));
    }
    for (label, lats) in per_class {
        m.mean_by_class.insert(label, mean(&lats));
    }
    m
}

/// Score one arm. `oracle_mean_ms` supplies the efficiency-ratio
/// denominator; pass `None` when scoring the oracle itself.
pub fn score(report: &RunReport, gossip_window_ms: u64, oracle_mean_ms: Option<f64>) -> ArmScore {
    let truth = truth_metrics(report);
    let efficiency_ratio = oracle_mean_ms.and_then(|o| {
        (truth.mean_total_ms > 0.0).then_some((o / truth.mean_total_ms).clamp(0.0, 1.0))
    });
    ArmScore {
        arm: report.arm,
        records: record_metrics(&report.records, gossip_window_ms),
        tier: tier_metrics(&report.records),
        truth,
        efficiency_ratio,
    }
}

/// Render a set of arm scores as a fixed-width table.
///
/// Glassbox obligation: a scoreboard nobody can read is a scoreboard
/// nobody checks. Every column here answers one §5 question and is
/// named after it.
pub fn render(scenario: &str, seed: u64, scores: &[ArmScore]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "mesh-sim scoreboard — scenario `{scenario}`, seed {seed}\n\n"
    ));
    out.push_str(&format!(
        "{:<18} {:>7} {:>7} {:>6} {:>8} {:>7} {:>6} {:>6} {:>7} {:>8} {:>5}\n",
        "arm",
        "p50 s",
        "p95 s",
        "eff",
        "topshare",
        "herdCV",
        "flip",
        "waste",
        "slower",
        "spread s",
        "off%"
    ));
    out.push_str(&"-".repeat(96));
    out.push('\n');
    for s in scores {
        let r = &s.records;
        let t = &s.truth;
        let pct = |n: usize, d: usize| {
            if d > 0 {
                100.0 * n as f64 / d as f64
            } else {
                0.0
            }
        };
        out.push_str(&format!(
            // NB: no `.2` precision on the two string columns — on a
            // `&str` that is a *truncation*, which silently rendered
            // "3.11" as "3." in the first version of this table.
            "{:<18} {:>7.1} {:>7.1} {:>6} {:>8.2} {:>7} {:>6.2} {:>5.0}% {:>6.0}% {:>8.1} {:>4.0}%\n",
            s.arm.label(),
            r.p50_total_ms / 1000.0,
            r.p95_total_ms / 1000.0,
            s.efficiency_ratio
                .map(|e| format!("{e:.2}"))
                .unwrap_or_else(|| "—".into()),
            r.top_server_share,
            r.herding_cov
                .map(|c| format!("{c:.2}"))
                .unwrap_or_else(|| "—".into()),
            r.route_flip_rate,
            pct(t.wasted_offloads, t.offloads),
            pct(t.slower_than_local, t.offloads),
            r.tail_fairness_spread_ms / 1000.0,
            pct(r.offloaded, r.decisions),
        ));
    }
    out.push('\n');
    out.push_str("columns: p50/p95 = total latency (s) · eff = mean latency vs the perfect-information oracle (1.0 best)\n");
    out.push_str("         topshare = share of served requests on the busiest node (local service counted per origin, never pooled)\n");
    out.push_str("         herdCV = CoV of dispatches per gossip window across every peer that was ever ELIGIBLE; `—` when fewer\n");
    out.push_str("           than two were, since there was then nothing to spread across · flip = same-origin decisions that changed target\n");
    out.push_str("         waste = offloads slower than local BECAUSE the peer was backed up (scheduler error)\n");
    out.push_str("         slower = offloads slower than local at all (includes the deliberate capability-for-latency trade)\n");
    out.push_str("         spread = worst-minus-best per-origin p95 (tail fairness) · off% = decisions that chose a peer\n");

    // The quality column. Printed as its own block rather than four
    // more columns on an already-wide table, because the reading is
    // different in kind: everything above is a cost, everything here
    // is what was traded for it.
    out.push('\n');
    out.push_str(&format!(
        "{:<18} {:>7} {:>7} {:>9} {:>9} {:>10}\n",
        "arm", "down%", "declUp%", "servedBand", "bestBand", "unbanded"
    ));
    out.push_str(&"-".repeat(64));
    out.push('\n');
    for s in scores {
        let t = &s.tier;
        out.push_str(&format!(
            "{:<18} {:>6.0}% {:>6.0}% {:>9.2} {:>9.2} {:>10}\n",
            s.arm.label(),
            100.0 * t.downgrade_rate(),
            100.0 * t.declined_upgrade_rate(),
            t.mean_served_band,
            t.mean_best_band,
            t.unbanded_decisions,
        ));
    }
    out.push_str(
        "\ncapability (`crate::tier`): band 0 = the most capable models visible in that decision\n",
    );
    out.push_str("         down% = served in a WEAKER band than the origin's own local model — a real regression,\n");
    out.push_str("           and the one a tier-floor arm must hold at zero\n");
    out.push_str("         declUp% = a stronger band was feasible and was passed over, without going below local —\n");
    out.push_str("           not a regression against staying home, but the thing a household hub exists to prevent\n");
    out.push_str("         servedBand/bestBand = mean band served vs mean best available; the gap is the magnitude\n");
    out.push_str("         unbanded = decisions where NO candidate advertised a size, so nothing above could be judged\n");
    out.push_str("         these are the mechanism by which quality is lost, not quality itself — the sim cannot\n");
    out.push_str(
        "           score an answer, and §4.1's landing gate still needs a Tier-2 run that can\n",
    );
    if let Some(first) = scores.first() {
        out.push_str(&format!(
            "\nstaleness: median load-signal age {:.1}s true vs {:.1}s as recorded (the record understates by {:.1}s)\n",
            first.truth.median_true_signal_age_ms / 1000.0,
            first.truth.median_recorded_signal_age_ms / 1000.0,
            (first.truth.median_true_signal_age_ms - first.truth.median_recorded_signal_age_ms)
                / 1000.0,
        ));
        out.push_str(&format!(
            "join rate: {:.3} ({} decisions / {} outcomes)\n",
            first.records.join_rate, first.records.decisions, first.records.outcomes
        ));
        out.push_str(&format!(
            "eligible set: mean {:.2} peers strictly beat local; {}/{} offloads had exactly one candidate \
             (a sampling policy is inert on those)\n",
            first.truth.mean_eligible_peers, first.truth.singleton_choices, first.truth.offloads
        ));
        let inbound: Vec<String> = first
            .records
            .served_by
            .iter()
            .filter(|(k, _)| !k.starts_with("<local"))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        out.push_str(&format!(
            "offloads landed on: {}\n",
            if inbound.is_empty() {
                "nobody".to_string()
            } else {
                inbound.join(" ")
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&xs, 50.0), 5.0);
        assert_eq!(percentile(&xs, 95.0), 10.0);
        assert_eq!(percentile(&xs, 100.0), 10.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn jain_is_one_when_equal_and_falls_with_concentration() {
        assert!((jains_index(&[5.0, 5.0, 5.0]) - 1.0).abs() < 1e-9);
        let concentrated = jains_index(&[15.0, 0.0, 0.0]);
        assert!((concentrated - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn cov_is_zero_for_a_flat_distribution() {
        assert!(coefficient_of_variation(&[3.0, 3.0, 3.0]).abs() < 1e-9);
        assert!(coefficient_of_variation(&[0.0, 0.0]).abs() < 1e-9);
        assert!(coefficient_of_variation(&[10.0, 0.0]) > 0.9);
    }

    #[test]
    fn origin_is_recovered_from_a_sim_request_id_and_falls_back_otherwise() {
        let mut d = crate::decision_log::DecisionBuilder::new(
            "sim-7-1234",
            crate::decision_log::DecisionPath::RankedOicp,
            crate::decision_log::RequestFacts {
                capability_hint: "general".into(),
                latency_class: "Extended".into(),
                sharding: "MeshAllowed".into(),
                context_tokens: None,
                max_output_tokens: None,
                preferred_speed: "Slow".into(),
                explicit_model_id: None,
            },
        )
        .finish(Verdict::StayLocal, &[]);
        assert_eq!(origin_of(&d), "7");
        d.oicp_request_id = "wl-knowledge-42".into();
        assert_eq!(origin_of(&d), "wl-knowledge-42");
    }
}
