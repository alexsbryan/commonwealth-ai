// SPDX-License-Identifier: AGPL-3.0-or-later
//! The provider a `kind = "llama-server"` node serves: the remote half built
//! against the live router, forwarding every method, with the answers only
//! the router's owner can give: whose weights these are, the router's own
//! lifecycle for `/status`, and the host facts the remote half could only
//! assume (each model's residency, the code model, the trained context;
//! svrn-docs ENGINE_SWAP_PLAN.md A1), and the edit slot with a raw prompt's
//! own route (A3).

use std::borrow::Cow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use sovereign_contracts::setup_config::EditSection;
use sovereign_contracts::traits::{
    frames_to_text_stream, InferenceProvider, ResidentSlot, ServingLocus,
};
use sovereign_contracts::types::*;
use sovereign_contracts::{oicp, Result};

use super::live::LiveRouter;
use super::RouterPreset;

/// The remote half, the router it talks to, and the preset this build asked
/// it to serve.
pub struct RouterProvider {
    inner: Arc<dyn InferenceProvider>,
    router: Arc<LiveRouter>,
    preset: RouterPreset,
    n_ctx_train: Option<u32>,
    edit_slot: Option<EditSlotInfo>,
}

/// A typed completion stream.
type Frames = Pin<Box<dyn Stream<Item = StreamFrame> + Send>>;

impl RouterProvider {
    /// Wrap `inner`, the remote provider built from `preset`'s section of
    /// `router`. Reads the primary GGUF's trained context from its header;
    /// one that cannot be read is reported as unknown, never guessed. With
    /// `edit` (`[models.edit]`), asks the router for the editing model's FIM
    /// style; blocking, so call it off the async workers, as
    /// [`LiveRouter::start`].
    pub fn new(
        inner: Arc<dyn InferenceProvider>,
        router: Arc<LiveRouter>,
        preset: RouterPreset,
        edit: Option<&EditSection>,
    ) -> Self {
        let primary = preset.primary().map(str::to_string);
        let n_ctx_train = match primary.as_deref().and_then(|p| preset.path_of(p)) {
            Some(path) => match sovereign_inference::gguf_meta::read_context_length(path) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(target: "llama_router", model = %path.display(), error = %e, "n_ctx_train unknown: the GGUF header is unreadable");
                    None
                }
            },
            // An embed-only router serves no chat model, so there is no
            // primary header to read.
            None => None,
        };
        tracing::debug!(target: "llama_router", model = ?primary, ?n_ctx_train, "router provider: primary's trained context");
        let edit_slot = edit.zip(preset.edit()).map(|(section, model)| {
            // The fast turn's model: the fast role, else the primary, as
            // `ModelsSection::fast_path` resolves it for the embedded alias.
            let roles = preset.roles();
            let fast_model = roles
                .iter()
                .find(|(role, _)| *role == "fast")
                .map_or_else(|| preset.primary().unwrap_or_default(), |(_, m)| *m);
            let fast = fast_model == model;
            super::edit::edit_slot(&router, model, section, fast)
        });
        Self {
            inner,
            router,
            preset,
            n_ctx_train,
            edit_slot,
        }
    }

    /// Where a Raw request goes: the router model it names (else the one its
    /// speed selects) as a raw `/completion`, with the stop strings of the
    /// edit slot's FIM family when it is the editing model. `None` for a
    /// request that is not Raw, or names a model outside the preset, which
    /// the inner provider routes.
    fn raw_route(&self, request: &CompletionRequest) -> Option<(String, &'static [&'static str])> {
        if request.prompt_shape != Some(PromptShape::Raw) {
            return None;
        }
        let model = match &request.model_id {
            Some(m) if self.preset.path_of(m).is_some() => m.clone(),
            Some(m) => {
                tracing::debug!(target: "llama_router", model = %m, "raw request for a model outside the preset: forwarded");
                return None;
            }
            None => self.inner.model_id_for(request.preferred_speed),
        };
        let stops = self
            .edit_slot
            .as_ref()
            .filter(|e| e.model_id == model)
            .and_then(|e| e.fim.as_ref())
            .map_or(&[][..], |f| {
                sovereign_contracts::fim::markers_for(f.style).stop_strings
            });
        tracing::debug!(target: "llama_router", %model, stops = stops.len(), "raw request: verbatim to /completion");
        Some((model, stops))
    }

    async fn raw(
        &self,
        model: &str,
        stops: &[&str],
        request: &CompletionRequest,
    ) -> Result<Frames> {
        oicp_client::llama_server_raw_completion(&self.router.base_url(), model, request, stops)
            .await
    }

    /// The router loads only the models its preset names, and the slots
    /// `/internal/models/load` manages are GGUFs outside it; the inner
    /// refusal would say this node holds no weights, which is not so here.
    fn extras_refusal(&self, verb: &str, slot: &str) -> sovereign_contracts::Error {
        tracing::debug!(target: "llama_router", verb, slot, "router provider: extra slot refused");
        sovereign_contracts::Error::Inference(format!(
            "runtime slot {verb} of {slot:?} is refused: the llama-server router serves only its \
             preset ({}); add the GGUF to [models] and restart the daemon",
            self.preset.warm_models().join(", ")
        ))
    }

    /// [`slot_penalties`] for this router's code model.
    fn with_slot_penalties<'a>(
        &self,
        request: &'a CompletionRequest,
    ) -> Cow<'a, CompletionRequest> {
        slot_penalties(request, self.model_for("code"))
    }

    /// The served kind's role name for the cross-encoder, so the preset's
    /// role lookup and the kind registry cannot disagree about it.
    fn rerank_role(&self) -> &'static str {
        sovereign_inference::served_kind::RERANK.role
    }

    fn model_for(&self, role: &str) -> Option<&str> {
        self.preset
            .roles()
            .into_iter()
            .find(|(r, _)| *r == role)
            .map(|(_, m)| m)
    }
}

