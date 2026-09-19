#![cfg(feature = "dst")]
//! The ring-room fleet in the Tier-1 simulator, and the two directions any
//! change to its deciding term must be measured in (ralph A35).
//!
//! Split out of `mesh_sim_scoreboard.rs`, which is at its arch-gate ceiling;
//! the sweep helpers there are `pub(crate)` so there is ONE implementation of
//! "run every arm and score it", not two.

use sovereign_mesh_test_harness::mesh_sim::scenario;
use sovereign_mesh_test_harness::mesh_sim::scoreboard::render;
use sovereign_mesh_test_harness::mesh_sim::{Arm, RunReport};

use super::mesh_sim_scoreboard::{assert_hard_invariants, print_candidate_breakdown, sweep, SEED};

// ---------------------------------------------------------------
// The ring-room fleet, before any ranking change (ralph A35)
// ---------------------------------------------------------------

/// **The measurement the ranking question has to be asked against.**
///
/// `SCHEDULER_QUALITY.md` §6: behavioural routing work goes INTO the sim as an
/// arm before it goes into production. The room showed a CPU asker sending
/// three of five syntheses to a GPU peer and keeping two — plus a Fast-class
/// classify whose gate now opens and which still ranked local for 38.2 s. This
/// reproduces that fleet with every rate measured on the host
/// (`scenario::ring_room_gpu_keeper`) and prints what the scorer saw, so the
/// term that decides is read rather than guessed.
///
/// Not an assertion of a target. It asserts only the things that would make
/// the fixture a lie — that the fleet is blind (no rate card, as the room is)
/// and flat in capability (one model, one band, so nothing but speed differs)
/// — and prints the rest. Run it with:
///
/// ```text
/// ./scripts/sovereign-test.sh --human --package sovereign-mesh \
///   --filter the_ring_room_fleet_before_any_ranking_change -- --nocapture
/// ```
#[test]
fn the_ring_room_fleet_before_any_ranking_change() {
    let sc = scenario::ring_room_gpu_keeper(SEED);

    // Fixture fidelity, asserted rather than trusted (principle 7: validate
    // the instrument before the result).
    assert!(
        sc.nodes.iter().all(|n| !n.advertises_benchmark),
        "the room advertises no BenchmarkResult — a fixture that did would hand \
         the scorer the one signal it does not have"
    );
    assert!(
        sc.nodes.iter().all(|n| n.availability.is_none()),
        "nothing in the room gossips an availability"
    );
    let sizes: Vec<f32> = sc.nodes.iter().map(|n| n.size_gb).collect();
    assert!(
        sizes.windows(2).all(|w| w[0] == w[1]),
        "one model, one band: capability must be flat so only speed differs, got {sizes:?}"
    );

    let (reports, scores) = sweep(&sc);
    // Written to a file as well as printed: nextest captures stdout on a
    // PASSING test, and a measurement nobody can read is not a measurement.
    // The artifact is what A35 quotes.
    let mut out = String::new();
    out.push_str(&render(&sc.name, SEED, &scores));
    println!("\n{out}");

    let shipped = scores
        .iter()
        .find(|s| s.arm == Arm::AsImplemented)
        .expect("the as-implemented arm is always run");
    let t = &shipped.truth;
    println!("── ring-room, as-implemented ──");
    println!("  mean_total_ms        {:.0}", t.mean_total_ms);
    println!("  offloads             {}", t.offloads);
    println!("  slower_than_local    {}", t.slower_than_local);
    println!("  wasted_offloads      {}", t.wasted_offloads);
    println!("  mean_eligible_peers  {:.2}", t.mean_eligible_peers);
    println!("  declined_upgrades    {}", shipped.tier.declined_upgrades);
    println!("  downgrades           {}", shipped.tier.downgrades);
    println!("  unbanded_decisions   {}", shipped.tier.unbanded_decisions);
    println!("  served_by            {:?}", shipped.records.served_by);

    let report = reports
        .iter()
        .find(|r| r.arm == Arm::AsImplemented)
        .expect("as-implemented ran");
    print_candidate_breakdown(report, 6);

    out.push_str(&format!(
        "\n── ring-room, as-implemented ──\n  mean_total_ms        {:.0}\n  \
         offloads             {}\n  slower_than_local    {}\n  \
         wasted_offloads      {}\n  mean_eligible_peers  {:.2}\n  \
         declined_upgrades    {}\n  downgrades           {}\n  \
         unbanded_decisions   {}\n  served_by            {:?}\n",
        t.mean_total_ms,
        t.offloads,
        t.slower_than_local,
        t.wasted_offloads,
        t.mean_eligible_peers,
        shipped.tier.declined_upgrades,
        shipped.tier.downgrades,
        shipped.tier.unbanded_decisions,
        shipped.records.served_by,
    ));
    out.push_str(&candidate_breakdown_text(report, 6));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/ralph");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("ring-room-sim.txt"), &out);

    assert_hard_invariants(&reports, &scores);
}

