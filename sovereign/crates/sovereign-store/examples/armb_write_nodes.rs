// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP2 Arm B writer: load extractive-summary node rows from JSONL and
//! persist them through the public `SqliteStateStore::save_conv_raptor_nodes`
//! seam (atomic per-conversation delete+insert, correct f32-BLOB encoding).
//!
//! The JSONL is produced by research/enrichment-spikes/scripts/armb_extractive.py.
//! Rows mirror `ConvRaptorNodeRow` with embeddings as f32 arrays.
//!
//! Usage:
//!   cargo run -p sovereign-store --example armb_write_nodes -- \
//!     <db_path> <jsonl_path> <expected_corpus_id>

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

use sovereign_core::conv_tiered::ConvRaptorNodeRow;
use sovereign_store::sqlite::SqliteStateStore;

#[derive(serde::Deserialize)]
struct JsonRow {
    node_id: String,
    corpus_id: String,
    conv_uuid: String,
    level: i64,
    summary: String,
    summary_embedding: Vec<f32>,
    centroid_embedding: Vec<f32>,
    children_node_ids_json: String,
    direct_member_chunk_ids_json: Option<String>,
    evidence_chunk_ids_json: String,
    quote_spans_json: String,
    primary_entities_json: String,
    cluster_coherence: f64,
    created_at: i64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, db_path, jsonl_path, expected_corpus] = &args[..] else {
        eprintln!("usage: armb_write_nodes <db_path> <jsonl_path> <expected_corpus_id>");
        std::process::exit(2);
    };

    let file = std::fs::File::open(jsonl_path)?;
    let mut by_conv: BTreeMap<String, Vec<ConvRaptorNodeRow>> = BTreeMap::new();
    let mut total = 0usize;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let r: JsonRow = serde_json::from_str(&line)?;
        assert_eq!(
            &r.corpus_id, expected_corpus,
            "row {} has corpus_id {}, expected {}",
            r.node_id, r.corpus_id, expected_corpus
        );
        total += 1;
        by_conv.entry(r.conv_uuid.clone()).or_default().push(ConvRaptorNodeRow {
            node_id: r.node_id,
            corpus_id: r.corpus_id,
            conv_uuid: r.conv_uuid,
            level: r.level,
            summary: r.summary,
            summary_embedding: r.summary_embedding,
            centroid_embedding: r.centroid_embedding,
            children_node_ids_json: r.children_node_ids_json,
            direct_member_chunk_ids_json: r.direct_member_chunk_ids_json,
            evidence_chunk_ids_json: r.evidence_chunk_ids_json,
            quote_spans_json: r.quote_spans_json,
            primary_entities_json: r.primary_entities_json,
            cluster_coherence: r.cluster_coherence,
            created_at: r.created_at,
            prompt_version: String::new(),
            summarizer_model: String::new(),
        });
    }

    let store = SqliteStateStore::open(Path::new(db_path))?;
    for (conv_uuid, nodes) in &by_conv {
        store
            .save_conv_raptor_nodes(expected_corpus, conv_uuid, nodes)
            .await?;
        println!("saved {:>3} nodes for {conv_uuid}", nodes.len());
    }
    println!(
        "done: {total} nodes across {} conversations written to {db_path}",
        by_conv.len()
    );
    Ok(())
}
