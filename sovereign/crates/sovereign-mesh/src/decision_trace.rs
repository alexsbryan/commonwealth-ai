// SPDX-License-Identifier: AGPL-3.0-or-later
//! Observation-state export and trace-replay fixtures — Phase 0
//! (P3 + P4) of
//! [`docs/specs/SCHEDULER_QUALITY.md`](../../../docs/specs/SCHEDULER_QUALITY.md).
//!
//! # P3 — observation-state export
//!
//! [`FleetSnapshot`] dumps everything the scheduler's service-time
//! model would need to be *fit* rather than hand-tuned: per-peer
//! `NodeObservations` (the latency and throughput EWMAs), the
//! gossiped `BenchmarkResult` for each node, `PeerHealth` state, and
//! the local side of all three.
//!
//! Why it matters is narrow and specific. §3 of the spec reports
//! "p95 improves 2.5× under two-choices sampling" from a probe whose
//! fleet composition — 25 tok/s hub, 30 tok/s desktops, 45 tok/s
//! laptops — was *chosen by the author*. Until those numbers come
//! from a real mesh, that result is a property of the chosen
//! constants and not evidence about the mesh. This export is what
//! converts one into the other.
//!
//! # P4 — trace-replay fixtures
//!
//! [`SchedulerTrace`] is a snapshot plus an ordered stream of
//! [`DecisionEvent`]s, loaded back from the JSONL a
//! [`crate::decision_log::TracingDecisionSink`] wrote. It is the
//! replay substrate the calibration contract requires: a real
//! household evening becomes a repeatable Tier-1 scenario, and the
//! gate — decision-agreement between simulator and hardware — is
//! computable because [`Episode`] pairs each decision with the
//! outcome that actually followed it.
//!
//! The loader is deliberately lenient about *ordering* and strict
//! about *schema*. Decisions and outcomes are minutes apart in a live
//! log and interleave freely with other requests, so grouping is by
//! `decision_id`, not adjacency. A record whose schema tag is not
//! understood is refused rather than partially read — a fixture that
//! silently drops fields would produce a calibration number that
//! looks fine and means nothing.

use std::collections::HashMap;
use std::io::BufRead;

use serde::{Deserialize, Serialize};

use crate::decision_log::{
    DecisionEvent, FleetSnapshot, RoutingDecision, RoutingOutcome, DECISION_LOG_SCHEMA,
};

/// Schema tag for the snapshot / trace envelope. Independent of the
/// record schema: the fixture format can gain fields without
/// invalidating already-captured record streams.
pub const TRACE_SCHEMA: &str = "oicp-trace/v1";

// ---------------------------------------------------------------
// P4 — trace-replay fixtures
// ---------------------------------------------------------------

/// One routing decision paired with what actually happened.
///
/// The pairing is the unit the calibration contract is defined over:
/// replay `decision` through the simulator, compare its choice to
/// `decision`'s own verdict (decision-agreement), and compare the
/// distribution of `outcome` latencies ordinally (never absolutely).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Episode {
    pub decision: RoutingDecision,
    /// `None` when the request was still in flight at capture time,
    /// or the process died before the stream dropped.
    pub outcome: Option<RoutingOutcome>,
}

impl Episode {
    /// The name the decision selected, if any.
    pub fn chosen(&self) -> Option<&str> {
        self.decision
            .candidates
            .iter()
            .find(|c| c.selected)
            .map(|c| c.name.as_str())
    }

    /// Whether the request was served by the candidate the decision
    /// picked. `false` means a failover happened — the waste signal.
    pub fn served_first_choice(&self) -> bool {
        self.outcome.as_ref().is_some_and(|o| o.attempt_index == 0)
    }
}

/// A replayable scheduler trace: fleet composition plus the episodes
/// that ran against it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SchedulerTrace {
    pub schema: String,
    /// Observation states captured during the trace, oldest first.
    ///
    /// A vector rather than one snapshot because a capture spanning a
    /// household evening spans a *changing* fleet — peers join, a
    /// laptop's throughput EWMA warms up, a node gets quarantined.
    /// Replaying every episode against a single snapshot would model
    /// a mesh that never existed; [`Self::snapshot_at`] gives each
    /// episode the fleet as of its own timestamp.
    ///
    /// Empty for a trace assembled from decisions alone — still
    /// usable for decision-agreement, but the service-time model then
    /// has to be hand-tuned again, which is what P3 exists to avoid.
    #[serde(default)]
    pub snapshots: Vec<FleetSnapshot>,
    pub episodes: Vec<Episode>,
    /// Outcomes whose decision was not in the stream — typically the
    /// head of a log that was rotated mid-request. Kept rather than
    /// dropped so a low join rate is visible instead of silent.
    pub orphan_outcomes: Vec<RoutingOutcome>,
}

