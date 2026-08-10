// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-phase resource accounting for bench-driven ingest/enrichment.
//!
//! The enrichment pipeline (skeleton, GLiNER, RAPTOR tree) has no
//! token/call ledger of its own — the only resource signal the benches
//! historically recorded was wall-clock. That confounds "the model got
//! faster" with "the pipeline did less work", which is exactly the
//! distinction a tuning experiment needs.
//!
//! [`MeteredInference`] is a transparent decorator over any
//! `InferenceProvider`: every `complete`/`complete_batch` records the
//! call count, prompt/completion token split, and provider-reported
//! latency into the [`ResourceLedger`]; every `embed*` call records
//! text counts and wall time. Streaming calls are counted (no usage
//! metadata flows on the plain stream surface) — the enrichment path
//! under measurement is non-streaming, so this is a completeness
//! backstop, not a gap in the numbers.
//!
//! Buckets are keyed by a phase label the harness sets at pipeline
//! state transitions (`ledger.set_phase("building_skeleton")`). The
//! ingest pipeline is sequential, so attributing calls to the phase
//! that is live when they complete is exact.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

use sovereign_core::error::Result;
use sovereign_core::traits::{ComputeChildStatus, InferenceProvider, ResidentSlot};
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, EditSlotInfo, ProviderCapabilities, Speed, StreamFrame,
};

/// Accumulated resource counters for one pipeline phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseBucket {
    /// Non-streaming LLM completions (includes each request inside a
    /// `complete_batch`).
    pub llm_calls: u64,
    /// Streaming LLM calls — counted only; usage isn't reported on the
    /// plain stream surface.
    pub llm_stream_calls: u64,
    /// LLM calls that returned an error.
    pub llm_errors: u64,
    /// Prompt-side tokens across all completions in this phase.
    pub prompt_tokens: u64,
    /// Completion-side tokens. Falls back to
    /// `tokens_used - prompt_tokens` when the provider doesn't report
    /// the split.
    pub completion_tokens: u64,
    /// Wall-clock spent inside `complete`/`complete_batch`, ms.
    pub llm_wall_ms: u64,
    /// Embedding calls (an `embed_batch` counts once here…)
    pub embed_calls: u64,
    /// …and its text count lands here.
    pub embed_texts: u64,
    /// Wall-clock spent inside `embed*`, ms.
    pub embed_wall_ms: u64,
    /// Rerank calls forwarded through the decorator.
    pub rerank_calls: u64,
}

impl PhaseBucket {
    fn add(&mut self, other: &PhaseBucket) {
        self.llm_calls += other.llm_calls;
        self.llm_stream_calls += other.llm_stream_calls;
        self.llm_errors += other.llm_errors;
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.llm_wall_ms += other.llm_wall_ms;
        self.embed_calls += other.embed_calls;
        self.embed_texts += other.embed_texts;
        self.embed_wall_ms += other.embed_wall_ms;
        self.rerank_calls += other.rerank_calls;
    }

    fn is_empty(&self) -> bool {
        self.llm_calls == 0
            && self.llm_stream_calls == 0
            && self.llm_errors == 0
            && self.embed_calls == 0
            && self.rerank_calls == 0
    }
}

/// One phase's row in the serialized report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResources {
    pub phase: String,
    #[serde(flatten)]
    pub bucket: PhaseBucket,
}

/// One metered call, logged individually.
///
/// The phase buckets answer "how much work did this phase do"; they
/// cannot answer "which call family inside the phase is expensive"
/// — several unrelated call families share a phase label (the window
/// skeleton and the RAPTOR tree both run under `building_skeleton`),
/// and a summed `llm_wall_ms` over concurrent calls is not a duration.
/// The per-call log restores both: `start_ms` reconstructs the real
/// concurrency picture, and `prompt_head` identifies the call family
/// without needing a new phase label for every prompt site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    /// `"llm"` or `"embed"`.
    pub kind: String,
    /// Phase label live when the call was issued.
    pub phase: String,
    /// Call start, ms since ledger construction (≈ attach). With
    /// `wall_ms` this gives the true overlap structure of a phase.
    pub start_ms: u64,
    pub wall_ms: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub prompt_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub completion_tokens: u64,
    /// Texts submitted (embed calls only).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub embed_texts: u64,
    /// First ~64 chars of the prompt, whitespace-collapsed — the call
    /// family fingerprint. Empty for embeds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Whitespace-collapsed prompt prefix used as the call-family key.
