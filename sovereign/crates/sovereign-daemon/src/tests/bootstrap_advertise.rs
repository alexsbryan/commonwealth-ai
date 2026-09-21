// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for advertisement — see `bootstrap.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, Depth, Speed};

/// A provider whose embed probe SUCCEEDS while owning no weights — the
/// terminal's actual shape, since `SplitInferenceProvider` forwards the
/// probe to the entry node and returns a perfectly good vector.
///
/// This is the whole point of the test: the probe cannot be the thing that
/// decides whether to advertise, because on a terminal it answers a
/// question about somebody else's machine.
struct ForwardingProbe;

#[async_trait::async_trait]
impl InferenceProvider for ForwardingProbe {
    async fn complete(
        &self,
        _request: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unreachable!("advertise_embed_model never completes a turn")
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send + 'static>,
        >,
    > {
        unreachable!("advertise_embed_model never streams")
    }

    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(vec![0.0; 1024])
    }

    fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
        sovereign_core::types::ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// A terminal advertises NO embed model, even though its probe answers.
///
/// Before the guard, `model_id` was resolved as `stem → entry_embed_model`,
/// so this node published its ENTRY node's model as its own capability —
/// and `capabilities.rs` filters collaborative-ingestion candidates by
/// exact match on that field, so the planner would partition chunks onto a
/// machine that can only proxy each one back to the node it was spreading
/// load off (§18.3).
#[tokio::test]
async fn a_terminal_advertises_no_embed_model_even_though_its_probe_answers() {
    let mut cfg = SetupConfig::unconfigured();
    cfg.node.entry = Some("http://halo:9741/v1".into());
    cfg.node.entry_embed_model = Some("qwen3-embedding-0.6b".into());

    let ad = advertise_embed_model(Arc::new(ForwardingProbe), &cfg, ModelFamily::Unknown).await;

    assert!(
        ad.info().is_none(),
        "a terminal published an embed capability it does not hold: {:?}",
        ad.info().map(|i| i.model_id.clone()),
    );
    match ad {
        crate::EmbedAdvertisement::Unavailable { reason } => assert!(
            reason.contains("terminal") && reason.contains("halo"),
            "the absence must say WHY and name the entry node, got: {reason}"
        ),
        crate::EmbedAdvertisement::Advertised(_) => {
            unreachable!("asserted absent above")
        }
    }
}
