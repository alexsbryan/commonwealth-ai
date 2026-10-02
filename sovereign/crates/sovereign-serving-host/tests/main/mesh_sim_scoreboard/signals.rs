// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scoreboard tests: fresh signals: piggybacked backpressure, and what the scorer loses blind to its own load or to measurement. Split out of
//! mesh_sim_scoreboard.rs when it moved here (pb-mesh-dissolve).

use super::*;

/// **§4.2 step 1, priced: what does the *implementable* half of fresh
/// signals actually buy?**
///
/// `fresh-signals` is the arm every other finding in this file leans
/// on, and it is not a policy anybody can ship. It hands every decider
/// the truth about every peer at every instant. §3.1 priced it at −11%
/// p95 on `household-evening-12` and −51% on `twin-hubs`, and §4.2
/// step 1 proposes to collect that by piggybacking the serving node's
/// load on the responses it already sends.
///
/// Those are not the same thing, and the difference is not a detail. A
/// response can only carry news about the peer that *answered*, so the
/// mechanism is fresh on a subset of the candidate set and stale
/// everywhere else. `response-backpressure` is that subset made
/// explicit. Read the three arms as a bracket:
///
///   - `as-implemented → response-backpressure` — what the mechanism
///     is worth.
///   - `response-backpressure → fresh-signals` — what it cannot reach,
///     and therefore what a shorter gossip interval would still have
///     to buy.
///
/// The **recovery** column is the ratio of the two, and it is the
/// number §4.2 step 1 should be judged on. A recovery near 1.0 means
/// the response channel is sufficient and the gossip interval can be
/// left alone. Near 0.0 means the win lives entirely in peers a
/// decider never talks to, and the proposal is aimed at the wrong
/// place.
///
/// The coverage line is not decoration. A null result here has two
/// incompatible explanations — the mechanism fired and did not help,
/// or it never fired — and the latency column cannot tell them apart.
/// That is F7's lesson, and the wiring assertions below are the part
/// of this test that is allowed to fail the build.
#[test]
fn what_does_piggybacked_backpressure_recover_of_fresh_signals() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 4] = [
        ("household-evening-12", scenario::household_evening_12),
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
        // The density control. A response can only be fresher than
        // gossip inside the window between it landing and the next
        // gossip round, so this mechanism's coverage is a function of
        // how often a decider talks to the *same* peer — a property of
        // the traffic, not of the code. `isolation` carries a
        // background actor dispatching every ~8s against a household's
        // ~4 min, which is the widest density contrast the scenario set
        // offers. If coverage does not move across this span, low
        // coverage is not a traffic artifact.
        ("isolation", scenario::isolation),
    ];
    let arms = [
        Arm::AsImplemented,
        Arm::ResponseBackpressure,
        Arm::FreshSignals,
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — §4.2 step 1: fresh where a response can reach ===");
        let mut cells: Vec<(f64, f64)> = Vec::new();
        for arm in arms {
            let (mut means, mut p95s, mut ages) = (Vec::new(), Vec::new(), Vec::new());
            let (mut with_signal, mut from_response, mut offs) = (0u64, 0u64, 0usize);
            for seed in seeds {
                let s = build(seed);
                let report = run(&s, arm, seed);
                with_signal += report.backpressure.dispatches_with_signal;
                from_response += report.backpressure.dispatches_from_response;
                offs += offloads(&report);
                let sc = score(&report, GOSSIP_WINDOW_MS, None);
                means.push(sc.truth.mean_total_ms / 1000.0);
                p95s.push(sc.records.p95_total_ms / 1000.0);
                ages.push(sc.truth.median_true_signal_age_ms / 1000.0);
            }
            let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
            let (m, p95) = (mean(&means), mean(&p95s));
            cells.push((m, p95));
            println!(
                "  {:<24} mean {:>5.1}s  p95 {:>5.1}s  median signal age {:>5.1}s  \
                 {:>3.0}% of {} peer-dispatches decided on a response",
                arm.label(),
                m,
                p95,
                mean(&ages),
                100.0 * from_response as f64 / with_signal.max(1) as f64,
                with_signal,
            );

            // The wiring checks. Both directions, because "the arm did
            // nothing" and "the arm is not connected" print the same
            // latency row and mean opposite things.
            match arm {
                Arm::AsImplemented | Arm::FreshSignals => assert_eq!(
                    from_response,
                    0,
                    "{label}/{}: a non-backpressure arm consumed a response-carried \
                     signal — the arm predicate leaks",
                    arm.label()
                ),
                Arm::ResponseBackpressure => {
                    assert!(
                        offs > 0,
                        "{label}: no offloads, so this fleet cannot test §4.2 step 1"
                    );
                    assert!(
                        from_response > 0,
                        "{label}: the backpressure arm never consumed a response-carried \
                         signal despite {offs} offloads — the mechanism is not wired"
                    );
                }
                _ => unreachable!("arms list is closed"),
            }
        }

        let (base_m, base_p95) = cells[0];
        let (bp_m, bp_p95) = cells[1];
        let (fresh_m, fresh_p95) = cells[2];
        // Recovery: how much of the oracle-freshness win the mechanism
        // collects. `None` when fresh-signals did not win by enough to
        // divide by — a **relative** floor, not an absolute one,
        // because a saturated fleet's 86s baseline turns a 1.5s wobble
        // into "recovers 305%". That is a denominator artifact, and
        // §6 has collected enough of those.
        let recovery = |base: f64, bp: f64, fresh: f64| {
            let gap = base - fresh;
            (gap.abs() >= 0.02 * base.abs()).then(|| 100.0 * (base - bp) / gap)
        };
        let show = |r: Option<f64>| match r {
            Some(v) => format!("{v:.0}%"),
            None => "n/a (fresh signals bought <2% here — no gap to recover)".to_string(),
        };
        println!(
            "  → mean: {:+.1}% vs arm 0 (fresh-signals {:+.1}%) — recovers {}",
            100.0 * (bp_m - base_m) / base_m.max(0.001),
            100.0 * (fresh_m - base_m) / base_m.max(0.001),
            show(recovery(base_m, bp_m, fresh_m)),
        );
        println!(
            "  → p95:  {:+.1}% vs arm 0 (fresh-signals {:+.1}%) — recovers {}",
            100.0 * (bp_p95 - base_p95) / base_p95.max(0.001),
            100.0 * (fresh_p95 - base_p95) / base_p95.max(0.001),
            show(recovery(base_p95, bp_p95, fresh_p95)),
        );
    }
}

