// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end smoke for the PRODUCTION GLiNER2 backend
//! (`sovereign_gliner::gliner2::Gliner2Extractor`), as opposed to
//! `gliner2_probe.rs` which drives a hand-rolled copy of the same graph.
//!
//! This exists because the module's unit tests cover span-NMS and the
//! generation guards — both pure — but nothing in `cargo test` touches
//! the 795 MB ONNX graph. Without this, "the port works" would rest on
//! the port compiling.
//!
//! What it proves, per run: the real graph loads through
//! `resolve_model_paths`, inference returns typed spans, throughput
//! matches the probe's, and NMS actually collapses the duplicate
//! overlapping spans SP1 observed (probe: 8.5 mentions/chunk with no
//! suppression).
//!
//! Run — `SOVEREIGN_GLINER_MODEL_DIR` must contain a
//! `gliner2-base-v1-onnx/` directory (or symlink) holding `model.onnx` +
//! `tokenizer.json`:
//!
//! ```text
//! cargo run --release -p sovereign-gliner --example gliner2_backend_smoke -- \
//!   research/enrichment-spikes/data/chunks_50.jsonl
//! ```

use sovereign_core::traits::EntityExtractor;
use sovereign_gliner::gliner2::{Gliner2Extractor, GLINER2_DEFAULT_THRESHOLD};
use sovereign_gliner::gliner_ner::{DEFAULT_LABELS, GLINER2_MODEL_ID};
use std::collections::BTreeMap;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "research/enrichment-spikes/data/chunks_50.jsonl".into());
    // Optional threshold override — the export's default is 0.5 and v1's
    // tuned value is 0.6, and the two heads are NOT on a shared scale, so
    // the crossover has to be measured rather than inherited.
    let threshold: f32 = std::env::args()
        .nth(2)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(GLINER2_DEFAULT_THRESHOLD);
    let raw = std::fs::read_to_string(&fixture)?;
    let chunks: Vec<String> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["text"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    eprintln!("fixture: {} chunks", chunks.len());

    let t = Instant::now();
    let extractor = Gliner2Extractor::new(GLINER2_MODEL_ID, DEFAULT_LABELS, threshold)?;
    eprintln!(
        "[backend] {} loaded in {:.2?} (threshold {threshold})",
        extractor.model_id(),
        t.elapsed()
    );

    // Warm once so the timed loop measures steady-state, matching the
    // probe's method.
    let _ = extractor.extract(&chunks[0])?;

    let t = Instant::now();
    let mut mentions = 0usize;
    let mut by_label: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    for (ci, c) in chunks.iter().enumerate() {
        let hits = extractor.extract(c)?;
        mentions += hits.len();
        for h in &hits {
            *by_label.entry(h.label.clone()).or_default() += 1;
        }
        if ci < 3 {
            samples.push(format!(
                "  [backend] chunk {ci}: {:?}",
                hits.iter()
                    .take(6)
                    .map(|h| format!("{}:{} ({:.2})", h.label, h.text, h.score))
                    .collect::<Vec<_>>()
            ));
        }
    }
    let el = t.elapsed().as_secs_f64();
    println!("backend_total_s        {el:.2}");
    println!("backend_chunks_per_s   {:.2}", chunks.len() as f64 / el);
    println!(
        "backend_mentions_per_chunk {:.1}",
        mentions as f64 / chunks.len() as f64
    );
    // Label distribution is the type-quality signal: v1 collapsed types,
    // and the P2.1 claim is that GLiNER2's joint typing fixes that. A
    // Person-heavy corpus reporting mostly Work says otherwise.
    for (label, n) in &by_label {
        println!("backend_label_{label:<14} {n}");
    }
    for s in &samples {
        eprintln!("{s}");
    }

    // The trait surface the retrieval call sites actually use.
    let entities = extractor.extract_entities(&chunks[2]);
    println!("trait_entities_chunk2  {}", entities.len());
    eprintln!("  [trait] chunk 2: {:?}", &entities);

    Ok(())
}
