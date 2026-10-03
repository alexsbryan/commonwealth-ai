// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for `raptor_atlas`, in a sibling file so the module stays under
//! its arch-gate slack (ARCH §3.1). Mounted with `#[path]`, so names are unchanged.

use super::*;

fn ortho_clusters(cluster_count: usize, per_cluster: usize) -> Vec<Vec<f32>> {
    // Each cluster shares a near-orthogonal one-hot signature.
    let mut out = Vec::new();
    for c in 0..cluster_count {
        for k in 0..per_cluster {
            let mut v = vec![0.05; 8];
            v[c % 8] = 1.0;
            v[(c + k + 1) % 8] += 0.02;
            out.push(v);
        }
    }
    out
}

use async_trait::async_trait;
use std::pin::Pin;

#[test]
fn split_sentences_keeps_terminators_and_order() {
    let text = "First sentence here. Second one follows! Third asks a question? trailing fragment";
    let got = split_sentences(text);
    assert_eq!(
        got,
        vec![
            "First sentence here.",
            "Second one follows!",
            "Third asks a question?",
            "trailing fragment"
        ]
    );
}

#[test]
fn extractive_selection_stops_at_target_and_restores_source_order() {
    // Three sentences; centroid points at [1,0]. Rank order is
    // 0 (1.0), 2 (0.9), 1 (0.0). Target forces two picks — the
    // result must be source order {0, 2}, not rank order.
    let lens = vec![50usize, 50, 50];
    let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.9, 0.1]];
    let centroid = vec![1.0, 0.0];
    let selected = select_extractive_sentences(&lens, &embs, &centroid, 100);
    assert_eq!(selected, vec![0, 2]);
    // A target below one sentence still takes the top-ranked one.
    let selected = select_extractive_sentences(&lens, &embs, &centroid, 10);
    assert_eq!(selected, vec![0]);
}

/// Deterministic 2-dim embedding shared by the summarize-path
/// mocks: sentences mentioning "anchor" align with the test
/// centroid [1,0]; everything else is orthogonal.
fn direction_embed(text: &str) -> Vec<f32> {
    if text.contains("anchor") {
        vec![1.0, 0.0]
    } else {
        vec![0.0, 1.0]
    }
}

fn mock_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        max_context_tokens: 8192,
        supports_structured_output: false,
        relative_speed: Speed::Fast,
        relative_reasoning: Depth::Shallow,
    }
}

/// LLM path errors (the daemon-down case); embeds work.
struct FailingLlmEmbedOk;

#[async_trait]
impl InferenceProvider for FailingLlmEmbedOk {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
        Err(sovereign_core::error::Error::Storage("llm down".into()))
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unreachable!("summarize path does not stream")
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(direction_embed(text))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        mock_caps()
    }
}

/// Any LLM call is a test failure; embeds work. Proves the
/// extractive mode is LLM-free.
struct PanicLlmEmbedOk;

#[async_trait]
impl InferenceProvider for PanicLlmEmbedOk {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
        panic!("extractive mode must not call the LLM")
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        panic!("extractive mode must not stream")
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(direction_embed(text))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        mock_caps()
    }
}

/// LLM returns a valid summary JSON; embeds work. For exercising
/// the P1.2 verification gate around a "successful" abstractive
/// generation.
struct OkLlmEmbedOk;

#[async_trait]
impl InferenceProvider for OkLlmEmbedOk {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: r#"{"summary": "The cluster centers on the anchor sentence theme.", "primary_entities": ["Anchor"]}"#.to_string(),
            tokens_used: 10,
            prompt_tokens: 5,
            model_id: "mock-abstractive-llm".into(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unreachable!("summarize path does not stream")
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(direction_embed(text))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        mock_caps()
    }
}

/// Scripted verifier: pops verdicts front-to-back; panics when
/// called more often than scripted.
struct ScriptedVerifier {
    verdicts: std::sync::Mutex<Vec<Option<crate::summary_verify::SummaryVerdict>>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl crate::summary_verify::SummaryVerifier for ScriptedVerifier {
    async fn verify(
        &self,
        _summary: &str,
        _member_texts: &[String],
    ) -> Option<crate::summary_verify::SummaryVerdict> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.verdicts
            .lock()
            .unwrap()
            .pop()
            .expect("verifier called more often than scripted")
    }
}