/// A daemon call with the penalties the embedded engine samples it with.
///
/// The arithmetic is not this function's: the embedded quirks sampler
/// answers it (`embedded::sampler::quirks_repeat_penalties`), and asking
/// it is what keeps the two engines on one decider instead of two — the
/// drift between them was the whole of the 2026-10-25 finding. So is the
/// "which requests are sampled from the quirks at all" question: the
/// embedded engine samples a conversation with no `sampling_mode` as
/// llama-server does (llama.cpp's defaults, the GGUF's, then the
/// client's), and that call answers none, so the router passes it
/// untouched too. A caller's own value stands.
fn slot_penalties<'a>(
    request: &'a CompletionRequest,
    code_model: Option<&str>,
) -> Cow<'a, CompletionRequest> {
    if sovereign_contracts::types::is_server_default_shape(
        request.prompt_shape.as_ref(),
        request.sampling_mode,
    ) {
        return Cow::Borrowed(request);
    }
    if request.repeat_penalty.is_some() && request.frequency_penalty.is_some() {
        return Cow::Borrowed(request);
    }
    let family = match request.model_id.as_deref() == code_model && code_model.is_some() {
        true => sovereign_inference::engine_factory::CODE_SLOT_FAMILY,
        false => sovereign_inference::engine_factory::CHAT_SLOT_FAMILY,
    };
    let quirks = family.default_quirks();
    let (repeat, frequency) = sovereign_inference::embedded::quirks_repeat_penalties(&quirks);
    let mut penalised = request.clone();
    penalised.repeat_penalty = penalised.repeat_penalty.or(Some(repeat));
    penalised.frequency_penalty = penalised.frequency_penalty.or(Some(frequency));
    tracing::debug!(
        target: "llama_router",
        ?family,
        repeat_penalty = ?penalised.repeat_penalty,
        frequency_penalty = ?penalised.frequency_penalty,
        "a daemon call carries its slot's penalties"
    );
    Cow::Owned(penalised)
}

