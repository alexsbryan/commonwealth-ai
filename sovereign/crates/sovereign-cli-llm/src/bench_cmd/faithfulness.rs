// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench faithfulness` — the T1 P0.3 faithfulness-lane orchestrator.
//!
//! Walks a corpus's RAPTOR nodes (`conv_raptor_nodes`), decomposes each
//! node summary into checkable claims via the production
//! `extract_claim_list` register, judges every claim against the node's
//! own member-chunk texts via the production per-chunk support register
//! (`claim_chunk_support` — the gate's `forced_choice_ab` pass), and
//! writes one JSONL row per judged claim. The pure scorer
//! (`sovereign_eval::faithfulness`) turns those rows into the
//! per-corpus unsupported-claim rate; `svrn bench gate faithfulness`
//! diffs that rate against the committed `LaneBaseline` (TRACKED run +
//! HARD gate twin, same shape as chaos/mechanism).
//!
//! Row schema: the superset of verifier-v0 Stream B's `HarvestItem`
//! proposed to the node-44a stream (see the 2026-07-31 schema decision
//! note) — sealed member TEXTS ride in every row so each tuple is
//! self-contained training substrate. Rows land at `--out` (a run
//! artifact); appending into `sovereign/bench/faithfulness/` (the
//! shared training feed) stays a separate, deliberate step pending the
//! Stream B ack.
//!
//! Judge economics (SP3): full scoring at or below `--full-threshold`
//! nodes (default 1,500); above it a seeded level-stratified sample at
//! `--rate` (default 0.12). Ops caveat (SP3): a 503-burst mid-run is a
//! daemon Metal-OOM wedge signal, not data — the run counts judge
//! failures and says so.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{claim_chunk_support, extract_claim_list};
use sovereign_core::traits::InferenceProvider;
use sovereign_eval::faithfulness::{plan_judge_sample, score, ClaimRecord, NodeMeta, SampleMode};
use sovereign_inference::remote::RemoteApiProvider;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const PROVIDER_CTX: u32 = 8192;
/// Production parity: the gate checks at most 12 chunks per claim
/// (judge.rs `cap`), and stops early once support is decisive
/// (judge.rs early-exit, mirrored by SP3's probe).
const CHUNK_CAP: usize = 12;
const EARLY_EXIT_SUPPORT: f64 = 0.95;
const SUPPORTED_TAU: f64 = 0.5;
/// The claim-extraction "question" for a RAPTOR summary — the
/// summarization instruction itself (SP3 register; also emitted as the
/// row's `question` unless the Stream B ack picks a different shape).
const NODE_QUESTION: &str = "Summarize the passages.";

const HELP: Help = Help {
    command: "svrn bench faithfulness",
    summary: "Per-corpus unsupported-claim rate over RAPTOR node summaries (T1 P0.3 faithfulness lane).",
    sections: &[
        HelpSection::Usage(
            "svrn bench faithfulness run --corpus <id> [--out <path.jsonl>] [--model <stem>] \
             [--base-url <url>] [--max-claims N] [--rate F] [--full-threshold N] [--seed N] [--limit N]",
        ),
        HelpSection::Notes(
            "run: loads the corpus's RAPTOR nodes, extracts claims from each summary \
             (production extract_claim_list register), judges each claim against the \
             node's member-chunk texts (production per-chunk forced-choice register), \
             and writes one JSONL row per claim. Gate twin: `svrn bench gate \
             faithfulness --report <out.jsonl> --id <corpus>`. Needs the daemon at \
             --base-url with the judge model resident; bench never offloads \
             (LocalOnly). --limit caps judged nodes for smoke runs — the artifact is \
             then NOT baseline-worthy and the run says so.",
        ),
    ],
};

pub async fn cmd_faithfulness(rest: &[String]) -> i32 {
    match rest.first().map(String::as_str) {
        Some("run") => run(&rest[1..]).await,
        Some("--help") | Some("-h") | None => {
            help::print(&HELP);
            if rest.is_empty() { 2 } else { 0 }
        }
        Some(other) => {
            eprintln!("error: unknown subcommand `{other}` (expected: run)");
            2
        }
    }
}

/// One emitted row — the HarvestItem-superset shape from the schema
/// proposal note. `sovereign_eval::faithfulness::ClaimRecord` reads the
/// subset it scores; the Stream B side reads the HarvestItem core.
#[derive(Serialize)]
struct Row<'a> {
    id: String,
    corpus_id: &'a str,
    question: &'a str,
    claim: &'a str,
    evidence_chunks: &'a [String],
    evidence_chunk_ids: &'a [String],
    verdict: &'a str,
    max_support: f64,
    chunks_checked: usize,
    judge_model: &'a str,
    node_id: &'a str,
    level: i64,
    sampling: &'a str,
}

struct NodeWork {
    node_id: String,
    level: i64,
    summary: String,
    chunk_ids: Vec<u64>,
}