fn verify_ctx(
    verdicts: Vec<Option<crate::summary_verify::SummaryVerdict>>,
) -> (
    Arc<crate::summary_verify::VerifyCtx>,
    Arc<crate::summary_verify::VerifyStats>,
) {
    // Scripted pops from the back — reverse so the vec reads in
    // call order at the test site.
    let mut v = verdicts;
    v.reverse();
    let stats = Arc::new(crate::summary_verify::VerifyStats::default());
    let ctx = Arc::new(crate::summary_verify::VerifyCtx {
        verifier: Arc::new(ScriptedVerifier {
            verdicts: std::sync::Mutex::new(v),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }),
        policy: crate::summary_verify::VerifyPolicy::On,
        stats: Arc::clone(&stats),
    });
    (ctx, stats)
}

fn fail_verdict() -> Option<crate::summary_verify::SummaryVerdict> {
    Some(crate::summary_verify::SummaryVerdict {
        claims_total: 3,
        claims_unsupported: 2,
        whole_summary_violation: Some(0.9),
        name_violations: Vec::new(),
    })
}

fn pass_verdict() -> Option<crate::summary_verify::SummaryVerdict> {
    Some(crate::summary_verify::SummaryVerdict {
        claims_total: 3,
        claims_unsupported: 0,
        whole_summary_violation: Some(0.1),
        name_violations: Vec::new(),
    })
}

#[tokio::test]
async fn verify_gate_passes_verified_abstractive_through() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(OkLlmEmbedOk);
    let (ctx, stats) = verify_ctx(vec![pass_verdict()]);
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        Some(ctx),
    )
    .await
    .expect("verified abstractive summary must persist");
    assert_eq!(node.summarizer_model, "mock-abstractive-llm");
    assert_eq!(node.prompt_version, RAPTOR_PROMPT_VERSION);
    assert_eq!(
        stats
            .passed_first
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[tokio::test]
async fn verify_gate_retries_then_falls_back_to_extractive() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(OkLlmEmbedOk);
    // First verdict fails → faithful retry generates again → second
    // verdict fails → extractive floor.
    let (ctx, stats) = verify_ctx(vec![fail_verdict(), fail_verdict()]);
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        Some(ctx),
    )
    .await
    .expect("failed verification must fall back to extractive, not drop the node");
    assert_eq!(node.summarizer_model, EXTRACTIVE_SUMMARIZER);
    assert_eq!(node.prompt_version, EXTRACTIVE_ALGO_VERSION);
    assert_eq!(stats.retried.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        stats.fell_back.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[tokio::test]
async fn verify_gate_verifier_failure_is_not_a_pass() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(OkLlmEmbedOk);
    // Verifier unreachable (None) on the first attempt → extractive
    // floor immediately, no unverified abstractive persists.
    let (ctx, stats) = verify_ctx(vec![None]);
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        Some(ctx),
    )
    .await
    .expect("verifier failure must fall back to extractive");
    assert_eq!(node.summarizer_model, EXTRACTIVE_SUMMARIZER);
    assert_eq!(
        stats
            .verifier_failed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

/// The writer reads the full member text the verifier judges, not the
/// 280-char preview: on the pilot tree the preview-fed writer lost 13 of
/// 14 nodes to the extractive floor.
#[test]
fn the_summary_writer_reads_full_member_text() {
    let tail = "the hidden detail past the preview";
    let full = format!("{} {tail}", "lead ".repeat(80));
    let mut input = extractive_test_input();
    input.member_descriptors = vec![full.chars().take(280).collect()];
    input.member_full_texts = vec![full];
    let req = build_abstractive_request(&input, &DocumentTypeTag::Narrative, false);
    assert!(
        req.prompt.contains(tail),
        "the writer's prompt lacks the text past the preview"
    );
}

fn extractive_test_input() -> ClusterSummarizationInput {
    let anchor =
        "The anchor sentence describes the central theme of this cluster in detail.".to_string();
    let aside =
        "An unrelated aside wanders far away from the cluster topic entirely today.".to_string();
    ClusterSummarizationInput {
        level: 0,
        member_descriptors: vec![anchor.clone(), aside.clone()],
        member_full_texts: vec![anchor, aside],
        direct_member_chunk_ids: vec![1, 2],
        evidence_chunk_ids: vec![1, 2],
        children_node_ids: Vec::new(),
        quote_spans: Vec::new(),
        centroid_embedding: vec![1.0, 0.0],
        cluster_coherence: 0.9,
        correction_hint: None,
    }
}

#[tokio::test]
async fn abstractive_llm_failure_falls_back_to_extractive() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(FailingLlmEmbedOk);
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        None,
    )
    .await
    .expect("LLM failure must fall back to an extractive node, not thin the tree");
    assert_eq!(node.summarizer_model, EXTRACTIVE_SUMMARIZER);
    assert_eq!(node.prompt_version, EXTRACTIVE_ALGO_VERSION);
    assert!(
        node.summary.contains("anchor sentence"),
        "summary should carry the centroid-aligned source sentence verbatim, got: {}",
        node.summary
    );
}