/// Every method forwards, defaults included: a default left in place here
/// would answer for the wrapper instead of the engine behind it.
#[async_trait]
impl InferenceProvider for RouterProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        match self.raw_route(request) {
            Some((model, stops)) => {
                let started = Instant::now();
                let frames = self.raw(&model, stops, request).await?;
                super::edit::collect(frames, model, started).await
            }
            None => {
                self.inner
                    .complete(&self.with_slot_penalties(request))
                    .await
            }
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        match self.raw_route(request) {
            Some((model, stops)) => Ok(frames_to_text_stream(
                self.raw(&model, stops, request).await?,
            )),
            None => {
                self.inner
                    .complete_stream(&self.with_slot_penalties(request))
                    .await
            }
        }
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        match self.raw_route(request) {
            Some((model, stops)) => {
                let frames = self.raw(&model, stops, request).await?;
                Ok((frames_to_text_stream(frames), model))
            }
            None => {
                self.inner
                    .complete_stream_with_id(&self.with_slot_penalties(request))
                    .await
            }
        }
    }

    async fn complete_stream_with_finish(&self, request: &CompletionRequest) -> Result<Frames> {
        match self.raw_route(request) {
            Some((model, stops)) => self.raw(&model, stops, request).await,
            None => {
                self.inner
                    .complete_stream_with_finish(&self.with_slot_penalties(request))
                    .await
            }
        }
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Frames, String)> {
        match self.raw_route(request) {
            Some((model, stops)) => Ok((self.raw(&model, stops, request).await?, model)),
            None => {
                self.inner
                    .complete_stream_with_id_and_finish(&self.with_slot_penalties(request))
                    .await
            }
        }
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_batch(texts).await
    }

    /// A batch holding a Raw request runs one by one, each by its own route.
    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        if !requests.iter().any(|r| self.raw_route(r).is_some()) {
            let penalised: Vec<CompletionRequest> = requests
                .iter()
                .map(|r| self.with_slot_penalties(r).into_owned())
                .collect();
            return self.inner.complete_batch(&penalised).await;
        }
        let mut out = Vec::with_capacity(requests.len());
        for request in requests {
            out.push(self.complete(request).await?);
        }
        Ok(out)
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.inner.embed_query(query).await
    }

    /// The preset's rerank section, named to the route (A4): the router's
    /// `/v1/rerank` serves one model per preset entry, so a request must name
    /// the rerank section; an id outside the preset is a chat model on a
    /// route that does not know it. No rerank section means the route
    /// refuses and the caller falls back to un-reranked fusion, the trait's
    /// contract for an engine without a reranker.
    async fn rerank_batch(
        &self,
        model: Option<&str>,
        query: &str,
        docs: &[String],
    ) -> Result<Vec<f32>> {
        let Some(section) = self.model_for(self.rerank_role()) else {
            tracing::debug!(
                target: "llama_router",
                docs = docs.len(),
                "rerank: the preset has no rerank section; the route refuses and fusion carries the search"
            );
            return Err(sovereign_contracts::Error::NotImplemented(
                "this router's preset has no rerank section; add [models.kinds] rerank".to_string(),
            ));
        };
        let model = match model {
            Some(m) if m == section => m,
            Some(m) => {
                tracing::warn!(
                    target: "llama_router",
                    named = %m,
                    rerank = %section,
                    "rerank: a model id outside the rerank section was named; sending the section"
                );
                section
            }
            None => section,
        };
        tracing::debug!(
            target: "llama_router",
            model,
            docs = docs.len(),
            "rerank: on the preset's rerank section"
        );
        self.inner.rerank_batch(Some(model), query, docs).await
    }

    /// The rerank section's model id, which is the id its route answers to.
    fn rerank_model_id(&self) -> Option<String> {
        self.model_for(self.rerank_role()).map(str::to_string)
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.inner.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.inner.embed_model_id()
    }

    /// The remote half sees a loopback server and says it forwards on the
    /// box; that server is this process's own supervised child, so the
    /// weights are this node's (decided 2026-10-10, host.serving-locus).
    fn serving_locus(&self) -> ServingLocus {
        match self.inner.serving_locus() {
            ServingLocus::ForwardsOnBox => ServingLocus::OwnWeights,
            other => other,
        }
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.inner.effective_context_size()
    }

    /// The remote half is a `GrammarSupport::Llguidance` client, which checks
    /// a `required` reply for the call (`llama_router.rs` sets the grammar).
    fn conversation_keeps_call_and_budget(&self) -> bool {
        true
    }

    /// The primary GGUF's `<arch>.context_length`, read at build.
    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.n_ctx_train
    }

    /// An estimate: the remote half's chars/4. The exact count is
    /// [`Self::count_tokens_exact`], which asks the router.
    fn count_tokens(&self, text: &str) -> u32 {
        self.inner.count_tokens(text)
    }

    /// `POST /tokenize` on the router, with the model a fast turn goes to
    /// (the fast model, else the primary), as the embedded engine counts on
    /// its fast slot.
    async fn count_tokens_exact(&self, text: &str) -> Result<u32> {
        let Some(model) = self.model_for("fast").or_else(|| self.preset.primary()) else {
            return Err(sovereign_contracts::Error::NotImplemented(
                "an embed-only router has no chat model to tokenize on".to_string(),
            ));
        };
        self.router
            .token_count(model, text)
            .await
            .map_err(sovereign_contracts::Error::Inference)
    }

    /// The preset's `[models] code` entry, which the router serves by name.
    fn code_model_id(&self) -> Option<String> {
        self.model_for("code").map(str::to_string)
    }

    /// `[models.edit]`'s section on the router, probed at build.
    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.edit_slot.clone()
    }

    /// Load the primary through the router's `POST /models/load` and wait
    /// for `loaded`; a load that fails is an error, never Ok.
    async fn warmup_primary(&self) -> Result<()> {
        let Some(primary) = self.preset.primary() else {
            return Err(sovereign_contracts::Error::NotImplemented(
                "an embed-only router serves no primary chat model to warm".to_string(),
            ));
        };
        let result = self.router.load(primary).await;
        tracing::debug!(target: "llama_router", model = primary, ok = result.is_ok(), "router provider: warmup");
        result.map_err(sovereign_contracts::Error::Inference)
    }

    async fn probe_logprobs(
        &self,
        probe: &sovereign_contracts::engine_observe::ProbeRequest,
    ) -> Result<sovereign_contracts::engine_observe::ProbeAnswer> {
        self.inner.probe_logprobs(probe).await
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
        let _ = (path, context_size);
        Err(self.extras_refusal("load", &slot_name))
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        Err(self.extras_refusal("unload", slot_name))
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inner.extras_inventory()
    }

    /// Every role the preset fills, embed and rerank included, resident as
    /// the router last listed its model in `GET /models`: `loaded` is
    /// resident, `loading` is transitioning, anything else (or no listing
    /// yet) is not resident. Sizes are not reported by the router.
    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.preset
            .roles()
            .into_iter()
            .map(|(role, model)| {
                let status = self.router.model_status(model);
                ResidentSlot {
                    role: role.to_string(),
                    model_id: model.to_string(),
                    resident: status.as_deref() == Some("loaded"),
                    size_bytes: None,
                    transitioning: status.as_deref() == Some("loading"),
                    placement: None,
                }
            })
            .collect()
    }

    fn decode_evidence(&self) -> Vec<oicp::SlotDecodeEvidence> {
        self.inner.decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.resident_slots()
            .into_iter()
            .find(|s| s.role == "primary")
    }

    /// The router and each of its models, after whatever the inner
    /// provider reports.
    fn compute_children(&self) -> Vec<oicp::ComputeChildStatus> {
        let mut out = self.inner.compute_children();
        out.extend(self.router.statuses());
        out
    }

    async fn peer_manifests(&self) -> Vec<(String, oicp::ProviderManifest)> {
        self.inner.peer_manifests().await
    }

    async fn lender_manifest(&self) -> Option<(String, Vec<String>)> {
        self.inner.lender_manifest().await
    }
}

