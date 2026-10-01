// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Sim`'s methods: what one decider sees and how a job is dispatched and served.
//! Split out of mod.rs when mesh_sim moved here (pb-mesh-dissolve).

use super::*;

impl Sim {
    /// §4.2 step 2, implemented as written: *sample two among
    /// candidates whose predictions are within noise, take the less
    /// loaded.*
    ///
    /// Two clauses separate this from the blunt
    /// [`Arm::PredictedTimeTierFloorTwoChoices`], and §4.1.2 says both
    /// are load-bearing:
    ///
    ///   - the draw is over the **tie band**, not the whole ranked
    ///     list, so a candidate the objective can genuinely distinguish
    ///     is never sampled away. On `mixed-hubs` this is what keeps a
    ///     turn the 34 tok/s hub was measurably better for from landing
    ///     on the 25 tok/s one.
    ///   - the winner of the two is the **less loaded**, not the
    ///     better-ranked. Inside the band, rank order *is* queue order
    ///     (the band is defined by the uncontended times not
    ///     separating), so taking the smaller index would be reading
    ///     the stale signal back out of the very set that was built by
    ///     setting it aside.
    ///
    /// Falls back to the argmax whenever the band holds one candidate
    /// or the objective produced no band at all — an arm that samples
    /// must still be a total policy when its prerequisite is absent.
    pub(super) fn sample_within_noise(&mut self, result: &RankResult) -> usize {
        // `None` means the product objective, which has no scale on
        // which two candidates are close; a band of 1 is the honest
        // reading, not a degenerate one.
        let band = result.tie_band.unwrap_or(1).min(result.ranked.len());
        self.sampler.decisions += 1;
        self.sampler.band_total += band as u64;
        if band < 2 {
            return result.ranked[0].view_idx;
        }
        self.sampler.band_at_least_two += 1;
        let a = self.policy_rng.below(band);
        let b = self.policy_rng.below(band);
        let (lo, hi) = (a.min(b), a.max(b));
        // Rank order goes in first, so an exact `queue_ms` tie keeps
        // the better-ranked candidate — invariant 5's rule one layer up.
        let picked = match (result.ranked[lo].predicted, result.ranked[hi].predicted) {
            (Some(p_lo), Some(p_hi)) if !predicted_time::prefer_less_loaded(&p_lo, &p_hi) => hi,
            // Includes the unreachable-in-practice case where a band
            // ≥2 exists without predictions: degrade to the
            // better-ranked draw rather than panic on a policy detail.
            _ => lo,
        };
        if picked != 0 {
            self.sampler.moved_off_argmax += 1;
        }
        result.ranked[picked].view_idx
    }

    /// The origin's view of itself.
    ///
    /// On arm 0 load is exact — a node knows its own queue with zero
    /// delay, and that asymmetry against [`VenueView`]'s three
    /// age fields is F1.
    ///
    /// On the F9 arms it is exact in the other direction: **zero**,
    /// always, which is what the shipped dispatch path hands the
    /// scorer. F1's asymmetry is real but it is not the one production
    /// has; production's decider is blind to *both* sides, and only
    /// systematically wrong about one of them.
    pub(super) fn local_view_observations(&self, origin: usize) -> NodeObservations {
        let node = &self.nodes[origin];
        if self.arm.blind_local_load() {
            // Exactly the struct `InferenceRouter` constructs at
            // `peer_inference.rs:559` and then never mutates on the
            // dispatch path: seeded above the cold-start threshold,
            // zero in flight, zero failures. The two throughput EWMAs
            // are carried through from `local_obs` because production
            // *does* keep those current, via `ThroughputTarget::Local`
            // on the streaming path — they are the `T` term in F9's
            // arithmetic and the only local signal that still moves.
            return NodeObservations {
                in_flight: 0,
                samples: oicp_types::COLD_START_SAMPLES * 2,
                recent_failure_rate: 0.0,
                p50_latency_ms: 0,
                p95_latency_ms: 0,
                ttft_ewma_ms: node.local_obs.ttft_ewma_ms,
                tg_tok_s_ewma: node.local_obs.tg_tok_s_ewma,
            };
        }
        NodeObservations {
            in_flight: node.in_flight(),
            ..node.local_obs.clone()
        }
    }

