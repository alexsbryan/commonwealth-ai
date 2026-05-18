//! Multi-turn thread bench runner — companion to `runner::run_bank_synth`.
//!
//! Where `run_bank_synth` drives one question per fresh `conversation_id`,
//! this module drives a *thread* of N turns under a SINGLE
//! `conversation_id`. Sequential turns see prior turns' history via the
//! runtime's conversation store, so the bench measures whether
//! coreference, anaphora, and topic continuity survive across turns.
//!
//! Per-turn scoring is deterministic (substring fact_recall, title
//! source_recall) — no LLM calls. The full transcript is then graded
//! ONCE by a primary-slot judge that returns per-fact coverage +
//! evidence_turn. See `feedback_wikipedia_learn_thread_judge` for the
//! cost rationale: 90 turns × 5-fact judges would be 450 LLM calls;
//! 12 threads × 1 transcript judge is 12.
//!
//! Headline output: `DegradationCurve` — first-failure turn and
//! prompt-token growth across the thread. The bench's purpose is
//! surfacing the conversational-memory degradation point, not the
//! per-fact answer-equivalence judgment that single-shot synth bench
//! already covers.

use std::time::Instant;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::chat_cmd::bootstrap::ChatSession;
use crate::chat_cmd::render::split_reasoning;
use crate::eval_cmd::bank::{EvalThreadBank, Thread, Turn};
use crate::eval_cmd::runner::{RetrievedChunk, ScoreSnapshot};
use crate::eval_cmd::score::{score_facts_in_text, score_sources_titles, FactScore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEvalRun {
    pub bank: String,
    pub corpus: String,
    pub started_at: String,
    pub finished_at: String,
    pub threads: Vec<ThreadResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResult {
    pub thread_id: String,
    pub category: String,
    pub description: String,
    pub conversation_id: String,
    pub turns: Vec<TurnResult>,
    /// Single judge call covering the union of expected_facts across
    /// turns. `None` under `--no-judge`.
    pub judge: Option<ThreadJudge>,
    pub degradation: DegradationCurve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    pub turn_index: usize,
    pub turn_id: String,
    pub question: String,
    pub answer: String,
    pub reasoning_chars: usize,
    pub stream_wall_ms: u64,
    pub total_latency_ms: Option<u64>,
    /// Per-turn retrieval (search + hybrid + merge) latency in ms,
    /// pulled from the assistant message's metadata. Split out
    /// from `total_latency_ms` (which folds in synthesis) so the
    /// bench can hold the search-cost knob accountable: pre-merge
    /// K boost trades latency for recall, and the bench
    /// surfaces both sides of that ratio.
    pub search_ms: Option<u64>,
    pub intent: Option<String>,
    pub fact_recall: ScoreSnapshot,
    pub source_recall: ScoreSnapshot,
    pub retrieved: Vec<RetrievedChunk>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadJudge {
    /// Aggregated coverage — what fraction of the thread's expected
    /// facts the judge marked `present=yes` somewhere in the transcript.
    /// Under multi-trial (judge_trials > 1) the coverage is computed
    /// from per-fact majority: a fact is "present" if ≥ ⌈N/2⌉ trials
    /// marked it so. `coverage_mean` carries the fractional mean
    /// across trials (sum-of-present / N / total_expected).
    pub coverage: ScoreSnapshot,
    pub per_fact: Vec<ThreadFactEvidence>,
    /// Number of judge trials this thread's verdict aggregates over.
    /// `1` is the single-judge default; >1 indicates multi-judge with
    /// the per-fact `present_count` capturing how many trials agreed.
    #[serde(default = "default_judge_trials")]
    pub trials: usize,
    /// Mean coverage across the N trials. Equals `coverage.ratio`
    /// when trials=1. Useful as a continuous signal next to the
    /// majority-vote binary in `coverage`.
    #[serde(default)]
    pub coverage_mean: Option<f32>,
}

fn default_judge_trials() -> usize { 1 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadFactEvidence {
    pub fact: String,
    pub present: bool,
    /// 0-indexed turn whose answer covered the fact. `None` if absent
    /// or the judge couldn't attribute it.
    pub evidence_turn: Option<usize>,
    pub evidence_quote: String,
    /// Number of trials (of `ThreadJudge::trials`) that marked this
    /// fact present. Equals `1` (present) or `0` (absent) under
    /// single-judge. Under multi-judge, ranges 0..=N; `present` is
    /// derived as `present_count >= ⌈N/2⌉` (majority).
    #[serde(default)]
    pub present_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DegradationCurve {
    /// First turn with deterministic fact_recall strictly less than 1.0.
    /// `None` if every turn was perfect.
    pub first_failure_turn: Option<usize>,
    /// Linear-regression slope of fact_recall on turn_index. Negative
    /// = degrading across the thread. Computed only when ≥2 turns
    /// scored without error.
    pub fact_recall_slope: f64,
    /// Per-turn `total_latency_ms` (None values dropped). Lets the
    /// report plot wall time vs turn position.
    pub latency_ms_per_turn: Vec<Option<u64>>,
}

pub async fn run_thread_bank(
    session: &ChatSession,
    bank: &EvalThreadBank,
    judge_trials: usize,
) -> ThreadEvalRun {
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut threads = Vec::with_capacity(bank.threads.len());
    for th in &bank.threads {
        eprintln!("  → thread {} ({} turns)", th.id, th.turns.len());
        let result = run_thread_synth(session, th, judge_trials).await;
        threads.push(result);
    }
    let finished_at = chrono::Utc::now().to_rfc3339();
    ThreadEvalRun {
        bank: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        started_at,
        finished_at,
        threads,
    }
}

async fn run_thread_synth(
    session: &ChatSession,
    thread: &Thread,
    judge_trials: usize,
) -> ThreadResult {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let mut turns: Vec<TurnResult> = Vec::with_capacity(thread.turns.len());

    for (i, turn) in thread.turns.iter().enumerate() {
        let res = run_one_turn(session, &conversation_id, i, thread, turn).await;
        eprintln!(
            "    turn {i} fact={:.2} src={:.2} {}ms{}",
            res.fact_recall.ratio.unwrap_or(0.0),
            res.source_recall.ratio.unwrap_or(0.0),
            res.stream_wall_ms,
            res.error
                .as_deref()
                .map(|e| format!(" ERR={e}"))
                .unwrap_or_default()
        );
        turns.push(res);
    }

    let degradation = compute_degradation(&turns);
    let judge_result = if judge_trials == 0 {
        None
    } else if judge_trials == 1 {
        score_thread_coverage(session, thread, &turns).await
    } else {
        score_thread_coverage_multi(session, thread, &turns, judge_trials).await
    };

    ThreadResult {
        thread_id: thread.id.clone(),
        category: thread.category.clone(),
        description: thread.description.clone(),
        conversation_id,
        turns,
        judge: judge_result,
        degradation,
    }
}

async fn run_one_turn(
    session: &ChatSession,
    conversation_id: &str,
    turn_index: usize,
    thread: &Thread,
    turn: &Turn,
) -> TurnResult {
    let t_wall = Instant::now();
    let stream_result = session
        .runtime
        .handle_message_stream(&turn.question, conversation_id)
        .await;

    let (message_id, raw, stream_wall_ms, err): (String, String, u64, Option<String>) =
        match stream_result {
            Ok(handle) => {
                let mid = handle.message_id.clone();
                let mut stream = handle.stream;
                let mut buf = String::new();
                let mut e: Option<String> = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => buf.push_str(&chunk),
                        Err(err) => {
                            e = Some(format!("stream error: {err}"));
                            break;
                        }
                    }
                }
                let wall = t_wall.elapsed().as_millis() as u64;
                (mid, buf, wall, e)
            }
            Err(sovereign_core::error::Error::NotImplemented(_)) => match session
                .runtime
                .handle_message(&turn.question, conversation_id)
                .await
            {
                Ok(resp) => {
                    let wall = t_wall.elapsed().as_millis() as u64;
                    (resp.message.id, resp.message.content, wall, None)
                }
                Err(e) => (
                    String::new(),
                    String::new(),
                    t_wall.elapsed().as_millis() as u64,
                    Some(format!("non-streaming fallback failed: {e}")),
                ),
            },
            Err(e) => (
                String::new(),
                String::new(),
                t_wall.elapsed().as_millis() as u64,
                Some(format!("stream start: {e}")),
            ),
        };

    let (_reasoning_blocks, visible) = split_reasoning(&raw);
    let reasoning_chars: usize = raw.chars().count() - visible.chars().count();

    let metadata = if message_id.is_empty() {
        None
    } else {
        session
            .store
            .get_conversation(conversation_id)
            .await
            .ok()
            .and_then(|c| {
                c.messages
                    .iter()
                    .find(|m| m.id == message_id)
                    .and_then(|m| m.metadata.clone())
            })
    };

    let prov = metadata.as_ref().and_then(|m| m.get("provenance"));
    let total_latency_ms = prov
        .and_then(|p| p.get("total_latency_ms"))
        .and_then(|v| v.as_u64());
    let search_ms = metadata
        .as_ref()
        .and_then(|m| m.get("search_ms"))
        .and_then(|v| v.as_u64());
    let intent = prov
        .and_then(|p| p.get("intent"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let retrieved_chunks_meta = metadata
        .as_ref()
        .and_then(|m| m.get("retrieved_chunks"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let titles: Vec<String> = retrieved_chunks_meta
        .iter()
        .filter_map(|c| c.get("title").and_then(|t| t.as_str()))
        .map(str::to_string)
        .collect();

    let retrieved: Vec<RetrievedChunk> = retrieved_chunks_meta
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c
                .get("corpus_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: c
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            url: c.get("url").and_then(|v| v.as_str()).map(str::to_string),
            score: c
                .get("score")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.0),
            snippet: c
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    let fact_recall: ScoreSnapshot =
        score_facts_in_text(&turn.expected_facts, &visible).into();
    let source_recall: ScoreSnapshot =
        score_sources_titles(&turn.expected_sources, &titles).into();

    TurnResult {
        turn_index,
        turn_id: thread.turn_id(turn_index),
        question: turn.question.clone(),
        answer: visible,
        reasoning_chars,
        stream_wall_ms,
        total_latency_ms,
        search_ms,
        intent,
        fact_recall,
        source_recall,
        retrieved,
        error: err,
    }
}

fn compute_degradation(turns: &[TurnResult]) -> DegradationCurve {
    let mut first_failure = None;
    for t in turns {
        // total_expected==0 means the turn has no expected_facts, so
        // ratio is None — skip it from failure detection (not a miss).
        if let Some(r) = t.fact_recall.ratio {
            if r < 1.0 {
                first_failure = Some(t.turn_index);
                break;
            }
        }
    }

    let pts: Vec<(f64, f64)> = turns
        .iter()
        .filter(|t| t.error.is_none())
        .filter_map(|t| t.fact_recall.ratio.map(|r| (t.turn_index as f64, r as f64)))
        .collect();

    let slope = if pts.len() < 2 {
        0.0
    } else {
        let n = pts.len() as f64;
        let mean_x: f64 = pts.iter().map(|(x, _)| x).sum::<f64>() / n;
        let mean_y: f64 = pts.iter().map(|(_, y)| y).sum::<f64>() / n;
        let mut num = 0.0;
        let mut den = 0.0;
        for (x, y) in &pts {
            num += (x - mean_x) * (y - mean_y);
            den += (x - mean_x).powi(2);
        }
        if den == 0.0 {
            0.0
        } else {
            num / den
        }
    };

    let latency_ms_per_turn: Vec<Option<u64>> =
        turns.iter().map(|t| t.total_latency_ms).collect();

    DegradationCurve {
        first_failure_turn: first_failure,
        fact_recall_slope: slope,
        latency_ms_per_turn,
    }
}

/// One LLM call over the full transcript. Returns per-fact coverage
/// + the turn the judge attributes the fact to. Uses the primary
/// slot (Speed::Slow) because the transcript can be large and the
/// judgment is the bench's headline qualitative signal — running
/// it on Fast would compromise the score under load. See
/// `feedback_wikipedia_learn_thread_judge`.
async fn score_thread_coverage(
    session: &ChatSession,
    thread: &Thread,
    turns: &[TurnResult],
) -> Option<ThreadJudge> {
    let facts = thread.aggregated_expected_facts();
    if facts.is_empty() {
        return None;
    }

    let mut transcript = String::new();
    for t in turns {
        transcript.push_str(&format!("[Turn {}] LEARNER: {}\n", t.turn_index, t.question));
        transcript.push_str(&format!("[Turn {}] TUTOR: {}\n\n", t.turn_index, t.answer));
    }

    let facts_block = facts
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{}. {f}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are grading a learning conversation. The LEARNER asked a chain of \
         follow-up questions; the TUTOR answered each. You judge whether each \
         expected fact was covered SOMEWHERE in the TUTOR's answers, and which \
         turn covered it.\n\n\
         A fact counts as present if the TUTOR mentioned it directly, paraphrased \
         it, or surfaced it in context as one of several relevant items. Mark \
         absent only if no answer in the conversation conveys it.\n\n\
         For each fact return: {{fact, present (\"yes\"|\"no\"), evidence_turn \
         (0-indexed turn or null), evidence_quote (≤25 words from the turn, or \
         \"(absent)\")}}.\n\n\
         TRANSCRIPT:\n{transcript}\n\
         EXPECTED FACTS:\n{facts_block}\n\n\
         Respond with JSON only."
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "fact": {"type": "string"},
                        "present": {"type": "string", "enum": ["yes", "no"]},
                        "evidence_turn": {"type": ["integer", "null"]},
                        "evidence_quote": {"type": "string"}
                    },
                    "required": ["fact", "present", "evidence_turn", "evidence_quote"]
                }
            }
        },
        "required": ["facts"]
    });

    let request = sovereign_core::types::CompletionRequest {
        prompt,
        system_message: Some(
            "You evaluate whether a tutor's conversation covered each expected \
             fact. Be generous: mention-in-context counts. Respond with JSON only."
                .into(),
        ),
        preferred_speed: sovereign_core::types::Speed::Slow,
        max_tokens: Some(2048 + facts.len() * 80),
        temperature: Some(0.0),
        structured_output: Some(schema),
        think_budget: Some(0),
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: None,
        enable_thinking: Some(false),
        sampling_mode: None,
    };

    let resp = match session.inference.as_ref().complete(&request).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  [thread-judge] inference failed for {}: {e}", thread.id);
            return Some(ThreadJudge {
                coverage: empty_coverage(&facts),
                per_fact: facts
                    .iter()
                    .map(|f| ThreadFactEvidence {
                        fact: f.clone(),
                        present: false,
                        evidence_turn: None,
                        evidence_quote: format!("(inference failed: {e})"),
                        present_count: 0,
                    })
                    .collect(),
                trials: 1,
                coverage_mean: Some(0.0),
            });
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&resp.text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "  [thread-judge] parse failed for {}: {e} — raw head: {:.120}",
                thread.id, resp.text
            );
            return Some(ThreadJudge {
                coverage: empty_coverage(&facts),
                per_fact: facts
                    .iter()
                    .map(|f| ThreadFactEvidence {
                        fact: f.clone(),
                        present: false,
                        evidence_turn: None,
                        evidence_quote: "(parse failed)".into(),
                        present_count: 0,
                    })
                    .collect(),
                trials: 1,
                coverage_mean: Some(0.0),
            });
        }
    };

    let mut per_fact: Vec<ThreadFactEvidence> = Vec::with_capacity(facts.len());
    let mut matched_facts: Vec<String> = Vec::new();
    let mut missing_facts: Vec<String> = Vec::new();
    let entries = parsed.get("facts").and_then(|v| v.as_array());
    for fact in &facts {
        let entry = entries.and_then(|arr| {
            arr.iter().find(|e| {
                e.get("fact")
                    .and_then(|v| v.as_str())
                    .map(|s| s.eq_ignore_ascii_case(fact))
                    .unwrap_or(false)
            })
        });
        let present = entry
            .and_then(|e| e.get("present").and_then(|v| v.as_str()))
            .map(|s| s.eq_ignore_ascii_case("yes"))
            .unwrap_or(false);
        let evidence_turn = entry
            .and_then(|e| e.get("evidence_turn"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let evidence_quote = entry
            .and_then(|e| e.get("evidence_quote").and_then(|v| v.as_str()))
            .unwrap_or("(absent)")
            .to_string();
        if present {
            matched_facts.push(fact.clone());
        } else {
            missing_facts.push(fact.clone());
        }
        per_fact.push(ThreadFactEvidence {
            fact: fact.clone(),
            present,
            evidence_turn,
            evidence_quote,
            present_count: if present { 1 } else { 0 },
        });
    }

    let coverage_score = FactScore {
        matched: matched_facts,
        missing: missing_facts,
        total_expected: facts.len(),
    };
    let coverage_snap: ScoreSnapshot = coverage_score.into();
    let coverage_mean = coverage_snap.ratio;
    Some(ThreadJudge {
        coverage: coverage_snap,
        per_fact,
        trials: 1,
        coverage_mean,
    })
}

/// Multi-trial variant of `score_thread_coverage`. Runs the judge
/// prompt N times against the SAME transcript, aggregates per-fact
/// `present_count`, derives `present` from majority vote
/// (≥ ⌈N/2⌉ trials agree). Reports both the binary coverage and the
/// mean coverage across trials. See the `ThreadJudge` docstring for
/// the rationale: multi-trial of the synth pipeline costs Nx wall;
/// multi-judge of the same transcript only costs Nx the judge call
/// (~10s/thread × 13 threads × N), which is the right granularity
/// for catching judge-side flips on borderline facts without paying
/// the synth-side cost.
async fn score_thread_coverage_multi(
    session: &ChatSession,
    thread: &Thread,
    turns: &[TurnResult],
    trials: usize,
) -> Option<ThreadJudge> {
    if trials == 0 {
        return None;
    }
    let mut trial_results: Vec<ThreadJudge> = Vec::with_capacity(trials);
    for t in 0..trials {
        eprintln!("  [thread-judge] trial {}/{} for {}", t + 1, trials, thread.id);
        if let Some(j) = score_thread_coverage(session, thread, turns).await {
            trial_results.push(j);
        }
    }
    if trial_results.is_empty() {
        return None;
    }

    // Aggregate per fact: count trials marking present, build evidence
    // from the first trial that marked it present (so the snippet is
    // a real quote, not a synthesised average).
    let facts = thread.aggregated_expected_facts();
    let majority_floor = (trials + 1) / 2; // ⌈N/2⌉
    let mut per_fact: Vec<ThreadFactEvidence> = Vec::with_capacity(facts.len());
    let mut matched: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut trial_sums: Vec<usize> = vec![0; trials]; // per-trial matched count for coverage_mean
    for (fact_idx, fact) in facts.iter().enumerate() {
        let mut present_count = 0usize;
        let mut first_present_evidence: Option<&ThreadFactEvidence> = None;
        for (t_idx, trial) in trial_results.iter().enumerate() {
            if let Some(e) = trial.per_fact.get(fact_idx) {
                if e.present {
                    present_count += 1;
                    trial_sums[t_idx] += 1;
                    if first_present_evidence.is_none() {
                        first_present_evidence = Some(e);
                    }
                }
            }
        }
        let present = present_count >= majority_floor;
        if present {
            matched.push(fact.clone());
        } else {
            missing.push(fact.clone());
        }
        let (evidence_turn, evidence_quote) = match first_present_evidence {
            Some(e) => (e.evidence_turn, e.evidence_quote.clone()),
            None => (None, "(absent in all trials)".into()),
        };
        per_fact.push(ThreadFactEvidence {
            fact: fact.clone(),
            present,
            evidence_turn,
            evidence_quote,
            present_count,
        });
    }

    let coverage_score = FactScore {
        matched,
        missing,
        total_expected: facts.len(),
    };
    let coverage_snap: ScoreSnapshot = coverage_score.into();
    // Mean coverage across trials: average of (matched_in_trial / total)
    let total = facts.len() as f32;
    let coverage_mean = if total > 0.0 && !trial_sums.is_empty() {
        let sum: f32 = trial_sums.iter().map(|&n| n as f32 / total).sum();
        Some(sum / trial_sums.len() as f32)
    } else {
        None
    };

    Some(ThreadJudge {
        coverage: coverage_snap,
        per_fact,
        trials: trial_results.len(),
        coverage_mean,
    })
}

fn empty_coverage(facts: &[String]) -> ScoreSnapshot {
    FactScore {
        matched: Vec::new(),
        missing: facts.to_vec(),
        total_expected: facts.len(),
    }
    .into()
}

/// Print one terse per-thread block + global rollup. Caller can pipe
/// to --output for the full JSON.
pub fn print_threads_text(run: &ThreadEvalRun) {
    println!("── thread bench: {} (corpus {}) ─────────────────", run.bank, run.corpus);
    println!("started:  {}", run.started_at);
    println!("finished: {}", run.finished_at);
    println!("threads:  {}", run.threads.len());
    println!();

    let mut total_turns = 0usize;
    let mut total_fact_matched = 0usize;
    let mut total_fact_expected = 0usize;
    let mut total_src_matched = 0usize;
    let mut total_src_expected = 0usize;
    let mut total_wall_ms: u64 = 0;
    let mut judges_run = 0usize;
    let mut coverage_acc: f32 = 0.0;

    for th in &run.threads {
        println!("[{}] {} — {}", th.thread_id, th.category, th.description);
        for t in &th.turns {
            let fact = t
                .fact_recall
                .ratio
                .map(|r| format!("{r:.2}"))
                .unwrap_or_else(|| "n/a".into());
            let src = t
                .source_recall
                .ratio
                .map(|r| format!("{r:.2}"))
                .unwrap_or_else(|| "n/a".into());
            let intent = t.intent.as_deref().unwrap_or("?");
            let err = t
                .error
                .as_deref()
                .map(|e| format!(" ERR={e}"))
                .unwrap_or_default();
            let search_label = t
                .search_ms
                .map(|s| format!("  search={s}ms"))
                .unwrap_or_default();
            println!(
                "  T{:<2}  fact={fact}  src={src}  {}ms{}  intent={intent}{}",
                t.turn_index, t.stream_wall_ms, search_label, err
            );
            total_turns += 1;
            total_fact_matched += t.fact_recall.matched.len();
            total_fact_expected += t.fact_recall.total_expected;
            total_src_matched += t.source_recall.matched.len();
            total_src_expected += t.source_recall.total_expected;
            total_wall_ms += t.stream_wall_ms;
        }
        let first_fail = th
            .degradation
            .first_failure_turn
            .map(|t| t.to_string())
            .unwrap_or_else(|| "none".into());
        println!(
            "  degradation: first_failure_turn={first_fail}  slope={:.3}",
            th.degradation.fact_recall_slope
        );
        if let Some(j) = &th.judge {
            let cov = j.coverage.ratio.unwrap_or(0.0);
            println!(
                "  judge coverage: {:.0}% ({}/{} facts)",
                cov * 100.0,
                j.coverage.matched.len(),
                j.coverage.total_expected
            );
            judges_run += 1;
            coverage_acc += cov;
            for ev in &j.per_fact {
                let mark = if ev.present { "✓" } else { "·" };
                let turn = ev
                    .evidence_turn
                    .map(|t| format!("T{t}"))
                    .unwrap_or_else(|| "—".into());
                println!("    {mark} [{turn}] {}", ev.fact);
            }
        }
        println!();
    }

    println!("── rollup ──────────────────────────────────────────────");
    println!("turns:           {}", total_turns);
    let fact_pct = if total_fact_expected == 0 {
        0.0
    } else {
        total_fact_matched as f32 / total_fact_expected as f32
    };
    let src_pct = if total_src_expected == 0 {
        0.0
    } else {
        total_src_matched as f32 / total_src_expected as f32
    };
    println!(
        "fact_recall:     {fact_pct:.3} ({total_fact_matched}/{total_fact_expected})"
    );
    println!(
        "source_recall:   {src_pct:.3} ({total_src_matched}/{total_src_expected})"
    );
    println!(
        "wall total:      {:.1}s",
        total_wall_ms as f64 / 1000.0
    );
    if judges_run > 0 {
        println!(
            "judge coverage:  {:.3} (mean over {judges_run} threads)",
            coverage_acc / judges_run as f32
        );
    }
}

pub fn print_threads_json(run: &ThreadEvalRun) -> Result<(), String> {
    let s = serde_json::to_string_pretty(run).map_err(|e| e.to_string())?;
    println!("{s}");
    Ok(())
}

pub fn write_threads_json_file(run: &ThreadEvalRun, path: &std::path::Path) -> Result<(), String> {
    let s = serde_json::to_string_pretty(run).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}