/// **Is §4.2 step 1 a prerequisite for §4.1, or an independent
/// improvement?**
///
/// §4.2 asserts the first: the tier floor's relaxation rule and the
/// within-noise band both want an *observed* rate rather than an
/// advertised one, and §4.1.3 closed on exactly that sentence. But
/// there is a sharper reason to expect it, and it is measurable here.
///
/// The two objectives consume `in_flight` differently. The product
/// passes it through `load_penalty`, a **bounded** multiplier — a
/// stale count moves the score a little. Predicted time **multiplies
/// it by a service time**, so a stale count is a first-order error
/// that scales with the queue. The same asymmetry
/// `predicted-time+outbound-only` exists to price for load
/// *attribution* should appear here for load *staleness*.
///
/// So: run the mechanism under both objectives and compare the two
/// deltas. If freshness is worth materially more to predicted time
/// than to the product, §4.2's ordering is measured rather than
/// argued, and the two changes should land together. If the deltas
/// match, they are independent and can be sequenced by cost.
///
/// Reported, not asserted, apart from the tier floor's own invariant —
/// a freshness change must not become a quality change by relaxing the
/// floor through the back door.
#[test]
fn is_fresh_backpressure_worth_more_to_predicted_time_than_to_the_product() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];
    // (objective label, without the mechanism, with it)
    let pairs = [
        (
            "product (arm 0)",
            Arm::AsImplemented,
            Arm::ResponseBackpressure,
        ),
        (
            "predicted-time+floor",
            Arm::PredictedTimeTierFloor,
            Arm::PredictedTimeTierFloorBackpressure,
        ),
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — what is a fresh load count worth, per objective? ===");
        for (objective, without, with) in pairs {
            let cell = |arm: Arm| {
                let (mut means, mut p95s) = (Vec::new(), Vec::new());
                for seed in seeds {
                    let s = build(seed);
                    let report = run(&s, arm, seed);
                    let sc = score(&report, GOSSIP_WINDOW_MS, None);
                    // Only the floor arms owe the floor's invariant.
                    // Arm 0 declines upgrades by design — it has no
                    // floor to escape, and asserting on it would be
                    // asserting that the baseline is the treatment.
                    if matches!(
                        arm,
                        Arm::PredictedTimeTierFloor | Arm::PredictedTimeTierFloorBackpressure
                    ) {
                        assert_eq!(
                            (sc.tier.downgrades, sc.tier.declined_upgrades),
                            (0, 0),
                            "{label}/{seed}/{}: a freshness arm escaped the tier floor",
                            arm.label()
                        );
                    }
                    means.push(sc.truth.mean_total_ms / 1000.0);
                    p95s.push(sc.records.p95_total_ms / 1000.0);
                }
                let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
                (mean(&means), mean(&p95s))
            };
            let (m0, p0) = cell(without);
            let (m1, p1) = cell(with);
            println!(
                "  {:<22} mean {:>5.1}s → {:>5.1}s ({:+.1}%)   p95 {:>5.1}s → {:>5.1}s ({:+.1}%)",
                objective,
                m0,
                m1,
                100.0 * (m1 - m0) / m0.max(0.001),
                p0,
                p1,
                100.0 * (p1 - p0) / p0.max(0.001),
            );
        }
    }
}