#[cfg(test)]
mod tests {
    /// The host facts the router answers itself instead of the remote half
    /// (ENGINE_SWAP_PLAN A1, A4, A5), in trait order.
    ///
    /// `conversation_keeps_call_and_budget` is answered, not forwarded: the
    /// remote half is an HTTP client that cannot know, and the router does
    /// keep a forced call (its llguidance grammar checks the reply,
    /// `hermes_path_reason`). Forwarding it would ask a provider with no
    /// grammar support and get the trait default `false` — the embedded
    /// engine's answer, on the engine whose whole point is the opposite.
    const ANSWERED_HERE: [&str; 10] = [
        "rerank_model_id",
        "conversation_keeps_call_and_budget",
        "n_ctx_train_for_primary",
        "count_tokens_exact",
        "code_model_id",
        "edit_slot_info",
        "warmup_primary",
        "load_extra_slot",
        "unload_extra_slot",
        "primary_slot_status",
    ];

    #[test]
    fn the_router_provider_forwards_every_method_it_does_not_answer_itself() {
        let unforwarded = sovereign_contracts::reloadable_provider::unforwarded_methods(
            include_str!("provider.rs"),
            "impl InferenceProvider for RouterProvider",
        );
        assert_eq!(
            unforwarded, ANSWERED_HERE,
            "RouterProvider leaves a default in place, or forwards a fact it should answer"
        );
    }

