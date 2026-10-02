// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scoreboard tests: the tier floor where the top band has capacity: the herd, the within-noise band, dishonest size advertisements. Split out of
//! mesh_sim_scoreboard.rs when it moved here (pb-mesh-dissolve).

use super::*;

/// **§4.1.1 consequence 1, with the sample it lacked.**
///
/// The one result that changed the plan was a *constant-quality*
/// comparison: put the tier floor on both arms, so both answer from
/// band 0, and ask whether ranking on predicted time still beats
/// ranking on the product. §4.1.1 could only run that comparison on
/// `twin-hubs`, because it was the suite's only fleet whose top band
/// could absorb the offered load — everywhere else the floor's latency
/// is a queue that never drains, and two schedulers inside an unbounded
/// queue are both just measuring the fleet's capacity. One fleet, one
/// seed, and a −5% headline that contradicted §4.1's +126–250%.
///
/// This widens the sample on both axes that were n=1:
///
///   - **Seeds.** Five, per fleet, world and policy both re-seeded.
///   - **Fleets.** `mixed-hubs` joins it, and it is deliberately the
///     *opposite* bracket. `twin-hubs` band 0 is three identical hubs,
///     so predicted time has nothing to discriminate on but a stale
///     queue count — the condition most hostile to it. `mixed-hubs`
///     band 0 spans 34 / 25 / 11 tok/s, which is what predicting a
///     completion time is *for*. If the objective loses on both, the
///     −5% was not an artifact of homogeneity. If it wins on one, the
///     honest answer is "it depends on the fleet", and that is a
///     different plan than either headline implies.
///
/// The precondition is asserted rather than assumed, because it is the
/// only thing that makes the comparison mean anything: both fleets must
/// still be *unsaturated* under the floor, or this test silently
/// becomes another capacity measurement. Everything else is reported.
#[test]
fn does_predicted_time_beat_the_product_where_the_top_band_has_capacity() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — constant quality (both arms wear the tier floor) ===");
        println!(
            "  {:<8} {:>18} {:>22} {:>9}  {}",
            "seed", "arm0+floor mean/p95", "predicted+floor mean/p95", "Δ mean", "top-server share"
        );
        let mut base_means = Vec::new();
        let mut pred_means = Vec::new();
        let mut pred_wins = 0;
        for seed in seeds {
            let s = build(seed);
            let base_report = run(&s, Arm::TierFloor, seed);
            let pred_report = run(&s, Arm::PredictedTimeTierFloor, seed);
            let base = score(&base_report, GOSSIP_WINDOW_MS, None);
            let pred = score(&pred_report, GOSSIP_WINDOW_MS, None);

            // Constant quality is the premise, not a hope: if either
            // arm served a turn below the best band available to it,
            // the latency columns below are comparing two different
            // products and the whole test is void.
            for (arm, sc) in [("arm0+floor", &base), ("predicted+floor", &pred)] {
                assert_eq!(
                    (sc.tier.downgrades, sc.tier.declined_upgrades),
                    (0, 0),
                    "{label}/{seed}: {arm} traded quality ({} downgrades, {} declined \
                     upgrades) — the constant-quality comparison below is void",
                    sc.tier.downgrades,
                    sc.tier.declined_upgrades
                );
            }

            // The property the fleet exists to provide. Asserted for
            // the same reason: an unbounded queue makes both arms
            // measure capacity instead of policy. Three turns is a
            // deliberately loose gate — the fleets §4.1.1 called
            // saturated sit at 6.6 and 38, these two at well under 1 —
            // so it fails on the thing it is watching for and not on
            // the ordinary queueing a loaded fleet does.
            let mut depths = Vec::new();
            for (arm, report) in [
                ("arm0+floor", &base_report),
                ("predicted+floor", &pred_report),
            ] {
                let Some(sat) = saturation(report) else {
                    continue;
                };
                depths.push(sat.backlog_depth());
                assert!(
                    sat.backlog_depth() < 3.0,
                    "{label}/{seed}: {arm} ended the run {:.1} turns deep in queue \
                     (wait {:.1}s → {:.1}s against {:.1}s of service) — this fleet's top band \
                     is saturated under the floor, so it cannot host a constant-quality \
                     comparison of schedulers",
                    sat.backlog_depth(),
                    sat.q1_wait_s,
                    sat.q4_wait_s,
                    sat.service_s
                );
            }

            let b = base.truth.mean_total_ms / 1000.0;
            let p = pred.truth.mean_total_ms / 1000.0;
            base_means.push(b);
            pred_means.push(p);
            if p < b {
                pred_wins += 1;
            }
            println!(
                "  {:<8} {:>10.1}s {:>6.1}s {:>14.1}s {:>6.1}s {:>+8.0}%   {:.2} → {:.2}   \
                 backlog {}",
                seed % 1000,
                b,
                base.records.p95_total_ms / 1000.0,
                p,
                pred.records.p95_total_ms / 1000.0,
                100.0 * (p - b) / b.max(0.001),
                base.records.top_server_share,
                pred.records.top_server_share,
                depths
                    .iter()
                    .map(|d| format!("{d:.2}"))
                    .collect::<Vec<_>>()
                    .join(" → "),
            );
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
        let (b, p) = (mean(&base_means), mean(&pred_means));
        println!(
            "  ── {label}: predicted-time is {:+.0}% vs the product at constant quality \
             ({:.1}s → {:.1}s), winning {}/{} seeds",
            100.0 * (p - b) / b.max(0.001),
            b,
            p,
            pred_wins,
            seeds.len()
        );

        // Where the work actually went, on one seed. A latency delta
        // without this is a number without a mechanism.
        let s = build(SEED);
        for (arm_label, arm) in [
            ("arm0+floor", Arm::TierFloor),
            ("pred+floor", Arm::PredictedTimeTierFloor),
        ] {
            let sc = score(&run(&s, arm, SEED), GOSSIP_WINDOW_MS, None);
            let mut by: Vec<(&String, &usize)> = sc.records.served_by.iter().collect();
            by.sort_by(|a, b| b.1.cmp(a.1));
            let cols: Vec<String> = by.iter().map(|(n, c)| format!("{n}×{c}")).collect();
            println!("     {arm_label:<12} {}", cols.join("  "));
        }
    }

    // **Which half of the fleet's heterogeneity is doing the work?**
    //
    // `mixed-hubs` band 0 contains two different gaps: 11 tok/s vs the
    // rest, which the product *can* see (11/20 scores 0.55), and 34
    // vs 25 tok/s, which it cannot (`throughput_factor` clamps both to
    // 1.0). If predicted-time's win survives deleting the first gap,
    // the mechanism is the clamp — F3 costing real latency — and not
    // merely "one node was obviously bad".
    //
    // The counterfactual is exact: same seed, same arrival stream, same
    // sizes and therefore the same bands. Only `hub-slow`'s hardware
    // changes, and arrival generation reads neither field.
    println!("\n=== mixed-hubs with the slow hub deleted (34/25/25 — every gap invisible to the scorer) ===");
    let mut no_slow = scenario::mixed_hubs(SEED);
    let mid = no_slow.nodes[1].hardware;
    no_slow.nodes[2].hardware = mid;
    no_slow.name = "mixed-hubs-no-slow".into();
    let with_slow = scenario::mixed_hubs(SEED);
    assert_eq!(
        no_slow.arrivals.len(),
        with_slow.arrivals.len(),
        "changing a node's hardware must not change the arrival stream"
    );
    for (fleet_label, fleet) in [("34/25/11", &with_slow), ("34/25/25", &no_slow)] {
        let base = score(&run(fleet, Arm::TierFloor, SEED), GOSSIP_WINDOW_MS, None);
        let pred = score(
            &run(fleet, Arm::PredictedTimeTierFloor, SEED),
            GOSSIP_WINDOW_MS,
            None,
        );
        let (b, p) = (
            base.truth.mean_total_ms / 1000.0,
            pred.truth.mean_total_ms / 1000.0,
        );
        println!(
            "  band 0 = {fleet_label}   arm0+floor {:>5.1}s   predicted+floor {:>5.1}s   \
             Δ {:+.0}%",
            b,
            p,
            100.0 * (p - b) / b.max(0.001)
        );
    }

    // **The flattering assumption underneath all of it.**
    //
    // `Arm::PredictedTime`'s own doc bounds what may be claimed from
    // it: the objective consumes `pp_tok_s` / `tg_tok_s` directly, and
    // this module's service-time model is computed from those same two
    // fields. On a fleet built out of *speed variance*, that is not a
    // small caveat — it hands the objective a perfect model of the
    // world it is predicting, which is exactly the advantage being
    // measured. A win that only exists at zero rate-card error is a
    // property of the simulator, not of the objective.
    //
    // `advertised_rate_error` is the instrument that already exists for
    // this: nodes still *serve* at their true rate, they only advertise
    // a perturbed one. It is two-sided, so it degrades the product's
    // `throughput_factor` too — the comparison stays fair.
    //
    // One asymmetry could have broken that fairness, so it is counted
    // rather than argued. The product has an error-correcting path the
    // predicted time does not: `throughput_factor` switches to the
    // *observed* decode EWMA past five samples, while
    // `PredictInputs::from_candidate` reads `bench_tg_tok_s` and
    // nothing else. If that path were hot, the sweep would be
    // handicapping only one arm. The printed share says it is not —
    // about 5% of candidate scorings, because most peers never
    // accumulate five samples in half an hour, which is F7's ramp
    // wearing a different hat. Both objectives are therefore reading
    // the same perturbed number in ~95% of decisions.
    println!("\n=== mixed-hubs: how much of the win survives a wrong rate card? ===");
    println!(
        "   (both arms wear the floor; nodes serve at the true rate, advertise a perturbed one)"
    );
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let mut base_means = Vec::new();
        let mut pred_means = Vec::new();
        let mut pred_wins = 0;
        let mut observed = 0usize;
        let mut estimated = 0usize;
        for seed in seeds {
            let s = scenario::mixed_hubs(seed);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            let base_report = run_with(&s, Arm::TierFloor, seed, cfg.clone());
            // Which rate did the *product* actually score on? It is the
            // only one of the two objectives with an error-correcting
            // path — `throughput_factor` prefers the observed EWMA past
            // five samples, where `predicted_time` reads
            // `bench_tg_tok_s` and nothing else. Whether that path was
            // hot decides how the rows below may be read.
            for ev in &base_report.records {
                if let sovereign_scheduler::decision_log::DecisionEvent::Decision(d) = ev {
                    for c in &d.candidates {
                        match c.score.throughput_source.as_str() {
                            "observed" => observed += 1,
                            "benchmark_estimate" => estimated += 1,
                            _ => {}
                        }
                    }
                }
            }
            let b = score(&base_report, GOSSIP_WINDOW_MS, None)
                .truth
                .mean_total_ms
                / 1000.0;
            let p = score(
                &run_with(&s, Arm::PredictedTimeTierFloor, seed, cfg),
                GOSSIP_WINDOW_MS,
                None,
            )
            .truth
            .mean_total_ms
                / 1000.0;
            if p < b {
                pred_wins += 1;
            }
            base_means.push(b);
            pred_means.push(p);
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
        let (b, p) = (mean(&base_means), mean(&pred_means));
        println!(
            "  rate error ±{:>4.0}%   arm0+floor {:>5.1}s   predicted+floor {:>5.1}s   \
             Δ {:+.0}%   predicted wins {}/{} seeds   (product scored on observed rate \
             {:.0}% of the time)",
            100.0 * err,
            b,
            p,
            100.0 * (p - b) / b.max(0.001),
            pred_wins,
            seeds.len(),
            100.0 * observed as f64 / (observed + estimated).max(1) as f64,
        );
    }
}

