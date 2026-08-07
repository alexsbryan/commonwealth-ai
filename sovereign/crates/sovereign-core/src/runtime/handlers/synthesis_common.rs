// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conventions every synthesis-shaped handler stamps identically —
//! extracted from `simple.rs` / `complex_task.rs` / `attached_doc.rs`.
//!
//! An 18-month git-coupling analysis flagged the three handlers as
//! hidden coupling (11-12 joint commits per pair, no structural
//! edge). The mirrored hunks were always the same three shapes:
//!
//! 1. the synthesis `CompletionRequest` core — `Speed::Slow` + the
//!    `inference_config`-derived knobs, everything else default
//!    (`tools`/`tool_choice` in dd6a523c, `assistant_prefix`/
//!    `cmd_prefix` in 95d8517d, `url_allowlist` in e40e9297 — each
//!    landed as N identical `None` lines);
//! 2. the completion-telemetry tail of `ResponseProvenance`
//!    (`finish_reason`/`max_tokens_budget`/`completion_tokens` in
//!    2d7fa715, `context_window` in dc8ee229 — three identical hunks
//!    each);
//! 3. the transcript-shaped grounding `EvidenceContext` defaults
//!    (`top_similarity` in 154effda, `chunk_labels` in 6d6d25ee).
//!
//! Each shape now has one owner here. Handlers override only their
//! surface-varying fields via struct-update syntax, so a new plumbed
//! field is a one-file edit.

use std::sync::Arc;

use super::super::*;
use crate::runtime::grounding::{EvidenceContext, SealedEvidenceSearch};

impl Runtime {
    /// The synthesis-call request core shared by the non-streaming
    /// handlers: primary (Slow) slot + the operator-configured
    /// sampling knobs. Surface-specific fields (`system_message`,
    /// speed/budget overrides, allowlists) are layered on by the
    /// caller with struct-update syntax; unset knobs take
    /// `CompletionRequest`'s derived defaults.
    pub(crate) fn synthesis_request(
        &self,
        prompt: String,
        oicp: Option<crate::oicp::InferenceRequirements>,
    ) -> CompletionRequest {
        CompletionRequest {
            prompt,
            preferred_speed: Speed::Slow,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            top_k: self.inference_config.top_k,
            oicp,
            ..Default::default()
        }
    }

    /// The completion-derived + config-derived telemetry tail of
    /// [`ResponseProvenance`], identical on every synthesis surface.
    /// Surface-varying fields (`search_method`, `sources`,
    /// `coarse_intent`, `self_assessment`, `routing_trigger`,
    /// `coverage`) come back neutral (`None`/empty) for the caller
    /// to override via struct-update syntax.
    pub(crate) fn synthesis_provenance(
        &self,
        intent: impl Into<String>,
        completion: &CompletionResponse,
    ) -> ResponseProvenance {
        completion_provenance(
            intent.into(),
            completion,
            self.inference_config.max_tokens,
            self.inference.effective_context_size(),
        )
    }
}

/// Pure core of [`Runtime::synthesis_provenance`] — separated so the
/// field mapping is unit-testable without constructing a `Runtime`.
pub(crate) fn completion_provenance(
    intent: String,
    completion: &CompletionResponse,
    max_tokens_budget: usize,
    context_window: Option<u32>,
) -> ResponseProvenance {
    ResponseProvenance {
        intent,
        search_method: None,
        sources: Vec::new(),
        inference_backend: completion.model_id.clone(),
        oicp_match: completion
            .oicp_meta
            .as_ref()
            .and_then(|m| m.match_quality.as_ref())
            .map(|q| format!("{q:?}")),
        total_latency_ms: completion.latency_ms,
        tokens_used: completion.tokens_used,
        coarse_intent: None,
        self_assessment: None,
        routing_trigger: None,
        coverage: None,
        finish_reason: completion.finish_reason.clone(),
        max_tokens_budget: Some(max_tokens_budget),
        completion_tokens: completion.completion_tokens,
        context_window,
    }
}

