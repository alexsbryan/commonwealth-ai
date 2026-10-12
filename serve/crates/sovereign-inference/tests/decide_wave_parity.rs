// SPDX-License-Identifier: AGPL-3.0-or-later
//! A forced-choice wave answers each question as the single request would.
//!
//! Through the engine's public `complete`, on real weights: the same closed
//! questions about one document, once as single requests and once as one wave
//! (`oicp_types::forced_choice::wave`). Every branch must carry the single
//! request's distribution (the wave restores one decoded state per branch, so
//! the arithmetic is the single request's: `decide_wave.rs`) and the wave must
//! cost less wall time. Use an MTP gguf to exercise the speculative target
//! context the production primary answers forced choices on.
//!
//! Run explicitly (never in CI — needs local GGUF weights):
//! ```sh
//! WAVE_GGUF=sovereign/models/Qwopus3.5-4B-v3-MTP-Q8_0.gguf \
//!   cargo test -p sovereign-inference --test decide_wave_parity \
//!   -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use oicp_types::forced_choice::{self, Branch};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::CompletionRequest;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

const SYSTEM: &str =
    "You read one document and answer one closed question about one of its lines. Answer with a single label.";

const LINES: [&str; 12] = [
    "Thanks for sending the RFP through Summit Energy last week.",
    "We reviewed the volumes for the Jacksonville and Fernandina mills.",
    "Enron North America can offer a fixed price of $4.85 per MMBtu.",
    "The term would run from January 2001 through December 2001.",
    "Delivery point is the FGT Zone 3 citygate.",
    "Please let me know by Friday whether this works for your team.",
    "I will send the master agreement draft once we hear back.",
    "Our credit group still needs the parent guarantee from Smurfit.",
    "Summit mentioned you are also talking to Duke and El Paso.",
    "We can hold this price until 2pm Central on Friday.",
    "Bids are due Friday at noon.",
    "Smurfit prefers a one-year fixed price with a swing option.",
];

fn document() -> String {
    let mut s = String::from(
        "Declared facts of this document:\nsender: jeff.ward@enron.com\nthe sender is in `ours`\n\n",
    );
    for (i, l) in LINES.iter().enumerate() {
        s.push_str(&format!("{}: {l}\n", i + 1));
    }
    s.push('\n');
    s
}

fn suffix(i: usize) -> String {
    format!(
        "Line {}: \"{}\"\nWhich does this line state?\nA: a deal's stage changes\nB: someone commits to do something\n0: neither\nAnswer with A, B or 0.",
        i + 1,
        LINES[i]
    )
}

fn request(prompt: String, body: serde_json::Value) -> CompletionRequest {
    CompletionRequest {
        prompt,
        system_message: Some(SYSTEM.to_string()),
        max_tokens: Some(1),
        temperature: Some(0.0),
        think_budget: Some(0),
        structured_output: Some(body),
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "manual: needs WAVE_GGUF pointing at a local gguf"]
async fn a_wave_answers_each_question_as_its_single_request() {
    let Ok(gguf) = std::env::var("WAVE_GGUF") else {
        eprintln!("SKIP: set WAVE_GGUF to a local gguf path");
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "prefix_state=info,sovereign_inference=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let path = PathBuf::from(gguf);
    let engine = EmbeddedLlamaCpp::load_dual(&path, Some(&path), 8192, None).expect("load");
    let labels = vec!["A".to_string(), "B".to_string(), "0".to_string()];
    let doc = document();
    let body = forced_choice::schema(&["A", "B", "0"]);

    // Warm the primary slot so neither arm pays the load.
    let _ = engine
        .complete(&request(format!("{doc}{}", suffix(0)), body.clone()))
        .await
        .expect("warm");

    let t = Instant::now();
    let mut singles: Vec<BTreeMap<String, f64>> = Vec::new();
    for i in 0..LINES.len() {
        let out = engine
            .complete(&request(format!("{doc}{}", suffix(i)), body.clone()))
            .await
            .expect("single");
        singles.push(forced_choice::parse(&out.text).expect("a distribution"));
    }
    let singles_ms = t.elapsed().as_millis();

    let branches: Vec<Branch> = (0..LINES.len())
        .map(|i| Branch {
            suffix: suffix(i),
            labels: labels.clone(),
        })
        .collect();
    let t = Instant::now();
    let out = engine
        .complete(&request(doc.clone(), forced_choice::wave(&branches)))
        .await
        .expect("wave");
    let wave_ms = t.elapsed().as_millis();
    let waved = forced_choice::parse_wave(&out.text).expect("a wave answer");
    assert_eq!(waved.len(), LINES.len());

    let mut worst = 0f64;
    let mut agree = 0;
    for (i, (w, s)) in waved.iter().zip(&singles).enumerate() {
        let w = w
            .as_ref()
            .unwrap_or_else(|e| panic!("branch {i} refused: {e}"));
        for l in &labels {
            worst = worst.max((w[l] - s[l]).abs());
        }
        let top = |d: &BTreeMap<String, f64>| {
            labels
                .iter()
                .max_by(|a, b| d[*a].total_cmp(&d[*b]))
                .cloned()
        };
        agree += usize::from(top(w) == top(s));
    }
    eprintln!(
        "singles {singles_ms} ms ({:.0}/question)  wave {wave_ms} ms ({:.0}/question)  max|dp|={worst:.5}  argmax {agree}/{}",
        singles_ms as f64 / LINES.len() as f64,
        wave_ms as f64 / LINES.len() as f64,
        LINES.len()
    );
    assert_eq!(
        agree,
        LINES.len(),
        "a branch chose differently from its single request"
    );
    assert!(worst < 1e-3, "a branch's distribution moved by {worst}");
}