/// **Is breaking the herd the prerequisite §4.1.1 said it was?**
///
/// §4.1.1's third consequence was that predicted time *concentrates*
/// harder than the product once the floor makes candidates homogeneous
/// — 40/28/10 across three identical hubs against the product's
/// 31/27/18 — and concluded that §4.2 step 2 is a prerequisite for the
/// floor rather than a follow-on. That was a mechanism inferred from a
/// distribution. This measures it, by putting the sampler the sim has
/// had since S0 in front of the §4.1 landing candidate.
///
/// The two unsaturated fleets should disagree, and the disagreement is
/// the finding:
///
///   - `twin-hubs` — band 0 is three identical hubs, so a uniform draw
///     over the ranked list *is* a draw over near-ties. Sampling can
///     only help, and how much it helps is the price of the herd.
///   - `mixed-hubs` — band 0 spans 34 / 25 / 11 tok/s, so a uniform
///     draw discards the information the objective exists to use.
///
/// If both improve, §4.2 step 2 can ship as written and blunt. If
/// `mixed-hubs` regresses, the "within noise" qualifier in §4.2 step 2
/// is the load-bearing part of that sentence and a blunt sampler is a
/// quality-neutral latency regression waiting to happen.
///
/// Reported, not asserted, other than the floor's own invariant: the
/// trade between a fleet-mean and a tail has no agreed threshold.
#[test]
fn does_breaking_the_herd_recover_what_the_floor_costs() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];
    let arms = [
        Arm::TierFloor,
        Arm::PredictedTimeTierFloor,
        Arm::PredictedTimeTierFloorTwoChoices,
        // §4.2 step 2 as written. The blunt arm above reads the two
        // fleets in opposite directions; this one is the claim that
        // restricting the draw to the tie band keeps BOTH readings —
        // twin-hubs' recovery and mixed-hubs' win. Printed beside its
        // predecessor because the comparison is the whole point.
        Arm::PredictedTimeTierFloorWithinNoise,
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — does sampling break the herd, and what does it cost? ===");
        let mut baseline = None;
        for arm in arms {
            let (mut means, mut p95s, mut shares, mut covs) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            // Summed across seeds: how often the sampler HAD a choice,
            // and how often it took one. Without it, "same mean as the
            // arm below" is ambiguous between "never fired" and "fired
            // constantly and it was a wash".
            let (mut fired, mut moved, mut decided, mut band_sum) = (0u64, 0u64, 0u64, 0u64);
            for seed in seeds {
                let s = build(seed);
                let report = run(&s, arm, seed);
                fired += report.sampler.band_at_least_two;
                moved += report.sampler.moved_off_argmax;
                decided += report.sampler.decisions;
                band_sum += report.sampler.band_total;
                let sc = score(&report, GOSSIP_WINDOW_MS, None);
                // The floor still has to hold, or the arm is buying its
                // latency with the quality the floor exists to protect.
                assert_eq!(
                    (sc.tier.downgrades, sc.tier.declined_upgrades),
                    (0, 0),
                    "{label}/{seed}/{}: sampling escaped the tier floor",
                    arm.label()
                );
                means.push(sc.truth.mean_total_ms / 1000.0);
                p95s.push(sc.records.p95_total_ms / 1000.0);
                shares.push(sc.records.top_server_share);
                if let Some(cov) = sc.records.herding_cov {
                    covs.push(cov);
                }
            }
            let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
            let m = mean(&means);
            let base = *baseline.get_or_insert(m);
            println!(
                "  {:<40} mean {:>5.1}s  p95 {:>5.1}s  top-server {:.2}  herding CoV {:.2}   \
                 {:+.0}% vs arm0+floor",
                arm.label(),
                m,
                mean(&p95s),
                mean(&shares),
                mean(&covs),
                100.0 * (m - base) / base.max(0.001),
            );
            if decided > 0 {
                let pct = |n: u64| 100.0 * n as f64 / decided as f64;
                println!(
                    "  {:<40}   └─ band ≥2 on {:.0}% of {decided} decisions \
                     (mean band {:.2}), moved off the argmax on {:.0}%",
                    "",
                    pct(fired),
                    band_sum as f64 / decided as f64,
                    pct(moved),
                );
            }
        }
    }
}