/// The host answers the first `sheds` summary calls with a typed shed (the
/// pilot's single-permit lane: seven of eight parked calls refused together
/// at the 30 s park bound), then serves as [`OkLlmEmbedOk`].
struct ShedThenOkLlm {
    sheds: std::sync::atomic::AtomicUsize,
    calls: std::sync::atomic::AtomicUsize,
}

impl ShedThenOkLlm {
    fn new(sheds: usize) -> Self {
        Self {
            sheds: std::sync::atomic::AtomicUsize::new(sheds),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl InferenceProvider for ShedThenOkLlm {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse> {
        use std::sync::atomic::Ordering::Relaxed;
        self.calls.fetch_add(1, Relaxed);
        if self
            .sheds
            .fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(sovereign_core::error::Error::queue_shed(3, 30_000));
        }
        OkLlmEmbedOk.complete(req).await
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unreachable!("summarize path does not stream")
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(direction_embed(text))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        mock_caps()
    }
}

/// A shed is backpressure, not a failed summary: the call comes back and the
/// node is abstractive. Before this, 5 of the pilot's 6 extractive nodes were
/// sheds that took the floor without a summary ever being written.
#[tokio::test(start_paused = true)]
async fn a_shed_summary_call_comes_back_instead_of_taking_the_extractive_floor() {
    let host = Arc::new(ShedThenOkLlm::new(2));
    let inference: Arc<dyn InferenceProvider> = host.clone();
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        None,
    )
    .await
    .expect("a node");
    assert_eq!(node.summarizer_model, "mock-abstractive-llm");
    assert_eq!(host.calls.load(std::sync::atomic::Ordering::Relaxed), 3);
}

/// The wait is bounded: a host that never stops shedding is waited out to
/// [`SHED_WAIT_CAP`], then the cluster takes the extractive floor.
#[tokio::test(start_paused = true)]
async fn a_host_that_never_stops_shedding_gets_the_floor_at_the_cap() {
    let host = Arc::new(ShedThenOkLlm::new(usize::MAX));
    let inference: Arc<dyn InferenceProvider> = host.clone();
    let started = tokio::time::Instant::now();
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Abstractive,
        None,
    )
    .await
    .expect("the floor, not a dropped node");
    assert_eq!(node.summarizer_model, EXTRACTIVE_SUMMARIZER);
    let calls = host.calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        calls > 1,
        "the shed was not waited out at all ({calls} call)"
    );
    assert!(
        started.elapsed() >= SHED_WAIT_CAP,
        "gave up after {:?}, before the {SHED_WAIT_CAP:?} cap",
        started.elapsed()
    );
}

#[tokio::test]
async fn extractive_mode_is_llm_free_and_stamps_provenance() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(PanicLlmEmbedOk);
    let node = summarize_one_cluster(
        &inference,
        extractive_test_input(),
        DocumentTypeTag::Narrative,
        SummaryMode::Extractive,
        None,
    )
    .await
    .expect("extractive mode should produce a node");
    assert_eq!(node.summarizer_model, EXTRACTIVE_SUMMARIZER);
    assert_eq!(node.prompt_version, EXTRACTIVE_ALGO_VERSION);
    assert!(node.primary_entities.is_empty());
    assert_eq!(node.direct_member_chunk_ids, vec![1, 2]);
}

#[test]
fn kmeans_recovers_orthogonal_clusters() {
    let embs = ortho_clusters(3, 5); // 15 vectors, 3 true clusters
    let assignments = kmeans_cluster(&embs, 3, 50);
    assert_eq!(assignments.len(), 15);
    // Every input from the same true cluster must end up in the
    // same predicted cluster.
    let true_cluster = |i: usize| i / 5;
    for true_c in 0..3 {
        let preds: std::collections::HashSet<usize> = (0..15)
            .filter(|&i| true_cluster(i) == true_c)
            .map(|i| assignments[i])
            .collect();
        assert_eq!(
            preds.len(),
            1,
            "true cluster {true_c} should map to one predicted cluster, got {preds:?}"
        );
    }
}

