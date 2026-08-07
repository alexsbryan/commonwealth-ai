// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-bridge sizing gate — where does the second conversation rank?
//!
//! Report-only probe. Answers ONE question, on the real archive, through the
//! production hybrid search path:
//!
//! > For a candidate bridge pair — entity `E` appearing in exactly two
//! > conversations `A` and `B` — under a query that names `E`, where does
//! > conversation `B` land in the retrieval ranking?
//!
//! The three-way split is what sizes the two competing investments in
//! `sovereign/bench/conversation-bridge/README.md`:
//!
//! | `rank_B` | meaning |
//! |---|---|
//! | `<= 10` | cosine/FTS already surfaces B. No headroom for ANY entity mechanism. |
//! | `11..=20` | the operating window of the per-document PPR prior. B is inside the pool `rerank_conv_chunks_via_ppr` re-sorts, so the prior can promote it. |
//! | `> 20` or absent | B is outside the pool. PPR re-ranks in place and NEVER adds, so it provably cannot help. Only a real cross-document bridge could. |
//!
//! Pool size 20 = `KQ_MERGED_LIMIT` (`sovereign-core/src/runtime/prompts.rs:362`).
//! The deep path truncates to `KQ_MERGED_LIMIT + raptor_n`
//! (`retrieval_pipeline.rs:1919`) and PPR runs AFTER it
//! (`runtime/retrieval/mod.rs:285`).
//!
//! # Why three legs
//!
//! `CorpusIndex::search` gates its two legs independently
//! (`corpus-engine/src/index/search.rs:344-351`): an empty `query_text`
//! disables FTS, an empty embedding disables vector. Running all three
//! separates a result that would otherwise be unreadable — if B is already
//! found by the FTS leg alone (and by construction B *does* contain E's
//! surface form), then keyword search is doing the entity layer's job and
//! neither investment has a case. That decomposition is the point.
//!
//! # Instrument validation (ARCH §18.4) — read before any result
//!
//! `rank_A` is the POSITIVE CONTROL. Conversation A holds MORE mentions of E
//! than B does; a query naming E that cannot retrieve A is not exercising
//! retrieval at all, and that row's `rank_B` means nothing. The summary
//! reports the control pass rate and every aggregate is computed over
//! controlled rows ONLY. A low pass rate voids the run — report
//! `could-not-judge`, never "no effect".
//!
//! The query embedding MUST carry the instruction prefix production prepends
//! in `embed_query` (`model_family.rs:302-304`). `/v1/embeddings` does NOT add
//! it (`oicp-client/src/lib.rs:54-56`), so this probe adds it explicitly.
//! Without it the cosines are self-consistent but are not what retrieval sees.
//!
//! # Privacy
//!
//! Reads a real personal archive. Emits ONLY entity surface forms, conversation
//! UUIDs and ranks — never conversation content. Keep the output in scratchpad;
//! do not commit it.
//!
//! # Usage
//!
//! ```text
//! cargo run -p corpus-engine --features treesitter --example bridge_rank_probe \
//!   -- <sample.tsv> <out.tsv> [limit]
//! ```
//!
//! `sample.tsv` columns: `entity  label  convA  mentionsA  convB  mentionsB`

use std::collections::HashMap;
use std::path::PathBuf;

use corpus_engine::{CorpusIndex, ScoredChunk};

/// Production query prefix for Qwen3Embedding — `model_family.rs:302-304`.
/// `/v1/embeddings` does not apply it; we must.
const QUERY_PREFIX: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";

/// `KQ_MERGED_LIMIT` — the pool PPR re-ranks. `prompts.rs:362`.
const POOL: usize = 20;

const DAEMON: &str = "http://127.0.0.1:9741";

struct Candidate {
    entity: String,
    label: String,
    conv_a: String,
    m_a: i64,
    conv_b: String,
    m_b: i64,
}

/// Query phrasings. `bare` is the most entity-focused query possible and so is
/// the BEST case for finding B; `nat` reads like an actual bench question.
/// Reporting both stops a single phrasing choice from silently deciding the
/// outcome.
fn variants(entity: &str) -> Vec<(&'static str, String)> {
    vec![
        ("bare", entity.to_string()),
        ("nat", format!("What did we discuss about {entity}?")),
    ]
}