    /// What `origin` has learned about `peer` from actually dispatching
    /// to it — the history half of the peer view, as distinct from the
    /// gossiped load and the cached manifest.
    ///
    /// On the F9 arms this history does not exist. `record_dispatch` /
    /// `record_success` are called for a peer at exactly one site
    /// (`peer_inference.rs:2173`/`:2181`, the non-streaming **named**
    /// arm), so on the ranked path a peer's `samples` never leaves 0.
    /// Two consequences, and the second is the one that surprises:
    ///
    /// 1. `cold_start_weight(0)` pins the peer at 0.7 forever — the
    ///    permanent form of F7's ramp rather than the transient one.
    /// 2. `throughput_factor` gates its source-of-truth on
    ///    `samples >= THROUGHPUT_OBSERVATION_THRESHOLD`
    ///    (`scoring.rs:231`), so it returns neutral 1.0 *even though*
    ///    `tg_tok_s_ewma` is being kept current for peers by
    ///    `ThroughputTarget::Peer`. The mesh measures peer throughput
    ///    on every stream and then declines to read it.
    ///
    /// `in_flight` is zeroed under
    /// [`blind_peer_inflight`](Arm::blind_peer_inflight) for the same
    /// reason and with a narrower blast radius: gossip overrides it
    /// whenever a record exists (`scheduler_core.rs:512`), so it only
    /// bites for a peer never heard from — where it makes the peer look
    /// idle rather than loaded. That is a bias *toward* offload, the
    /// opposite direction to the rest of F9, which is exactly why it is
    /// a separate switch: [`BlindPeerRamp`](Arm::BlindPeerRamp) leaves
    /// it alone so the two directions can be told apart. It is kept in
    /// the faithful arm because it is what production does — a model
    /// that quietly corrects production's counter-biases would not
    /// answer the question being asked.
    pub(super) fn peer_view_observations(&self, origin: usize, peer: usize) -> NodeObservations {
        let obs = &self.nodes[origin].peer_obs[peer];
        if !self.arm.blind_peer_samples() {
            return obs.clone();
        }
        NodeObservations {
            in_flight: if self.arm.blind_peer_inflight() {
                0
            } else {
                obs.in_flight
            },
            samples: 0,
            recent_failure_rate: 0.0,
            p50_latency_ms: 0,
            p95_latency_ms: 0,
            // Written in production by `ThroughputTarget::Peer`, and
            // then ignored by the `samples` gate above. Carried so the
            // arm models the *measurement* being taken.
            ttft_ewma_ms: obs.ttft_ewma_ms,
            tg_tok_s_ewma: obs.tg_tok_s_ewma,
        }
    }

