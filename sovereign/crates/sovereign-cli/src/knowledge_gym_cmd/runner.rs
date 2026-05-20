//! Knowledge-gym replay loop + predicate evaluation.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::Fixture;

const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TURNS: usize = 6;

pub struct RunnerCfg {
    pub base_url: String,
    pub replays: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct Transcript {
    pub tool_calls: Vec<ToolCallRecord>,
    pub final_message: Option<String>,
    pub runner_error: Option<String>,
    pub model_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct ToolCallRecord {
    pub turn: usize,
    pub name: String,
    pub query: Option<String>,
    pub returned_evidence_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PredicateOutcome {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ReplayReport {
    pub transcript: Transcript,
    pub predicates: Vec<PredicateOutcome>,
}

impl ReplayReport {
    fn passed(&self) -> bool {
        self.transcript.runner_error.is_none()
            && self.predicates.iter().all(|p| p.passed)
    }
}

#[derive(Debug, Serialize)]
pub struct FixtureReport {
    pub slug: String,
    pub replays: Vec<ReplayReport>,
}

impl FixtureReport {
    pub fn pass_count(&self) -> usize {
        self.replays.iter().filter(|r| r.passed()).count()
    }

    pub fn pass_rate(&self) -> f32 {
        if self.replays.is_empty() {
            0.0
        } else {
            self.pass_count() as f32 / self.replays.len() as f32
        }
    }

    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "replays: {}, passed: {} ({:.0}%)",
            self.replays.len(),
            self.pass_count(),
            self.pass_rate() * 100.0
        )];
        for (i, r) in self.replays.iter().enumerate() {
            if let Some(err) = &r.transcript.runner_error {
                lines.push(format!("  [{i}] RUNNER ERROR: {err}"));
                continue;
            }
            for pred in &r.predicates {
                if pred.passed {
                    lines.push(format!("  [{i}] ✓ {} — {}", pred.name, pred.detail));
                } else {
                    lines.push(format!("  [{i}] ✗ {} — {}", pred.name, pred.detail));
                    if let Some(msg) = &r.transcript.final_message {
                        let excerpt: String = msg.chars().take(1200).collect();
                        lines.push(format!("    final_message excerpt: {excerpt}"));
                    }
                }
            }
        }
        lines
    }
}

#[derive(Debug, Serialize)]
pub struct AggregateSummary {
    pub fixtures: usize,
    pub total_replays: usize,
    pub total_passes: usize,
    pub pass_rate: f32,
    pub per_fixture: Vec<(String, usize, usize, f32)>,
}

impl AggregateSummary {
    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "fixtures: {}  replays: {}  passes: {} ({:.0}%)",
            self.fixtures,
            self.total_replays,
            self.total_passes,
            self.pass_rate * 100.0
        )];
        for (slug, pass, total, rate) in &self.per_fixture {
            lines.push(format!("  {slug}: {pass}/{total} ({:.0}%)", rate * 100.0));
        }
        lines
    }
}

pub fn summarise(reports: &[FixtureReport]) -> AggregateSummary {
    let total_replays: usize = reports.iter().map(|r| r.replays.len()).sum();
    let total_passes: usize = reports.iter().map(|r| r.pass_count()).sum();
    let pass_rate = if total_replays == 0 {
        0.0
    } else {
        total_passes as f32 / total_replays as f32
    };
    AggregateSummary {
        fixtures: reports.len(),
        total_replays,
        total_passes,
        pass_rate,
        per_fixture: reports
            .iter()
            .map(|r| (r.slug.clone(), r.pass_count(), r.replays.len(), r.pass_rate()))
            .collect(),
    }
}

pub async fn run_fixture_replays(
    client: &reqwest::Client,
    cfg: &RunnerCfg,
    fx: &Fixture,
) -> FixtureReport {
    let mut replays = Vec::with_capacity(cfg.replays as usize);
    for _ in 0..cfg.replays {
        let tx = run_once(client, cfg, fx).await;
        let predicates = evaluate_predicates(fx, &tx);
        replays.push(ReplayReport {
            transcript: tx,
            predicates,
        });
    }
    FixtureReport {
        slug: fx.slug.clone(),
        replays,
    }
}

