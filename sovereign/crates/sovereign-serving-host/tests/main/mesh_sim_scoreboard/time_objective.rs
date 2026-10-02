// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scoreboard tests: the predicted-time objective against the product: the oracle gap, rate-card error, mis-attributed load, model load time, the tier floor. Split out of
//! mesh_sim_scoreboard.rs when it moved here (pb-mesh-dissolve).

use super::*;

/// **§4.1, priced: how much of the gap is a wrong objective, and how
/// much is imperfect information?**
///
/// `AsImplemented` and `Oracle` bracket the problem but do not
/// decompose it. `PredictedTime` is the missing middle term — the
/// oracle's *objective* (minimise time to answer) computed from what a
/// decider can actually see:
///
///   - `arm 0 → predicted` — the cost of a **wrong objective**, with
///     the information held constant.
///   - `predicted → oracle` — the cost of **imperfect information**,
///     with the objective held constant. The only term the two
///     disagree on is the queue: the oracle knows `backlog_ms`, a
///     decider knows a gossiped in-flight *count*.
///
/// Also prints the estimator's own error — predicted vs actual, joined
/// through the record — because a decomposition whose middle term is
/// built on a bad estimate is a decomposition of nothing.
///
/// Reported, not asserted, apart from two wiring checks. F7's lesson:
/// a knob that turns out to be disconnected prints the same flat table
/// as a mechanism that does not matter.
#[test]
fn predicted_time_decomposes_the_oracle_gap() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::isolation(SEED),
    ] {
        let arm0 = run(&s, Arm::AsImplemented, SEED);
        let pred = run(&s, Arm::PredictedTime, SEED);
        let oracle = run(&s, Arm::Oracle, SEED);

        // Wiring check 1: the arm must actually decide differently. If
        // the objective never reached `rank`, the world is identical
        // and so is the outcome.
        assert_ne!(
            offloads(&arm0),
            offloads(&pred),
            "{}: predicted-time took exactly as many offloads as the product — \
             the objective is not wired through RankInputs",
            s.name
        );

        // Wiring check 2: predictions must exist. A request with no
        // token shape is unpredictable for *every* candidate including
        // local, which collapses the arm into stay-local-always — a
        // table that would look like a strong result and mean nothing.
        let joined = chosen_predictions(&pred);
        assert!(
            !joined.is_empty(),
            "{}: no decision under predicted-time yielded a prediction — the OICP \
             envelope is carrying no token shape, so the arm degenerated to \
             stay-local and its numbers are meaningless",
            s.name
        );

        let mean_of = |r: &RunReport| mean_total_ms(r) / 1000.0;
        let (a, p, o) = (mean_of(&arm0), mean_of(&pred), mean_of(&oracle));
        let pct = |from: f64, to: f64| 100.0 * (to - from) / from.max(1.0);

        // How wrong was the estimate, as a fraction of what actually
        // happened? Median *and* p90: with an exact rate card an idle
        // candidate predicts exactly, so the median is 0 and says
        // nothing — the whole error lives in the tail, where the queue
        // substitution bites.
        let mut rel: Vec<f64> = joined
            .iter()
            .map(|(_, predicted, got)| (predicted - got).abs() / got.max(1.0))
            .collect();
        rel.sort_by(|x, y| x.total_cmp(y));
        let at = |q: f64| -> f64 {
            let i = ((q * rel.len() as f64).ceil() as usize).clamp(1, rel.len().max(1)) - 1;
            rel.get(i).copied().unwrap_or(0.0)
        };
        let (median_rel, p90_rel) = (at(0.5), at(0.9));

        // Where the work actually went. This is the line that matters
        // most for the landing: ranking on time alone prefers whichever
        // node answers soonest, which on this fleet is a small fast
        // model rather than the big capable one.
        let inbound = |r: &RunReport| -> String {
            let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
            for f in r.truth.iter().filter(|f| f.origin != f.server) {
                *counts.entry(r.node_names[f.server].as_str()).or_default() += 1;
            }
            if counts.is_empty() {
                return "nobody".into();
            }
            counts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };

        println!("── §4.1 decomposition — `{}` ──", s.name);
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}",
            "arm 0 (product)",
            a,
            offloads(&arm0)
        );
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}   ← wrong objective costs {:+.1}%",
            "predicted-time",
            p,
            offloads(&pred),
            pct(p, a)
        );
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}   ← imperfect info costs {:+.1}%",
            "oracle (perfect info)",
            o,
            offloads(&oracle),
            pct(o, p)
        );
        println!(
            "  estimator error |predicted − actual| / actual: median {:.0}%, p90 {:.0}% \
             over {} joined decisions",
            100.0 * median_rel,
            100.0 * p90_rel,
            joined.len()
        );
        println!("  arm 0 offloads landed on:          {}", inbound(&arm0));
        println!("  predicted-time offloads landed on: {}", inbound(&pred));
        println!(
            "  READ WITH CARE, two ways. (1) At `advertised_rate_error: 0.0` every node's \
             rate card is EXACT TRUTH by construction, so the middle term above carries \
             queue error only and no model error — see \
             `how_much_of_predicted_times_win_survives_a_wrong_rate_card`. (2) This arm \
             ranks on TIME ALONE. Compare the two `landed on` lines: it prefers whichever \
             node answers soonest, which is a small fast model, not the big capable one. \
             §4.1 requires a tier floor as a SEPARATE explicit input, and this arm does \
             not have one — no metric on this scoreboard can see what that costs."
        );
    }
}