    /// Assemble what `origin` currently believes about every peer —
    /// the *gather* half of the production selector, with HTTP
    /// replaced by a cache-age model.
    pub(super) fn build_peer_views(&mut self, origin: usize) -> Vec<VenueView> {
        let now = self.now_ms;
        let ttl = self.cfg.manifest_ttl_ms;
        let fresh = self.arm.fresh_signals();
        let n = self.nodes.len();
        let mut views = Vec::with_capacity(n.saturating_sub(1));
        for peer in 0..n {
            if peer == origin {
                continue;
            }
            let rtt = self.rtt_ms[origin][peer];
            let fetched = self.nodes[origin].manifest_fetched_ms.get(&peer).copied();
            let (age_ms, from_cache) = match fetched {
                Some(at) if now.saturating_sub(at) < ttl => (now - at, true),
                _ => {
                    self.nodes[origin].manifest_fetched_ms.insert(peer, now);
                    (0, false)
                }
            };
            let (gossiped_in_flight, availability, last_seen_unix) = if fresh {
                // The counterfactual: the same scorer, told the truth.
                (
                    Some(self.nodes[peer].in_flight()),
                    self.nodes[peer].availability,
                    now / 1000,
                )
            } else {
                match self.load_belief(origin, peer) {
                    Some((b, src)) => {
                        self.backpressure.peer_views_with_signal += 1;
                        if src == LoadSource::Response {
                            self.backpressure.peer_views_from_response += 1;
                        }
                        (Some(b.in_flight), b.availability, b.received_at_ms / 1000)
                    }
                    // Never heard from: no gossiped load signal at
                    // all, which is what a cold peer looks like.
                    None => (None, None, 0),
                }
            };
            views.push(VenueView {
                name: self.nodes[peer].name.clone(),
                node_id_hex: self.nodes[peer].node_id_hex.clone(),
                quarantined: self.nodes[origin]
                    .peer_health
                    .is_quarantined(&self.nodes[peer].name),
                // The sim has no yield-to-local-user model: it has no
                // local users. Held at `None` deliberately rather than
                // wired to something plausible — a behavioural change
                // enters the sim as a named ARM (§6 of
                // `SCHEDULER_QUALITY.md`), so every arm recorded before
                // 2026-08-14 keeps its numbers bit-identical.
                yield_backoff_secs: None,
                pinned_transport: false,
                gossiped_in_flight,
                availability,
                gossip_last_seen_unix: last_seen_unix,
                // F10: gossip carries no benchmark in production
                // (`capabilities.rs:194`), so every peer reaches the
                // scorer rate-cardless on the blind arms.
                benchmark: if self.arm.blind_rate_card() {
                    None
                } else {
                    self.nodes[peer].benchmark.clone()
                },
                observations: self.peer_view_observations(origin, peer),
                manifest: Some(VenueManifestView {
                    manifest: self.nodes[peer].manifest.clone(),
                    rtt_ms: rtt,
                    age_secs: age_ms / 1000,
                    from_cache,
                }),
            });
        }
        views
    }

    /// The load reading `origin` would use for `peer` right now, and
    /// where it came from.
    ///
    /// Two rival readings of one quantity: the gossiped belief, and —
    /// under §4.2 step 1 — whatever the peer said about itself on the
    /// last response it returned to us. **Newest measurement wins**,
    /// which is the only rule that needs no tuning constant and the
    /// only one that cannot be worse than either source alone. A
    /// gossip record that arrived after our last response but was
    /// *measured* before it is correctly ignored; `received_at_ms`
    /// orders arrival, `measured_at_ms` orders truth, and this picks on
    /// truth.
    ///
    /// Returned by value (a `Belief` is a handful of words) so callers
    /// can keep mutating `self` — the coverage counters at the call
    /// site are the reason.
    pub(super) fn load_belief(&self, origin: usize, peer: usize) -> Option<(Belief, LoadSource)> {
        let gossiped = self.nodes[origin].gossip.get(&peer);
        let piggybacked = if self.arm.response_backpressure() {
            self.nodes[origin].backpressure.get(&peer)
        } else {
            None
        };
        match (gossiped, piggybacked) {
            (Some(g), Some(p)) => {
                if p.measured_at_ms >= g.measured_at_ms {
                    Some((p.clone(), LoadSource::Response))
                } else {
                    Some((g.clone(), LoadSource::Gossip))
                }
            }
            (Some(g), None) => Some((g.clone(), LoadSource::Gossip)),
            (None, Some(p)) => Some((p.clone(), LoadSource::Response)),
            (None, None) => None,
        }
    }

    /// True vs recorded age of the load signal behind a dispatch, plus
    /// which channel carried it. Reads through
    /// [`load_belief`](Sim::load_belief) so the recorded provenance can
    /// never disagree with the number the scorer actually consumed.
    pub(super) fn signal_ages(
        &self,
        origin: usize,
        peer: usize,
    ) -> (Option<u64>, Option<u64>, LoadSource) {
        match self.load_belief(origin, peer) {
            Some((b, src)) => (
                Some(self.now_ms.saturating_sub(b.measured_at_ms)),
                Some(self.now_ms.saturating_sub(b.received_at_ms)),
                src,
            ),
            None => (None, None, LoadSource::None),
        }
    }