async fn run_once(client: &reqwest::Client, cfg: &RunnerCfg, fx: &Fixture) -> Transcript {
    let mut tx = Transcript::default();
    let mut request = fx.input.clone();
    request["stream"] = Value::Bool(false);

    let endpoint = format!(
        "{}/v1/chat/completions",
        cfg.base_url.trim_end_matches('/')
    );

    for turn in 0..MAX_TURNS {
        let started = Instant::now();
        let resp = match client
            .post(&endpoint)
            .json(&request)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tx.runner_error = Some(format!("http error turn={turn}: {e}"));
                return tx;
            }
        };
        let status = resp.status();
        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                tx.runner_error = Some(format!("read body turn={turn}: {e}"));
                return tx;
            }
        };
        tx.model_ms += started.elapsed().as_millis();
        if !status.is_success() {
            tx.runner_error = Some(format!(
                "daemon http {} turn={turn}: {}",
                status.as_u16(),
                body_text.chars().take(400).collect::<String>()
            ));
            return tx;
        }

        let resp_json: Value = match serde_json::from_str(&body_text) {
            Ok(v) => v,
            Err(e) => {
                tx.runner_error = Some(format!("parse daemon response turn={turn}: {e}"));
                return tx;
            }
        };

        let message = match resp_json.pointer("/choices/0/message").cloned() {
            Some(m) => m,
            None => {
                tx.runner_error = Some(format!(
                    "daemon response missing choices[0].message turn={turn}"
                ));
                return tx;
            }
        };

        let tool_calls = message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            tx.final_message = message
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            return tx;
        }

        // Append the assistant message with tool_calls to the
        // conversation, then append our mocked tool result. Mirror
        // the OpenAI shape.
        let msgs = request
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
            .expect("messages must be an array");
        msgs.push(message.clone());

        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("call_unknown")
                .to_string();
            let func = tc.get("function").cloned().unwrap_or(json!({}));
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    func.get("arguments")
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                })
                .unwrap_or_default();
            let args_val: Value = serde_json::from_str(&args_str).unwrap_or(Value::Null);
            let query = args_val
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let (result_str, returned_ids) = if name == "knowledge_lookup" {
                let mut payload = fx.mock_evidence.clone();
                let ids: Vec<String> = payload
                    .get("evidence")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                e.get("id").and_then(|s| s.as_str().map(str::to_string))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // Inject explicit allowlist + the warning the tool
                // descriptor declares, on EVERY response envelope.
                // The model sees this with the evidence and the
                // copy-from-the-allowlist instinct cuts down on
                // fabrication versus a description-only nudge.
                if let Some(map) = payload.as_object_mut() {
                    map.insert(
                        "valid_citation_ids".into(),
                        Value::Array(ids.iter().map(|id| Value::String(id.clone())).collect()),
                    );
                    let warning = if ids.is_empty() {
                        "Evidence empty: cite zero ev-* ids in your final answer.".to_string()
                    } else {
                        format!(
                            "The ONLY valid citation handles in your final answer are: {}. \
                             Any other ev-* token is fabrication.",
                            ids.join(", ")
                        )
                    };
                    map.insert("citation_contract".into(), Value::String(warning));
                }
                (
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
                    ids,
                )
            } else {
                (
                    format!("(knowledge-gym: tool {name} not mocked)"),
                    Vec::new(),
                )
            };

            tx.tool_calls.push(ToolCallRecord {
                turn,
                name: name.clone(),
                query,
                returned_evidence_ids: returned_ids,
            });

            msgs.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": result_str,
            }));
        }
    }

    tx.runner_error = Some(format!("hit MAX_TURNS={MAX_TURNS} without a final message"));
    tx
}