/// Why a trace could not be loaded.
#[derive(Debug)]
pub enum TraceError {
    Io(std::io::Error),
    /// A record carried a schema tag this build does not understand.
    /// Refused rather than partially read.
    UnknownSchema {
        line: usize,
        found: String,
    },
    /// A line was not a decision record at all.
    Malformed {
        line: usize,
        error: String,
    },
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::Io(e) => write!(f, "decision trace: io error: {e}"),
            TraceError::UnknownSchema { line, found } => write!(
                f,
                "decision trace: line {line} carries schema '{found}', \
                 this build understands '{DECISION_LOG_SCHEMA}' — refusing \
                 to partially read it"
            ),
            TraceError::Malformed { line, error } => {
                write!(f, "decision trace: line {line} is not a record: {error}")
            }
        }
    }
}

impl std::error::Error for TraceError {}

impl From<std::io::Error> for TraceError {
    fn from(e: std::io::Error) -> Self {
        TraceError::Io(e)
    }
}

impl SchedulerTrace {
    /// Load a trace from a decision-log JSONL file.
    pub fn from_jsonl_path(path: &std::path::Path) -> Result<Self, TraceError> {
        let file = std::fs::File::open(path)?;
        Self::from_jsonl(std::io::BufReader::new(file))
    }

    /// Load a trace from any line reader over decision-log JSONL.
    pub fn from_jsonl<R: BufRead>(reader: R) -> Result<Self, TraceError> {
        let mut decisions: Vec<RoutingDecision> = Vec::new();
        let mut outcomes: HashMap<String, RoutingOutcome> = HashMap::new();
        let mut snapshots: Vec<FleetSnapshot> = Vec::new();

        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: DecisionEvent =
                serde_json::from_str(line).map_err(|e| TraceError::Malformed {
                    line: i + 1,
                    error: e.to_string(),
                })?;
            match event {
                DecisionEvent::Decision(d) => {
                    if d.schema != DECISION_LOG_SCHEMA {
                        return Err(TraceError::UnknownSchema {
                            line: i + 1,
                            found: d.schema,
                        });
                    }
                    decisions.push(*d);
                }
                DecisionEvent::Outcome(o) => {
                    if o.schema != DECISION_LOG_SCHEMA {
                        return Err(TraceError::UnknownSchema {
                            line: i + 1,
                            found: o.schema,
                        });
                    }
                    // Last writer wins. A decision has exactly one
                    // terminal outcome by construction; a duplicate
                    // means a bug upstream, and keeping the later one
                    // keeps the loader total rather than panicking on
                    // data it did not produce.
                    outcomes.insert(o.decision_id.clone(), *o);
                }
                DecisionEvent::Snapshot(s) => snapshots.push(*s),
            }
        }

        // Group by id, not adjacency: a live log interleaves requests
        // and a decision's outcome lands minutes later.
        let mut episodes = Vec::with_capacity(decisions.len());
        for d in decisions {
            let outcome = outcomes.remove(&d.decision_id);
            episodes.push(Episode {
                decision: d,
                outcome,
            });
        }
        let mut orphan_outcomes: Vec<RoutingOutcome> = outcomes.into_values().collect();
        orphan_outcomes.sort_by_key(|o| o.ts_unix_ms);
        snapshots.sort_by_key(|s| s.captured_at_unix);