fn prompt_fingerprint(prompt: &str) -> String {
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(64).collect()
}

/// The serialized ledger: ordered per-phase rows + totals + every
/// model id that actually served a metered call (mesh routing can
/// attribute calls to a peer's model — surfacing that is the point).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReport {
    pub phases: Vec<PhaseResources>,
    pub totals: PhaseBucket,
    pub models_seen: Vec<String>,
    /// Per-call detail in completion order. See [`CallRecord`] — this is
    /// what makes "which call family owns this phase's wall clock"
    /// answerable after the fact instead of by re-running with eprintln.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CallRecord>,
}

impl ResourceReport {
    /// Fixed-width table for stderr + the markdown report. Empty
    /// phases (label set but no calls landed) are skipped.
    pub fn render_table(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<22} {:>6} {:>8} {:>12} {:>12} {:>9} {:>7} {:>9} {:>9}",
            "phase",
            "calls",
            "streams",
            "prompt_tok",
            "compl_tok",
            "llm_s",
            "embeds",
            "texts",
            "embed_s",
        );
        for row in self.phases.iter().filter(|r| !r.bucket.is_empty()) {
            let b = &row.bucket;
            let _ = writeln!(
                s,
                "{:<22} {:>6} {:>8} {:>12} {:>12} {:>9.1} {:>7} {:>9} {:>9.1}",
                row.phase,
                b.llm_calls,
                b.llm_stream_calls,
                b.prompt_tokens,
                b.completion_tokens,
                b.llm_wall_ms as f64 / 1000.0,
                b.embed_calls,
                b.embed_texts,
                b.embed_wall_ms as f64 / 1000.0,
            );
        }
        let t = &self.totals;
        let _ = writeln!(
            s,
            "{:<22} {:>6} {:>8} {:>12} {:>12} {:>9.1} {:>7} {:>9} {:>9.1}",
            "TOTAL",
            t.llm_calls,
            t.llm_stream_calls,
            t.prompt_tokens,
            t.completion_tokens,
            t.llm_wall_ms as f64 / 1000.0,
            t.embed_calls,
            t.embed_texts,
            t.embed_wall_ms as f64 / 1000.0,
        );
        if t.llm_errors > 0 {
            let _ = writeln!(s, "⚠ {} LLM call(s) errored", t.llm_errors);
        }
        if !self.models_seen.is_empty() {
            let _ = writeln!(s, "models: {}", self.models_seen.join(", "));
        }
        s
    }

    /// Roll the per-call log up by call family (phase + prompt
    /// fingerprint) and render it in first-call order.
    ///
    /// This is the table that says *where the wall clock actually went*.
    /// `window` is elapsed from the family's first call start to its
    /// last call's end — a real duration, unlike the phase bucket's
    /// `llm_wall_ms`, which sums concurrent calls and routinely exceeds
    /// the run length. `p_tok/call` and `c_tok/call` separate
    /// prefill-bound families from decode-bound ones.
    pub fn render_call_families(&self) -> String {
        use std::fmt::Write as _;
        if self.calls.is_empty() {
            return String::new();
        }
        // (phase, head) -> aggregate, in first-appearance order.
        let mut order: Vec<(String, String)> = Vec::new();
        let mut agg: Vec<FamilyAgg> = Vec::new();
        for c in &self.calls {
            let key = (c.phase.clone(), c.prompt_head.clone());
            let idx = match order.iter().position(|k| *k == key) {
                Some(i) => i,
                None => {
                    order.push(key);
                    agg.push(FamilyAgg::new(&c.kind));
                    agg.len() - 1
                }
            };
            agg[idx].absorb(c);
        }
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<20} {:>5} {:>10} {:>10} {:>8} {:>8} {:>8}  {}",
            "phase", "n", "p_tok/call", "c_tok/call", "med_s", "window", "start", "call family",
        );
        for (i, (phase, head)) in order.iter().enumerate() {
            let a = &agg[i];
            let label = if a.kind == "embed" {
                format!("<embed {} texts>", a.embed_texts)
            } else if head.is_empty() {
                "<no prompt>".to_string()
            } else {
                head.chars().take(52).collect()
            };
            let _ = writeln!(
                s,
                "{:<20} {:>5} {:>10} {:>10} {:>8.1} {:>8.1} {:>8.1}  {}",
                phase,
                a.n,
                a.prompt_tokens / a.n.max(1),
                a.completion_tokens / a.n.max(1),
                a.median_wall_ms() as f64 / 1000.0,
                (a.last_end_ms.saturating_sub(a.first_start_ms)) as f64 / 1000.0,
                a.first_start_ms as f64 / 1000.0,
                label,
            );
        }
        s
    }
}