#[test]
fn kmeans_handles_k_geq_n() {
    // k >= n: degenerate, every input is its own cluster.
    let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
    let assignments = kmeans_cluster(&embs, 5, 10);
    assert_eq!(assignments, vec![0, 1]);
}

#[test]
fn target_k_picks_sensible_counts() {
    assert_eq!(target_k(1006, 20), 50);
    assert_eq!(target_k(200, 20), 10);
    assert_eq!(target_k(15, 20), 2); // tiny doc → minimum
    assert_eq!(target_k(0, 20), 0);
}

#[test]
fn mean_vector_averages_elementwise() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![3.0, 2.0, 1.0];
    let m = mean_vector(&[&a, &b]);
    assert_eq!(m, vec![2.0, 2.0, 2.0]);
}

/// A cluster larger than the prompt cap keeps only its most-central
/// members in the prompt — and keeps them in document order.
#[test]
fn descriptors_for_prompt_caps_by_centrality_and_reorders_chronologically() {
    // 40 chunks. Even indices sit on the centroid; odd indices are
    // orthogonal to it. 20 evens > the cap, so centrality alone
    // decides and no odd member can slip in to fill a spare slot.
    let chunks: Vec<ChunkInput> = (0..40)
        .map(|i| ChunkInput {
            chunk_id: i as u32,
            content: format!("chunk {i} body text"),
        })
        .collect();
    let embeddings: Vec<Vec<f32>> = (0..40)
        .map(|i| {
            if i % 2 == 0 {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            }
        })
        .collect();
    let members: Vec<usize> = (0..40).collect();
    let centroid = vec![1.0, 0.0];

    let out = descriptors_for_prompt(&members, &centroid, &chunks, &embeddings);
    assert_eq!(out.len(), MAX_MEMBERS_IN_SUMMARY_PROMPT);
    // Only central (even) chunks survive…
    for d in &out {
        let n: usize = d
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("preview starts `chunk <n>`");
        assert_eq!(n % 2, 0, "kept an off-centroid member: {d}");
    }
    // …and they read in document order, not similarity order.
    let order: Vec<usize> = out
        .iter()
        .map(|d| d.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "previews must stay chronological");
}

/// Small clusters are untouched — the cap is a ceiling, not a quota.
#[test]
fn descriptors_for_prompt_leaves_small_clusters_whole() {
    let chunks: Vec<ChunkInput> = (0..4)
        .map(|i| ChunkInput {
            chunk_id: i as u32,
            content: format!("chunk {i} body text"),
        })
        .collect();
    let embeddings: Vec<Vec<f32>> = (0..4).map(|_| vec![1.0, 0.0]).collect();
    let members: Vec<usize> = (0..4).collect();
    let out = descriptors_for_prompt(&members, &[1.0, 0.0], &chunks, &embeddings);
    assert_eq!(out.len(), 4);
}

#[test]
fn extract_quote_spans_pulls_longest_sentence_per_chunk() {
    let chunks = [
        ChunkInput {
            chunk_id: 1,
            content: "Short. This is the load-bearing sentence with quite a few words. Tiny."
                .to_string(),
        },
        ChunkInput {
            chunk_id: 2,
            content: "Another chunk where this longer sentence is the one to anchor on. End."
                .to_string(),
        },
    ];
    let embs = [vec![1.0, 0.0], vec![0.0, 1.0]];
    let refs: Vec<&Vec<f32>> = embs.iter().collect();
    let chunk_refs: Vec<&ChunkInput> = chunks.iter().collect();
    let centroid = vec![0.5, 0.5];
    let spans = extract_quote_spans_for_cluster(&chunk_refs, &refs, &centroid, 5);
    assert_eq!(spans.len(), 2);
    assert!(spans[0].text.contains("load-bearing"));
    assert!(spans[1].text.contains("longer sentence"));
    // chunk_id preserved.
    assert_eq!(spans[0].chunk_id, 1);
    assert_eq!(spans[1].chunk_id, 2);
}

#[test]
fn extract_quote_spans_dedupes_by_prefix() {
    let chunks = [
        ChunkInput {
            chunk_id: 1,
            content: "The professor walked through London streets alone and unsuspected by men."
                .to_string(),
        },
        ChunkInput {
            chunk_id: 2,
            content: "The professor walked through London streets alone and unsuspected by men."
                .to_string(),
        },
    ];
    let embs = [vec![1.0, 0.0], vec![1.0, 0.0]];
    let refs: Vec<&Vec<f32>> = embs.iter().collect();
    let chunk_refs: Vec<&ChunkInput> = chunks.iter().collect();
    let centroid = vec![1.0, 0.0];
    let spans = extract_quote_spans_for_cluster(&chunk_refs, &refs, &centroid, 5);
    assert_eq!(
        spans.len(),
        1,
        "identical spans across chunks should dedupe"
    );
}

