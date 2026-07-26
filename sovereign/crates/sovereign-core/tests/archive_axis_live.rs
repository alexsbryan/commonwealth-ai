// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live calibration check for the router's ARCHIVE-vs-THREAD axis.
//!
//! `#[ignore]`d: it needs a running daemon with an embedding model
//! (`http://localhost:9741/v1/embeddings`), so it is a developer tool,
//! not a CI gate. The unit tests in `archive_classifier.rs` cover the
//! gate's LOGIC on synthetic vectors; this one answers the question
//! those cannot — does the gate separate real questions in the real
//! embedding space?
//!
//! Run it after touching `sovereign/router/archive_examples.toml`, the
//! archive thresholds, or the embedding model:
//!
//! ```text
//! cargo test -p sovereign-core --features corpus-engine/treesitter \
//!     --test archive_axis_live -- --ignored --nocapture
//! ```
//!
//! ## What is held out
//!
//! Every question below is DISJOINT from the shipped example bank —
//! `archive_classifier::tests::shipped_bank_is_disjoint_from_evaluation_sets`
//! enforces that mechanically, so this file cannot silently become a
//! measurement of its own training set.
//!
//! Baseline 2026-07-26, Qwen3-Embedding-0.6B-Q8_0, gate (0.50, 0.04):
//! 5/6 archive positives fire, 0/21 negatives fire. Target case fires
//! at sim 0.645 / margin +0.077. cells_v1's metalingual row sits at
//! margin −0.079 — a 0.099 cushion under the gate.
//!
//! The absolute gate started at 0.45 and was raised to 0.50 when the
//! routing bench surfaced `voice_H09_journal_think_leak` (below) at
//! sim 0.452 / margin +0.038 — held out by only 0.002 of margin. See
//! `archive_classifier::DEFAULT_MIN_ARCHIVE_SIM`.

use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::archive_classifier::ConversationArchiveClassifier;
use sovereign_core::error::{Error, Result};
use sovereign_core::router_bootstrap::BAKED_ARCHIVE_EXAMPLES;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

const ENDPOINT: &str = "http://localhost:9741/v1/embeddings";

/// The query-side instruction the embedded engine prepends in
/// `EmbedSlot::embed_query_sync`. The daemon's OpenAI-shaped
/// `/v1/embeddings` route does NOT apply it, so this provider must —
/// otherwise the vectors sit in a different space than production and
/// the numbers mean nothing.
const QUERY_INSTRUCTION: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";

/// Questions about the user's PAST conversations — a search over their
/// own archive. These SHOULD fire. The first is the production failure
/// this axis was built for.
const ARCHIVE_POSITIVES: &[&str] = &[
    "Have I mentioned kayaking in any of our past chats?",
    "Across my past conversations, what have I said about my sleep?",
    "Did I ever bring up learning the cello in a previous chat?",
    "What have I asked you about gardening across all our chats?",
    "Is there an old conversation where I talked about quitting?",
    "In earlier sessions, what did I say my goals were?",
];

/// Questions about THIS conversation. These must NEVER fire the
/// archive gate — they belong to the locator axis (Pre-check -2.5) or
/// the metalingual handler, both of which answer from the message list.
const THREAD_NEGATIVES: &[&str] = &[
    // cells_v1 `regression_meta_conversation` — the hard gate. If this
    // fires, cells_v1 leaves 27/27 and the bank is wrong.
    "What did you mention earlier about retrieval?",
    "what was the first thing I asked?",
    "summarize this conversation so far",
    "did I already ask you that?",
    "what's the last thing you said before this?",
    "what did I say my deadline was earlier in this chat?",
    "remind me what we've talked about",
    "what was my second question?",
];

/// World / other questions. Blocked by the ABSOLUTE gate rather than
/// the margin — the calibration relies on them sitting well below
/// `min_archive_sim`, so a regression here means the prefixed
/// embedding space shifted.
const OTHER_NEGATIVES: &[&str] = &[
    "What did Kant say about duty?",
    "Summarize the first chapter of Moby-Dick",
    "What was the first thing Darwin published?",
    "What does 'tier' mean in this codebase?",
    "What does 'session_id' mean in this codebase?",
    "According to SEP, what does eudaimonia refer to?",
    "Compare Rust and Go for systems programming",
    "What's the population of Lisbon?",
    "Write me a haiku about rain",
    "Remind me to call the vet tomorrow",
    "Why does this job decision feel so heavy?",
    "Search the web for today's launch coverage",
    // `voice_routing_v1::voice_H09_journal_think_leak`, verbatim. Long
    // reflective first-person prose is the nearest non-archive
    // neighbour found so far: first-person and memory-flavoured
    // without asking about past chats at all. It scored sim 0.452 /
    // margin +0.038 — cleared the original 0.45 absolute gate and was
    // held out by 0.002 of margin alone. It is pinned here so any
    // future bank edit that drifts toward it fails loudly instead of
    // showing up as a routing-bench regression days later.
    "Something keeps surfacing when I try to write about work. There's a story I tell \
     myself about discipline — that staying is the harder thing, the more honest thing, \
     the thing my parents would have done. And underneath that I notice a smaller voice \
     that just says \"you're tired.\" I don't know which one to trust.",
];

