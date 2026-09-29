// SPDX-License-Identifier: AGPL-3.0-or-later
//! The resource ledger's serialized report: what an attached-document build
//! cost, per phase and per call. svrn's probe meters the build and writes this
//! as evidence; bench renders and persists it (pb-bench-dials-docs).

use serde::{Deserialize, Serialize};

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
    /// Fold `other`'s counters into this bucket.
    pub fn add(&mut self, other: &PhaseBucket) {
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
