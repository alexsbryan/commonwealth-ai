// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke test: load a reranker GGUF via StandaloneReranker, score a
//! handful of (query, doc) pairs, print the scores. Run via:
//!
//!     cargo run --release -p sovereign-inference --example rerank_smoke \
//!         -- sovereign/models/qwen3-reranker-0.6b-q8_0.gguf
//!
//! Default model is the official Qwen3-Reranker (YesNoLogit protocol —
//! the working family; the public jina-v3 GGUF drops its scoring head,
//! see rerank_slot.rs protocol notes).
//!
//! Expectation: docs clearly relevant to the query score higher than
//! clearly irrelevant ones. Absolute magnitude is model-specific
//! (rank logit, not a probability).

use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::reranker_standalone::StandaloneReranker;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "sovereign/models/qwen3-reranker-0.6b-q8_0.gguf".to_string());
    eprintln!("loading reranker: {path}");
    let reranker =
        StandaloneReranker::load(std::path::Path::new(&path), ModelFamily::Reranker, None)?;
    eprintln!("loaded.\n");

    let cases = vec![
        (
            "What is the boiling point of water?",
            vec![
                "Water boils at 100 degrees Celsius at standard atmospheric pressure.",
                "The Eiffel Tower is located in Paris, France.",
                "At sea level, the boiling point of water is exactly 100 °C (212 °F).",
                "Cats are popular household pets known for their independence.",
            ],
        ),
        (
            "Who wrote Hamlet?",
            vec![
                "Hamlet is a tragedy written by William Shakespeare around 1600.",
                "Photosynthesis converts sunlight into chemical energy in plants.",
                "Shakespeare authored numerous plays including Macbeth, Othello, and Hamlet.",
                "The capital of Australia is Canberra, not Sydney.",
            ],
        ),
        (
            "What is the categorical imperative in Kant's ethics?",
            vec![
                "Kant's categorical imperative is a moral principle: act only on maxims you could will to be universal law.",
                "Pizza was first popularized in Naples, Italy in the 18th century.",
                "Immanuel Kant grounded his deontological ethics in the categorical imperative — a test for moral maxims.",
                "Penguins are flightless birds found primarily in the Southern Hemisphere.",
            ],
        ),
    ];

    for (query, docs) in &cases {
        println!("Query: {query}");
        let docs_owned: Vec<String> = docs.iter().map(|s| s.to_string()).collect();
        let scores = reranker.rerank_batch(query, &docs_owned).await?;
        let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (rank, (i, score)) in indexed.iter().enumerate() {
            println!("  #{} score={:>+8.4}  {}", rank + 1, score, &docs[*i],);
        }
        println!();
    }

    Ok(())
}