fn evaluate_predicates(fx: &Fixture, tx: &Transcript) -> Vec<PredicateOutcome> {
    let mut out = Vec::new();
    let pass = &fx.predicates;

    let lookup_calls: Vec<&ToolCallRecord> = tx
        .tool_calls
        .iter()
        .filter(|tc| tc.name == "knowledge_lookup")
        .collect();
    let first_tool = tx.tool_calls.first().map(|t| t.name.as_str());

    if let Some(expected) = pass.get("should_call_knowledge_lookup").and_then(|v| v.as_bool()) {
        let actual = !lookup_calls.is_empty();
        out.push(PredicateOutcome {
            name: "should_call_knowledge_lookup".into(),
            passed: actual == expected,
            detail: format!("expected={expected}, actual={actual}"),
        });
    }

    if let Some(expected) = pass.get("expected_first_tool").and_then(|v| v.as_str()) {
        let passed = first_tool == Some(expected);
        out.push(PredicateOutcome {
            name: "expected_first_tool".into(),
            passed,
            detail: format!(
                "expected={expected}, actual={}",
                first_tool.unwrap_or("(none)")
            ),
        });
    }

    if let Some(max) = pass.get("max_lookup_calls").and_then(|v| v.as_integer()) {
        let actual = lookup_calls.len();
        out.push(PredicateOutcome {
            name: "max_lookup_calls".into(),
            passed: actual as i64 <= max,
            detail: format!("max={max}, actual={actual}"),
        });
    }

    if let Some(max) = pass.get("max_query_tokens").and_then(|v| v.as_integer()) {
        // Approximate token count = whitespace-split words.
        let worst = lookup_calls
            .iter()
            .filter_map(|tc| tc.query.as_ref())
            .map(|q| q.split_whitespace().count())
            .max()
            .unwrap_or(0);
        out.push(PredicateOutcome {
            name: "max_query_tokens".into(),
            passed: (worst as i64) <= max,
            detail: format!("max={max}, worst={worst}"),
        });
    }

    let mut cited_ids: Vec<String> = Vec::new();
    if let Some(msg) = tx.final_message.as_deref() {
        // Match `ev-NNNN` tokens anywhere in the final answer.
        for part in msg.split(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
            if part.starts_with("ev-") && part.len() > 3 && part[3..].chars().all(|c| c.is_ascii_digit()) {
                cited_ids.push(part.to_string());
            }
        }
        cited_ids.sort();
        cited_ids.dedup();
    }

    let returned_ids: Vec<String> = tx
        .tool_calls
        .iter()
        .flat_map(|tc| tc.returned_evidence_ids.clone())
        .collect();

    if pass
        .get("must_cite_at_least_one_evidence_id")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        out.push(PredicateOutcome {
            name: "must_cite_at_least_one_evidence_id".into(),
            passed: !cited_ids.is_empty(),
            detail: format!("cited={cited_ids:?}"),
        });
    }

    if pass
        .get("must_not_cite_evidence_id_outside_returned")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let bad: Vec<&String> = cited_ids
            .iter()
            .filter(|id| !returned_ids.contains(id))
            .collect();
        out.push(PredicateOutcome {
            name: "must_not_cite_evidence_id_outside_returned".into(),
            passed: bad.is_empty(),
            detail: format!(
                "cited={cited_ids:?}, returned={returned_ids:?}, fabricated={bad:?}"
            ),
        });
    }

    if let Some(max) = pass.get("max_cited_evidence_ids").and_then(|v| v.as_integer()) {
        let passed = (cited_ids.len() as i64) <= max;
        out.push(PredicateOutcome {
            name: "max_cited_evidence_ids".into(),
            passed,
            detail: format!("max={max}, cited={}", cited_ids.len()),
        });
    }

    if pass
        .get("answer_acknowledges_gap")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let msg_l = tx
            .final_message
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        // SHAPE-level — no bank-derived phrases (per
        // feedback_no_teaching_to_test). The predicate looks for
        // SHAPE of "honest gap acknowledgement":
        //   negated possession ("don't have", "no information")
        //   OR temporal-scope acknowledgement ("real-time",
        //   "live data", "current") — signals the model is
        //   distinguishing its snapshot from the present
        //   OR external-source pointer ("check X", "visit Y") —
        //   the honest "go look elsewhere" move when local
        //   evidence is empty.
        // Any one of these clusters is sufficient. Multiple
        // disjoint clusters keep the predicate from over-fitting
        // to a single phrasing convention.

        let negated_possession = [
            "don't have", "do not have", "doesn't have",
            "don't know", "do not know", "doesn't know",
            "cannot find", "can't find", "cannot retrieve",
            "no information", "no data", "no records",
            "no evidence", "no result", "no idea",
            "not available", "not in my", "not in the",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let temporal_scope = [
            "real-time", "real time", "live data", "up-to-date",
            "current information", "current data", "today's",
            "recent", "latest",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let external_pointer = [
            "check ", "visit ", "look at ", "consult ",
            "recommend checking", "would need to", "you can find",
            "you could check",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let direct_uncertainty = [
            "unfortunately", "sorry", "unsure", "i'm not sure",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let passed = negated_possession
            || (temporal_scope && (negated_possession || external_pointer || direct_uncertainty))
            || (external_pointer && direct_uncertainty);

        out.push(PredicateOutcome {
            name: "answer_acknowledges_gap".into(),
            passed,
            detail: format!(
                "neg_poss={negated_possession}, temp_scope={temporal_scope}, ext_ptr={external_pointer}, direct_unc={direct_uncertainty}, msg_len={}",
                msg_l.len()
            ),
        });
    }

    out
}