/// **The within-noise band's stated limit, measured rather than
/// asserted.**
///
/// `predicted_time::tie_band` admits a candidate only when its
/// *uncontended* prediction does not separate it from the leader's, and
/// on `twin-hubs` that is every band-0 hub — identical hardware on a
/// uniform LAN advertises an identical rate card, so nothing separates
/// them. That identity is precisely the flattering assumption
/// `advertised_rate_error` exists to price (note 963a8d88's method
/// rule: an arm must price the harness assumption that most flatters
/// it, and this arm's band is built out of one). Perturb the card, the
/// hubs stop looking identical *to the decider*, the band narrows, and
/// §4.1.2's −4% recovery should decay with it.
///
/// The decay is the safe direction — a narrow band falls back to the
/// argmax, which is the arm this one refines, so the cost is a
/// forfeited recovery and not a regression. But "conservative" is a
/// claim about a number, and §6's rule is that claims about numbers get
/// numbers. A real fleet of near-identical hubs (34 vs 33 tok/s) sits
/// somewhere on this curve, and this is the table that says where.
///
/// Reported, not asserted, apart from the mechanical claim: the band
/// has to actually narrow, or the paragraph above describes something
/// the code does not do.
#[test]
fn what_the_within_noise_band_costs_when_identical_hubs_stop_looking_identical() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    println!("\n=== twin-hubs: the band is built on an exact rate card — what if it is wrong? ===");
    println!(
        "   (identical hubs; only what they ADVERTISE is perturbed, never what they serve at)"
    );
    let mut first_band = None;
    let mut last_band = 0.0;
    for err in [0.0_f32, 0.1, 0.25, 0.5, 1.0] {
        let (mut floor_means, mut argmax_means, mut blunt_means, mut noise_means) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut decided, mut band_sum, mut fired) = (0u64, 0u64, 0u64);
        for seed in seeds {
            let s = scenario::twin_hubs(seed);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            let mean_of = |r: &_| score(r, GOSSIP_WINDOW_MS, None).truth.mean_total_ms / 1000.0;
            floor_means.push(mean_of(&run_with(&s, Arm::TierFloor, seed, cfg.clone())));
            argmax_means.push(mean_of(&run_with(
                &s,
                Arm::PredictedTimeTierFloor,
                seed,
                cfg.clone(),
            )));
            blunt_means.push(mean_of(&run_with(
                &s,
                Arm::PredictedTimeTierFloorTwoChoices,
                seed,
                cfg.clone(),
            )));
            let sampled = run_with(&s, Arm::PredictedTimeTierFloorWithinNoise, seed, cfg);
            decided += sampled.sampler.decisions;
            band_sum += sampled.sampler.band_total;
            fired += sampled.sampler.band_at_least_two;
            noise_means.push(mean_of(&sampled));
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
        let (floor, argmax, blunt, noise) = (
            mean(&floor_means),
            mean(&argmax_means),
            mean(&blunt_means),
            mean(&noise_means),
        );
        let band = band_sum as f64 / decided.max(1) as f64;
        first_band.get_or_insert(band);
        last_band = band;
        println!(
            "  ±{:>4.0}%  arm0+floor {:>5.1}s  argmax {:>5.1}s  blunt {:>5.1}s  \
             within-noise {:>5.1}s ({:+.0}% vs argmax)   mean band {:.2}, ≥2 on {:.0}%",
            err * 100.0,
            floor,
            argmax,
            blunt,
            noise,
            100.0 * (noise - argmax) / argmax.max(0.001),
            band,
            100.0 * fired as f64 / decided.max(1) as f64,
        );
    }
    let first = first_band.expect("the sweep ran at least one row");
    assert!(
        last_band < first,
        "a perturbed rate card must narrow the band ({first:.2} → {last_band:.2}); if it does \
         not, the band is not reading the rate card the way tie_band's docs claim"
    );
}

