// SPDX-License-Identifier: AGPL-3.0-or-later
//! P5.1 probe — budgeted tree-descent answerer vs one-shot top-K (gate G8,
//! `research/enrichment-spikes/README.md`).
//!
//! Nothing in production walks `children_node_ids` — both consumers
//! fetch-all-then-filter. This probe descends the RAPTOR tree top-down with
//! an LLM pick-next-children call per hop under a relevance-call budget
//! (LazyGraphRAG-style), answers from the reached leaves' member chunks, and
//! compares against one-shot cosine top-K at the SAME evidence-token budget.
//! Both arms answer with the same model + prompt; every hop is logged as
//! JSONL. Report-only: fact coverage + hop logs; no downstream commitment.
//!
//! Usage (daemon on :9741, tree built by `enrich raptor sep --titles-file
//! data/sp2_bank_articles.txt --group-by-article`):
//!   cargo run -p sovereign-inference --example p51_descent -- \
//!     research/enrichment-spikes/data <chat_model_id> \
//!     research/enrichment-spikes/runs/p51 [max_llm_calls] [evidence_tokens]

use serde::Deserialize;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_inference::remote::SplitInferenceProvider;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

const FRONTIER_CAP: usize = 48; // candidates shown per pick call (must cover the forest roots)
const SUMMARY_CAP_CHARS: usize = 420; // per-candidate summary excerpt
const PICKS_PER_HOP: usize = 2;

#[derive(Deserialize)]
struct NodeRow {
    node_id: String,
    level: i64,
    summary: String,
    children_node_ids: Vec<String>,
    direct_member_chunk_ids: Vec<String>,
}

#[derive(Deserialize)]
struct ChunkRow {
    id: String,
    content: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct Question {
    id: String,
    bank: String,
    question: String,
    expected_facts: Vec<String>,
    query_embedding: Vec<f32>,
}

fn approx_tokens(s: &str) -> usize {
    s.len() / 4
}

fn pick_request(question: &str, cands: &[(usize, &str)], model: &str) -> CompletionRequest {
    let list = cands
        .iter()
        .map(|(i, s)| {
            format!(
                "{}. {}",
                i,
                s.chars().take(SUMMARY_CAP_CHARS).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    CompletionRequest {
        prompt: format!(
            "Question: {question}\n\nSection summaries:\n{list}\n\n\
             Which {PICKS_PER_HOP} sections are most likely to contain material that \
             answers the question? Reply with ONLY the numbers, comma-separated."
        ),
        system_message: Some("You route a search. Reply with only numbers.".to_string()),
        preferred_speed: Speed::Slow,
        max_tokens: Some(16),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    }
}

fn answer_request(question: &str, passages: &[&str], model: &str) -> CompletionRequest {
    let evidence = passages
        .iter()
        .enumerate()
        .map(|(i, p)| format!("[{}] {}", i + 1, p))
        .collect::<Vec<_>>()
        .join("\n\n");
    CompletionRequest {
        prompt: format!("Evidence passages:\n\n{evidence}\n\nQuestion: {question}"),
        system_message: Some(
            "Answer the question thoroughly using ONLY the evidence passages. \
             Name the specific positions, thinkers, and arguments the evidence supports."
                .to_string(),
        ),
        preferred_speed: Speed::Slow,
        max_tokens: Some(700),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    }
}

async fn complete_retry(
    provider: &Arc<dyn InferenceProvider>,
    req: &CompletionRequest,
) -> Result<String, String> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(700 * attempt)).await;
        }
        match provider.complete(req).await {
            Ok(r) => return Ok(r.text),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn parse_picks(reply: &str, valid: &[usize]) -> Vec<usize> {
    let mut picks: Vec<usize> = Vec::new();
    let mut cur = String::new();
    for c in reply.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse::<usize>() {
                if valid.contains(&n) && !picks.contains(&n) {
                    picks.push(n);
                }
            }
            cur.clear();
        }
    }
    picks.truncate(PICKS_PER_HOP);
    picks
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb).max(1e-12)
}

