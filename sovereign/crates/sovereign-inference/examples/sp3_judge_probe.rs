// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP3 spike probe (research/enrichment-spikes): what does judge-scoring one
//! corpus's RAPTOR summaries cost, per model tier?
//!
//! Replicates the production grounding-judge protocol standalone — claim
//! decomposition (`extract_claim_list`, runtime/grounding/judge.rs:375-448)
//! then per-claim forced-choice support against member chunk passages
//! (judge.rs:321-348: 2,400-char passages, 12-chunk cap, early exit at
//! support >= 0.95) — and times every call. No production code is touched.
//!
//! HARNESS VALIDITY GATE (README G3): the provider MUST be
//! `SplitInferenceProvider`. `forced_choice_ab` sends `structured_output`
//! with `x_forced_choice` + max_tokens 1 and parses a calibrated
//! `{"A":p,"B":p}` distribution; only this provider emits the
//! `response_format: json_schema` envelope the daemon needs for that. A
//! naive /v1/chat/completions client gets one greedy token and silently
//! corrupts every verdict.
//!
//! Run (daemon on :9741; nodes fixture from scripts/sp3_dump_nodes.py):
//!   cargo run -p sovereign-inference --example sp3_judge_probe -- \
//!     research/enrichment-spikes/data/sp3_nodes_obsidian.jsonl \
//!     <chat_model_id> \
//!     research/enrichment-spikes/runs/sp3/<model>/results.jsonl \
//!     [limit]

use serde::Deserialize;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_inference::remote::SplitInferenceProvider;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

const MAX_CLAIMS: usize = 4; // production default claim budget for short answers
const PASSAGE_CAP_CHARS: usize = 2_400; // judge.rs:322
const CHUNK_CAP: usize = 12; // judge.rs:318
const EARLY_EXIT_SUPPORT: f64 = 0.95; // judge.rs:344

#[derive(Deserialize)]
struct NodeRow {
    node_id: String,
    level: i64,
    summary: String,
    member_chunk_ids: Vec<serde_json::Value>,
    member_texts: Vec<String>,
}

fn claim_extract_request(summary: &str, model: &str) -> CompletionRequest {
    // Template mirrors extract_claim_list verbatim; the "question" analog for
    // a RAPTOR summary is the summarization instruction itself.
    let question = "Summarize the passages.";
    let prompt = format!(
        "A user asked: {}\n\nAn assistant wrote this long answer:\n\"\"\"\n{}\n\"\"\"\n\n\
         List the SPECIFIC factual claims the answer asserts — concrete who/what/when \
         relations a passage could confirm or refute (names, identifications, events, \
         attributions). One claim per line, each a short standalone sentence naming \
         both sides of the relation. At most {n} lines; pick the most load-bearing \
         claims, and when the answer is long, sample across ALL of it — include \
         specific claims from the later sections, not only the opening. Skip \
         opinions, summaries of the question, and anything the answer itself flags \
         as not from the sources.\n\
         Reply with exactly NO_CLAIM if there are no such checkable claims.",
        question,
        summary.chars().take(14_000).collect::<String>(),
        n = MAX_CLAIMS,
    );
    CompletionRequest {
        prompt,
        system_message: Some(format!(
            "You extract claims precisely. Reply with up to {MAX_CLAIMS} lines, or NO_CLAIM."
        )),
        preferred_speed: Speed::Slow,
        max_tokens: Some((MAX_CLAIMS * 48).max(160)),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    }
}

fn forced_choice_request(passage: &str, claim: &str, model: &str) -> CompletionRequest {
    let prompt = format!(
        "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
         CLAIM: {claim}\n\n\
         Does the passage state or clearly imply this claim? Paraphrase counts; \
         the passage merely mentioning the people or things involved, without \
         establishing the claimed connection between them, does NOT count.\n\n\
         Answer with exactly one letter — A = the passage supports the claim, \
         B = it does not."
    );
    CompletionRequest {
        prompt,
        system_message: Some("You are a careful classifier. Answer with a single letter.".into()),
        preferred_speed: Speed::Slow,
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    }
}