/// **The tier floor's flattering assumption, priced.**
///
/// `size_gb` is peer-advertised, and it is the *only* input to a
/// quality gate that a node states about itself. `advertised_rate_error`
/// exists because a rate card built from the same `Hardware` the sim
/// serves at is exact truth by construction; the same objection applies
/// here, and the same instrument answers it.
///
/// The adversarial direction is a small model over-selling its way into
/// the top band — which is exactly how a 4B would come to serve
/// synthesis despite the floor. Nothing here scores or serves
/// differently, so any movement is the floor mis-banding somebody and
/// nothing else.
///
/// Reported, not asserted, apart from the knob's own wiring: what
/// counts as an acceptable mis-banding rate is a policy question nobody
/// has answered yet.
#[test]
fn what_a_dishonest_size_advertisement_does_to_the_tier_floor() {
    use sovereign_scheduler::decision_log::DecisionEvent;

    // Band 0 membership is what the floor reads; everything below it is
    // scenery. Reported per seed because whether a lie crosses a band
    // edge depends on which way each node's draw went, and one seed
    // would report an accident as a property.
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    println!("\n── tier floor under mis-advertised size (household-evening-12) ──");
    println!("   the floor reads band 0 only. hub 21.0 GB is 3.5x the next model,");
    println!("   and the band edge is 2.0x — so a lie must move the RATIO by 1.75x to matter.");
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let mut intruded = 0;
        let mut downgrades = 0;
        let mut means = Vec::new();
        for seed in seeds {
            let s = scenario::household_evening_12(seed);
            let cfg = SimConfig {
                advertised_size_error: err,
                ..SimConfig::default()
            };
            let report = run_with(&s, Arm::PredictedTimeTierFloor, seed, cfg);
            let sc = score(&report, GOSSIP_WINDOW_MS, None);
            downgrades += sc.tier.downgrades;
            means.push(sc.truth.mean_total_ms / 1000.0);
            // Did anything but the hub reach band 0?
            let non_hub_in_top = report.records.iter().any(|ev| match ev {
                DecisionEvent::Decision(d) => d
                    .candidates
                    .iter()
                    .any(|c| c.tier_band == Some(0) && c.name != "hub" && c.name != "local"),
                _ => false,
            });
            if non_hub_in_top {
                intruded += 1;
            }
        }
        println!(
            "  size error +/-{:>4.0}%   seeds where a non-hub reached band 0: {}/{}   \
             downgrades {}   mean {:>6.1}s",
            100.0 * err,
            intruded,
            seeds.len(),
            downgrades,
            means.iter().sum::<f64>() / means.len() as f64,
        );
        if err == 0.0 {
            assert_eq!(
                intruded, 0,
                "an honest fleet put something other than the 35B hub in the top band"
            );
        }
        // The invariant is about the floor's INTEGRITY, not the fleet's
        // honesty: whatever the decider believes the bands are, it must
        // never serve a turn below the origin's own local band. A lie
        // can move a node between bands; it must not be able to make
        // the filter stop filtering.
        assert_eq!(
            downgrades,
            0,
            "size error +/-{:.0}%: the floor allowed {} downgrades — a mis-advertisement \
             changed WHICH band a node is in, which is expected, but it must not defeat \
             the filter itself",
            100.0 * err,
            downgrades
        );
    }

    // The knob's default must be inert, or every number recorded before
    // it existed silently changed meaning.
    let s = scenario::household_evening_12(SEED);
    let plain = run(&s, Arm::PredictedTimeTierFloor, SEED);
    let explicit = run_with(
        &s,
        Arm::PredictedTimeTierFloor,
        SEED,
        SimConfig {
            advertised_size_error: 0.0,
            ..SimConfig::default()
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
        "the size-advertisement knob's default is not inert"
    );
}