/// Grounding-gate evidence for transcript-shaped surfaces (tool-result
/// transcripts, step summaries): synthesized/relayed prose, not
/// retrieved chunks — so no source/chunk labels (the citation check
/// runs body-only) and no retrieval similarity (the retry floor stays
/// disabled). Retrieval-shaped surfaces (e.g. `handle_simple`) build
/// their `EvidenceContext` from the `gate_evidence_*` helpers instead.
pub(crate) fn transcript_gate_evidence(
    chunks: Vec<String>,
    searcher: Option<Arc<dyn SealedEvidenceSearch>>,
    entity_anchored: bool,
) -> EvidenceContext {
    EvidenceContext {
        chunks,
        source_labels: Vec::new(),
        chunk_labels: Vec::new(),
        chunk_locators: Vec::new(),
        chunk_targets: Vec::new(),
        searcher,
        entity_anchored,
        top_similarity: None,
        // Transcript prose is not RAPTOR-derived; empty = all-Leaf
        // degradation (T1 P1.4).
        chunk_sources: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FinishReason;

    fn completion() -> CompletionResponse {
        CompletionResponse {
            text: "answer".to_string(),
            tokens_used: 321,
            prompt_tokens: 100,
            model_id: "qwen-test".to_string(),
            latency_ms: 42,
            oicp_meta: Some(crate::oicp::OicpResponseMeta {
                quantization: None,
                match_quality: Some(crate::oicp::MatchQuality::Full),
                request_id: None,
                model_fingerprint: None,
            }),
            finish_reason: Some(FinishReason::Length),
            completion_tokens: Some(221),
            ..Default::default()
        }
    }

    /// Round-trip of the telemetry tail: every completion-derived
    /// field lands, every surface-varying field comes back neutral.
    #[test]
    fn completion_provenance_maps_telemetry_and_leaves_surface_fields_neutral() {
        let p = completion_provenance("ComplexTask".to_string(), &completion(), 4096, Some(16384));

        // Completion/config-derived tail (the fields git shows being
        // mirror-edited across the three handlers).
        assert_eq!(p.intent, "ComplexTask");
        assert_eq!(p.inference_backend, "qwen-test");
        assert_eq!(p.oicp_match.as_deref(), Some("Full"));
        assert_eq!(p.total_latency_ms, 42);
        assert_eq!(p.tokens_used, 321);
        assert!(matches!(p.finish_reason, Some(FinishReason::Length)));
        assert_eq!(p.max_tokens_budget, Some(4096));
        assert_eq!(p.completion_tokens, Some(221));
        assert_eq!(p.context_window, Some(16384));

        // Surface-varying fields stay neutral for struct-update.
        assert!(p.search_method.is_none());
        assert!(p.sources.is_empty());
        assert!(p.coarse_intent.is_none());
        assert!(p.self_assessment.is_none());
        assert!(p.routing_trigger.is_none());
        assert!(p.coverage.is_none());
    }

    #[test]
    fn completion_provenance_without_oicp_meta() {
        let mut c = completion();
        c.oicp_meta = None;
        c.finish_reason = None;
        c.completion_tokens = None;
        let p = completion_provenance("AttachedDoc".to_string(), &c, 2048, None);
        assert!(p.oicp_match.is_none());
        assert!(p.finish_reason.is_none());
        assert!(p.completion_tokens.is_none());
        assert!(p.context_window.is_none());
        assert_eq!(p.max_tokens_budget, Some(2048));
    }

    /// Locks the transcript-evidence convention: body-only citation
    /// check (no labels), no similarity floor.
    #[test]
    fn transcript_gate_evidence_is_label_free_and_floor_free() {
        let ev = transcript_gate_evidence(vec!["Step 0: x".to_string()], None, true);
        assert_eq!(ev.chunks, vec!["Step 0: x".to_string()]);
        assert!(ev.source_labels.is_empty());
        assert!(ev.chunk_labels.is_empty());
        assert!(ev.searcher.is_none());
        assert!(ev.entity_anchored);
        assert!(ev.top_similarity.is_none());
    }
}
