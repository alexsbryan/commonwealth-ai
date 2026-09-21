// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recording sink — file, env, clock, ids.
//!
//! `sovereign-scheduler`'s `decision_log` holds the record model and the
//! `DecisionSink` seam, but the sink that writes the records, the clock that
//! stamps them and the sequence that orders their ids are host concerns
//! (`sovereign/SERVING_BOUNDARY.md` "Corrected 2026-09-14", bullet 1). The
//! ranker stays pure: it takes the decision id and `now` as arguments the way
//! `DecisionBuilder::finish_at` always has.
//!
//! [`TracingDecisionSink`] always emits to `tracing` under
//! [`DECISION_TRACE_TARGET`]: a human-readable summary at `info`, the full JSON
//! record at `debug`. When [`DECISION_LOG_ENV`] names a path it also appends the
//! full record as one JSON object per line, flushed per record — a daemon that
//! runs for weeks must not hold the last decisions in a buffer.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use sovereign_scheduler::decision_log::{
    DecisionEvent, DecisionSink, ServedBy, Verdict, DECISION_LOG_ENV, DECISION_LOG_SCHEMA,
    DECISION_TRACE_TARGET,
};

/// The production sink.
///
/// Always emits to `tracing` under [`DECISION_TRACE_TARGET`]: a
/// human-readable summary at `info`, the full JSON record at `debug`.
/// When [`DECISION_LOG_ENV`] names a path, also appends the full
/// record as one JSON object per line, flushed per record — a daemon
/// that runs for weeks must not hold the last decisions in a buffer.
pub struct TracingDecisionSink {
    file: Option<Mutex<std::io::BufWriter<std::fs::File>>>,
}

impl std::fmt::Debug for TracingDecisionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TracingDecisionSink")
            .field("jsonl", &self.file.is_some())
            .finish()
    }
}

impl TracingDecisionSink {
    /// Build from the environment. A configured path that cannot be
    /// opened is a warning, not a failure — instrumentation must
    /// never take the daemon down.
    pub fn from_env() -> Self {
        let Some(path) = std::env::var_os(DECISION_LOG_ENV) else {
            return Self { file: None };
        };
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => {
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    path = %path.display(),
                    schema = DECISION_LOG_SCHEMA,
                    "decision log: appending routing decision records"
                );
                Self {
                    file: Some(Mutex::new(std::io::BufWriter::new(f))),
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: DECISION_TRACE_TARGET,
                    path = %path.display(),
                    error = %e,
                    "decision log: could not open sink path — records stay in tracing only"
                );
                Self { file: None }
            }
        }
    }

    /// Build a sink that writes JSONL to an explicit path, ignoring
    /// the environment. The trace-capture CLI verb uses this.
    pub fn to_path(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Some(Mutex::new(std::io::BufWriter::new(f))),
        })
    }
}