async fn embed_query(
    client: &reqwest::Client,
    text: &str,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "model": "qwen-embedding-0.6b",
        "input": format!("{QUERY_PREFIX}{text}"),
    });
    let resp: serde_json::Value = client
        .post(format!("{DAEMON}/v1/embeddings"))
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    let arr = resp["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| format!("bad embedding response: {resp}"))?;
    let v: Vec<f32> = arr
        .iter()
        .map(|x| x.as_f64().unwrap_or(0.0) as f32)
        .collect();
    if v.len() != 1024 {
        return Err(format!("dim mismatch: got {} want 1024", v.len()).into());
    }
    Ok(v)
}

/// Conversation-level rank = best (lowest) 1-based rank of any chunk carrying
/// that `source_doc_id`. 0 means "not found within `limit`".
fn rank_of(hits: &[ScoredChunk], conv: &str) -> usize {
    hits.iter()
        .position(|h| h.source_doc_id.as_deref() == Some(conv))
        .map(|i| i + 1)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No tracing subscriber — corpus-engine has no tracing-subscriber dep.
    // The dimension check that matters (query dims vs stored dims) is asserted
    // directly in `embed_query`, which is the only way this probe can silently
    // produce meaningless cosines.
    let mut args = std::env::args().skip(1);
    let sample_path = args
        .next()
        .expect("usage: bridge_rank_probe <sample.tsv> <out.tsv> [limit]");
    let out_path = args
        .next()
        .expect("usage: bridge_rank_probe <sample.tsv> <out.tsv> [limit]");
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);

    let dir =
        PathBuf::from(std::env::var("HOME")?).join(".sovereign/indexes/conversations-anthropic");
    let index = CorpusIndex::open(&dir).await?;
    let info = index.info().await?;
    eprintln!(
        "index: {} ({} chunks, model {:?})",
        info.corpus_id, info.chunk_count, info.embedding_model
    );

    let raw = std::fs::read_to_string(&sample_path)?;
    let cands: Vec<Candidate> = raw
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\t').collect();
            if p.len() < 6 {
                return None;
            }
            Some(Candidate {
                entity: p[0].to_string(),
                label: p[1].to_string(),
                conv_a: p[2].to_string(),
                m_a: p[3].parse().ok()?,
                conv_b: p[4].to_string(),
                m_b: p[5].parse().ok()?,
            })
        })
        .collect();
    eprintln!("candidates: {}", cands.len());

    let client = reqwest::Client::new();
    let mut out = String::from(
        "entity\tlabel\tvariant\tleg\tconvA\tconvB\tmA\tmB\trank_A\trank_B\tn_hits\tn_convs\tcontrol_ok\n",
    );
    // Cache embeddings — `bare` for one entity is identical across legs.
    let mut emb_cache: HashMap<String, Vec<f32>> = HashMap::new();

    for (i, c) in cands.iter().enumerate() {
        for (vname, qtext) in variants(&c.entity) {
            let emb = match emb_cache.get(&qtext) {
                Some(e) => e.clone(),
                None => {
                    let e = embed_query(&client, &qtext).await?;
                    emb_cache.insert(qtext.clone(), e.clone());
                    e
                }
            };
            // Three legs, gated by search.rs:344-351.
            let legs: Vec<(&str, &[f32], &str)> = vec![
                ("hybrid", &emb[..], qtext.as_str()),
                ("vector", &emb[..], ""),
                ("fts", &[], qtext.as_str()),
            ];
            for (leg, e, t) in legs {
                let hits = index.search(e, t, limit).await?;
                let rank_a = rank_of(&hits, &c.conv_a);
                let rank_b = rank_of(&hits, &c.conv_b);
                let n_convs = hits
                    .iter()
                    .filter_map(|h| h.source_doc_id.as_deref())
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let control_ok = if rank_a > 0 && rank_a <= POOL { 1 } else { 0 };
                out.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    c.entity,
                    c.label,
                    vname,
                    leg,
                    c.conv_a,
                    c.conv_b,
                    c.m_a,
                    c.m_b,
                    rank_a,
                    rank_b,
                    hits.len(),
                    n_convs,
                    control_ok
                ));
            }
        }
        if (i + 1) % 10 == 0 {
            eprintln!("  {}/{} candidates", i + 1, cands.len());
        }
    }

    std::fs::write(&out_path, out)?;
    eprintln!("wrote {out_path}");
    Ok(())
}