fn facts_hit(facts: &[String], text: &str) -> usize {
    let hay = text.to_lowercase();
    facts
        .iter()
        .filter(|f| hay.contains(&f.to_lowercase()))
        .count()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().expect("arg 1: data dir (p51_dump.py output)");
    let model = args.next().expect("arg 2: chat model id");
    let out_dir = args.next().expect("arg 3: out dir");
    let max_calls: usize = args.next().map(|s| s.parse().unwrap()).unwrap_or(8);
    let evidence_budget: usize = args.next().map(|s| s.parse().unwrap()).unwrap_or(3000);

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

    let read_lines = |name: &str| -> Vec<String> {
        std::fs::read_to_string(format!("{data_dir}/{name}"))
            .unwrap_or_else(|e| panic!("read {name}: {e}"))
            .lines()
            .map(|l| l.to_string())
            .collect()
    };
    let nodes: Vec<NodeRow> = read_lines("p51_nodes.jsonl")
        .iter()
        .map(|l| serde_json::from_str(l).expect("node row"))
        .collect();
    let chunks: Vec<ChunkRow> = read_lines("p51_chunks.jsonl")
        .iter()
        .map(|l| serde_json::from_str(l).expect("chunk row"))
        .collect();
    let questions: Vec<Question> = serde_json::from_str(
        &std::fs::read_to_string(format!("{data_dir}/p51_questions.json")).expect("questions"),
    )
    .expect("questions json");

    let by_id: HashMap<&str, &NodeRow> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let chunk_by_id: HashMap<&str, &ChunkRow> = chunks.iter().map(|c| (c.id.as_str(), c)).collect();
    // The RAPTOR build with --group-by-article yields a FOREST: per-article
    // subtrees whose tops are parentless at whatever level clustering reached.
    // Descent must start from every parentless node, not just max-level ones.
    let child_ids: std::collections::HashSet<&str> = nodes
        .iter()
        .flat_map(|n| n.children_node_ids.iter())
        .map(|s| s.as_str())
        .collect();
    let roots: Vec<&NodeRow> = nodes
        .iter()
        .filter(|n| n.level > 0 && !child_ids.contains(n.node_id.as_str()))
        .collect();
    eprintln!(
        "nodes={} forest_roots={} chunks={} questions={} budget: {} calls / {} evidence tokens",
        nodes.len(),
        roots.len(),
        chunks.len(),
        questions.len(),
        max_calls,
        evidence_budget
    );

    std::fs::create_dir_all(&out_dir).unwrap();
    let mut hops_out =
        std::io::BufWriter::new(std::fs::File::create(format!("{out_dir}/hops.jsonl")).unwrap());
    let mut results_out =
        std::io::BufWriter::new(std::fs::File::create(format!("{out_dir}/results.jsonl")).unwrap());

    let mut agg: HashMap<(String, String), (usize, usize, usize)> = HashMap::new(); // (bank,arm)->(hit,total,n)

    for q in &questions {
        // ── Arm descent ────────────────────────────────────────────────
        let t0 = Instant::now();
        let mut frontier: Vec<&NodeRow> = roots.clone();
        let mut reached_leaves: Vec<&NodeRow> = Vec::new();
        let mut calls = 0usize;
        let mut hop = 0usize;
        while calls < max_calls && !frontier.is_empty() {
            // Leaves in the frontier are collected, non-leaves compete for picks.
            let (leaves, inner): (Vec<&NodeRow>, Vec<&NodeRow>) = frontier
                .iter()
                .partition(|n| n.children_node_ids.is_empty());
            reached_leaves.extend(leaves);
            if inner.is_empty() {
                break;
            }
            let cands: Vec<(usize, &str)> = inner
                .iter()
                .take(FRONTIER_CAP)
                .enumerate()
                .map(|(i, n)| (i + 1, n.summary.as_str()))
                .collect();
            let valid: Vec<usize> = cands.iter().map(|(i, _)| *i).collect();
            let req = pick_request(&q.question, &cands, &model);
            let reply = complete_retry(&provider, &req).await.unwrap_or_else(|e| {
                eprintln!("PICK CALL FAILED {} hop {hop}: {e}", q.id);
                String::new()
            });
            calls += 1;
            let picks = parse_picks(&reply, &valid);
            // Fallback: no parseable pick keeps the first candidates.
            let picked: Vec<&NodeRow> = if picks.is_empty() {
                inner.iter().take(PICKS_PER_HOP).copied().collect()
            } else {
                picks.iter().map(|&i| inner[i - 1]).collect()
            };
            writeln!(
                hops_out,
                "{}",
                serde_json::json!({
                    "question_id": q.id, "hop": hop, "call": calls,
                    "frontier": inner.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
                    "picked": picked.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
                    "raw_reply": reply.trim(),
                })
            )
            .unwrap();
            hops_out.flush().unwrap();
            frontier = picked
                .iter()
                .flat_map(|n| n.children_node_ids.iter())
                .filter_map(|id| by_id.get(id.as_str()).copied())
                .collect();
            hop += 1;
        }
        reached_leaves.extend(frontier.iter().filter(|n| n.children_node_ids.is_empty()));

        let mut evidence: Vec<&str> = Vec::new();
        let mut used = 0usize;
        'outer: for leaf in &reached_leaves {
            for cid in &leaf.direct_member_chunk_ids {
                if let Some(c) = chunk_by_id.get(cid.as_str()) {
                    let t = approx_tokens(&c.content);
                    if used + t > evidence_budget {
                        break 'outer;
                    }
                    evidence.push(&c.content);
                    used += t;
                }
            }
        }
        let descent_evidence_text = evidence.join("\n");
        let answer = if evidence.is_empty() {
            String::new()
        } else {
            complete_retry(&provider, &answer_request(&q.question, &evidence, &model))
                .await
                .unwrap_or_else(|e| {
                    eprintln!("ANSWER CALL FAILED {} descent: {e}", q.id);
                    String::new()
                })
        };
        let descent_ms = t0.elapsed().as_millis();
        emit(
            &mut results_out,
            &mut agg,
            q,
            "descent",
            &answer,
            &descent_evidence_text,
            used,
            calls + 1,
            descent_ms,
        );

        // ── Arm one-shot top-K ─────────────────────────────────────────
        let t0 = Instant::now();
        let mut scored: Vec<(f32, &ChunkRow)> = chunks
            .iter()
            .map(|c| (cosine(&q.query_embedding, &c.embedding), c))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let mut evidence: Vec<&str> = Vec::new();
        let mut used = 0usize;
        for (_, c) in &scored {
            let t = approx_tokens(&c.content);
            if used + t > evidence_budget {
                break;
            }
            evidence.push(&c.content);
            used += t;
        }
        let oneshot_evidence_text = evidence.join("\n");
        let answer = complete_retry(&provider, &answer_request(&q.question, &evidence, &model))
            .await
            .unwrap_or_else(|e| {
                eprintln!("ANSWER CALL FAILED {} oneshot: {e}", q.id);
                String::new()
            });
        let oneshot_ms = t0.elapsed().as_millis();
        emit(
            &mut results_out,
            &mut agg,
            q,
            "oneshot",
            &answer,
            &oneshot_evidence_text,
            used,
            1,
            oneshot_ms,
        );
        eprintln!("done {}", q.id);
    }

    eprintln!("\n== aggregate (facts in answer / total) ==");
    let mut keys: Vec<_> = agg.keys().cloned().collect();
    keys.sort();
    for k in keys {
        let (hit, total, n) = agg[&k];
        eprintln!(
            "{:20} {:8}  {}/{} = {:.4}  (n={})",
            k.0,
            k.1,
            hit,
            total,
            hit as f64 / total as f64,
            n
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut impl std::io::Write,
    agg: &mut HashMap<(String, String), (usize, usize, usize)>,
    q: &Question,
    arm: &str,
    answer: &str,
    evidence_text: &str,
    evidence_tokens: usize,
    llm_calls: usize,
    wall_ms: u128,
) {
    let in_answer = facts_hit(&q.expected_facts, answer);
    let in_evidence = facts_hit(&q.expected_facts, evidence_text);
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "question_id": q.id, "bank": q.bank, "arm": arm,
            "facts_total": q.expected_facts.len(),
            "facts_in_answer": in_answer,
            "facts_in_evidence": in_evidence,
            "evidence_tokens": evidence_tokens,
            "llm_calls": llm_calls,
            "wall_ms": wall_ms,
            "answer": answer,
        })
    )
    .unwrap();
    out.flush().unwrap();
    let e = agg
        .entry((q.bank.clone(), arm.to_string()))
        .or_insert((0, 0, 0));
    e.0 += in_answer;
    e.1 += q.expected_facts.len();
    e.2 += 1;
}
