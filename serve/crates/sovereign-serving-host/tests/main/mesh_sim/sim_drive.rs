// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Sim`'s methods: the event loop: construction, the gossip tick and an arrival.
//! Split out of mod.rs when mesh_sim moved here (pb-mesh-dissolve).

use super::*;

impl Sim {
    pub(super) fn new(scenario: &Scenario, arm: Arm, seed: u64, cfg: SimConfig) -> Self {
        let n = scenario.nodes.len();
        // A THIRD stream, and deliberately not `world_rng`: drawing
        // rate-card errors from the world stream at construction time
        // would shift every later gossip draw, so an
        // `advertised_rate_error: 0.0` run would stop reproducing the
        // runs recorded before this knob existed.
        let mut rate_rng = Rng::new(seed ^ 0xC0DE_FACE_5A17_3E11);
        let rate_error = cfg.advertised_rate_error.max(0.0);
        // F10: applied before `rate_error` so the perturbation lands on
        // the card a shipped probe would have produced, not on the
        // idealised one.
        let probe_baseline_gb = cfg.probe_baseline_size_gb;
        let probe_sublinearity = cfg.probe_sublinearity;
        // A FOURTH stream, for the same reason the third exists: sharing
        // `rate_rng` would make an `advertised_size_error` run shift the
        // rate-card draws, and the two knobs must be independently
        // attributable.
        let mut size_rng = Rng::new(seed ^ 0x51_2E_FA_11_AC_1D_99_07);
        let size_error = cfg.advertised_size_error.max(0.0);
        let load_per_gb = cfg.model_load_sec_per_gb.max(0.0);
        let nodes = scenario
            .nodes
            .iter()
            .enumerate()
            .map(|(i, spec)| SimNode {
                name: spec.name.clone(),
                // Deterministic stand-in for a real 16-byte node id.
                node_id_hex: format!("{:032x}", i as u128 + 1),
                // A cold node ADVERTISES that it is cold, and how long
                // it would take — which is the only reason a decider
                // can price the load at all.
                manifest: {
                    let mut m = spec.manifest();
                    if load_per_gb > 0.0 {
                        if let Some(model) = m.models.first_mut() {
                            model.status.loaded = false;
                            model.status.estimated_load_time_sec =
                                Some((spec.size_gb as f64 * load_per_gb).round() as u32);
                        }
                    }
                    // What the node CLAIMS it weighs. Only the tier
                    // floor consumes this, so any behaviour change under
                    // the knob is the floor mis-banding somebody — the
                    // whole point of keeping it out of the score.
                    if size_error > 0.0 {
                        if let Some(model) = m.models.first_mut() {
                            let u = size_rng.next_f64() as f32;
                            let factor = (1.0 + size_error).powf(2.0 * u - 1.0);
                            model.size_gb = model.size_gb.map(|s| s * factor);
                        }
                    }
                    m
                },
                load_ms: (spec.size_gb as f64 * load_per_gb * 1000.0) as u64,
                resident: load_per_gb <= 0.0,
                // What the node CLAIMS it can do, which is only the
                // truth when `advertised_rate_error` is zero. Two-sided
                // and multiplicative-symmetric: the factor lands in
                // `[1/(1+e), 1+e]`, so a node is as likely to
                // under-sell itself as to over-sell.
                benchmark: spec.benchmark().map(|mut b| {
                    // F10: what a *shipped* probe would have measured.
                    // The daemon benchmarks its `Speed::Fast` slot, not
                    // the model it serves knowledge turns from, so the
                    // advertised card describes a smaller model on the
                    // same hardware — and `throughput_factor` has to
                    // extrapolate back up. `rate ∝ size^-β` gives that
                    // smaller model its rate; β = 1 reproduces the
                    // linear assumption the extrapolation makes, so the
                    // whole knob is an identity there and any deviation
                    // below is the extrapolation error alone.
                    if let Some(probe_gb) = probe_baseline_gb {
                        if probe_gb > 0.0 && b.baseline_size_gb > 0.0 {
                            let shrink = b.baseline_size_gb / probe_gb;
                            let speedup = shrink.powf(probe_sublinearity);
                            b.pp_tok_s *= speedup;
                            b.tg_tok_s *= speedup;
                            b.baseline_size_gb = probe_gb;
                            b.baseline_model_id = format!("{}-fast-slot", b.baseline_model_id);
                        }
                    }
                    if rate_error > 0.0 {
                        let u = rate_rng.next_f64() as f32;
                        let factor = (1.0 + rate_error).powf(2.0 * u - 1.0);
                        b.pp_tok_s *= factor;
                        b.tg_tok_s *= factor;
                    }
                    b
                }),
                availability: spec.availability,
                pp_tok_s: spec.hardware.pp_tok_s,
                tg_tok_s: spec.hardware.tg_tok_s,
                running: Vec::new(),
                queue: VecDeque::new(),
                // Production seeds local samples above the cold-start
                // threshold: a node always knows itself.
                local_obs: NodeObservations {
                    samples: oicp_types::COLD_START_SAMPLES * 2,
                    ..Default::default()
                },
                // F7 arm: a warm-started decider begins already
                // believing it has finished the cold-start ramp for
                // every peer, so `cold_start_weight` is 1.0 from the
                // first decision instead of 0.7.
                peer_obs: vec![
                    NodeObservations {
                        samples: if arm.warm_start() {
                            oicp_types::COLD_START_SAMPLES
                        } else {
                            0
                        },
                        ..Default::default()
                    };
                    n
                ],
                peer_health: PeerHealthTracker::new(),
                manifest_fetched_ms: HashMap::new(),
                gossip: HashMap::new(),
                backpressure: HashMap::new(),
            })
            .collect();
        Self {
            cfg,
            arm,
            nodes,
            rtt_ms: scenario.rtt_ms.clone(),
            events: BinaryHeap::new(),
            now_ms: 0,
            seq: 0,
            world_rng: Rng::new(seed ^ 0xA5A5_5A5A_C3C3_3C3C),
            policy_rng: Rng::new(seed ^ 0x1357_9BDF_2468_ACE0),
            sampler: SamplerTrace::default(),
            backpressure: BackpressureTrace::default(),
            records: Vec::new(),
            truth: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, at_ms: u64, kind: EventKind) {
        self.seq += 1;
        let seq = self.seq;
        self.events.push(Scheduled { at_ms, seq, kind });
    }

    pub(super) fn seed_events(&mut self, scenario: &Scenario) {
        for (idx, arrival) in scenario.arrivals.iter().enumerate() {
            self.push(arrival.at_ms, EventKind::Arrival(idx));
        }
        let mut t = self.cfg.gossip_interval_ms;
        while t <= scenario.duration_ms {
            self.push(t, EventKind::GossipTick);
            t += self.cfg.gossip_interval_ms;
        }
    }

    pub(super) fn drive(&mut self, scenario: &Scenario) {
        while let Some(ev) = self.events.pop() {
            self.now_ms = ev.at_ms;
            match ev.kind {
                EventKind::Arrival(idx) => self.on_arrival(&scenario.arrivals[idx]),
                EventKind::ServiceDone { node, job_seq } => self.on_service_done(node, job_seq),
                EventKind::GossipTick => self.on_gossip_tick(),
                EventKind::GossipDeliver {
                    from,
                    to,
                    in_flight,
                    availability_milli,
                    measured_at_ms,
                } => {
                    let received_at_ms = self.now_ms;
                    self.nodes[to].gossip.insert(
                        from,
                        Belief {
                            in_flight,
                            availability: availability_milli.map(|m| m as f32 / 1000.0),
                            received_at_ms,
                            measured_at_ms,
                        },
                    );
                }
            }
        }
    }

    // ── gossip ──────────────────────────────────────────────────

    pub(super) fn on_gossip_tick(&mut self) {
        let n = self.nodes.len();
        let now = self.now_ms;
        let policy = self.arm.published_load();
        for from in 0..n {
            let in_flight = self.nodes[from].published_in_flight(from, policy);
            let availability_milli = self.nodes[from].availability.map(|a| (a * 1000.0) as u32);
            for to in 0..n {
                if to == from {
                    continue;
                }
                // Anti-entropy: a value reaches a given peer after
                // between zero and `gossip_max_extra_rounds` further
                // rounds, plus the wire.
                let extra = self.world_rng.next_u64() % (self.cfg.gossip_max_extra_rounds + 1);
                let delay = extra * self.cfg.gossip_interval_ms + self.rtt_ms[from][to] as u64;
                self.push(
                    now + delay,
                    EventKind::GossipDeliver {
                        from,
                        to,
                        in_flight,
                        availability_milli,
                        measured_at_ms: now,
                    },
                );
            }
        }
    }

    // ── arrivals and decisions ──────────────────────────────────

    pub(super) fn on_arrival(&mut self, arrival: &Arrival) {
        let origin = arrival.origin;
        // The OICP envelope carries the request's size in production —
        // `InferenceRouter::request_facts` reads both counts
        // straight off it — and both are hard feasibility gates in the
        // scorer (`scoring.rs:590`). Setting them here is a fidelity
        // fix that stands on its own: without it the gates never bind
        // in the sim, and §4.1 has no job size to predict.
        //
        // It cannot move arm 0, and that is checked rather than
        // asserted in prose: the gates only ever *exclude*, every
        // simulated claim advertises 32768/4000, and no arrival exceeds
        // either — see `no_arrival_is_gated_out_by_its_own_size`.
        let req = arrival
            .class
            .requirements()
            .with_context_tokens(arrival.context_tokens)
            .with_max_output_tokens(arrival.output_tokens);
        let facts = request_facts(&req, arrival);
        self.seq += 1;
        let oicp_request_id = format!("sim-{origin}-{}", self.seq);
        // The id is a deterministic sim-local join key, never the host's
        // random mint: the simulator must not reach `sovereign-serving-host`
        // (`[[forbid]] sovereign-mesh-test-harness -> sovereign-*` has no
        // except for it — domains dm-mesh-sim-move, ralph/DECISIONS.md
        // 2026-09-17) and a random id would break the module's stated
        // determinism. `scoreboard::origin_of` reads `oicp_request_id`, not
        // this id, so its shape is free.
        let rec = DecisionBuilder::new(
            format!("d-{oicp_request_id}"),
            &oicp_request_id,
            DecisionPath::RankedOicp,
            facts,
        );

        // The production gate, called directly — which is what makes
        // "LocalOnly never crossed the wire" a property of the code
        // under test rather than of the harness.
        if !offload_eligible(&req) {
            let decision = rec.finish_at(
                Verdict::Gated {
                    gate: "not_offload_eligible".into(),
                },
                &[],
                self.now_ms,
            );
            let decision_id = decision.decision_id.clone();
            self.records
                .push(DecisionEvent::Decision(Box::new(decision)));
            self.dispatch(
                origin,
                origin,
                arrival,
                decision_id,
                oicp_request_id,
                DispatchFacts::default(),
            );
            return;
        }

        if self.arm == Arm::Oracle {
            let server = self.oracle_pick(arrival);
            let ranked = if server == origin {
                Vec::new()
            } else {
                vec![self.nodes[server].name.clone()]
            };
            let verdict = if ranked.is_empty() {
                Verdict::StayLocal
            } else {
                Verdict::Peers {
                    ranked: ranked.clone(),
                }
            };
            let decision = rec.finish_at(verdict, &ranked, self.now_ms);
            let decision_id = decision.decision_id.clone();
            self.records
                .push(DecisionEvent::Decision(Box::new(decision)));
            self.dispatch(
                origin,
                server,
                arrival,
                decision_id,
                oicp_request_id,
                DispatchFacts::default(),
            );
            return;
        }

        let views = self.build_peer_views(origin);
        let local_obs = self.local_view_observations(origin);
        let result = {
            let node = &self.nodes[origin];
            scheduler_core::rank(
                rec,
                RankInputs {
                    now_unix: self.now_ms / 1000,
                    oicp_request_id: &oicp_request_id,
                    req: &req,
                    needs_forced_choice: false,
                    objective: self.arm.objective(),
                    tier_floor: self.arm.tier_floor(&req),
                    local: LocalCandidateView {
                        manifest: &node.manifest,
                        observations: &local_obs,
                        // F10: `None` on the blind arms, and `None` in
                        // production unconditionally — the local
                        // benchmark field and its setter were deleted
                        // on 2026-07-28, so production passes a literal
                        // `None` here with no state behind it.
                        benchmark: if self.arm.blind_rate_card() {
                            None
                        } else {
                            node.benchmark.as_ref()
                        },
                    },
                    peers: &views,
                },
            )
        };

        // Which of the ranked candidates the decider dispatches to.
        // Arm 0 takes the argmax — that determinism is F5.
        let chosen_view = if result.ranked.is_empty() {
            None
        } else if self.arm.within_noise_sampling() {
            Some(self.sample_within_noise(&result))
        } else if self.arm.two_choices() && result.ranked.len() > 1 {
            let a = self.policy_rng.below(result.ranked.len());
            let b = self.policy_rng.below(result.ranked.len());
            // `ranked` is best-first, so the better of two samples is
            // the one with the smaller index.
            Some(result.ranked[a.min(b)].view_idx)
        } else {
            Some(result.ranked[0].view_idx)
        };

        let decision_id = result.decision.decision_id.clone();
        let eligible_peers = result.ranked.len();
        self.records
            .push(DecisionEvent::Decision(Box::new(result.decision)));

        let (server, ages) = match chosen_view {
            Some(view_idx) => {
                let peer = views_index_to_node(origin, view_idx);
                let ages = self.signal_ages(origin, peer);
                if ages.2 != LoadSource::None {
                    self.backpressure.dispatches_with_signal += 1;
                    if ages.2 == LoadSource::Response {
                        self.backpressure.dispatches_from_response += 1;
                    }
                }
                (peer, ages)
            }
            None => (origin, (None, None, LoadSource::None)),
        };
        self.dispatch(
            origin,
            server,
            arrival,
            decision_id,
            oicp_request_id,
            DispatchFacts {
                true_signal_age_ms: ages.0,
                recorded_signal_age_ms: ages.1,
                eligible_peers,
            },
        );
    }
}