#[test]
fn parse_cluster_summary_extracts_json_from_preamble() {
    let resp = r#"Here you go: {"summary": "Winnie kills Verloc in the parlour after learning of Stevie's death.", "primary_entities": ["Winnie", "Verloc", "Stevie"]} done."#;
    let parsed = parse_cluster_summary(resp).expect("should parse");
    assert!(parsed.summary.contains("Winnie kills Verloc"));
    assert_eq!(parsed.primary_entities, vec!["Winnie", "Verloc", "Stevie"]);
}

#[test]
fn parse_cluster_summary_returns_none_on_garbage() {
    assert!(parse_cluster_summary("no JSON here, just prose").is_none());
    assert!(parse_cluster_summary("{ malformed").is_none());
}

#[test]
fn mean_cosine_to_centroid_is_in_zero_one() {
    let a = vec![1.0, 0.0];
    let b = vec![0.0, 1.0];
    let c = vec![1.0, 1.0];
    let refs: Vec<&Vec<f32>> = vec![&a, &b];
    let m = mean_cosine_to_centroid(&refs, &c);
    // a→c and b→c each have cosine ~0.707; mean clamped to [0,1].
    assert!(m > 0.6 && m < 0.8, "expected ~0.707, got {m}");
}

/// Embeds each distinct text to its own direction, so every summary level has
/// distinct points to cluster. `direction_embed`'s two vectors collapse a
/// level of summaries onto duplicates and k-means fills one cluster.
struct DistinctEmbed;

#[async_trait]
impl InferenceProvider for DistinctEmbed {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
        unreachable!("extractive builds make no completion call")
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unreachable!("extractive builds make no completion call")
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let h = text
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        let theta = (h % 10_000) as f32 / 10_000.0 * std::f32::consts::FRAC_PI_2;
        Ok(vec![theta.cos(), theta.sin()])
    }
    fn capabilities(&self) -> ProviderCapabilities {
        mock_caps()
    }
}

/// `--to-root` recurses to ONE node; the default stops at a top layer of up to
/// `ROOT_BRANCHING_CEILING` nodes. Same 40 chunks, same leaf target, the two
/// shapes differ only in the root rule — failing input: a loop that still
/// reads the constant ceiling builds the same two-node top for both.
#[tokio::test]
async fn to_root_builds_one_root_and_the_default_stops_short_of_it() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(DistinctEmbed);
    let chunks: Vec<ChunkInput> = (0..40u32)
        .map(|i| ChunkInput {
            chunk_id: i,
            content: if i % 2 == 0 {
                format!("The anchor sentence {i} describes the lighthouse keeper at length.")
            } else {
                format!("A different passage {i} wanders along the coast toward the town.")
            },
        })
        .collect();
    let embeddings: Vec<Vec<f32>> = (0..40)
        .map(|i| {
            let jitter = i as f32 * 0.01;
            if i % 2 == 0 {
                vec![1.0, jitter]
            } else {
                vec![jitter, 1.0]
            }
        })
        .collect();
    let top = |nodes: &[RaptorNode]| {
        let max = nodes.iter().map(|n| n.level).max().unwrap();
        nodes.iter().filter(|n| n.level == max).count()
    };
    let build = |shape: TreeShape| {
        let inference = Arc::clone(&inference);
        let (chunks, embeddings) = (chunks.clone(), embeddings.clone());
        async move {
            build_raptor_atlas_with_verify(
                &inference,
                &chunks,
                &embeddings,
                DocumentTypeTag::Narrative,
                None,
                None,
                None,
                SummaryMode::Extractive,
                None,
                shape,
            )
            .await
            .expect("build")
        }
    };
    let default = build(TreeShape {
        leaf_target: 4,
        ..TreeShape::DEFAULT
    })
    .await;
    let rooted = build(TreeShape {
        leaf_target: 4,
        root_ceiling: 1,
    })
    .await;
    let default_top = top(&default);
    assert!(
        (2..=ROOT_BRANCHING_CEILING).contains(&default_top),
        "default top layer holds {default_top} nodes"
    );
    assert_eq!(top(&rooted), 1, "--to-root must end in a single root");
    assert!(
        rooted.iter().map(|n| n.level).max() > default.iter().map(|n| n.level).max(),
        "the root is a level above the default's top layer"
    );
}