/// Embeds through the running daemon. `curl` rather than an HTTP crate
/// so this costs the crate no dependency it wouldn't otherwise carry.
struct DaemonEmbed;

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
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_query(text).await
    }
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let body = serde_json::json!({
            "input": [format!("{QUERY_INSTRUCTION}{query}")],
            "model": "embed",
        })
        .to_string();
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
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

fn normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[tokio::test]
#[ignore = "needs a running daemon with an embedding model on :9741"]
async fn archive_axis_separates_real_questions() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(DaemonEmbed);
    let cls =
        ConversationArchiveClassifier::from_toml_str(BAKED_ARCHIVE_EXAMPLES, Arc::clone(&inference))
            .await
            .expect("archive classifier built from the shipped bank");
    assert!(
        cls.archive_count() >= 15 && cls.thread_count() >= 15,
        "shipped bank too small: archive={} thread={}",
        cls.archive_count(),
        cls.thread_count()
    );

    // Score a group, printing the glassbox table and returning the
    // questions on which the gate fired.
    async fn score(
        cls: &ConversationArchiveClassifier,
        inference: &dyn InferenceProvider,
        label: &str,
        items: &[&'static str],
    ) -> Vec<&'static str> {
        println!("\n=== {label} ===");
        let mut fired = Vec::new();
        for q in items {
            let mut e = inference.embed_query(q).await.expect("embed");
            normalize(&mut e);
            match cls.classify_from_embedding(&e) {
                Some(v) => {
                    println!(
                        "  FIRE     sim_a={:.3} sim_t={:.3} margin={:+.3}  {q}",
                        v.sim_archive, v.sim_thread, v.margin
                    );
                    fired.push(*q);
                }
                None => println!("  abstain                                    {q}"),
            }
        }
        fired
    }

    let fired_pos = score(&cls, &*inference, "ARCHIVE POSITIVES", ARCHIVE_POSITIVES).await;
    let fired_thread = score(&cls, &*inference, "THREAD NEGATIVES", THREAD_NEGATIVES).await;
    let fired_other = score(&cls, &*inference, "OTHER NEGATIVES", OTHER_NEGATIVES).await;

    println!(
        "\npositives firing: {}/{} · false positives: {}/{}",
        fired_pos.len(),
        ARCHIVE_POSITIVES.len(),
        fired_thread.len() + fired_other.len(),
        THREAD_NEGATIVES.len() + OTHER_NEGATIVES.len()
    );

    // ── The hard assertions ──────────────────────────────────────
    // Called out separately from the bulk check: this is the exact
    // question the prior attempt proved would flip under a rule built
    // on the scope + locator axes. It is the reason this axis exists
    // as its own centroid, so its failure deserves its own message.
    assert!(
        !fired_thread.contains(&"What did you mention earlier about retrieval?"),
        "archive gate fired on cells_v1's metalingual regression question — \
         the bank has drifted toward the thread class and cells_v1 will leave 27/27"
    );

    // The gate is asymmetric by design (see `archive_classifier` docs):
    // a false positive restricts a world question to personal corpora,
    // an abstained positive merely keeps today's behaviour. So false
    // positives are a hard failure and the positive rate has a floor.
    assert!(
        fired_thread.is_empty(),
        "archive gate fired on this-thread questions: {fired_thread:?}"
    );
    assert!(
        fired_other.is_empty(),
        "archive gate fired on world questions: {fired_other:?}"
    );

    // The production failure must be fixed, by name.
    assert!(
        fired_pos.contains(&"Have I mentioned kayaking in any of our past chats?"),
        "the target case did not fire — this axis exists to route it"
    );
    assert!(
        fired_pos.len() >= 4,
        "expected at least 4 of {} archive positives to clear the gate, got {}",
        ARCHIVE_POSITIVES.len(),
        fired_pos.len()
    );
}