    /// Perfect-information greedy: whoever finishes it soonest.
    pub(super) fn oracle_pick(&self, arrival: &Arrival) -> usize {
        let mut best = arrival.origin;
        let mut best_ms = u64::MAX;
        for candidate in 0..self.nodes.len() {
            let rtt = if candidate == arrival.origin {
                0
            } else {
                self.rtt_ms[arrival.origin][candidate]
            } as u64;
            let node = &self.nodes[candidate];
            let finish = node.backlog_ms(self.now_ms)
                + node.service_ms(arrival.context_tokens, arrival.output_tokens)
                + rtt;
            if finish < best_ms {
                best_ms = finish;
                best = candidate;
            }
        }
        best
    }

    // ── dispatch and service ────────────────────────────────────

    pub(super) fn dispatch(
        &mut self,
        origin: usize,
        server: usize,
        arrival: &Arrival,
        decision_id: String,
        oicp_request_id: String,
        facts: DispatchFacts,
    ) {
        let rtt = if server == origin {
            0
        } else {
            self.rtt_ms[origin][server]
        };
        // Counterfactual: what staying local would have cost, given
        // the origin's true queue right now.
        let local_alternative_ms = {
            let node = &self.nodes[origin];
            node.backlog_ms(self.now_ms)
                + node.service_ms(arrival.context_tokens, arrival.output_tokens)
        };
        self.seq += 1;
        let job = Job {
            seq: self.seq,
            origin,
            server,
            decision_id,
            oicp_request_id,
            context_tokens: arrival.context_tokens,
            output_tokens: arrival.output_tokens,
            class: arrival.class,
            arrived_ms: self.now_ms,
            started_ms: None,
            rtt_ms: rtt,
            load_paid_ms: 0,
            local_alternative_ms,
            model_id: self.nodes[server]
                .manifest
                .models
                .first()
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            facts,
        };
        // Belief bookkeeping, through the same helpers production
        // uses.
        if server == origin {
            scheduler_core::observe_dispatch(&mut self.nodes[origin].local_obs);
        } else {
            scheduler_core::observe_dispatch(&mut self.nodes[origin].peer_obs[server]);
        }
        self.nodes[server].queue.push_back(job);
        self.maybe_start(server);
    }

    pub(super) fn maybe_start(&mut self, node_idx: usize) {
        while self.nodes[node_idx].running.len() < self.cfg.slots_per_node {
            let Some(mut job) = self.nodes[node_idx].queue.pop_front() else {
                return;
            };
            // The first request to a cold node pays the model load;
            // everything behind it inherits a warm slot. Flipping
            // `manifest`'s `loaded` here is what lets a decider stop
            // charging for it.
            let load = if self.nodes[node_idx].resident {
                0
            } else {
                let n = &mut self.nodes[node_idx];
                n.resident = true;
                if let Some(model) = n.manifest.models.first_mut() {
                    model.status.loaded = true;
                }
                n.load_ms
            };
            let service =
                load + self.nodes[node_idx].service_ms(job.context_tokens, job.output_tokens);
            job.load_paid_ms = load;
            job.started_ms = Some(self.now_ms);
            let job_seq = job.seq;
            self.nodes[node_idx].running.push(job);
            self.push(
                self.now_ms + service,
                EventKind::ServiceDone {
                    node: node_idx,
                    job_seq,
                },
            );
        }
    }