impl DecisionSink for TracingDecisionSink {
    fn record(&self, event: DecisionEvent) {
        match &event {
            DecisionEvent::Decision(d) => {
                let winner = d
                    .candidates
                    .iter()
                    .find(|c| c.selected)
                    .map(|c| c.name.as_str())
                    .unwrap_or("<none>");
                // A gated decision scored nothing — it is policy, not
                // scheduling — and it used to sit at `debug` so the
                // `info` line stayed one-per-decision that actually
                // chose between candidates.
                //
                // That reasoning cost a whole measurement. The deployed
                // daemon filters `mesh.decision=info`
                // (`sovereign-cli-daemon`'s `DAEMON_TRACING_FILTER`), so
                // "why did this turn stay home?" was the ONE routing
                // question the operator's log could not answer. §9.1.1 of
                // MESH_SCALE_100_USERS_1000_CORPORA.md ran 100 turns at a
                // census-verified 2-node mesh, saw zero peer dispatches,
                // and had to record the firing gate as UNKNOWN — every
                // one of those turns emitted a `Gated` record naming it,
                // at a level nothing was listening to. The record was
                // there; the operator was not allowed to see it.
                //
                // Cost of the promotion, measured on this host's own
                // decision log (49,167 records, 2026-08-06 → 08-13):
                // 2,245 gated decisions, ~320/day, ~1 line per 4.5
                // minutes. That is not a stream. `gate` is lifted out of
                // the label into its own field so it is greppable
                // without parsing, and `path` rides along because
                // "gated on the ranked path" and "gated on the named
                // fallthrough" are different stories.
                if let Verdict::Gated { gate } = &d.verdict {
                    tracing::info!(
                        target: DECISION_TRACE_TARGET,
                        decision_id = %d.decision_id,
                        oicp_request_id = %d.oicp_request_id,
                        path = ?d.path,
                        gate = %gate,
                        verdict = %verdict_label(&d.verdict),
                        hint = %d.request.capability_hint,
                        latency = %d.request.latency_class,
                        sharding = %d.request.sharding,
                        "routing decision (gated) — stayed local before scoring"
                    );
                } else {
                    // `scores` and `decided_by` are RENDERED, not gathered: the
                    // record has carried every term per candidate since the
                    // scorer was written, and nothing put them where an
                    // operator reads. That gap cost a measurement — ring-room's
                    // `wl-route-06303aff` read `verdict=stay_local scored=3`
                    // and then spent 38.2 s locally with an idle GPU peer in
                    // the mesh, and the artifact could not say which term kept
                    // it home (ralph A33 / A35). Principle 1.
                    //
                    // Cost: one short string per non-gated decision, on a line
                    // that already exists. The full per-candidate breakdown
                    // stays in the record for anyone collecting it.
                    let (scores, decided_by) = score_summary(&d.candidates, &d.verdict);
                    tracing::info!(
                        target: DECISION_TRACE_TARGET,
                        decision_id = %d.decision_id,
                        oicp_request_id = %d.oicp_request_id,
                        path = ?d.path,
                        verdict = %verdict_label(&d.verdict),
                        winner = %winner,
                        scored = d.candidates.len(),
                        excluded = d.excluded.len(),
                        scores = %scores,
                        decided_by = %decided_by,
                        hint = %d.request.capability_hint,
                        latency = %d.request.latency_class,
                        "routing decision"
                    );
                }
            }
            DecisionEvent::Outcome(o) => {
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    decision_id = %o.decision_id,
                    oicp_request_id = %o.oicp_request_id,
                    served_by = %served_by_label(&o.served_by),
                    attempt = o.attempt_index,
                    ttft_ms = ?o.ttft_ms,
                    total_ms = ?o.total_ms,
                    tokens = ?o.output_tokens,
                    shed = o.shed,
                    "routing outcome"
                );
            }
            DecisionEvent::Snapshot(s) => {
                // The two headline diagnostics, in the log where an
                // operator will actually see them: is gossip lag
                // comparable to service time (F1), and does the
                // throughput term discriminate this fleet (F3)?
                tracing::info!(
                    target: DECISION_TRACE_TARGET,
                    peers = s.peers.len(),
                    median_gossip_age_secs = ?s.median_gossip_age_secs(),
                    observed_tg_spread = ?s.observed_tg_spread(),
                    quarantined = s.peers.iter().filter(|p| p.quarantined).count(),
                    "fleet observation snapshot"
                );
            }
        }

        // The full record is expensive to render and only wanted when
        // someone is actually collecting. Serialise once and reuse for
        // both the debug event and the file.
        let want_json = self.file.is_some()
            || tracing::enabled!(target: DECISION_TRACE_TARGET, tracing::Level::DEBUG);
        if !want_json {
            return;
        }
        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    target: DECISION_TRACE_TARGET,
                    error = %e,
                    "decision log: record failed to serialise"
                );
                return;
            }
        };
        tracing::debug!(target: DECISION_TRACE_TARGET, record = %line, "routing record");
        if let Some(file) = &self.file {
            if let Ok(mut w) = file.lock() {
                if let Err(e) = writeln!(w, "{line}").and_then(|()| w.flush()) {
                    tracing::warn!(
                        target: DECISION_TRACE_TARGET,
                        error = %e,
                        "decision log: write failed"
                    );
                }
            }
        }
    }
}