/// **The harness's most flattering assumption, priced.**
///
/// `Arm::PredictedTime` reads `pp_tok_s` / `tg_tok_s` off each node's
/// advertised `BenchmarkResult` — and in this sim that benchmark is
/// built from the very `Hardware` the service-time model consumes. So
/// at the default `advertised_rate_error: 0.0` the rate card is *exact
/// truth*, the predictor's only error is the queue-count substitution,
/// and its efficiency ratio is an upper bound no real fleet can reach.
///
/// Publishing the decomposition without this table would repeat exactly
/// the mistake F7's first write-up made: a number that one mechanism
/// explains, presented as though only that mechanism could.
///
/// The oracle is unaffected — it reads true hardware, never the
/// advertised card — so it stays a valid denominator at every error
/// level. Arm 0 *is* affected (`throughput_factor` reads the same
/// benchmark), which keeps the comparison honest.
///
/// Reported, not asserted, apart from the knob's own wiring.
#[test]
fn how_much_of_predicted_times_win_survives_a_wrong_rate_card() {
    let advertised_rates = |r: &RunReport| -> Vec<String> {
        let mut v: Vec<String> = candidates(r)
            .iter()
            .filter_map(|c| c.inputs.bench_tg_tok_s)
            .map(|t| format!("{t:.3}"))
            .collect();
        v.sort();
        v.dedup();
        v
    };

    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
    ] {
        println!("── predicted-time vs a wrong rate card — `{}` ──", s.name);
        println!(
            "  {:>9}  {:>14}  {:>14}  {:>10}",
            "rate err", "arm 0 eff", "predicted eff", "pred mean"
        );
        let mut baseline_rates: Option<Vec<String>> = None;
        for err in [0.0f32, 0.10, 0.25, 0.50, 1.00] {
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..Default::default()
            };
            let oracle = run_with(&s, Arm::Oracle, SEED, cfg.clone());
            let arm0 = run_with(&s, Arm::AsImplemented, SEED, cfg.clone());
            let pred = run_with(&s, Arm::PredictedTime, SEED, cfg.clone());

            // Knob wiring: at err > 0 the advertised rates must differ
            // from the perfect-card set, or this whole table is one
            // number printed five times.
            let rates = advertised_rates(&pred);
            match &baseline_rates {
                None => baseline_rates = Some(rates),
                Some(base) => assert_ne!(
                    base, &rates,
                    "{}: advertised_rate_error {err} left the rate card unchanged — \
                     the knob is not wired",
                    s.name
                ),
            }

            let o = mean_total_ms(&oracle);
            let eff = |r: &RunReport| (o / mean_total_ms(r).max(1.0)).clamp(0.0, 1.0);
            println!(
                "  {:>9.2}  {:>14.2}  {:>14.2}  {:>9.1}s",
                err,
                eff(&arm0),
                eff(&pred),
                mean_total_ms(&pred) / 1000.0,
            );
        }
        println!(
            "  a rate card off by ±{}× is the realistic case (hardware changes, \
             quantisation swaps, a benchmark measured under different thermal \
             conditions); ±0.0 is the harness being kind to itself.",
            2.0
        );
    }
}