async fn run(rest: &[String]) -> i32 {
    let mut corpus_arg: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut model = sovereign_core::role::default_profile_for(sovereign_core::role::Role::Critic)
        .preferred_tier
        .model_stem()
        .to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut max_claims: usize = 4;
    let mut rate: f64 = 0.12;
    let mut full_threshold: usize = 1500;
    let mut seed: u64 = 17;
    let mut limit: Option<usize> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--corpus" => corpus_arg = Some(val!("--corpus")),
            "--out" => out = Some(PathBuf::from(val!("--out"))),
            "--model" => model = val!("--model"),
            "--base-url" => base_url = val!("--base-url"),
            "--max-claims" => match val!("--max-claims").parse() {
                Ok(v) if v > 0 => max_claims = v,
                _ => {
                    eprintln!("error: --max-claims must be a positive integer");
                    return 2;
                }
            },
            "--rate" => match val!("--rate").parse::<f64>() {
                Ok(v) if v > 0.0 && v <= 1.0 => rate = v,
                _ => {
                    eprintln!("error: --rate must be in (0, 1]");
                    return 2;
                }
            },
            "--full-threshold" => match val!("--full-threshold").parse() {
                Ok(v) => full_threshold = v,
                _ => {
                    eprintln!("error: --full-threshold must be an integer");
                    return 2;
                }
            },
            "--seed" => match val!("--seed").parse() {
                Ok(v) => seed = v,
                _ => {
                    eprintln!("error: --seed must be a u64");
                    return 2;
                }
            },
            "--limit" => match val!("--limit").parse() {
                Ok(v) if v > 0 => limit = Some(v),
                _ => {
                    eprintln!("error: --limit must be a positive integer");
                    return 2;
                }
            },
            "--help" | "-h" => {
                help::print(&HELP);
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(corpus_arg) = corpus_arg else {
        eprintln!("error: --corpus <id> is required");
        return 2;
    };

    // Resolve corpus + open stores (same path derivation as enrich raptor).
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_indexes();
    let corpus_id = match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, &corpus_arg) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let data_dir = sovereign_contracts::rebrand::data_dir();
    let db_path = sovereign_contracts::rebrand::state_db_path(&data_dir);
    let store = match sovereign_store::sqlite::SqliteStateStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: open {}: {e}", db_path.display());
            return 1;
        }
    };
    let nodes = match store.list_corpus_raptor_nodes(&corpus_id, 0).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: list raptor nodes for {corpus_id}: {e}");
            return 1;
        }
    };
    if nodes.is_empty() {
        eprintln!(
            "error: corpus `{corpus_id}` has no RAPTOR nodes in {} — build the tier first \
             (`svrn enrich raptor`); an empty walk verifies nothing and is not a pass",
            db_path.display()
        );
        return 4;
    }

    // Parse member ids; drop synthetic tiny-doc sentinel nodes
    // (primary_entities == [] && coherence ≈ 1.0 — single-node "trees").
    let mut work: Vec<NodeWork> = Vec::new();
    for n in &nodes {
        let entities: Vec<String> =
            serde_json::from_str(&n.primary_entities_json).unwrap_or_default();
        if entities.is_empty() && (n.cluster_coherence - 1.0).abs() < 1e-6 {
            continue;
        }
        let ids_json = n
            .direct_member_chunk_ids_json
            .as_deref()
            .unwrap_or(&n.evidence_chunk_ids_json);
        let chunk_ids: Vec<u64> = serde_json::from_str(ids_json).unwrap_or_default();
        if chunk_ids.is_empty() || n.summary.trim().is_empty() {
            continue;
        }
        work.push(NodeWork {
            node_id: n.node_id.clone(),
            level: n.level,
            summary: n.summary.clone(),
            chunk_ids,
        });
    }
    let metas: Vec<NodeMeta> = work
        .iter()
        .map(|w| NodeMeta { node_id: w.node_id.clone(), level: w.level as u32 })
        .collect();
    let plan = plan_judge_sample(&metas, full_threshold, rate, seed);
    let sampling_label = match plan.mode {
        SampleMode::Full => "full".to_string(),
        SampleMode::Stratified { rate, seed } => format!("stratified(rate={rate},seed={seed})"),
    };
    let selected: std::collections::BTreeSet<&str> =
        plan.selected.iter().map(String::as_str).collect();
    let mut todo: Vec<&NodeWork> = work.iter().filter(|w| selected.contains(&*w.node_id)).collect();
    if let Some(cap) = limit {
        todo.truncate(cap);
        eprintln!("--limit {cap}: smoke run — artifact is NOT baseline-worthy");
    }
    eprintln!(
        "faithfulness: corpus={corpus_id} nodes={} (of {} total, {} sentinel-filtered) \
         sampling={sampling_label} judge={model}",
        todo.len(),
        nodes.len(),
        nodes.len() - work.len(),
    );

    // Resolve member texts in one batched read.
    let index_path = indexes_dir.join(&corpus_id);
    let index = match corpus_engine::index::CorpusIndex::open(&index_path).await {
        Ok(ix) => ix,
        Err(e) => {
            eprintln!("error: open corpus index {}: {e}", index_path.display());
            return 1;
        }
    };
    let all_ids: Vec<u64> = {
        let mut v: Vec<u64> = todo.iter().flat_map(|w| w.chunk_ids.iter().copied()).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let texts: BTreeMap<u64, String> = match index.chunks_by_ids(&all_ids).await {
        Ok(rows) => rows.into_iter().map(|r| (r.id, r.content)).collect(),
        Err(e) => {
            eprintln!("error: read member chunks: {e}");
            return 1;
        }
    };

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &model, PROVIDER_CTX));

    let out_path = out.unwrap_or_else(|| {
        PathBuf::from(format!("target/ci-bench/faithfulness-{corpus_id}.jsonl"))
    });
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut lines: Vec<String> = Vec::new();
    let mut records: Vec<ClaimRecord> = Vec::new();
    let mut n_no_claim = 0usize;
    let mut n_extract_fail = 0usize;
    let mut n_judge_fail = 0usize;

    for (done, w) in todo.iter().enumerate() {
        let member_texts: Vec<String> = w
            .chunk_ids
            .iter()
            .filter_map(|id| texts.get(id).cloned())
            .collect();
        if member_texts.is_empty() {
            continue;
        }
        let member_ids: Vec<String> = w.chunk_ids.iter().map(u64::to_string).collect();

        let claims = match extract_claim_list(
            &provider,
            NODE_QUESTION,
            &w.summary,
            max_claims,
            ShardingPrivacy::LocalOnly,
        )
        .await
        {
            Some(c) if c.is_empty() => {
                n_no_claim += 1;
                continue;
            }
            Some(c) => c,
            None => {
                n_extract_fail += 1;
                continue;
            }
        };

        for (ci, claim) in claims.iter().enumerate() {
            let mut max_support = 0.0f64;
            let mut checked = 0usize;
            for passage in member_texts.iter().take(CHUNK_CAP) {
                match claim_chunk_support(&provider, passage, claim, ShardingPrivacy::LocalOnly)
                    .await
                {
                    Some(support) => {
                        checked += 1;
                        if support > max_support {
                            max_support = support;
                        }
                        if max_support >= EARLY_EXIT_SUPPORT {
                            break;
                        }
                    }
                    None => {}
                }
            }
            if checked == 0 {
                // Judge unavailable for every probe of this claim — a
                // fabricated verdict would poison both the rate and the
                // training feed. Drop the claim, count the failure.
                n_judge_fail += 1;
                continue;
            }
            let supported = max_support >= SUPPORTED_TAU;
            let row = Row {
                id: format!("{corpus_id}/{}/c{ci}", w.node_id),
                corpus_id: &corpus_id,
                question: NODE_QUESTION,
                claim,
                evidence_chunks: &member_texts,
                evidence_chunk_ids: &member_ids,
                verdict: if supported { "supported" } else { "unsupported" },
                max_support: (max_support * 1000.0).round() / 1000.0,
                chunks_checked: checked,
                judge_model: &model,
                node_id: &w.node_id,
                level: w.level,
                sampling: &sampling_label,
            };
            match serde_json::to_string(&row) {
                Ok(s) => lines.push(s),
                Err(e) => {
                    eprintln!("error: serialize row: {e}");
                    return 1;
                }
            }
            records.push(ClaimRecord {
                claim: claim.clone(),
                verdict: if supported {
                    sovereign_eval::faithfulness::ClaimVerdict::Supported
                } else {
                    sovereign_eval::faithfulness::ClaimVerdict::Unsupported
                },
                max_support,
                corpus_id: corpus_id.clone(),
                node_id: w.node_id.clone(),
                level: w.level as u32,
                judge_model: model.clone(),
            });
        }
        if (done + 1) % 25 == 0 {
            eprintln!("  … {}/{} nodes judged", done + 1, todo.len());
        }
    }

    if let Err(e) = std::fs::write(&out_path, lines.join("\n") + "\n") {
        eprintln!("error: write {}: {e}", out_path.display());
        return 1;
    }

    // Glassbox summary — same numbers the gate twin will compute.
    let failures = n_extract_fail + n_judge_fail;
    if failures > 0 {
        eprintln!(
            "warning: {n_extract_fail} extraction failure(s) + {n_judge_fail} claim(s) with zero \
             completed support probes. A 503-burst here usually means the daemon's Metal backend \
             wedged (SP3 incident) — restart the daemon and re-run before trusting the rate."
        );
    }
    for report in score(&records) {
        println!(
            "faithfulness {}: {} nodes, {} claims, {} unsupported — rate {:.3} ({} no-claim nodes)",
            report.corpus_id,
            report.n_nodes,
            report.n_claims,
            report.n_unsupported,
            report.unsupported_rate,
            n_no_claim,
        );
        for l in &report.per_level {
            println!(
                "  level {}: {}/{} unsupported ({:.3})",
                l.level, l.n_unsupported, l.n_claims, l.unsupported_rate
            );
        }
    }
    println!("wrote {} rows -> {}", lines.len(), out_path.display());
    if records.is_empty() {
        eprintln!("error: zero claims judged — nothing verified is not a pass");
        return 4;
    }
    // High judge-failure runs must not quietly feed the gate.
    if failures * 5 > records.len() {
        eprintln!("error: judge-failure rate too high — rate untrustworthy (daemon wedge?)");
        return 1;
    }
    0
}