/// Accumulator behind [`ResourceReport::render_call_families`].
struct FamilyAgg {
    kind: String,
    n: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    embed_texts: u64,
    first_start_ms: u64,
    last_end_ms: u64,
    walls: Vec<u64>,
}

impl FamilyAgg {
    fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            n: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            embed_texts: 0,
            first_start_ms: u64::MAX,
            last_end_ms: 0,
            walls: Vec::new(),
        }
    }

    fn absorb(&mut self, c: &CallRecord) {
        self.n += 1;
        self.prompt_tokens += c.prompt_tokens;
        self.completion_tokens += c.completion_tokens;
        self.embed_texts += c.embed_texts;
        self.first_start_ms = self.first_start_ms.min(c.start_ms);
        self.last_end_ms = self.last_end_ms.max(c.start_ms + c.wall_ms);
        self.walls.push(c.wall_ms);
    }

    fn median_wall_ms(&self) -> u64 {
        if self.walls.is_empty() {
            return 0;
        }
        let mut w = self.walls.clone();
        w.sort_unstable();
        w[w.len() / 2]
    }
}

#[derive(Default)]
struct LedgerInner {
    /// Ordered (phase, bucket) — Vec keeps first-set-phase ordering so
    /// the report reads in pipeline order.
    phases: Vec<(String, PhaseBucket)>,
    current: String,
    models_seen: Vec<String>,
    calls: Vec<CallRecord>,
}

impl LedgerInner {
    fn bucket(&mut self) -> &mut PhaseBucket {
        // Split borrow dance: find index first, then index mutably.
        let cur = self.current.clone();
        if let Some(i) = self.phases.iter().position(|(p, _)| *p == cur) {
            &mut self.phases[i].1
        } else {
            self.phases.push((cur, PhaseBucket::default()));
            &mut self.phases.last_mut().expect("just pushed").1
        }
    }
}

/// Shared, phase-labelled resource accumulator. Cheap to clone the
/// `Arc`; all mutation is behind one short-hold `Mutex` (the metered
/// calls are seconds-long; the lock hold is nanoseconds).
pub struct ResourceLedger {
    inner: Mutex<LedgerInner>,
    /// Zero point for `CallRecord::start_ms` — ledger construction,
    /// which the harness does immediately before attach.
    origin: Instant,
}