/// **F9, priced — and the two halves point in opposite directions.**
///
/// The finding is that the scorer reads a local-load counter nothing
/// writes: `record_dispatch(None)` has zero callers, so
/// `load_penalty` is permanently 1.0 for the local candidate. The
/// same is true of peers on the ranked path — the sole
/// `record_dispatch(Some(..))` call site is the non-streaming *named*
/// arm — so peer `samples` never leaves 0 either.
///
/// The awkward part is that arm 0 does **not** model any of this: it
/// hands `rank` an exact local queue depth and lets peer samples
/// accumulate. Arm 0 is therefore the as-*designed* system, and every
/// number recorded against it is a comparison against a mesh that
/// does not exist. [`Arm::BlindObservations`] is the as-*shipped*
/// one, and the arms in between exist to say which half of the
/// blindness carries the difference.
///
/// The reason to split rather than report one total: the two halves
/// bias in opposite directions. A blind local slot never looks busy,
/// so the origin keeps work it should send away. A permanently cold
/// peer never looks trustworthy, so the origin sends away less than
/// it otherwise would. Reporting only the sum would net two real
/// effects into one small number.
///
/// The wiring checks are asserted because a null result depends on
/// them, and one directional claim is asserted because it is a
/// property of the arithmetic rather than of these fleets:
/// `load_penalty` is monotonically decreasing in `in_flight`, so
/// zeroing the local count can only raise the local score, and can
/// therefore only reduce offload. A run where blinding the local
/// count *increased* offload would mean the arm is wired backwards.
#[test]
fn what_the_scorer_loses_by_never_seeing_its_own_load() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let arms = [
        Arm::AsImplemented,
        Arm::BlindLocalLoad,
        Arm::BlindPeerRamp,
        Arm::BlindObservations,
    ];

    // ---- wiring, on one scenario, before any table is believed ----
    let s = scenario::isolation(SEED);
    let local_loads = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Local)
            .map(|c| c.score.load_penalty)
            .collect()
    };
    let peer_samples = |r: &RunReport| -> Vec<u32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Peer)
            .map(|c| c.inputs.samples)
            .collect()
    };
    let arm0 = run(&s, Arm::AsImplemented, SEED);
    let blind_local = run(&s, Arm::BlindLocalLoad, SEED);
    let blind_ramp = run(&s, Arm::BlindPeerRamp, SEED);

    assert!(
        local_loads(&arm0).iter().any(|p| *p < 0.999),
        "arm 0 never penalised the local slot for its own load — \
         nothing for the blind arm to take away, so the table below is unreadable"
    );
    assert!(
        local_loads(&blind_local)
            .iter()
            .all(|p| (*p - 1.0).abs() < 1e-6),
        "blind-local-load left a local load penalty in place — the arm is not wired"
    );
    assert!(
        peer_samples(&arm0).iter().any(|n| *n > 0),
        "arm 0 never accumulated a peer sample — nothing for blind-peer-ramp to freeze"
    );
    assert!(
        peer_samples(&blind_ramp).iter().all(|n| *n == 0),
        "blind-peer-ramp let a peer accumulate samples — the arm is not wired"
    );

    // ---- the table ----
    println!("\n=== F9 — what each half of the observation blindness costs ===");
    println!(
        "  {:<20} {:>9} {:>9} {:>9}   (mean over {} seeds)",
        "arm",
        "mean",
        "p95",
        "offloads",
        seeds.len()
    );
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::isolation(SEED),
    ] {
        println!("── {} ──", sc.name);
        let mut baseline = (0.0f64, 0.0f64);
        for (i, arm) in arms.iter().enumerate() {
            let (mut means, mut p95s, mut offs) = (0.0f64, 0.0f64, 0.0f64);
            for seed in seeds {
                let r = run(&sc, *arm, seed);
                let scored = score(&r, GOSSIP_WINDOW_MS, None);
                means += mean_total_ms(&r) / 1000.0;
                p95s += scored.records.p95_total_ms / 1000.0;
                offs += offloads(&r) as f64;
            }
            let n = seeds.len() as f64;
            let (m, p, o) = (means / n, p95s / n, offs / n);
            if i == 0 {
                baseline = (m, p);
                println!("  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}", arm.label());
            } else {
                println!(
                    "  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}   mean {:+.0}%  p95 {:+.0}%",
                    arm.label(),
                    100.0 * (m - baseline.0) / baseline.0.max(0.001),
                    100.0 * (p - baseline.1) / baseline.1.max(0.001),
                );
            }
        }
    }

    // ---- the one directional claim that is arithmetic, not fleet ----
    for sc in [scenario::isolation(SEED), scenario::twin_hubs(SEED)] {
        for seed in seeds {
            let wired = offloads(&run(&sc, Arm::BlindPeerRamp, seed));
            let blind = offloads(&run(&sc, Arm::BlindObservations, seed));
            assert!(
                blind <= wired,
                "{} seed {seed}: blinding the local load INCREASED offload ({wired} → {blind}). \
                 `load_penalty` is monotonically decreasing in `in_flight`, so zeroing the local \
                 count can only raise the local score — the arm must be wired backwards",
                sc.name
            );
        }
    }
    let _ = (blind_local, blind_ramp);
}

