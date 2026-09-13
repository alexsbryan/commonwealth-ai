# Tier-1 mesh-sim: four rules that keep its numbers admissible. (SCHEDULER_QUALITY.md §5/§6; landed 2026-07-26 in…

Tier-1 mesh-sim: four rules that keep its numbers admissible. (SCHEDULER_QUALITY.md §5/§6; landed 2026-07-26 in `sovereign-mesh/src/mesh_sim/`, feature `mesh-sim`.)

1. **Arm 0 must call `scheduler_core::rank`, never a copy of it.** The whole claim of S0 is "F1/F3/F5 reproduce against the REAL scorer". The moment the sim transcribes the arithmetic again it is worth exactly what §3's hand-model was worth — which measured the median cost of staleness at −29% when the real answer is −2%. If a change to `select_peers_ranked` cannot be expressed as a change to `rank` plus a change to the gather half, the split is wrong; fix the split, don't fork the logic.

2. World randomness and policy randomness need separate RNG streams. `Sim` holds `world_rng` (gossip propagation) and `policy_rng` (two-choices sampling). Sharing one stream means switching arms perturbs the gossip schedule, so the arms are no longer compared on the same world and every delta is confounded. There is a test for this (`an_arm_changes_where_work_runs_never_what_work_arrived`).

3. Metrics that a production capture cannot also compute do not get to define a calibration gate. `scoreboard::RecordMetrics` is computed from `&[DecisionEvent]` alone — the same vocabulary `SchedulerTrace` loads from a JSONL capture. `TruthMetrics` (counterfactual local cost, true signal age, eligible-set size) needs simulator ground truth. S1's decision-agreement gate must be built on the first type only; putting a TruthMetric in the gate makes the sim uncalibratable against hardware.

4. Scoreboard metrics need their denominator interrogated before they are believed. Three of mine were wrong on the first run and all three flattered the system: herding CoV over CHOSEN targets scores maximal herding as 0.00 (a one-element vector has no variance) — the denominator must be the ELIGIBLE set; pooling local service into one `<local>` bucket makes twelve nodes working locally read as more concentrated than one hub taking everything; and `{:>6.2}` applied to a `&str` is a TRUNCATION in Rust's formatter, which rendered "3.11" as "3.". When a metric comes out suspiciously clean, print the raw inputs (the decision records carry the full ScoreBreakdown — that is what Phase 0 was for) before believing it.

Reproduce the whole board: `cargo test -p sovereign-mesh --features mesh-sim,treesitter --test mesh_sim_scoreboard -- --nocapture --test-threads=1` (~0.3s).
