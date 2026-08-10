// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP4 spike probe (research/enrichment-spikes): ms/pair for a yes/no-family
//! reranker GGUF over the pre-registered fixture — 100 real (query, chunk)
//! pairs — plus the sanity gate that must pass BEFORE any timing counts.
//!
//! Sanity gate (frozen in research/enrichment-spikes/README.md G4):
//!   relevant − irrelevant mean-score separation ≥ 0.5 logits on a curated
//!   8-doc set, AND no score magnitude collapse (the missing
//!   `cls.output.weight` trap presents as ~1e-23 scores).
//!
//! Run:
//!   cargo run --release -p sovereign-inference --example rerank_pairs_probe -- \
//!     <model.gguf> research/enrichment-spikes/data/chunks_100.jsonl
//!
//! Timing: model load excluded; one warm batch first; then batched and
//! sequential passes over the 100 chunks, ms/pair = wall / n.

use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::reranker_standalone::StandaloneReranker;
use std::env;
use std::time::Instant;

const QUERY: &str = "What is the categorical imperative in Kant's ethics, and how does it \
                     differ from a hypothetical imperative?";

const RELEVANT: [&str; 4] = [
    "Kant's categorical imperative commands unconditionally: act only on maxims you could will \
     to be universal law. It is the supreme principle of his deontological ethics.",
    "A hypothetical imperative binds only relative to a desired end, whereas the categorical \
     imperative binds every rational agent regardless of inclination.",
    "In the Groundwork of the Metaphysics of Morals, Kant formulates the categorical imperative \
     as the formula of universal law and the formula of humanity as an end in itself.",
    "For Kant, moral worth attaches only to actions done from duty — from respect for the moral \
     law expressed in the categorical imperative.",
];

const IRRELEVANT: [&str; 4] = [
    "Photosynthesis converts sunlight into chemical energy in plant chloroplasts, fixing carbon \
     dioxide into glucose using chlorophyll.",
    "The Great Barrier Reef off Queensland is the world's largest coral reef system, spanning \
     thousands of individual reefs.",
    "Jazz emerged in New Orleans around the turn of the 20th century, blending blues, ragtime, \
     and marching-band traditions.",
    "Plate tectonics describes the slow movement of lithospheric plates over the asthenosphere, \
     driving earthquakes and mountain building.",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let model_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "sovereign/models/qwen3-reranker-0.6b-q8_0.gguf".to_string());
    let fixture = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "research/enrichment-spikes/data/chunks_100.jsonl".to_string());

    eprintln!("model:   {model_path}");
    eprintln!("fixture: {fixture}");
    let t_load = Instant::now();
    let reranker = StandaloneReranker::load(
        std::path::Path::new(&model_path),
        ModelFamily::Reranker,
        None,
    )?;
    eprintln!("loaded in {:.2?}\n", t_load.elapsed());

    // ── Sanity gate (G4 pre-condition; timing does not count unless this passes) ──
    let curated: Vec<String> = RELEVANT
        .iter()
        .chain(IRRELEVANT.iter())
        .map(|s| s.to_string())
        .collect();
    let scores = reranker.rerank_batch(QUERY, &curated).await?;
    let mean_rel: f32 = scores[..4].iter().sum::<f32>() / 4.0;
    let mean_irr: f32 = scores[4..].iter().sum::<f32>() / 4.0;
    let separation = mean_rel - mean_irr;
    let max_abs = scores.iter().fold(0f32, |m, s| m.max(s.abs()));
    eprintln!("sanity:  relevant mean {mean_rel:+.3}  irrelevant mean {mean_irr:+.3}  separation {separation:+.3}");
    eprintln!("         max |score| {max_abs:.3e}  per-doc: {scores:?}");
    if max_abs < 1e-6 {
        return Err(format!(
            "SANITY FAILURE: score magnitudes collapsed (max |score| {max_abs:.3e}) — \
             missing-scoring-head trap"
        )
        .into());
    }
    if separation < 0.5 {
        return Err(format!(
            "SANITY FAILURE: relevant−irrelevant separation {separation:+.3} < 0.5 logits"
        )
        .into());
    }
    eprintln!("         ✓ sanity gate passed\n");

    // ── Fixture: 100 real chunks ──
    let raw = std::fs::read_to_string(&fixture)?;
    let docs: Vec<String> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)?;
            Ok::<String, Box<dyn std::error::Error>>(
                v["text"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<Result<_, _>>()?;
    let n = docs.len();
    eprintln!("timing over {n} (query, chunk) pairs…");

    // Warm (allocation + shader compilation excluded from timing).
    let _ = reranker.rerank_batch(QUERY, &docs).await?;

    let t0 = Instant::now();
    let batched = reranker.rerank_batch(QUERY, &docs).await?;
    let batched_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = Instant::now();
    let sequential = reranker.rerank_sequential(QUERY, &docs).await?;
    let seq_ms = t1.elapsed().as_secs_f64() * 1e3;

    assert_eq!(batched.len(), n);
    assert_eq!(sequential.len(), n);

    println!("pairs           {n}");
    println!("batched_total   {batched_ms:.1} ms");
    println!("batched_per_pair {:.2} ms/pair", batched_ms / n as f64);
    println!("sequential_total {seq_ms:.1} ms");
    println!("sequential_per_pair {:.2} ms/pair", seq_ms / n as f64);
    println!("speedup         {:.2}x", seq_ms / batched_ms.max(1e-9));
    Ok(())
}