/// **Does mis-attributed load hurt the new objective MORE than it hurts
/// the product?**
///
/// This closes a real hole in the §4.1 arm as first landed:
/// `Arm::published_load()` returned `Total` for every arm but one, so
/// predicted-time had never once seen a gossiped in-flight count that
/// missed inbound peer work.
///
/// The structural prior says it should hurt more. The product passes
/// `in_flight` through `load_penalty`, a **bounded** multiplier — a
/// wrong count moves the score a little. The predicted time
/// **multiplies it by a service time**, so the same wrong count is a
/// first-order error that scales with the queue. An objective that
/// trades the product's fudge factors for accuracy is only as good as
/// the inputs it trusts, and this is the input F2 says may be broken.
///
/// Reported, not asserted, apart from the wiring check — the sign of
/// the comparison is the finding.
#[test]
fn does_mis_attributed_load_hurt_predicted_time_more_than_the_product() {
    let published_sum = |r: &RunReport| -> u64 {
        candidates(r)
            .iter()
            .filter_map(|c| c.inputs.gossiped_in_flight)
            .map(u64::from)
            .sum()
    };
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        let p_total = run(&s, Arm::PredictedTime, SEED);
        let p_out = run(&s, Arm::PredictedTimeOutboundOnly, SEED);
        let a_total = run(&s, Arm::AsImplemented, SEED);
        let a_out = run(&s, Arm::OutboundOnlyLoad, SEED);

        // Wiring: the composed arm must actually publish less, or a
        // flat result means "no inbound work existed" rather than "the
        // objective is robust".
        assert!(
            published_sum(&p_out) < published_sum(&p_total),
            "{}: the composed arm published as much as the honest one — \
             `published_load()` is not reaching the predicted-time path",
            s.name
        );

        let pct = |from: &RunReport, to: &RunReport| {
            100.0 * (mean_total_ms(to) - mean_total_ms(from)) / mean_total_ms(from).max(1.0)
        };
        let product_damage = pct(&a_total, &a_out);
        let predicted_damage = pct(&p_total, &p_out);

        println!("── F2 × §4.1 — `{}` ──", s.name);
        println!(
            "  product:        {:>7.1}s → {:>7.1}s  ({:+.1}%)  offloads {} → {}",
            mean_total_ms(&a_total) / 1000.0,
            mean_total_ms(&a_out) / 1000.0,
            product_damage,
            offloads(&a_total),
            offloads(&a_out),
        );
        println!(
            "  predicted-time: {:>7.1}s → {:>7.1}s  ({:+.1}%)  offloads {} → {}",
            mean_total_ms(&p_total) / 1000.0,
            mean_total_ms(&p_out) / 1000.0,
            predicted_damage,
            offloads(&p_total),
            offloads(&p_out),
        );
        // The exposure is conditional on how much the objective offloads
        // at all: a wrong peer-queue count cannot hurt a decision that
        // stayed local. Print the share so the conditional is legible
        // rather than inferred from two raw counts.
        let offload_share =
            |r: &RunReport| 100.0 * offloads(r) as f64 / r.truth.len().max(1) as f64;
        println!(
            "  predicted-time offloads {:.0}% of traffic → {}",
            offload_share(&p_total),
            if predicted_damage > product_damage + 5.0 {
                "MORE damage than the product. The multiply-by-service-time exposure is real \
                 wherever the objective actually hops, so the two-daemon audit is a \
                 PREREQUISITE for landing §4.1 in this regime, not merely earned."
            } else if predicted_damage < product_damage - 5.0 {
                "LESS damage than the product — but not because it is robust. It declines most \
                 hops on this fleet, so a corrupted peer-queue count has little to corrupt. \
                 Do NOT read this as the objective being immune."
            } else {
                "comparable damage — F2's priority is unchanged by the objective here."
            }
        );
    }
}