/// How many candidates' scores the `routing decision` line renders, best-first.
///
/// Four covers the decision — a winner and the rivals that were close — on
/// every fleet this repo runs, and bounds one `info` line at ~80 characters on
/// a 100-node mesh rather than ~1.5 KB.
const SCORES_RENDERED: usize = 4;

/// Render the leading candidates' final scores, and name the term that carried the
/// chosen one over its closest rival.
///
/// The chosen candidate is the one the cascade would try FIRST (`selected`),
/// which on a `stay_local` verdict is the local candidate — so "why did this
/// stay home" and "why did this peer win" are the same question and get the
/// same answer. `decided_by` reads `tie` when no term separates them by more
/// than `TERM_TIE_RATIO` (the scores were the same and a tie-break decided),
/// and `n/a` when there is nothing to compare against — a one-candidate
/// decision explains itself.
fn score_summary(
    candidates: &[sovereign_scheduler::decision_log::CandidateRecord],
    verdict: &Verdict,
) -> (String, String) {
    // Capped, and the cap is not cosmetic. This line is emitted once per
    // non-gated decision, and the mesh-scale target is 100 nodes: rendering
    // every candidate would put ~1.5 KB on a per-request `info` line, which is
    // how a glassbox field becomes a reason to turn the target off. The top
    // few by score are what a reader needs — the decision was between those —
    // and the count says what was elided. The FULL per-candidate breakdown is
    // in the record for anyone collecting it.
    let mut ranked: Vec<&sovereign_scheduler::decision_log::CandidateRecord> =
        candidates.iter().collect();
    ranked.sort_by(|a, b| {
        b.score
            .final_score
            .partial_cmp(&a.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut scores = ranked
        .iter()
        .take(SCORES_RENDERED)
        .map(|c| format!("{}={:.4}", c.name, c.score.final_score))
        .collect::<Vec<_>>()
        .join(" ");
    if ranked.len() > SCORES_RENDERED {
        scores.push_str(&format!(" (+{} more)", ranked.len() - SCORES_RENDERED));
    }
    // `selected` marks the first candidate the CASCADE would try, and a
    // `StayLocal` verdict marks nobody — which is why the info line has always
    // read `winner=<none>` for it. That is exactly the decision an operator
    // needs explained ("why did this stay home?"), so on `StayLocal` the local
    // candidate IS the chosen one and is named as such. Caught by the
    // simulator, where every ring-room decision is a `StayLocal`.
    let chosen = candidates
        .iter()
        .find(|c| c.selected)
        .or_else(|| match verdict {
            Verdict::StayLocal => candidates
                .iter()
                .find(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Local),
            _ => None,
        });
    let decided_by = match chosen {
        None => "n/a".to_string(),
        Some(win) => {
            // Closest rival by final score — the candidate the decision was
            // actually close to, not merely the next one in the vec.
            let rival = candidates
                .iter()
                .filter(|c| !std::ptr::eq(*c, win))
                .max_by(|a, b| {
                    a.score
                        .final_score
                        .partial_cmp(&b.score.final_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            match rival {
                None => "n/a".to_string(),
                Some(r) => {
                    match sovereign_scheduler::decision_log::deciding_term(&win.score, &r.score) {
                        Some(t) => format!("{} vs {}", t, r.name),
                        None => format!("tie vs {}", r.name),
                    }
                }
            }
        }
    };
    (scores, decided_by)
}

fn verdict_label(v: &Verdict) -> String {
    match v {
        Verdict::Gated { gate } => format!("gated:{gate}"),
        Verdict::StayLocal => "stay_local".into(),
        Verdict::Peers { ranked } => format!("peers:{}", ranked.len()),
        Verdict::NamedLocal { .. } => "named_local".into(),
        Verdict::NamedPeer { peer, .. } => format!("named_peer:{peer}"),
        Verdict::NamedLender { lender, .. } => format!("named_lender:{lender}"),
        Verdict::NamedUnknown { .. } => "named_unknown".into(),
    }
}

fn served_by_label(s: &ServedBy) -> String {
    match s {
        ServedBy::Local { model_id } => format!("local:{model_id}"),
        ServedBy::LocalFallback { model_id } => format!("local_fallback:{model_id}"),
        ServedBy::Peer { name, .. } => format!("peer:{name}"),
        ServedBy::Failed => "failed".into(),
    }
}

/// Monotonic within a process, so a decision id sorts by issue order
/// even when two decisions land in the same millisecond.
static DECISION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Mint a decision id. Prefixed with the process-local sequence so a
/// tailed log reads in order; suffixed with a short random tail so
/// ids from different nodes never collide when traces are merged.
pub fn new_decision_id() -> String {
    let seq = DECISION_SEQ.fetch_add(1, Ordering::Relaxed);
    let tail = uuid::Uuid::new_v4().simple().to_string();
    format!("d{seq:08x}-{}", &tail[..12])
}

pub fn now_unix_ms() -> u64 {
    sovereign_time::unix_millis()
}

pub fn now_unix_secs() -> u64 {
    sovereign_time::unix_now_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_scheduler::decision_log::{DecisionBuilder, DecisionPath, RequestFacts};

    fn facts() -> RequestFacts {
        RequestFacts {
            capability_hint: "general".into(),
            latency_class: "normal".into(),
            sharding: "mesh_allowed".into(),
            context_tokens: Some(1500),
            max_output_tokens: Some(250),
            preferred_speed: "slow".into(),
            explicit_model_id: None,
        }
    }

    fn score(
        final_score: f32,
        cold: f32,
        loc: f32,
    ) -> sovereign_scheduler::decision_log::ScoreRecord {
        sovereign_scheduler::decision_log::ScoreRecord {
            claim_score: 0.95,
            observation_mult: 1.0,
            load_penalty: 1.0,
            locality_bonus: loc,
            cold_start_weight: cold,
            throughput_factor: 1.0,
            throughput_source: "neutral".into(),
            availability: 1.0,
            final_score,
        }
    }

    fn cand(
        name: &str,
        kind: sovereign_scheduler::decision_log::CandidateKind,
        s: sovereign_scheduler::decision_log::ScoreRecord,
    ) -> sovereign_scheduler::decision_log::CandidateRecord {
        sovereign_scheduler::decision_log::CandidateRecord {
            kind,
            name: name.into(),
            node_id: None,
            model_id: "m".into(),
            size_gb: Some(2.0),
            locality: "local".into(),
            rank: None,
            selected: false,
            score: s,
            inputs: sovereign_scheduler::decision_log::CandidateInputs::from_observations(
                &oicp_types::NodeObservations::default(),
                sovereign_scheduler::decision_log::LoadSource::Local,
            ),
            tier_band: None,
        }
    }

    /// **A `StayLocal` verdict marks NO candidate `selected`** — which is why
    /// the info line has always read `winner=<none>` for it. That is exactly
    /// the decision an operator needs explained, and the ring-room
    /// investigation could not: `wl-route-06303aff` read
    /// `verdict=stay_local scored=3` and then spent 38.2 s locally with an idle
    /// GPU peer in the mesh (ralph A33/A35).
    ///
    /// So on `StayLocal` the local candidate IS the chosen one, and the line
    /// names the term that kept the work home. Caught by the simulator, where
    /// every ring-room decision is a `StayLocal`.
    #[test]
    fn a_stay_local_decision_still_names_the_term_that_kept_it_home() {
        use sovereign_scheduler::decision_log::CandidateKind;
        // The sim's own numbers, decision `sim-0-150`.
        let cands = vec![
            cand("local", CandidateKind::Local, score(1.092, 1.0, 1.15)),
            cand("keeper-gpu", CandidateKind::Peer, score(0.698, 0.7, 1.05)),
        ];
        let (scores, decided_by) = score_summary(&cands, &Verdict::StayLocal);
        assert_eq!(
            scores, "local=1.0920 keeper-gpu=0.6980",
            "best-first, and both fit under the render cap"
        );
        assert!(
            decided_by.starts_with("cold_start_weight(1.000>0.700) vs keeper-gpu"),
            "a stay-local line must name the term, got {decided_by:?}"
        );
    }

    /// The other direction: when a peer IS selected, that peer is the chosen
    /// one and local is the rival — the same question, the same answer shape.
    #[test]
    fn a_peer_decision_names_the_term_that_carried_the_peer() {
        use sovereign_scheduler::decision_log::CandidateKind;
        let mut peer = cand("keeper-gpu", CandidateKind::Peer, score(0.735, 0.7, 1.05));
        peer.selected = true;
        let cands = vec![
            cand("local", CandidateKind::Local, score(0.732, 1.0, 1.15)),
            peer,
        ];
        let (_, decided_by) = score_summary(
            &cands,
            &Verdict::Peers {
                ranked: vec!["keeper-gpu".into()],
            },
        );
        assert!(
            decided_by.contains("vs local"),
            "the rival of a chosen peer is local, got {decided_by:?}"
        );
    }

    /// The render cap, and why it is there: this line fires once per non-gated
    /// decision and the mesh-scale target is 100 nodes. Best-first, capped,
    /// and the count says what was elided — a glassbox field that turns a
    /// per-request `info` line into 1.5 KB is one an operator switches off.
    #[test]
    fn the_scores_line_is_capped_best_first_and_says_what_it_elided() {
        use sovereign_scheduler::decision_log::CandidateKind;
        let mut cands = vec![cand("local", CandidateKind::Local, score(0.50, 1.0, 1.15))];
        // Pushed worst-first, so a rendering that echoed vec order would fail.
        for (i, sc) in [0.10_f32, 0.20, 0.30, 0.40, 0.60, 0.70].iter().enumerate() {
            cands.push(cand(
                &format!("peer-{i}"),
                CandidateKind::Peer,
                score(*sc, 0.7, 1.05),
            ));
        }
        let (scores, _) = score_summary(&cands, &Verdict::StayLocal);
        assert_eq!(
            scores, "peer-5=0.7000 peer-4=0.6000 local=0.5000 peer-3=0.4000 (+3 more)",
            "best-first, capped at {SCORES_RENDERED}, and the remainder counted"
        );
    }

    /// A gated decision scores nothing, so there is nothing to explain and the
    /// line must say so rather than reach into an empty vec.
    #[test]
    fn an_empty_candidate_set_explains_itself() {
        let (scores, decided_by) = score_summary(&[], &Verdict::StayLocal);
        assert_eq!(scores, "");
        assert_eq!(decided_by, "n/a");
    }

    #[test]
    fn decision_ids_are_unique_and_ordered() {
        let a = new_decision_id();
        let b = new_decision_id();
        assert_ne!(a, b);
        // The sequence prefix sorts by issue order.
        assert!(a < b, "{a} should sort before {b}");
    }

    #[test]
    fn jsonl_sink_appends_one_line_per_record() {
        let dir = std::env::temp_dir().join(format!("decision-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.jsonl");
        let sink = TracingDecisionSink::to_path(&path).unwrap();
        for i in 0..3 {
            sink.record(DecisionEvent::Decision(Box::new(
                DecisionBuilder::new(
                    new_decision_id(),
                    &format!("r{i}"),
                    DecisionPath::RankedOicp,
                    facts(),
                )
                .finish_at(Verdict::StayLocal, &[], now_unix_ms()),
            )));
        }
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in lines {
            let ev: DecisionEvent = serde_json::from_str(l).unwrap();
            assert!(matches!(ev, DecisionEvent::Decision(_)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
