//! Workflow analytics derived from the manifest's tool-call stream.
//!
//! What we get from `atos_tool_events`:
//! - tool name, phase (before/after), args_json, outcome, duration_ms
//! - per-call paired by call_id
//!
//! What we don't get: the tool's actual response payload (the opencode
//! plugin only captures outcome="success"|"empty_result"|...). Per-call
//! correctness grading vs. oracle requires either a daemon-side
//! intercept (deferred) or offline replay (future work). For now this
//! module computes analytics that don't need the response body.

use crate::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowReport {
    pub total_tool_calls: u32,
    pub total_paired_calls: u32,
    pub orphaned_before: u32,
    pub orphaned_after: u32,
    pub elapsed_seconds: i64,
    pub tool_histogram: BTreeMap<String, ToolStats>,
    pub retry_calls: u32,
    pub stale_watcher_encounters: u32,
    pub empty_result_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolStats {
    pub call_count: u32,
    pub success_count: u32,
    pub empty_result_count: u32,
    pub other_outcome_count: u32,
    pub p50_duration_ms: Option<i64>,
    pub p95_duration_ms: Option<i64>,
    pub mean_duration_ms: Option<i64>,
}

pub fn analyze(m: &Manifest) -> WorkflowReport {
    let mut paired: HashMap<String, PairAccumulator> = HashMap::new();
    let mut orphaned_before = 0u32;
    let mut orphaned_after = 0u32;

    for ev in &m.tool_calls {
        let entry = paired
            .entry(ev.call_id.clone())
            .or_insert_with(|| PairAccumulator::new(&ev.tool_name));
        match ev.phase.as_str() {
            "before" => entry.before_at = Some(ev.fired_at),
            "after" => {
                entry.after_at = Some(ev.fired_at);
                entry.outcome = ev.outcome.clone();
                entry.duration_ms = ev.duration_ms;
            }
            _ => {}
        }
    }

    let mut total_paired = 0u32;
    let mut tool_durations: HashMap<String, Vec<i64>> = HashMap::new();
    let mut tool_outcomes: HashMap<String, ToolStats> = HashMap::new();
    let mut empty_results = 0u32;
    let mut stale_encounters = 0u32;
    let mut earliest: Option<i64> = None;
    let mut latest: Option<i64> = None;

    for (_call_id, p) in paired.iter() {
        if p.before_at.is_none() {
            orphaned_before += 1;
        }
        if p.after_at.is_none() {
            orphaned_after += 1;
            continue;
        }
        total_paired += 1;
        if let Some(t) = p.before_at {
            earliest = Some(earliest.map_or(t, |e| e.min(t)));
        }
        if let Some(t) = p.after_at {
            latest = Some(latest.map_or(t, |l| l.max(t)));
        }

        let stats = tool_outcomes.entry(p.tool_name.clone()).or_default();
        stats.call_count += 1;
        match p.outcome.as_deref() {
            Some("success") => stats.success_count += 1,
            Some("empty_result") => {
                stats.empty_result_count += 1;
                empty_results += 1;
            }
            Some(other) => {
                stats.other_outcome_count += 1;
                if other.contains("stale") {
                    stale_encounters += 1;
                }
            }
            None => stats.other_outcome_count += 1,
        }
        if let Some(d) = p.duration_ms {
            tool_durations
                .entry(p.tool_name.clone())
                .or_default()
                .push(d);
        }
    }

    for (tool, ds) in tool_durations.iter_mut() {
        if ds.is_empty() {
            continue;
        }
        ds.sort_unstable();
        let p50 = ds[ds.len() / 2];
        let p95_idx = ((ds.len() as f64) * 0.95) as usize;
        let p95 = ds[p95_idx.min(ds.len() - 1)];
        let mean = (ds.iter().sum::<i64>() as f64 / ds.len() as f64) as i64;
        if let Some(stats) = tool_outcomes.get_mut(tool) {
            stats.p50_duration_ms = Some(p50);
            stats.p95_duration_ms = Some(p95);
            stats.mean_duration_ms = Some(mean);
        }
    }

    let retry_calls = count_retries(m);
    let elapsed_seconds = match (earliest, latest) {
        (Some(s), Some(e)) => e - s,
        _ => 0,
    };
    let empty_result_rate = if total_paired == 0 {
        0.0
    } else {
        empty_results as f64 / total_paired as f64
    };

    let mut histogram = BTreeMap::new();
    for (tool, stats) in tool_outcomes {
        histogram.insert(tool, stats);
    }

    WorkflowReport {
        total_tool_calls: m.tool_calls.len() as u32,
        total_paired_calls: total_paired,
        orphaned_before,
        orphaned_after,
        elapsed_seconds,
        tool_histogram: histogram,
        retry_calls,
        stale_watcher_encounters: stale_encounters,
        empty_result_rate,
    }
}

/// Crude retry counter: same `(tool_name, args_json)` pair appearing
/// within a 3-call window. Catches "agent called symbols(X) twice in a
/// row hoping for a different answer."
fn count_retries(m: &Manifest) -> u32 {
    let befores: Vec<_> = m
        .tool_calls
        .iter()
        .filter(|e| e.phase == "before")
        .collect();
    let mut retries = 0u32;
    for (i, e) in befores.iter().enumerate() {
        let args = e.args_json.as_deref().unwrap_or("");
        let lookback = i.saturating_sub(3);
        for prev in &befores[lookback..i] {
            if prev.tool_name == e.tool_name && prev.args_json.as_deref().unwrap_or("") == args {
                retries += 1;
                break;
            }
        }
    }
    retries
}

struct PairAccumulator {
    tool_name: String,
    before_at: Option<i64>,
    after_at: Option<i64>,
    outcome: Option<String>,
    duration_ms: Option<i64>,
}

impl PairAccumulator {
    fn new(tool_name: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            before_at: None,
            after_at: None,
            outcome: None,
            duration_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::*;

    fn empty_manifest() -> Manifest {
        Manifest {
            schema_version: 1,
            run: RunInfo {
                run_id: "r".into(),
                feature_id: "f".into(),
                milestone_id: "m".into(),
                driver: "opencode".into(),
                session_id: None,
                started_at: 0,
                ended_at: None,
                exit_code: None,
                stop_passed: None,
                mode: "normal".into(),
                stop_stdout: None,
            },
            experiment_repo: ExperimentRepo {
                root: std::path::PathBuf::new(),
                charter_path: None,
                charter_sha256: None,
                spec_shas: vec![],
                git_head: None,
            },
            models: vec![],
            opencode_version: None,
            tool_calls: vec![],
            notes: NotesByKind::default(),
            generated_at_unix: 0,
        }
    }

    fn ev(
        call_id: &str,
        tool: &str,
        phase: &str,
        fired: i64,
        args: Option<&str>,
        outcome: Option<&str>,
        dur: Option<i64>,
    ) -> ToolCallEvent {
        ToolCallEvent {
            event_id: format!("e-{call_id}-{phase}"),
            call_id: call_id.into(),
            tool_name: tool.into(),
            phase: phase.into(),
            args_json: args.map(|s| s.to_string()),
            outcome: outcome.map(|s| s.to_string()),
            duration_ms: dur,
            fired_at: fired,
        }
    }

    #[test]
    fn analyze_pairs_before_after() {
        let mut m = empty_manifest();
        m.tool_calls = vec![
            ev(
                "c1",
                "symbols",
                "before",
                100,
                Some("{\"name\":\"X\"}"),
                None,
                None,
            ),
            ev(
                "c1",
                "symbols",
                "after",
                110,
                None,
                Some("success"),
                Some(10),
            ),
        ];
        let r = analyze(&m);
        assert_eq!(r.total_tool_calls, 2);
        assert_eq!(r.total_paired_calls, 1);
        assert_eq!(r.tool_histogram.get("symbols").unwrap().call_count, 1);
    }

    #[test]
    fn analyze_counts_retries() {
        let mut m = empty_manifest();
        m.tool_calls = vec![
            ev(
                "c1",
                "symbols",
                "before",
                100,
                Some("{\"name\":\"X\"}"),
                None,
                None,
            ),
            ev(
                "c1",
                "symbols",
                "after",
                110,
                None,
                Some("empty_result"),
                Some(10),
            ),
            ev(
                "c2",
                "symbols",
                "before",
                120,
                Some("{\"name\":\"X\"}"),
                None,
                None,
            ),
            ev(
                "c2",
                "symbols",
                "after",
                130,
                None,
                Some("empty_result"),
                Some(10),
            ),
        ];
        let r = analyze(&m);
        assert_eq!(r.retry_calls, 1);
    }

    #[test]
    fn analyze_handles_orphan_after() {
        let mut m = empty_manifest();
        m.tool_calls = vec![ev("c1", "symbols", "before", 100, Some("{}"), None, None)];
        let r = analyze(&m);
        assert_eq!(r.orphaned_after, 1);
        assert_eq!(r.total_paired_calls, 0);
    }
}
