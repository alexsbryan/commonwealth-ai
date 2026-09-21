// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for streaming continuation — see `streaming.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Minimal inference stub: every `complete` returns the same canned reply
/// and counts the calls, so the continuation loop is asserted
/// deterministically — no model, no routing (which we can't steer to the
/// kq path from a test anyway).
struct ContinuationMock {
    reply: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for ContinuationMock {
    async fn complete(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<crate::types::CompletionResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::types::CompletionResponse {
            text: self.reply.clone(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "continuation-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(crate::error::Error::NotImplemented(
            "continuation mock: no streaming".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> crate::types::ProviderCapabilities {
        crate::types::ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: crate::types::Depth::Moderate,
        }
    }
}

fn stub(reply: &str) -> (Arc<dyn crate::traits::InferenceProvider>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let inf: Arc<dyn crate::traits::InferenceProvider> = Arc::new(ContinuationMock {
        reply: reply.to_string(),
        calls: calls.clone(),
    });
    (inf, calls)
}

async fn run(inf: &Arc<dyn crate::traits::InferenceProvider>, full: &mut String) {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let cancel = tokio_util::sync::CancellationToken::new();
    let req = crate::types::CompletionRequest::default();
    // gate_on=true: held flow, so no token is streamed to the (dropped) rx.
    continue_truncated_synthesis(inf, &req, &tx, &cancel, full, true, "test").await;
}

#[tokio::test]
async fn lands_a_truncated_draft() {
    let (inf, calls) = stub(" man, and so the only cure is to control its effects.");
    let mut full =
        "Madison argues the latent causes of faction are sown into the nature of".to_string();
    assert!(crate::runtime::evidence::ends_mid_thought(&full));
    run(&inf, &mut full).await;
    assert!(
        !crate::runtime::evidence::ends_mid_thought(&full),
        "answer landed on a boundary: {full:?}"
    );
    assert!(
        full.ends_with("effects."),
        "continuation was stitched: {full:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one continuation sufficed");
}

#[tokio::test]
async fn bounded_when_never_landing() {
    // A reply that itself ends mid-thought every time must NOT loop forever.
    let (inf, calls) = stub(" and then it just keeps trailing on and on without any end so");
    // Above the min-length guard, so the loop actually engages.
    let mut full = "This particular answer was unfortunately cut off right at the".to_string();
    run(&inf, &mut full).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "stops at the round cap, never loops"
    );
}

#[tokio::test]
async fn skips_complete_or_short_drafts() {
    let (inf, calls) = stub(" extra text");
    let mut done = "A complete sentence.".to_string();
    run(&inf, &mut done).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a complete draft is left alone"
    );
    // A sub-threshold degenerate stub (e.g. a stray "search") is not "continued".
    let mut stubby = "search".to_string();
    run(&inf, &mut stubby).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a tiny stub is left alone");
}