/// F10's second half: no node on this mesh has ever advertised a
/// `BenchmarkResult`, so `throughput_factor` has neither of its two
/// sources and returns neutral 1.0 for every candidate on every fleet.
///
/// The test asserts the *structural* claim — that the term is a
/// constant in production, not merely a weak signal — and prints the
/// latency table for the arm that models tonight's mesh. The structural
/// claim is the durable one: it is arithmetic from `scoring.rs:362`'s
/// `(None, None)` branch, and it holds on any fleet, whereas the table
/// is fleet-specific by construction.
#[test]
fn what_the_scorer_loses_by_never_measuring_anyone() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let arms = [
        Arm::AsImplemented,
        Arm::BlindPeerRamp,
        Arm::BlindRateCard,
        Arm::BlindShipped,
    ];

    // ---- wiring, before any table is believed ----
    let s = scenario::mixed_hubs(SEED);
    let sources = |r: &RunReport| -> Vec<String> {
        candidates(r)
            .iter()
            .map(|c| c.score.throughput_source.clone())
            .collect()
    };
    let factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .map(|c| c.score.throughput_factor)
            .collect()
    };
    let peer_factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Peer)
            .map(|c| c.score.throughput_factor)
            .collect()
    };
    let local_factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Local)
            .map(|c| c.score.throughput_factor)
            .collect()
    };

    let arm0 = run(&s, Arm::AsImplemented, SEED);
    assert!(
        sources(&arm0).iter().any(|s| s == "benchmark_estimate"),
        "arm 0 never consulted a rate card on `mixed-hubs` — a fleet built out of \
         speed variance. Nothing for the blind arm to take away, so the table is unreadable"
    );
    assert!(
        factors(&arm0).iter().any(|f| *f < 0.999),
        "arm 0 never discriminated on throughput — F3's term is already inert in the \
         baseline, which would make this whole comparison vacuous"
    );

    // The finding, stated as an assertion rather than as prose — and it
    // is an *asymmetry*, not a uniform blindness. The first draft of
    // this test asserted every candidate scored neutral and failed on
    // the local one, which is the more interesting answer:
    //
    //   * Every **peer** is neutral 1.0. Both of `throughput_factor`'s
    //     sources are shut for peers — no rate card (F10) and no
    //     samples (F9's peer half) — so it takes the `(None, None)`
    //     branch every time.
    //   * The **local** node is scored on its observed decode rate.
    //     Production seeds local `samples` above the cold-start
    //     threshold at construction (`peer_inference.rs:559`) and keeps
    //     `tg_tok_s_ewma` current via `ThroughputTarget::Local`, so the
    //     observed gate opens for the local candidate and only for it.
    //
    // `throughput_factor` clamps to `[FLOOR, 1.0]`, so the local
    // candidate can only be scored *down* from the constant every peer
    // enjoys. F10's blindness therefore biases **toward offload** —
    // the opposite direction to F9's local half, which is why the two
    // must not be reported as one number.
    for sc in [
        scenario::mixed_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::twin_hubs(SEED),
    ] {
        let shipped = run(&sc, Arm::BlindShipped, SEED);
        assert!(
            peer_factors(&shipped)
                .iter()
                .all(|f| (*f - 1.0).abs() < 1e-6),
            "{}: a PEER was scored with a non-neutral throughput factor on the \
             as-shipped arm. Production gossips no rate card and never reaches the \
             observed-EWMA gate for a peer, so this term cannot be anything but 1.0 \
             — if it is, the arm is not wired",
            sc.name
        );
        assert!(
            !peer_factors(&shipped).is_empty(),
            "{}: no peer was ever scored, so the assertion above is vacuous",
            sc.name
        );
    }

    // The asymmetry itself, on the fleet built to expose it: `isolation`
    // sustains local contention, so the local EWMA is live and the
    // clamp has something to bite on.
    let shipped_iso = run(&scenario::isolation(SEED), Arm::BlindShipped, SEED);
    let locals = local_factors(&shipped_iso);
    let peers = peer_factors(&shipped_iso);
    assert!(
        peers.iter().all(|f| (*f - 1.0).abs() < 1e-6),
        "isolation: a peer escaped the neutral constant on the as-shipped arm"
    );
    println!(
        "\n  as-shipped throughput_factor — local min {:.3} / peers all {:.3}  \
         ({} local, {} peer scorings)",
        locals.iter().copied().fold(f32::INFINITY, f32::min),
        peers.first().copied().unwrap_or(f32::NAN),
        locals.len(),
        peers.len(),
    );

    // ---- the scope limit, pinned rather than argued ----
    // This is the caveat that decides whether the tables below license
    // a production landing, so it is an assertion and not a paragraph.
    //
    // `throughput_factor` does not read a rate card directly: it scales
    // `bench.tg_tok_s` by `baseline_size_gb / candidate_size_gb`
    // (`scoring.rs:384`) to extrapolate from the model that was
    // benchmarked to the model being scored. In this sim every node
    // advertises exactly one model and benchmarks *that* model
    // (`NodeSpec::benchmark`), so the ratio is 1.0 at every scoring and
    // the extrapolation never runs.
    //
    // Production would not have that property. `run_baseline_benchmark`
    // (deleted 2026-07-28 — see below) probed the **`Speed::Fast`
    // slot** — a ~4B model — while the
    // candidate being scored is whatever the peer advertises, often a
    // 35B. The ratio would be ~0.1 and the estimate an order of
    // magnitude below the measured rate. So a shipped probe activates a
    // linear-extrapolation heuristic that nothing in this suite
    // exercises, and the −32% below is NOT a prediction about it.
    for sc in [
        scenario::mixed_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::household_evening_12(SEED),
    ] {
        for n in &sc.nodes {
            if let Some(b) = n.benchmark() {
                assert!(
                    (b.baseline_size_gb - n.size_gb).abs() < 1e-6,
                    "{}/{}: this sim's rate card is measured on the node's own \
                     serving model, so `throughput_factor`'s size-ratio \
                     extrapolation is inert here. If that ever stops being true, \
                     the scope note above this assertion needs rewriting before \
                     the F10 tables are quoted at anyone.",
                    sc.name,
                    n.name
                );
            }
        }
    }

    // ---- the table ----
    println!("\n=== F10 — what the missing rate card costs ===");
    println!(
        "  {:<20} {:>9} {:>9} {:>9}   (mean over {} seeds)",
        "arm",
        "mean",
        "p95",
        "offloads",
        seeds.len()
    );
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::mixed_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        println!("── {} ──", sc.name);
        let mut baseline = (0.0f64, 0.0f64);
        for (i, arm) in arms.iter().enumerate() {
            let (mut means, mut p95s, mut offs) = (0.0f64, 0.0f64, 0.0f64);
            for seed in seeds {
                let r = run(&sc, *arm, seed);
                let scored = score(&r, GOSSIP_WINDOW_MS, None);
                means += mean_total_ms(&r) / 1000.0;
                p95s += scored.records.p95_total_ms / 1000.0;
                offs += offloads(&r) as f64;
            }
            let n = seeds.len() as f64;
            let (m, p, o) = (means / n, p95s / n, offs / n);
            if i == 0 {
                baseline = (m, p);
                println!("  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}", arm.label());
            } else {
                println!(
                    "  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}   mean {:+.0}%  p95 {:+.0}%",
                    arm.label(),
                    100.0 * (m - baseline.0) / baseline.0.max(0.001),
                    100.0 * (p - baseline.1) / baseline.1.max(0.001),
                );
            }
        }
    }

    // ---- the landing question, isolated ----
    // If the rate card were wired tomorrow, the mesh moves from
    // `blind-shipped` to `blind-peer-ramp` — the peer ramp stays frozen
    // either way, because §4.4 measured it protective. That pair, and
    // not anything against arm 0, is the delta an operator would feel.
    println!("\n=== F10 — the landing case: wiring the probe, peer ramp left alone ===");
    println!("  {:<26} {:>9} {:>9}", "fleet", "shipped", "+rate-card");
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::mixed_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            before += mean_total_ms(&run(&sc, Arm::BlindShipped, seed)) / 1000.0;
            after += mean_total_ms(&run(&sc, Arm::BlindPeerRamp, seed)) / 1000.0;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  {:<26} {b:>8.1}s {a:>8.1}s   {:+.0}%",
            sc.name,
            100.0 * (a - b) / b.max(0.001)
        );
    }

    // ---- and the flattery, priced ----
    // The sim builds each node's advertised rate card from the same
    // `Hardware` its service-time model consumes, so at
    // `advertised_rate_error: 0.0` a wired probe is *exact truth by
    // construction* — a real 10-second llama.cpp probe is not. The rows
    // above must not be read without this sweep (`SimConfig`'s own doc
    // comment, and note 963a8d88's method rule).
    //
    // `blind-shipped` is the control: it consults no rate card, so its
    // column must be flat across the sweep. If it moves, the harness is
    // perturbing something other than the thing under test.
    println!("\n=== F10 — does the win survive a mis-measured probe? (mixed-hubs) ===");
    println!(
        "  {:<12} {:>9} {:>11} {:>8}",
        "rate error", "shipped", "+rate-card", "Δ"
    );
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            // Scenario fixed at `SEED`, varying only the run seed —
            // the same convention as the two tables above and as F9's
            // table in §4.4, so the ±0% row is directly comparable to
            // the `mixed-hubs` row of the landing case. (§4.1.2's
            // sweeps rebuild the fleet per seed instead; the two
            // conventions give different absolute numbers and must not
            // be read across.)
            let sc = scenario::mixed_hubs(SEED);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            before += mean_total_ms(&run_with(&sc, Arm::BlindShipped, seed, cfg.clone())) / 1000.0;
            after += mean_total_ms(&run_with(&sc, Arm::BlindPeerRamp, seed, cfg)) / 1000.0;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  ±{:<11.0}% {b:>8.1}s {a:>10.1}s {:>7.0}%",
            err * 100.0,
            100.0 * (a - b) / b.max(0.001)
        );
    }

    // ---- and the mechanism production would actually ship ----
    // Everything above prices a rate card measured on the model each
    // node serves. `run_baseline_benchmark` measured the `Speed::Fast`
    // slot instead, so a shipped card would describe a ~2.5 GB model
    // and `throughput_factor` would extrapolate from it to whatever is
    // being scored, assuming rate scales as 1/size. This arm is why
    // that probe was deleted on 2026-07-28 rather than wired up: the
    // number it produced was aimed at a consumer that would misuse it.
    // `svrn mesh bench` measures the model actually being served and
    // reports to a human, not to this scorer.
    //
    // β = 1.0 is that assumption, and it must reproduce the rows above
    // exactly — asserted below rather than eyeballed, because the whole
    // reading of the sweep depends on the knob being an identity there.
    // Below 1.0 the probe over-states the hardware per GB and every
    // large candidate is extrapolated low; the clamp is one-sided, so
    // the error can only push candidates down.
    const FAST_SLOT_GB: f32 = 2.5;
    println!("\n=== F10 — the card a SHIPPED probe would advertise (mixed-hubs) ===");
    println!("   (2.5 GB Fast-slot probe extrapolated to each candidate; β=1 is the linear");
    println!("    assumption `throughput_factor` already makes, so it must be an identity)");
    println!("   Latency alone cannot read this table: §4.1.1 established that sending");
    println!("   knowledge turns to small fast models looks like a large latency win and");
    println!("   is a quality regression. `downgrades` is the column that tells them apart.");
    println!(
        "  {:<14} {:>9} {:>11} {:>8} {:>11} {:>9}",
        "β (size→rate)", "shipped", "+rate-card", "Δ", "downgrades", "declined"
    );

    let control = {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            let sc = scenario::mixed_hubs(SEED);
            before += mean_total_ms(&run(&sc, Arm::BlindShipped, seed)) / 1000.0;
            after += mean_total_ms(&run(&sc, Arm::BlindPeerRamp, seed)) / 1000.0;
        }
        let n = seeds.len() as f64;
        (before / n, after / n)
    };

    for beta in [1.0_f32, 0.9, 0.7, 0.5] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        let (mut down, mut declined) = (0usize, 0usize);
        for seed in seeds {
            let sc = scenario::mixed_hubs(SEED);
            let cfg = SimConfig {
                probe_baseline_size_gb: Some(FAST_SLOT_GB),
                probe_sublinearity: beta,
                ..SimConfig::default()
            };
            before += mean_total_ms(&run_with(&sc, Arm::BlindShipped, seed, cfg.clone())) / 1000.0;
            let wired = run_with(&sc, Arm::BlindPeerRamp, seed, cfg);
            after += mean_total_ms(&wired) / 1000.0;
            let scored = score(&wired, GOSSIP_WINDOW_MS, None);
            down += scored.tier.downgrades;
            declined += scored.tier.declined_upgrades;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  {beta:<14.1} {b:>8.1}s {a:>10.1}s {:>7.0}% {:>11.1} {:>9.1}",
            100.0 * (a - b) / b.max(0.001),
            down as f64 / n,
            declined as f64 / n,
        );
        if (beta - 1.0).abs() < 1e-6 {
            assert!(
                (a - control.1).abs() < 0.05 && (b - control.0).abs() < 0.05,
                "β=1 must reproduce the un-probed rate card exactly \
                 ({:.2}s/{:.2}s vs control {:.2}s/{:.2}s). `throughput_factor` scales \
                 linearly on the size ratio, so measuring a smaller model and scaling \
                 back up is an identity under a linear law — if it is not, this knob \
                 is perturbing something besides the extrapolation and no row in this \
                 table can be attributed to it",
                b,
                a,
                control.0,
                control.1
            );
        }
    }
}