/// **What model-load time does to the objective.** `predict()` charges
/// it as a single additive term; `SimConfig::model_load_sec_per_gb`
/// makes the world charge it too.
///
/// This term was missing when the arm landed, and its absence was the
/// most expensive of the three gaps: paging in a 21GB model is tens of
/// seconds, which dwarfs every other addend. With nothing in the
/// harness charging for it, the arm could not have found the mistake
/// itself — the same class of blind spot as the exact rate card.
///
/// What to read: whether pricing load changes *where* work goes. A cold
/// big model should stop being attractive, and a warm small one should
/// not — so this is also the first knob that gives the objective a
/// reason to prefer an already-loaded peer.
#[test]
fn what_model_load_time_does_to_the_predicted_time_objective() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
    ] {
        println!("── model-load time × §4.1 — `{}` ──", s.name);
        println!(
            "  {:>10}  {:>10}  {:>14}  {:>10}",
            "load s/GB", "arm 0 eff", "predicted eff", "pred mean"
        );
        for load in [0.0f64, 1.0, 3.0] {
            let cfg = SimConfig {
                model_load_sec_per_gb: load,
                ..Default::default()
            };
            let oracle = run_with(&s, Arm::Oracle, SEED, cfg.clone());
            let arm0 = run_with(&s, Arm::AsImplemented, SEED, cfg.clone());
            let pred = run_with(&s, Arm::PredictedTime, SEED, cfg.clone());

            // Wiring: above zero, candidates must actually advertise a
            // cold model with a load estimate, or the objective is
            // charging nothing and this row is a duplicate of the first.
            if load > 0.0 {
                let cold_advertised = candidates(&pred).iter().any(|c| {
                    c.inputs.model_loaded == Some(false)
                        && c.inputs.estimated_load_ms.unwrap_or(0) > 0
                });
                assert!(
                    cold_advertised,
                    "{}: load {load} s/GB but no candidate advertised a cold model with an \
                     estimate — the load term is not reaching the record or the objective",
                    s.name
                );
            }

            let o = mean_total_ms(&oracle);
            let eff = |r: &RunReport| (o / mean_total_ms(r).max(1.0)).clamp(0.0, 1.0);
            println!(
                "  {:>10.1}  {:>10.2}  {:>14.2}  {:>9.1}s",
                load,
                eff(&arm0),
                eff(&pred),
                mean_total_ms(&pred) / 1000.0,
            );
        }
    }
}

/// The defaults must stay inert, or every number recorded before the
/// rate-card and load-time knobs existed silently stops reproducing.
#[test]
fn a_perfect_rate_card_is_the_default_and_changes_nothing() {
    let s = scenario::household_evening_12(SEED);
    for arm in ALL_ARMS {
        let plain = run(&s, arm, SEED);
        let explicit = run_with(
            &s,
            arm,
            SEED,
            SimConfig {
                advertised_rate_error: 0.0,
                model_load_sec_per_gb: 0.0,
                ..Default::default()
            },
        );
        let key = |r: &RunReport| -> Vec<(usize, usize, u64, u64)> {
            r.truth
                .iter()
                .map(|f| (f.origin, f.server, f.dispatched_at_ms, f.total_ms))
                .collect()
        };
        assert_eq!(
            key(&plain),
            key(&explicit),
            "{}: the rate-card knob's default is not inert",
            arm.label()
        );
    }
}

/// The property every Tier-1 claim rests on.
#[test]
fn runs_are_bit_reproducible() {
    let s = scenario::household_evening_12(SEED);
    for arm in ALL_ARMS {
        let a = run(&s, arm, SEED);
        let b = run(&s, arm, SEED);
        let key = |r: &RunReport| -> Vec<(usize, usize, u64, u64)> {
            r.truth
                .iter()
                .map(|f| (f.origin, f.server, f.dispatched_at_ms, f.total_ms))
                .collect()
        };
        assert_eq!(key(&a), key(&b), "{} is not reproducible", arm.label());
    }
}