    pub(super) fn on_service_done(&mut self, node_idx: usize, job_seq: u64) {
        let Some(pos) = self.nodes[node_idx]
            .running
            .iter()
            .position(|j| j.seq == job_seq)
        else {
            return;
        };
        let job = self.nodes[node_idx].running.remove(pos);
        let started = job.started_ms.unwrap_or(job.arrived_ms);
        // Load is pre-first-token time, so it belongs to TTFT.
        let server_ttft = self.nodes[node_idx].ttft_ms(job.context_tokens) + job.load_paid_ms;
        let queue_wait = started.saturating_sub(job.arrived_ms);
        let ttft_ms = queue_wait + server_ttft + job.rtt_ms as u64;
        let total_ms = self.now_ms.saturating_sub(job.arrived_ms) + job.rtt_ms as u64;

        // Feedback into the decider's beliefs — the same EWMA and
        // failure-rate arithmetic production runs, so a peer that
        // served slowly is scored lower next time in the sim exactly
        // as it would be in the field.
        let decode_ms = total_ms.saturating_sub(ttft_ms).max(1);
        let observed_tg = job.output_tokens as f64 / (decode_ms as f64 / 1000.0);
        let origin = job.origin;
        let server = job.server;
        if server == origin {
            scheduler_core::observe_success(&mut self.nodes[origin].local_obs);
            apply_throughput_observation(
                &mut self.nodes[origin].local_obs,
                Some(ttft_ms as f64),
                Some(observed_tg),
            );
        } else {
            scheduler_core::observe_success(&mut self.nodes[origin].peer_obs[server]);
            apply_throughput_observation(
                &mut self.nodes[origin].peer_obs[server],
                Some(ttft_ms as f64),
                Some(observed_tg),
            );
            let peer_name = self.nodes[server].name.clone();
            self.nodes[origin].peer_health.record_success(&peer_name);
            // §4.2 step 1: the response carries the server's own view
            // of itself. Measured **after** this job left the running
            // set, because "how loaded am I now that yours is done" is
            // what the next decision needs — counting the finished job
            // would make a peer look permanently one-deeper than it is.
            //
            // `measured_at_ms == received_at_ms`: the reading rides a
            // response whose travel time is already charged to the
            // request, so there is no propagation delay to model. The
            // residual optimism is bounded by one RTT and documented on
            // `Arm::ResponseBackpressure`.
            if self.arm.response_backpressure() {
                let reading = Belief {
                    in_flight: self.nodes[server].in_flight(),
                    availability: self.nodes[server].availability,
                    received_at_ms: self.now_ms,
                    measured_at_ms: self.now_ms,
                };
                self.nodes[origin].backpressure.insert(server, reading);
            }
        }

        let served_by = if server == origin {
            ServedBy::LocalFallback {
                model_id: job.model_id.clone(),
            }
        } else {
            ServedBy::Peer {
                name: self.nodes[server].name.clone(),
                node_id: Some(self.nodes[server].node_id_hex.clone()),
                model_id: job.model_id.clone(),
            }
        };
        self.records
            .push(DecisionEvent::Outcome(Box::new(RoutingOutcome {
                schema: DECISION_LOG_SCHEMA.to_string(),
                decision_id: job.decision_id.clone(),
                oicp_request_id: job.oicp_request_id.clone(),
                ts_unix_ms: self.now_ms,
                served_by,
                attempt_index: 0,
                ttft_ms: Some(ttft_ms as f64),
                total_ms: Some(total_ms as f64),
                output_tokens: Some(job.output_tokens as u64),
                shed: false,
                error: None,
                failovers: Vec::new(),
            })));
        self.truth.push(ServedFact {
            decision_id: job.decision_id,
            origin,
            server,
            class: job.class,
            total_ms,
            ttft_ms,
            queue_wait_ms: queue_wait,
            local_alternative_ms: job.local_alternative_ms,
            dispatched_at_ms: job.arrived_ms,
            true_signal_age_ms: job.facts.true_signal_age_ms,
            recorded_signal_age_ms: job.facts.recorded_signal_age_ms,
            eligible_peers: job.facts.eligible_peers,
        });

        self.maybe_start(node_idx);
    }
}