        Ok(Self {
            schema: TRACE_SCHEMA.to_string(),
            snapshots,
            episodes,
            orphan_outcomes,
        })
    }

    /// Attach a P3 snapshot taken out-of-band (a test fixture, or a
    /// one-shot [`crate::peer_inference::MeshInferenceProvider::observation_snapshot`]
    /// call) to a trace that has none.
    pub fn with_snapshot(mut self, snapshot: FleetSnapshot) -> Self {
        self.snapshots.push(snapshot);
        self.snapshots.sort_by_key(|s| s.captured_at_unix);
        self
    }

    /// The fleet as of a given moment: the most recent snapshot taken
    /// at or before `unix_secs`, falling back to the earliest one
    /// when the episode predates every snapshot.
    ///
    /// Replaying an episode against a *later* snapshot would let the
    /// simulator see load and health that had not happened yet, which
    /// is the subtlest way to make a calibration number look good and
    /// mean nothing.
    pub fn snapshot_at(&self, unix_secs: u64) -> Option<&FleetSnapshot> {
        self.snapshots
            .iter()
            .rev()
            .find(|s| s.captured_at_unix <= unix_secs)
            .or_else(|| self.snapshots.first())
    }

    /// The fleet an episode ran against.
    pub fn snapshot_for(&self, episode: &Episode) -> Option<&FleetSnapshot> {
        self.snapshot_at(episode.decision.ts_unix_ms / 1000)
    }

    /// Serialise the whole fixture (snapshots + episodes) as one JSON
    /// document — the shape the simulator reads.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Load a fixture written by [`Self::to_json`].
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// Fraction of decisions whose outcome was found. A trace with a
    /// low join rate is not admissible calibration evidence, and this
    /// is the number that says so before anyone runs the simulator.
    pub fn join_rate(&self) -> f64 {
        if self.episodes.is_empty() {
            return 0.0;
        }
        let joined = self.episodes.iter().filter(|e| e.outcome.is_some()).count();
        joined as f64 / self.episodes.len() as f64
    }

    /// Episodes that actually exercised the scorer — the ranked path
    /// with at least one candidate. Gated decisions are policy, not
    /// scheduling; including them would inflate decision-agreement
    /// with choices no scheduler made.
    pub fn scored_episodes(&self) -> impl Iterator<Item = &Episode> {
        self.episodes
            .iter()
            .filter(|e| !e.decision.candidates.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::{
        CandidateInputs, CandidateKind, CandidateRecord, DecisionBuilder, DecisionPath, LoadSource,
        LocalObservationRecord, PeerObservationRecord, RequestFacts, ScoreRecord, ServedBy,
        Verdict,
    };
    use sovereign_core::oicp::NodeObservations;

    fn facts() -> RequestFacts {
        RequestFacts {
            capability_hint: "general".into(),
            latency_class: "Normal".into(),
            sharding: "MeshAllowed".into(),
            context_tokens: Some(1500),
            max_output_tokens: Some(250),
            preferred_speed: "Slow".into(),
            explicit_model_id: None,
        }
    }

    fn candidate(name: &str, score: f32) -> CandidateRecord {
        CandidateRecord {
            kind: CandidateKind::Peer,
            name: name.into(),
            node_id: None,
            model_id: "m".into(),
            size_gb: Some(9.0),
            locality: "far".into(),
            rank: None,
            selected: false,
            score: ScoreRecord {
                claim_score: score,
                observation_mult: 1.0,
                load_penalty: 1.0,
                locality_bonus: 1.0,
                cold_start_weight: 1.0,
                throughput_factor: 1.0,
                throughput_source: "neutral".into(),
                availability: 1.0,
                final_score: score,
            },
            inputs: CandidateInputs::from_observations(
                &NodeObservations::default(),
                LoadSource::Gossip,
            ),
            tier_band: None,
        }
    }

    /// `(jsonl, decision_ids)` for `n` decisions, each with an
    /// outcome, deliberately written *interleaved* so the loader is
    /// exercised on the ordering a real log produces.
    fn interleaved_log(n: usize) -> (String, Vec<String>) {
        let mut decisions = Vec::new();
        let mut ids = Vec::new();
        for i in 0..n {
            let mut b =
                DecisionBuilder::new(&format!("req-{i}"), DecisionPath::RankedOicp, facts());
            b.push_candidate(candidate("hub", 0.9));
            ids.push(b.decision_id().to_string());
            let ranked = vec!["hub".to_string()];
            decisions.push(b.finish(
                Verdict::Peers {
                    ranked: ranked.clone(),
                },
                &ranked,
            ));
        }
        let mut lines = Vec::new();
        // All decisions first, then all outcomes in reverse — the
        // worst case for an adjacency-based grouper.
        for d in &decisions {
            lines.push(
                serde_json::to_string(&DecisionEvent::Decision(Box::new(d.clone()))).unwrap(),
            );
        }
        for (i, d) in decisions.iter().enumerate().rev() {
            let o = RoutingOutcome {
                schema: DECISION_LOG_SCHEMA.into(),
                decision_id: d.decision_id.clone(),
                oicp_request_id: format!("req-{i}"),
                ts_unix_ms: 1000 + i as u64,
                served_by: ServedBy::Peer {
                    name: "hub".into(),
                    node_id: None,
                    model_id: "m".into(),
                },
                attempt_index: 0,
                ttft_ms: Some(200.0),
                total_ms: Some(4000.0),
                output_tokens: Some(250),
                shed: false,
                error: None,
                failovers: Vec::new(),
            };
            lines.push(serde_json::to_string(&DecisionEvent::Outcome(Box::new(o))).unwrap());
        }
        (lines.join("\n"), ids)
    }

    fn snapshot() -> FleetSnapshot {
        FleetSnapshot {
            schema: TRACE_SCHEMA.into(),
            captured_at_unix: 1_000_000,
            local: LocalObservationRecord {
                observations: NodeObservations::default(),
                benchmark: None,
                advertised_models: vec!["m".into()],
                in_flight_published: 0,
            },
            peers: vec![
                PeerObservationRecord {
                    name: "hub".into(),
                    node_id: None,
                    observations: NodeObservations {
                        tg_tok_s_ewma: 25.0,
                        ..Default::default()
                    },
                    benchmark: None,
                    gossiped_in_flight: Some(3),
                    inference_availability: Some(1.0),
                    gossip_age_secs: Some(12),
                    quarantined: false,
                    consecutive_failures: 0,
                    cooldown_remaining_secs: 0,
                },
                PeerObservationRecord {
                    name: "laptop".into(),
                    node_id: None,
                    observations: NodeObservations {
                        tg_tok_s_ewma: 45.0,
                        ..Default::default()
                    },
                    benchmark: None,
                    gossiped_in_flight: Some(0),
                    inference_availability: Some(1.0),
                    gossip_age_secs: Some(28),
                    quarantined: false,
                    consecutive_failures: 0,
                    cooldown_remaining_secs: 0,
                },
                PeerObservationRecord {
                    name: "desktop".into(),
                    node_id: None,
                    observations: NodeObservations::default(),
                    benchmark: None,
                    gossiped_in_flight: None,
                    inference_availability: None,
                    gossip_age_secs: Some(4),
                    quarantined: true,
                    consecutive_failures: 3,
                    cooldown_remaining_secs: 42,
                },
            ],
        }
    }

    #[test]
    fn loader_joins_by_id_not_adjacency() {
        let (jsonl, ids) = interleaved_log(4);
        let trace = SchedulerTrace::from_jsonl(jsonl.as_bytes()).unwrap();
        assert_eq!(trace.episodes.len(), 4);
        assert!(trace.orphan_outcomes.is_empty());
        assert_eq!(trace.join_rate(), 1.0);
        for (ep, id) in trace.episodes.iter().zip(ids.iter()) {
            assert_eq!(&ep.decision.decision_id, id);
            assert_eq!(ep.outcome.as_ref().unwrap().decision_id, *id);
        }
    }

    #[test]
    fn outcome_without_decision_is_kept_as_orphan() {
        let o = RoutingOutcome {
            schema: DECISION_LOG_SCHEMA.into(),
            decision_id: "gone".into(),
            oicp_request_id: "r".into(),
            ts_unix_ms: 5,
            served_by: ServedBy::Failed,
            attempt_index: 0,
            ttft_ms: None,
            total_ms: None,
            output_tokens: None,
            shed: true,
            error: Some("503".into()),
            failovers: Vec::new(),
        };
        let line = serde_json::to_string(&DecisionEvent::Outcome(Box::new(o))).unwrap();
        let trace = SchedulerTrace::from_jsonl(line.as_bytes()).unwrap();
        assert!(trace.episodes.is_empty());
        assert_eq!(trace.orphan_outcomes.len(), 1);
        // An empty episode set must not read as a perfect join.
        assert_eq!(trace.join_rate(), 0.0);
    }

    #[test]
    fn decision_without_outcome_lowers_the_join_rate() {
        let mut b = DecisionBuilder::new("r", DecisionPath::RankedOicp, facts());
        b.push_candidate(candidate("hub", 0.9));
        let d = b.finish(Verdict::StayLocal, &[]);
        let line = serde_json::to_string(&DecisionEvent::Decision(Box::new(d))).unwrap();
        let (joined, _) = interleaved_log(1);
        let both = format!("{joined}\n{line}");
        let trace = SchedulerTrace::from_jsonl(both.as_bytes()).unwrap();
        assert_eq!(trace.episodes.len(), 2);
        assert!((trace.join_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_schema_is_refused_not_partially_read() {
        let mut b = DecisionBuilder::new("r", DecisionPath::RankedOicp, facts());
        b.push_candidate(candidate("hub", 0.9));
        let mut d = b.finish(Verdict::StayLocal, &[]);
        d.schema = "oicp-decision/v99".into();
        let line = serde_json::to_string(&DecisionEvent::Decision(Box::new(d))).unwrap();
        match SchedulerTrace::from_jsonl(line.as_bytes()) {
            Err(TraceError::UnknownSchema { line, found }) => {
                assert_eq!(line, 1);
                assert_eq!(found, "oicp-decision/v99");
            }
            other => panic!("expected UnknownSchema, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_names_its_line_number() {
        let (jsonl, _) = interleaved_log(1);
        let corrupt = format!("{jsonl}\n{{not json");
        match SchedulerTrace::from_jsonl(corrupt.as_bytes()) {
            Err(TraceError::Malformed { line, .. }) => assert_eq!(line, 3),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_are_skipped() {
        let (jsonl, _) = interleaved_log(2);
        let padded = format!("\n{}\n\n", jsonl.replace('\n', "\n\n"));
        let trace = SchedulerTrace::from_jsonl(padded.as_bytes()).unwrap();
        assert_eq!(trace.episodes.len(), 2);
    }

    #[test]
    fn scored_episodes_exclude_gated_decisions() {
        let (jsonl, _) = interleaved_log(2);
        let gated = DecisionBuilder::new("f", DecisionPath::RankedOicp, facts()).finish(
            Verdict::Gated {
                gate: "not_offload_eligible".into(),
            },
            &[],
        );
        let line = serde_json::to_string(&DecisionEvent::Decision(Box::new(gated))).unwrap();
        let trace = SchedulerTrace::from_jsonl(format!("{jsonl}\n{line}").as_bytes()).unwrap();
        assert_eq!(trace.episodes.len(), 3);
        assert_eq!(trace.scored_episodes().count(), 2);
    }

    #[test]
    fn fixture_round_trips_with_snapshot_attached() {
        let (jsonl, _) = interleaved_log(3);
        let trace = SchedulerTrace::from_jsonl(jsonl.as_bytes())
            .unwrap()
            .with_snapshot(snapshot());
        let json = trace.to_json().unwrap();
        let back = SchedulerTrace::from_json(&json).unwrap();
        assert_eq!(trace, back);
        assert_eq!(back.snapshots[0].peers.len(), 3);
    }

    #[test]
    fn episode_reports_choice_and_first_choice_service() {
        let (jsonl, _) = interleaved_log(1);
        let trace = SchedulerTrace::from_jsonl(jsonl.as_bytes()).unwrap();
        let ep = &trace.episodes[0];
        assert_eq!(ep.chosen(), Some("hub"));
        assert!(ep.served_first_choice());
    }

    #[test]
    fn failover_episode_is_not_first_choice_service() {
        let (jsonl, ids) = interleaved_log(1);
        // Replace the outcome with one that failed over.
        let o = RoutingOutcome {
            schema: DECISION_LOG_SCHEMA.into(),
            decision_id: ids[0].clone(),
            oicp_request_id: "req-0".into(),
            ts_unix_ms: 2000,
            served_by: ServedBy::LocalFallback {
                model_id: "m".into(),
            },
            attempt_index: 1,
            ttft_ms: Some(900.0),
            total_ms: Some(9000.0),
            output_tokens: Some(250),
            shed: false,
            error: None,
            failovers: vec![crate::decision_log::FailoverAttempt {
                peer: "hub".into(),
                error: "503".into(),
                shed: true,
            }],
        };
        let line = serde_json::to_string(&DecisionEvent::Outcome(Box::new(o))).unwrap();
        let trace = SchedulerTrace::from_jsonl(format!("{jsonl}\n{line}").as_bytes()).unwrap();
        let ep = &trace.episodes[0];
        assert!(!ep.served_first_choice());
        assert_eq!(ep.outcome.as_ref().unwrap().failovers.len(), 1);
        assert!(ep.outcome.as_ref().unwrap().failovers[0].shed);
    }

    #[test]
    fn snapshot_median_gossip_age_is_the_f1_headline() {
        let s = snapshot();
        // ages 12, 28, 4 → sorted 4, 12, 28 → median 12
        assert_eq!(s.median_gossip_age_secs(), Some(12));
    }

    #[test]
    fn snapshot_reports_observed_throughput_spread_for_f3() {
        let s = snapshot();
        let (min, max) = s.observed_tg_spread().unwrap();
        assert_eq!(min, 25.0);
        assert_eq!(max, 45.0);
        // Both ends sit above THROUGHPUT_REFERENCE_TG_TOK_S (20.0),
        // which is F3: the heterogeneity term clamps to 1.0 for every
        // node in this fleet and therefore discriminates nothing.
        assert!(min > 20.0);
    }

    #[test]
    fn snapshot_with_no_gossip_ages_reports_none_not_zero() {
        let mut s = snapshot();
        for p in &mut s.peers {
            p.gossip_age_secs = None;
        }
        assert_eq!(s.median_gossip_age_secs(), None);
    }
}