    /// The daemon's own calls carry the slot's penalties; the OpenAI
    /// passthrough and a caller's own values do not change. Failing input:
    /// the citation call of 2026-10-25, which went out with neither penalty
    /// and picked another quote than the embedded engine on the same prompt.
    ///
    /// C1's verdict is what this pins: the defaults did not hold, so these
    /// two parameters are sent (and, by `quirks_repeat_penalties` being the
    /// only reader of the quirks here, nothing else in the tuple is). The
    /// values are asserted against the literal pair the measurement used,
    /// 1.15 and 0.1, not only against the family's own fields — a change to
    /// `ModelFamily::Unknown`'s defaults must break this test and reopen
    /// the question the check answered, not pass silently.
    #[test]
    fn a_daemon_call_carries_its_slot_penalties_and_a_conversation_does_not() {
        use super::slot_penalties;
        use sovereign_contracts::types::{CompletionRequest, PromptShape, SamplingMode};
        let chat = sovereign_inference::engine_factory::CHAT_SLOT_FAMILY.default_quirks();
        let code = sovereign_inference::engine_factory::CODE_SLOT_FAMILY.default_quirks();

        let call = CompletionRequest {
            temperature: Some(0.0),
            ..Default::default()
        };
        let out = slot_penalties(&call, Some("coder"));
        assert_eq!(out.repeat_penalty, Some(chat.default_repetition_penalty));
        assert_eq!(out.frequency_penalty, Some(chat.default_frequency_penalty));
        assert_eq!(
            (out.repeat_penalty, out.frequency_penalty),
            (Some(1.15), Some(0.1)),
            "the embedded primary's measured pair, which C1 found the defaults do not hold without"
        );
        assert_eq!(
            sovereign_inference::embedded::quirks_repeat_penalties(&chat),
            (1.15, 0.1),
            "the router and the embedded sampler must read one decider"
        );
        assert_eq!(
            out.temperature,
            Some(0.0),
            "no sampling field but the two is added"
        );
        assert_eq!(out.top_k, None);
        assert_eq!(out.top_p, None);

        let to_code = CompletionRequest {
            model_id: Some("coder".into()),
            ..Default::default()
        };
        let out = slot_penalties(&to_code, Some("coder"));
        assert_eq!(out.repeat_penalty, Some(code.default_repetition_penalty));

        let own = CompletionRequest {
            repeat_penalty: Some(1.0),
            ..Default::default()
        };
        assert_eq!(slot_penalties(&own, None).repeat_penalty, Some(1.0));

        let conversation = CompletionRequest {
            prompt_shape: Some(PromptShape::Conversation { messages: vec![] }),
            ..Default::default()
        };
        let out = slot_penalties(&conversation, None);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out.repeat_penalty, None);

        let moded = CompletionRequest {
            sampling_mode: Some(SamplingMode::Instruct),
            ..conversation.clone()
        };
        assert_eq!(
            slot_penalties(&moded, None).repeat_penalty,
            Some(chat.default_repetition_penalty),
            "a conversation with a sampling mode samples from the quirks on the embedded engine too"
        );
    }
}