/// The daemon intermittently 503s under fast-slot contention ("MTP
/// process(verify) failed", "Decode Error -3") — observed ~4% of calls in the
/// smoke run. A dropped call deflates the cost table (a failed extraction
/// makes a node look cheap), so retry with backoff and COUNT the retries;
/// the retry rate is itself part of the SP3 answer.
async fn complete_with_retry(
    provider: &Arc<dyn InferenceProvider>,
    req: &CompletionRequest,
    retries: &mut u64,
) -> Result<sovereign_core::types::CompletionResponse, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            *retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
        }
        match provider.complete(req).await {
            Ok(resp) => return Ok(resp),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn parse_claims(text: &str) -> Vec<String> {
    let t = text.trim();
    if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
        return Vec::new();
    }
    t.lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
        .map(|l| {
            l.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(['.', ')'])
                .trim()
                .to_string()
        })
        .filter(|l| l.len() > 12)
        .take(MAX_CLAIMS)
        .collect()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let nodes_path = args.next().expect("arg 1: nodes JSONL (sp3_dump_nodes.py output)");
    let model = args.next().expect("arg 2: chat model id");
    let out_path = args.next().expect("arg 3: results JSONL path");
    let limit: usize = args.next().map(|s| s.parse().expect("limit")).unwrap_or(usize::MAX);

    let base = std::env::var("SOVEREIGN_DAEMON_URL")
        .unwrap_or_else(|_| "http://localhost:9741".to_string());
    let v1 = format!("{base}/v1");
    let provider: Arc<dyn InferenceProvider> =
        match sovereign_inference::remote::fetch_manifest(&base, None).await {
            Some(m) => Arc::new(SplitInferenceProvider::from_manifest(
                &v1,
                &m,
                model.clone(),
                "qwen-embedding-0.6b".to_string(),
            )),
            None => panic!("daemon manifest unreachable at {base} — is the daemon up?"),
        };

    let raw = std::fs::read_to_string(&nodes_path).expect("read nodes JSONL");
    let nodes: Vec<NodeRow> = raw
        .lines()
        .map(|l| serde_json::from_str(l).expect("parse node row"))
        .collect();
    let n_total = nodes.len().min(limit);
    eprintln!("model={model}  nodes={} (of {})  out={out_path}", n_total, nodes.len());

    std::fs::create_dir_all(std::path::Path::new(&out_path).parent().unwrap()).unwrap();
    let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).expect("create out"));

    let mut tot_calls = 0u64;
    let mut tot_claims = 0u64;
    let mut tot_retries = 0u64;
    let mut tot_hard_fails = 0u64;
    let mut node_secs: Vec<f64> = Vec::with_capacity(n_total);
    let run_start = Instant::now();

    for (i, node) in nodes.iter().take(limit).enumerate() {
        let t_node = Instant::now();
        let mut calls = 0u64;
        let mut extract_failed = false;

        let t_extract = Instant::now();
        let claims = {
            calls += 1;
            match complete_with_retry(
                &provider,
                &claim_extract_request(&node.summary, &model),
                &mut tot_retries,
            )
            .await
            {
                Ok(resp) => parse_claims(&resp.text),
                Err(e) => {
                    eprintln!("node {} claim extraction failed after retries: {e}", node.node_id);
                    tot_hard_fails += 1;
                    extract_failed = true;
                    Vec::new()
                }
            }
        };
        let extract_ms = t_extract.elapsed().as_millis() as u64;

        let mut claim_rows = Vec::new();
        for claim in &claims {
            let t_claim = Instant::now();
            let mut max_support = 0.0f64;
            let mut checked = 0usize;
            for text in node.member_texts.iter().take(CHUNK_CAP) {
                let passage: String = text.chars().take(PASSAGE_CAP_CHARS).collect();
                calls += 1;
                match complete_with_retry(
                    &provider,
                    &forced_choice_request(&passage, claim, &model),
                    &mut tot_retries,
                )
                .await
                {
                    Ok(resp) => {
                        let dist: std::collections::HashMap<String, f64> =
                            match serde_json::from_str(resp.text.trim()) {
                                Ok(d) => d,
                                Err(e) => {
                                    // A non-distribution reply means the forced-choice
                                    // envelope did not reach the daemon — the run is
                                    // INVALID per README G3. Fail loudly, don't record.
                                    panic!(
                                        "forced-choice reply is not a distribution \
                                         ({e}): {:?} — envelope not honored, run invalid",
                                        resp.text
                                    );
                                }
                            };
                        let a = dist.get("A").copied().unwrap_or(0.0);
                        let b = dist.get("B").copied().unwrap_or(0.0);
                        let denom = a + b;
                        let support = if denom > 0.0 { a / denom } else { 0.0 };
                        if support > max_support {
                            max_support = support;
                        }
                        checked += 1;
                        if max_support >= EARLY_EXIT_SUPPORT {
                            break;
                        }
                    }
                    Err(e) => {
                        tot_hard_fails += 1;
                        eprintln!("node {} forced-choice failed after retries: {e}", node.node_id);
                    }
                }
            }
            claim_rows.push(serde_json::json!({
                "claim": claim,
                "supported": max_support >= 0.5,
                "max_support": (max_support * 1000.0).round() / 1000.0,
                "chunks_checked": checked,
                "ms": t_claim.elapsed().as_millis(),
            }));
        }
        tot_claims += claims.len() as u64;
        tot_calls += calls;
        let secs = t_node.elapsed().as_secs_f64();
        node_secs.push(secs);

        // Stream B seed row: (member_chunks, claim, verdict) per scored claim,
        // plus node-level timing for the cost table.
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "node_id": node.node_id,
                "level": node.level,
                "member_chunk_ids": node.member_chunk_ids,
                "n_member_texts": node.member_texts.len(),
                "claims": claim_rows,
                "n_claims": claims.len(),
                "extract_failed": extract_failed,
                "calls": calls,
                "extract_ms": extract_ms,
                "node_secs": (secs * 100.0).round() / 100.0,
            })
        )
        .unwrap();
        if (i + 1) % 10 == 0 {
            out.flush().unwrap();
            let elapsed = run_start.elapsed().as_secs_f64();
            eprintln!(
                "[{}/{}] {:.1}s elapsed, {:.2}s/node avg, {} calls",
                i + 1,
                n_total,
                elapsed,
                elapsed / (i + 1) as f64,
                tot_calls
            );
        }
    }
    out.flush().unwrap();

    node_secs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = node_secs.len().max(1);
    let mean = node_secs.iter().sum::<f64>() / n as f64;
    let p50 = node_secs[n / 2.min(n - 1)];
    println!("\n=== SP3 cost table (model={model}) ===");
    println!("nodes scored:      {}", node_secs.len());
    println!("claims/node:       {:.2}", tot_claims as f64 / n as f64);
    println!("calls/node:        {:.2}", tot_calls as f64 / n as f64);
    println!("s/node mean:       {mean:.2}   p50: {p50:.2}");
    for (label, count) in [("obsidian 608", 608u64), ("conv-anthropic 1262", 1262), ("sep-scale 11181", 11181)] {
        println!("min/corpus @ {label:>20}: {:.1}", mean * count as f64 / 60.0);
    }
    println!(
        "total wall: {:.1}s  total calls: {tot_calls}  retries: {tot_retries}  hard fails: {tot_hard_fails}",
        run_start.elapsed().as_secs_f64()
    );
}
