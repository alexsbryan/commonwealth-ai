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
                    tracing::info!(
                        target: DECISION_TRACE_TARGET,
                        decision_id = %d.decision_id,
                        oicp_request_id = %d.oicp_request_id,
                        path = ?d.path,
                        verdict = %verdict_label(&d.verdict),
                        winner = %winner,
                        scored = d.candidates.len(),
                        excluded = d.excluded.len(),
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