impl ResourceLedger {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LedgerInner {
                phases: Vec::new(),
                current: "setup".to_string(),
                models_seen: Vec::new(),
                calls: Vec::new(),
            }),
            origin: Instant::now(),
        }
    }

    /// Ms since ledger construction — the `start_ms` clock.
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn push_call(&self, rec: CallRecord) {
        if let Ok(mut g) = self.inner.lock() {
            g.calls.push(rec);
        }
    }

    /// Switch the attribution bucket. Idempotent per label — calls
    /// after a repeated label accumulate into the same bucket.
    pub fn set_phase(&self, phase: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.current = phase.to_string();
        }
    }

    fn record_completion(
        &self,
        prompt: &str,
        resp: &CompletionResponse,
        start_ms: u64,
        wall_ms: u64,
    ) {
        let rec = if let Ok(mut g) = self.inner.lock() {
            let model = resp.model_id.clone();
            if !model.is_empty() && !g.models_seen.contains(&model) {
                g.models_seen.push(model);
            }
            let phase = g.current.clone();
            let completion = resp
                .completion_tokens
                .map(u64::from)
                .unwrap_or_else(|| (resp.tokens_used.saturating_sub(resp.prompt_tokens)) as u64);
            let b = g.bucket();
            b.llm_calls += 1;
            b.prompt_tokens += resp.prompt_tokens as u64;
            b.completion_tokens += completion;
            b.llm_wall_ms += wall_ms;
            Some(CallRecord {
                kind: "llm".to_string(),
                phase,
                start_ms,
                wall_ms,
                prompt_tokens: resp.prompt_tokens as u64,
                completion_tokens: completion,
                embed_texts: 0,
                prompt_head: prompt_fingerprint(prompt),
                model: resp.model_id.clone(),
            })
        } else {
            None
        };
        if let Some(rec) = rec {
            self.push_call(rec);
        }
    }

    fn record_llm_error(&self, wall_ms: u64) {
        if let Ok(mut g) = self.inner.lock() {
            let b = g.bucket();
            b.llm_errors += 1;
            b.llm_wall_ms += wall_ms;
        }
    }

    fn record_stream_call(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.bucket().llm_stream_calls += 1;
        }
    }

    fn record_embed(&self, texts: u64, start_ms: u64, wall_ms: u64) {
        let rec = if let Ok(mut g) = self.inner.lock() {
            let phase = g.current.clone();
            let b = g.bucket();
            b.embed_calls += 1;
            b.embed_texts += texts;
            b.embed_wall_ms += wall_ms;
            Some(CallRecord {
                kind: "embed".to_string(),
                phase,
                start_ms,
                wall_ms,
                prompt_tokens: 0,
                completion_tokens: 0,
                embed_texts: texts,
                prompt_head: String::new(),
                model: String::new(),
            })
        } else {
            None
        };
        if let Some(rec) = rec {
            self.push_call(rec);
        }
    }

    fn record_rerank(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.bucket().rerank_calls += 1;
        }
    }

    pub fn snapshot(&self) -> ResourceReport {
        let g = self.inner.lock().expect("ledger lock poisoned");
        let phases: Vec<PhaseResources> = g
            .phases
            .iter()
            .map(|(p, b)| PhaseResources {
                phase: p.clone(),
                bucket: b.clone(),
            })
            .collect();
        let mut totals = PhaseBucket::default();
        for row in &phases {
            totals.add(&row.bucket);
        }
        let mut calls = g.calls.clone();
        calls.sort_by_key(|c| c.start_ms);
        ResourceReport {
            phases,
            totals,
            models_seen: g.models_seen.clone(),
            calls,
        }
    }
}

/// Transparent metering decorator. Forwards every trait method to the
/// wrapped provider (so mesh-aware overrides keep working) and records
/// usage into the shared ledger.
pub struct MeteredInference {
    inner: Arc<dyn InferenceProvider>,
    ledger: Arc<ResourceLedger>,
}

impl MeteredInference {
    pub fn new(inner: Arc<dyn InferenceProvider>, ledger: Arc<ResourceLedger>) -> Self {
        Self { inner, ledger }
    }
}

