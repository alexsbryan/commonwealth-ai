// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live calibration check for the router's conversation-locator axis.
//!
//! `#[ignore]`d: it needs a running daemon with an embedding model
//! (`http://localhost:9741/v1/embeddings`), so it is a developer tool,
//! not a CI gate. The unit tests in `router_embed.rs` cover the gate's
//! LOGIC on synthetic vectors; this one answers the question those
//! cannot — does the gate separate real questions in the real
//! embedding space, with the real exemplar bank as the negative set?
//!
//! Run it after touching `sovereign/router/exemplars.toml`, the
//! locator thresholds, or the embedding model:
//!
//! ```text
//! cargo test -p sovereign-core --features corpus-engine/treesitter \
//!     --test main locator_axis_live -- --ignored --nocapture
//! ```
//!
//! Baseline 2026-07-26, Qwen3-Embedding-0.6B-Q8_0: 8/8 positives
//! abstain-or-fire correctly, 0/14 negatives fire.

use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::error::{Error, Result};
use sovereign_core::router_bootstrap::BAKED_ROUTER_EXEMPLARS;
use sovereign_core::router_embed::EmbedRouter;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

const ENDPOINT: &str = "http://localhost:9741/v1/embeddings";

/// The query-side instruction the embedded engine prepends in
/// `EmbedSlot::embed_query_sync` (see `model_family.rs`, Qwen3-Embedding
/// `EmbedQuirks::query_instruction`). The daemon's OpenAI-shaped
/// `/v1/embeddings` route has no query/document distinction and does
/// NOT apply it, so this provider must — otherwise the vectors sit in a
/// different space than production and the numbers mean nothing.
const QUERY_INSTRUCTION: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";

/// Questions that mean "look at THIS conversation".
const POSITIVES: &[&str] = &[
    "what was the first thing I asked?",
    "remind me what we've talked about",
    "what did I say my deadline was earlier in this chat?",
    "summarize this conversation so far",
    "what was my second question?",
    "what have you and I been going back and forth on?",
    "did I already ask you that?",
    "what's the last thing you said before this?",
];

/// Questions that must NEVER hard-commit to the conversation route.
/// Includes the adversarial neighbours: archive recall over the user's
/// PAST conversations (a corpus search, not this thread), and world
/// questions that happen to use ordinal/summary vocabulary.
const NEGATIVES: &[&str] = &[
    "What did Kant say about duty?",
    "Summarize the first chapter of Moby-Dick",
    "What was the first thing Darwin published?",
    "Have I mentioned kayaking in any of our past chats?",
    "Across my past conversations, what have I said about my sleep?",
    "What does 'tier' mean in this codebase?",
    "How does this project define routing?",
    "Compare Rust and Go for systems programming",
    "stop",
    "Why does this job decision feel so heavy?",
    "Write me a haiku about rain",
    "What's the population of Lisbon?",
    "Search the web for today's launch coverage",
    "Remind me to call the vet tomorrow",
];

/// Embeds through the running daemon. `curl` rather than an HTTP crate
/// so this costs the crate no dependency it wouldn't otherwise carry.
struct DaemonEmbed;

impl DaemonEmbed {
    /// POST `input` to the daemon VERBATIM. The route applies no
    /// instruction of its own, so whatever the caller passes is exactly
    /// what the model sees — which is what lets `embed` and
    /// `embed_query` below sit in genuinely different spaces.
    async fn post(&self, input: &str) -> Result<Vec<f32>> {
        let body = serde_json::json!({ "input": [input], "model": "embed" }).to_string();
        let out = Command::new("curl")
            .args([
                "-s",
                "-m",
                "120",
                ENDPOINT,
                "-H",
                "content-type: application/json",
                "-d",
                &body,
            ])
            .output()
            .map_err(|e| Error::Inference(format!("curl: {e}")))?;
        let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| Error::Inference(format!("embeddings response: {e}")))?;
        let v = parsed["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| Error::Inference(format!("no embedding in response: {parsed}")))?
            .iter()
            .map(|x| x.as_f64().unwrap_or_default() as f32)
            .collect();
        Ok(v)
    }
}

#[async_trait]
impl InferenceProvider for DaemonEmbed {
    async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented("embed-only provider".into()))
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("embed-only provider".into()))
    }
    /// UNINSTRUCTED. It used to delegate to `embed_query`, which was
    /// harmless while every classifier embedded through `embed_query`
    /// anyway. It is not harmless now: the locator axis embeds via
    /// `router_instruction::embed_classifier`, which prepends the
    /// classifier instruction and then calls THIS — so delegating would
    /// send the model both instructions at once and measure a space
    /// production never uses.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.post(text).await
    }
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.post(&format!("{QUERY_INSTRUCTION}{query}")).await
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

#[tokio::test]
#[ignore = "needs a running daemon with an embedding model on :9741"]
async fn conversation_locator_separates_real_questions() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(DaemonEmbed);
    let router = EmbedRouter::from_toml_str(BAKED_ROUTER_EXEMPLARS, Arc::clone(&inference))
        .await
        .expect("embed router built from the shipped bank");
    assert!(
        router.locator_exemplar_count() >= 6,
        "the shipped bank must carry the locator axis"
    );

    let mut fired_positives = 0usize;
    println!("\n=== POSITIVES ===");
    for q in POSITIVES {
        let emb = router
            .embed_classifier_normalized(q, &*inference)
            .await
            .expect("embed");
        match router.locator_from_embedding(&emb) {
            Some(v) => {
                fired_positives += 1;
                println!("  FIRE  sim={:.3} margin={:+.3}  {q}", v.top_sim, v.margin);
            }
            None => println!("  abstain                       {q}"),
        }
    }

    println!("\n=== NEGATIVES ===");
    let mut false_positives = Vec::new();
    for q in NEGATIVES {
        let emb = router
            .embed_classifier_normalized(q, &*inference)
            .await
            .expect("embed");
        if let Some(v) = router.locator_from_embedding(&emb) {
            println!("  FIRE  sim={:.3} margin={:+.3}  {q}", v.top_sim, v.margin);
            false_positives.push(*q);
        } else {
            println!("  abstain                       {q}");
        }
    }

    // The gate is asymmetric by design: a false positive hard-commits a
    // world question to conversation-only answering, while an abstained
    // positive merely keeps today's behaviour. So the hard assertion is
    // on false positives; the positive rate is reported, and only its
    // floor is enforced.
    assert!(
        false_positives.is_empty(),
        "locator gate fired on non-conversation questions: {false_positives:?}"
    );
    println!(
        "\npositives firing: {fired_positives}/{} · false positives: 0/{}",
        POSITIVES.len(),
        NEGATIVES.len()
    );
    assert!(
        fired_positives >= 4,
        "expected at least 4 of {} positives to clear the gate, got {fired_positives}",
        POSITIVES.len()
    );
}