/// [`print_candidate_breakdown`]'s text, for the artifact. One renderer, two
/// sinks — the printed and the written form cannot disagree.
fn candidate_breakdown_text(report: &RunReport, n: usize) -> String {
    use sovereign_mesh::decision_log::{deciding_term, DecisionEvent};
    let mut out = String::from("\n── what the scorer saw ──\n");
    let mut shown = 0;
    for ev in &report.records {
        let DecisionEvent::Decision(d) = ev else {
            continue;
        };
        if d.candidates.len() < 2 || shown >= n {
            continue;
        }
        shown += 1;
        out.push_str(&format!(
            "  decision {} ({:?})\n",
            d.oicp_request_id, d.verdict
        ));
        for c in &d.candidates {
            let s = &c.score;
            out.push_str(&format!(
                "    {:<14} final {:>6.3} = claim {:>5.3} × obs {:>5.3} × load {:>5.3} \
                 × loc {:>5.3} × cold {:>5.3} × tput {:>5.3} ({}) × avail {:>5.3} \
                 [in_flight {} samples {}]\n",
                c.name,
                s.final_score,
                s.claim_score,
                s.observation_mult,
                s.load_penalty,
                s.locality_bonus,
                s.cold_start_weight,
                s.throughput_factor,
                s.throughput_source,
                s.availability,
                c.inputs.in_flight,
                c.inputs.samples,
            ));
        }
        // The stage-1 derivation, applied to the sim's own records.
        // Same rule as the production recorder: a `StayLocal` verdict marks no
        // candidate `selected`, and local is the one it chose.
        let winner = d.candidates.iter().find(|c| c.selected).or_else(|| {
            matches!(d.verdict, sovereign_mesh::decision_log::Verdict::StayLocal)
                .then(|| {
                    d.candidates
                        .iter()
                        .find(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Local)
                })
                .flatten()
        });
        if let Some(win) = winner {
            let rival = d
                .candidates
                .iter()
                .filter(|c| !std::ptr::eq(*c, win))
                .max_by(|a, b| {
                    a.score
                        .final_score
                        .partial_cmp(&b.score.final_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(rival) = rival {
                out.push_str(&format!(
                    "    -> {} chosen; decided_by {} vs {}\n",
                    win.name,
                    deciding_term(&win.score, &rival.score)
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "tie".into()),
                    rival.name,
                ));
            }
        }
    }
    out
}

/// **The other direction (principle 7).** `ring-room-gpu-keeper` says
/// `warm-start` — the arm that lifts `cold_start_weight`'s 0.7 floor — is a 5×
/// win on a fleet with one fast machine. A change reported only in the
/// direction it was meant to fix is not judged, so this prints the SAME arms
/// on `mixed-hubs`, the fleet `SCHEDULER_QUALITY.md` F7 measured the floor
/// protective on (+235% mean latency when lifted).
///
/// Prints rather than asserts a target: the point is to put both directions in
/// front of a reader before anyone changes a term. The artifact is
/// `target/ralph/ring-room-sim-other-direction.txt`.
#[test]
fn the_other_direction_the_mixed_fleet_must_not_regress() {
    let sc = scenario::mixed_hubs(SEED);
    let (_reports, scores) = sweep(&sc);
    let mut out = render(&sc.name, SEED, &scores);

    out.push_str("\n── the two arms that matter, both fleets ──\n");
    for arm in [Arm::AsImplemented, Arm::WarmStart] {
        let s = scores.iter().find(|s| s.arm == arm).expect("arm ran");
        out.push_str(&format!(
            "  mixed-hubs {:<16} eff {:?}  mean_ms {:.0}  offloads {}  \
             wasted {}  slower {}  downgrades {}  declined_upgrades {}\n",
            arm.label(),
            s.efficiency_ratio.map(|r| (r * 100.0).round() / 100.0),
            s.truth.mean_total_ms,
            s.truth.offloads,
            s.truth.wasted_offloads,
            s.truth.slower_than_local,
            s.tier.downgrades,
            s.tier.declined_upgrades,
        ));
    }
    println!("\n{out}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/ralph");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("ring-room-sim-other-direction.txt"), &out);
}
