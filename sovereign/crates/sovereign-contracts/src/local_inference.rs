// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI-shaped face of a local inference provider, kept out of
//! `traits.rs` so the contract crate's largest module does not grow with every
//! port it gains (ARCH §3.1, "trim or split"). Re-exported from
//! `crate::traits`, so `sovereign_core::traits::LocalInferenceService` and
//! `sovereign_contracts::traits::LocalInferenceService` are unchanged.
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::traits::InferenceProvider;

/// The OpenAI-shaped face of a local inference provider: chat completions,
/// the provider manifest, FIM inline completion, and editing-slot status.
///
/// This was `sovereign_api::state::LocalInferenceService`, a second inference
/// port minted when the API crate could not depend on the runtime. That reason
/// expired when it began to, so the duplicated half of the port collapses onto
/// [`InferenceProvider`], which this trait extends: every method that already
/// exists there — `embed`, `embed_batch`, `load_extra_slot`, `unload_extra_slot`,
/// `extras_inventory`, `resident_slots`, `decode_evidence`, `warmup_primary`,
/// `compute_children`, `peer_manifests`, `lender_manifest` — is reached through
/// the supertrait and is not re-declared here. What remains is the translation
/// the OpenAI wire needs and the two capabilities only a FIM-capable backend
/// carries (`sovereign/SERVING_BOUNDARY.md` "The two tiers"; domains
/// `REVIEW-build-local-inference`).
///
/// The OpenAI request and response translation is an EDGE adapter implemented in
/// the daemon (`sovereign_mesh::inference_adapter`), not part of the port's
/// arithmetic; the FIM stream and edit-slot status stay with `fim_adapter` over
/// the same provider.
#[async_trait]
pub trait LocalInferenceService: InferenceProvider {
    /// One-shot chat completion (non-streaming). Called when the
    /// incoming request did NOT set `stream: true`.
    async fn chat_completion(
        &self,
        request: crate::oicp::openai_types::ChatCompletionRequest,
    ) -> std::result::Result<
        crate::oicp::openai_types::ChatCompletionResponse,
        crate::oicp::LocalInferenceError,
    >;

    /// Streaming chat completion. Yields a sequence of typed
    /// [`StreamFrame`]s — `Token(piece)` for each text delta and a
    /// terminal `Finish { reason, usage }` carrying the OpenAI
    /// `finish_reason` (`Stop` / `Length` / `Cancelled` / etc.).
    /// `serve_local_stream` translates this into OpenAI-shaped SSE
    /// chunks, with a final empty-delta chunk that surfaces the
    /// real `finish_reason` to the wire.
    ///
    /// Streams MUST end with either `Finish` or `Error`; the route
    /// treats a closed channel without a terminal frame as `Cancelled`.
    async fn chat_completion_stream(
        &self,
        request: crate::oicp::openai_types::ChatCompletionRequest,
    ) -> std::result::Result<
        Pin<Box<dyn Stream<Item = crate::oicp::openai_types::StreamFrame> + Send>>,
        crate::oicp::LocalInferenceError,
    >;

    /// Provider manifest for `/oicp/v1/capabilities`. Peers fetch
    /// this to know what capabilities this node advertises — the
    /// MeshAwareSelector on the client side uses it to pick a
    /// backend. Returning `None` falls through to the scheduler-
    /// based manifest path.
    fn provider_manifest(&self) -> Option<crate::oicp::ProviderManifest>;

    /// FIM inline completion (`POST /v1/completions`,
    /// `sovereign/docs/INLINE_COMPLETION.md`). Default `Err` — the
    /// route maps it to 503 with the actionable `[models.fim]` fix.
    /// Only the embedded llama.cpp adapter overrides.
    async fn fim_completion_stream(
        &self,
        request: crate::oicp::FimCompletionRequest,
    ) -> std::result::Result<crate::oicp::FimStreamStart, String> {
        let _ = request;
        Err(
            "this local inference service does not serve FIM completions \
             — only the embedded llama.cpp service does"
                .to_string(),
        )
    }

    /// Static editing-slot description for `/status.inference.edit`.
    /// `None` (the default) = no editing model on this node.
    fn edit_status(&self) -> Option<crate::oicp::EditSlotStatus> {
        None
    }
}