#[async_trait]
impl InferenceProvider for MeteredInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let start_ms = self.ledger.now_ms();
        let start = Instant::now();
        let result = self.inner.complete(request).await;
        let wall_ms = start.elapsed().as_millis() as u64;
        match &result {
            Ok(resp) => self
                .ledger
                .record_completion(&request.prompt, resp, start_ms, wall_ms),
            Err(_) => self.ledger.record_llm_error(wall_ms),
        }
        result
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        let start_ms = self.ledger.now_ms();
        let start = Instant::now();
        let result = self.inner.complete_batch(requests).await;
        let wall_ms = start.elapsed().as_millis() as u64;
        match &result {
            Ok(responses) => {
                // Attribute the batch's wall time to its first response;
                // per-response provider latency is already in each row.
                for (i, resp) in responses.iter().enumerate() {
                    let prompt = requests.get(i).map(|r| r.prompt.as_str()).unwrap_or("");
                    self.ledger.record_completion(
                        prompt,
                        resp,
                        start_ms,
                        if i == 0 { wall_ms } else { 0 },
                    );
                }
            }
            Err(_) => self.ledger.record_llm_error(wall_ms),
        }
        result
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.ledger.record_stream_call();
        self.inner.complete_stream(request).await
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        self.ledger.record_stream_call();
        self.inner.complete_stream_with_id(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.ledger.record_stream_call();
        self.inner.complete_stream_with_finish(request).await
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        self.ledger.record_stream_call();
        self.inner.complete_stream_with_id_and_finish(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let start_ms = self.ledger.now_ms();
        let start = Instant::now();
        let result = self.inner.embed(text).await;
        self.ledger
            .record_embed(1, start_ms, start.elapsed().as_millis() as u64);
        result
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let start_ms = self.ledger.now_ms();
        let start = Instant::now();
        let result = self.inner.embed_batch(texts).await;
        self.ledger.record_embed(
            texts.len() as u64,
            start_ms,
            start.elapsed().as_millis() as u64,
        );
        result
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let start_ms = self.ledger.now_ms();
        let start = Instant::now();
        let result = self.inner.embed_query(query).await;
        self.ledger
            .record_embed(1, start_ms, start.elapsed().as_millis() as u64);
        result
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.ledger.record_rerank();
        self.inner.rerank_batch(query, docs).await
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.inner.warmup_primary().await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.inner.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.inner.embed_model_id()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.inner.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.inner.n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.inner.count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.inner.code_model_id()
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.inner.edit_slot_info()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.inner.load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.inner.unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inner.extras_inventory()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.inner.resident_slots()
    }

    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        self.inner.compute_children()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_ordering_and_totals() {
        let ledger = ResourceLedger::new();
        ledger.set_phase("indexing");
        ledger.record_embed(100, 0, 500);
        ledger.set_phase("building_skeleton");
        ledger.record_completion(
            "You are summarizing a group of related passages",
            &CompletionResponse {
                text: "x".into(),
                tokens_used: 1200,
                prompt_tokens: 1000,
                model_id: "test-model".into(),
                latency_ms: 900,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None, // exercise the fallback split
            },
            600,
            900,
        );
        // Return to a previously-seen phase — must accumulate, not duplicate.
        ledger.set_phase("indexing");
        ledger.record_embed(50, 1600, 200);

        let report = ledger.snapshot();
        assert_eq!(report.phases.len(), 2);
        assert_eq!(report.phases[0].phase, "indexing");
        assert_eq!(report.phases[0].bucket.embed_texts, 150);
        assert_eq!(report.phases[1].bucket.prompt_tokens, 1000);
        assert_eq!(report.phases[1].bucket.completion_tokens, 200);
        assert_eq!(report.totals.llm_calls, 1);
        assert_eq!(report.totals.embed_calls, 2);
        assert_eq!(report.models_seen, vec!["test-model".to_string()]);
    }

    fn resp(prompt_tokens: usize, completion: usize) -> CompletionResponse {
        CompletionResponse {
            text: "x".into(),
            tokens_used: prompt_tokens + completion,
            prompt_tokens,
            model_id: "test-model".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: Some(completion as u32),
        }
    }

    /// The regression this whole per-call log exists to prevent: two
    /// unrelated call families under one phase label, where the phase
    /// bucket alone cannot say which one owns the wall clock.
    #[test]
    fn call_families_separate_two_families_sharing_a_phase() {
        let ledger = ResourceLedger::new();
        ledger.set_phase("building_skeleton");
        // Family A: cheap, many, early.
        for i in 0..3u64 {
            ledger.record_completion(
                "Extract the entities named in each passage below",
                &resp(1000, 50),
                i * 1000,
                900,
            );
        }
        // Family B: expensive prefill, few, late — the one to find.
        for i in 0..2u64 {
            ledger.record_completion(
                "You are summarizing a group of related passages",
                &resp(3000, 120),
                10_000 + i * 1000,
                8_000,
            );
        }

        let report = ledger.snapshot();
        // One phase bucket; the phase table cannot tell these apart.
        assert_eq!(report.phases.len(), 1);
        assert_eq!(report.calls.len(), 5);

        let table = report.render_call_families();
        let lines: Vec<&str> = table.lines().skip(1).collect();
        assert_eq!(lines.len(), 2, "one row per family:\n{table}");
        assert!(lines[0].contains("Extract the entities"), "{table}");
        assert!(lines[1].contains("You are summarizing"), "{table}");
        // Family B's per-call prefill is the standout signal.
        assert!(lines[1].contains("3000"), "{table}");
        // window = 10.0s start → 19.0s last end = 9.0s of real elapsed.
        assert!(lines[1].contains("9.0"), "{table}");
    }

    #[test]
    fn embed_calls_roll_up_with_text_counts() {
        let ledger = ResourceLedger::new();
        ledger.set_phase("indexing");
        ledger.record_embed(64, 0, 5_000);
        ledger.record_embed(64, 5_000, 5_000);

        let table = ledger.snapshot().render_call_families();
        assert!(table.contains("<embed 128 texts>"), "{table}");
    }
}