/// **§4.1's landing gate: what does respecting capability cost?**
///
/// `Arm::PredictedTime` measured a large latency win and, in the same
/// run, exposed why it could not ship: ranking on time alone prefers
/// whichever node answers soonest, and on every fleet here that is a
/// small fast model. Nothing on §5's scoreboard could see it — latency,
/// fairness and waste all read the choice as an improvement.
///
/// So this test does two things the arm alone could not:
///
///   1. **Makes the damage a number.** `declUp%` is the share of turns
///      that passed over a strictly more capable feasible node. It is
///      the finding, counted rather than described.
///   2. **Prices the fix.** `predicted-time+tier-floor` is §4.1 made to
///      respect capability. The question is not whether it is faster —
///      it will not be. It is how much of the win survives.
///
/// Reported, not asserted, apart from the structural claims: nobody has
/// agreed a latency threshold for the trade, and asserting one would
/// invent the very fudge §4.1 exists to remove.
#[test]
fn the_tier_floor_prices_capability_against_latency() {
    let arms = [
        Arm::AsImplemented,
        Arm::TierFloor,
        Arm::PredictedTime,
        Arm::PredictedTimeTierFloor,
        Arm::Oracle,
    ];

    for scenario in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
    ] {
        let (reports, scores) = sweep(&scenario);
        assert_hard_invariants(&reports, &scores);
        println!("\n=== {} ===", scenario.name);
        print_tier_block(&scores, &arms);

        // Whether the floor's latency is a scheduling result or a
        // capacity fact. Asserting nothing — reading it is the point.
        println!("── is the latency queueing, and is the queue stable? ──");
        for arm in [
            Arm::AsImplemented,
            Arm::PredictedTime,
            Arm::PredictedTimeTierFloor,
        ] {
            if let Some(r) = reports.iter().find(|r| r.arm == arm) {
                print_saturation(r);
            }
        }

        let pick = |arm: Arm| {
            scores
                .iter()
                .find(|s| s.arm == arm)
                .unwrap_or_else(|| panic!("{} was not run", arm.label()))
        };
        let arm0 = pick(Arm::AsImplemented);
        let pred = pick(Arm::PredictedTime);
        let floor = pick(Arm::PredictedTimeTierFloor);

        let price = |a: &ArmScore, b: &ArmScore| {
            100.0 * (b.truth.mean_total_ms - a.truth.mean_total_ms) / a.truth.mean_total_ms.max(1.0)
        };
        println!(
            "  §4.1 win vs arm 0: {:+.0}%   ·   what the floor costs §4.1: {:+.0}%   ·   \
             floor vs arm 0: {:+.0}%",
            price(arm0, pred),
            price(pred, floor),
            price(arm0, floor),
        );
        println!(
            "  quality traded: predicted-time declined {} upgrades ({:.0}%); with the floor, {} ({:.0}%)",
            pred.tier.declined_upgrades,
            100.0 * pred.tier.declined_upgrade_rate(),
            floor.tier.declined_upgrades,
            100.0 * floor.tier.declined_upgrade_rate(),
        );

        // The floor's whole promise, and the only thing strong enough
        // to assert: with it, no turn is served below the best band
        // that was available to it. Both counts, so a zero in one
        // cannot hide a non-zero in the other.
        assert_eq!(
            floor.tier.downgrades, 0,
            "{}: tier floor still allowed a downgrade",
            scenario.name
        );
        assert_eq!(
            floor.tier.declined_upgrades, 0,
            "{}: tier floor still allowed {} declined upgrades — a binding floor admits \
             only band 0, so anything served below it means the filter did not run",
            scenario.name, floor.tier.declined_upgrades
        );
        // And the baseline it is measured against must be untouched by
        // any of it. If adding the floor arms moved arm 0, every
        // recorded number in §3/§4.1 is invalidated and the comparison
        // above is meaningless.
        assert!(
            arm0.tier.banded_decisions > 0,
            "{}: arm 0 recorded no banded decisions — the fleet advertises no sizes and \
             the tier columns are vacuous",
            scenario.name
        );
    }
}
