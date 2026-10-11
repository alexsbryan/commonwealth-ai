// SPDX-License-Identifier: AGPL-3.0-or-later
// Contract crate: the public surface IS the product — every pub item needs
// docs (count-ratcheted by lint-gate, never a hard deny).
#![warn(missing_docs)]
//! `oicp-client` — a pure-HTTP OICP / OpenAI-compatible inference client.
//!
//! `RemoteApiProvider` speaks the OpenAI chat/embeddings wire (plus the OICP
//! request envelope and `/oicp/v1/capabilities` manifest fetch);
//! `SplitInferenceProvider` fans chat and embed to two model ids over one
//! endpoint. Both implement `sovereign_contracts::traits::InferenceProvider`,
//! so a package can drive a Sovereign daemon (or any OICP-conforming host)
//! without linking the local llama.cpp engine. Moved wholesale from
//! `sovereign-inference/src/remote.rs`; the daemon crate re-exports it at the
//! historical `sovereign_inference::remote::*` path.

use std::pin::Pin;
use std::time::Instant;

pub mod daemon_inference;
pub mod daemon_models;
mod ner;
pub use ner::RemoteNer;
pub mod openai_passthrough;
mod pinned_provider;
pub use pinned_provider::provider_for_model;
mod rerank;
mod serve_loopback;
mod turn_admission;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::{OicpResponseMeta, ProviderManifest};
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot};
use sovereign_contracts::types::*;

/// A bounded, char-safe excerpt of a remote error body, for the one job
/// an error message has: saying what the other end actually said.
///
/// Both streaming surfaces used to drop the body entirely and report a
/// bare `returned 503 Service Unavailable`. That is an `Err` collapsed
/// into something less informative than it arrived as (§18.3), and it
/// cost a measurement: the mesh-serve-50 fleet-scaling run
/// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §9.5) watched a peer refuse
/// 421 selected dispatches and could not say why from this node, because
/// the peer's own reason — which it sent, in the body — was discarded
/// here. A refusal a peer explained is not a refusal you get to report
/// as unexplained.
///
/// `floor_char_boundary` rather than a byte slice: the non-streaming
/// path had `&body[..body.len().min(500)]`, which panics on a body whose
/// 500th byte lands mid-UTF-8 — a remote error message with an em dash
/// in the wrong place would have turned a peer's 503 into a local panic.
/// One implementation, three call sites (§10.6).
/// Attempts for a QUEUE SHED specifically — the initial call plus two.
///
/// Not a general retry, and the distinction is the whole point: a shed is
/// BACKPRESSURE with a stated delay, and the only honest response to
/// "busy, come back in 32s" is to come back. A 500, a 404, a malformed body
/// are FAILURES, and retrying those masks them.
pub const SHED_MAX_ATTEMPTS: u32 = 3;

/// Total time this client will spend WAITING on sheds for one logical call.
///
/// A cap rather than an unbounded honour of the hint: a host predicting a
/// two-minute wait should hand control back to the caller, which can decide
/// to route elsewhere, rather than have its client block silently.
pub const SHED_TOTAL_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(90);

/// The delay a 503 ASKED FOR, when the 503 is a shed.
///
/// `None` for every other refusal. The discriminator is the presence of
/// `retry_after_secs` in the body, NOT the 503 status: the admission layer is
/// the only thing that puts that field on the wire
/// (`commonwealth-api::admission::AdmissionRejection`), and a genuine
/// `backend_error` carries no such field. Keying on the status alone would
/// retry real failures into silence.
///
/// Minted 2026-08-26. The daemon computed this hint, set the `Retry-After`
/// header, and structured the body — and no client in the workspace had a
/// retry loop at all, so every caller threw it away and reported backpressure
/// as a hard error. Measured: three sub-requests of ONE turn refused inside
/// 17 ms against a hint that said 32 seconds (note `bf432b4d`).
/// PUBLIC because it is the ONE decider for "is this 503 a shed, and how
/// long did the host ask for" (§10.6). Three other readings of the same field
/// have existed in this workspace; a caller outside this crate calls THIS
/// rather than parsing `retry_after_secs` again.
///
/// Known gap, one place instead of four: the daemon also sets a JITTERED
/// `Retry-After` header (`commonwealth-api/src/admission.rs`), and this reads
/// only the body, so callers lose the anti-thundering-herd spread.
pub fn shed_retry_after(status: reqwest::StatusCode, body: &str) -> Option<std::time::Duration> {
    if status != reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let secs = parsed.get("retry_after_secs")?.as_u64()?;
    // Clamp: a hint of 0 would spin, and one of an hour would hang the caller
    // that [`SHED_TOTAL_WAIT_CAP`] exists to protect. The ceiling is DERIVED
    // from that cap rather than written twice — a per-hint ceiling above the
    // total budget is dead range, since such a hint could never be honoured
    // (ARCH §10.6, and this exact drift was caught by
    // `the_retry_hint_is_clamped_at_both_ends`).
    Some(std::time::Duration::from_secs(
        secs.clamp(1, SHED_TOTAL_WAIT_CAP.as_secs()),
    ))
}

fn error_excerpt(body: &str) -> &str {
    const MAX: usize = 500;
    if body.len() <= MAX {
        return body;
    }
    let mut end = MAX;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

/// OpenAI-compatible API client.
///
/// Where a forwarding provider's base URL comes from, when the operator did
/// not name one.
///
/// The binding a `terminal` node writes is its entry node's mesh IDENTITY, not
/// an address (ARCH §7.5 — "the address is a mutable attribute of the thing,
/// never its name"). Resolving that identity is the mesh's job and the mesh is
/// far above this crate, so the capability arrives as a trait: `oicp-client`
/// states what it needs, `sovereign-mesh` supplies it, and the layer map
/// (`quality/ARCH_LAYERS.toml`) keeps its one sovereign edge unchanged.
///
/// Resolution is per-call rather than per-boot on purpose. That is what makes
/// a moved DHCP lease, a peer that reconnects on a different interface, and a
/// mesh whose plaintext ingress is closed all reach the same place: whatever
/// the transport says the peer is reachable at *now*.
#[async_trait]
pub trait EndpointResolver: Send + Sync + std::fmt::Debug {
    /// The target's current OpenAI-shaped base (`…/v1`), or `None` when it
    /// cannot be located right now.
    ///
    /// `None` is a real, reportable state — a bound peer that is offline — and
    /// callers must surface it. It is never a cue to fall back to a remembered
    /// address: that is how a terminal ends up talking confidently to whichever
    /// machine inherited the lease (§18.3).
    async fn base_url(&self) -> Option<String>;

    /// Stable name of the binding for logs and errors. The IDENTITY, never an
    /// address — an error that names an address teaches the reader to trust it.
    fn describe(&self) -> String;
}

/// A provider's endpoint: fixed, or resolved on every call.
///
/// Two variants rather than an `Option<Arc<dyn EndpointResolver>>` beside a
/// `String`, because those two fields can disagree and this cannot (§2.1). A
/// provider has exactly one answer to "where do I send this".
#[derive(Clone, Debug)]
pub enum EndpointRef {
    /// A base URL the caller named. Every provider built before terminals
    /// existed, and every one pointed at a fixed host.
    Static(String),
    /// A binding resolved through the mesh on each call.
    Dynamic(std::sync::Arc<dyn EndpointResolver>),
}

impl EndpointRef {
    /// The base URL to use for THIS call.
    ///
    /// The `Dynamic` miss is an `Err`, not a silent skip: a turn that cannot
    /// locate its entry node has not been served, and the caller must hear
    /// that rather than receive a success-shaped nothing (§18.3).
    async fn resolve(&self) -> Result<String> {
        match self {
            Self::Static(url) => Ok(url.clone()),
            Self::Dynamic(resolver) => resolver.base_url().await.ok_or_else(|| {
                Error::Inference(format!(
                    "{} is not reachable right now — this node holds no weights, so \
                     there is nowhere else to run this turn",
                    resolver.describe()
                ))
            }),
        }
    }

    /// What this endpoint is bound to, for logs and errors.
    pub fn describe(&self) -> String {
        match self {
            Self::Static(url) => url.clone(),
            Self::Dynamic(resolver) => resolver.describe(),
        }
    }
}

/// Works with any endpoint implementing the OpenAI chat/completions API:
/// vLLM, Ollama, llama.cpp server, text-generation-inference, etc.
pub struct RemoteApiProvider {
    /// May this provider WAIT OUT a shed, or must it report it and let the
    /// caller route elsewhere?
    ///
    /// **Off by default, and that default is the invariant.** Waiting is only
    /// correct where there is no alternative holder. A PEER that sheds is
    /// giving a ROUTING signal — try local, try another peer — and re-dialling
    /// it inside its own retry window is the failed-hop tax
    /// `MESH_SCALE…§9.1.1` measures; `chat_completion_e2e`'s
    /// `a_yielding_peer_is_asked_once_not_once_per_turn` and
    /// `repeated_sheds_never_quarantine_a_healthy_peer` both pin it, and both
    /// caught this being on by default on 2026-08-26.
    ///
    /// Turn it on with [`Self::waiting_out_sheds`] only where this endpoint is
    /// the LAST RESORT — the local slot after peer selection has already been
    /// exhausted. There, "busy, come back in 32s" is the whole answer, and
    /// dropping it is what made three sub-requests of one turn fail inside
    /// 17ms against a 32-second hint (note `bf432b4d`).
    wait_out_sheds: bool,
    /// The HTTP half: client, bearer, node stamp and far end. Every request
    /// goes out through [`Self::outbound`].
    outbound: outbound::Outbound,
    endpoint: EndpointRef,
    model_id: String,
    /// When true, `model_id` is a routing/attribution LABEL rather than
    /// a name the remote endpoint can resolve, and must never be put on
    /// the wire as the `model` field.
    ///
    /// The mesh scheduler builds one provider per peer with the literal
    /// id `"mesh-peer"` (`peer_inference.rs::provider_for_peer`), which
    /// exists so logs and `CompletionResponse::model_id` can say where a
    /// turn went. It names no model anywhere in the fleet. Sending it as
    /// `model` puts the receiving node on its explicit-name path, where
    /// it resolves to nobody and returns `ModelNotLoaded` — the origin
    /// then books a peer failure and falls back to local, quarantining a
    /// healthy peer after three strikes. See the regression test
    /// `an_unnamed_ranked_dispatch_sends_a_model_the_peer_can_resolve`.
    model_id_is_placeholder: bool,
    /// Writes the turn's admission id onto the chat wire; see `turn_admission.rs`.
    carries_turn_admission: bool,
    context_size: u32,
    /// Query-side instruction prefix for this model, resolved once at
    /// construction from the bundled manifest (empty for chat / non-embedding
    /// models). The embedded engine applies this in `embed_query_sync`; the
    /// remote `/embeddings` API has no query/document distinction, so the
    /// client prepends it before sending — making the remote query-embedding
    /// path bit-identical to the embedded one. See
    /// `ModelsManifest::embed_query_instruction`.
    query_instruction: String,
    /// The embed family's whole input preparation (instruction AND EOS), for
    /// a host that does none of its own: llama-server, vLLM. `None` sends
    /// text as given, which is right for a Sovereign daemon, whose embed slot
    /// prepares inputs itself. Set by `[engine] embed_inputs = "client"`.
    input_prep: Option<sovereign_contracts::embed_quirks::EmbedQuirks>,
    /// How this provider's host is asked for schema-shaped output.
    structured_output_mode: StructuredOutputMode,
    /// How this provider's host takes a decode constraint
    /// (`[engine] grammar`).
    grammar: sovereign_contracts::engine_config::GrammarSupport,
    /// Operator-set vendor fields merged into every body last (OpenRouter's
    /// `provider` routing, OpenAI's `seed`).
    extra_params: Option<serde_json::Value>,
    /// Set once this host refused `json_schema` and answered a forced function
    /// call instead (`chat_wire::send_chat`); schemas go that way from then on.
    json_schema_refused: std::sync::atomic::AtomicBool,
    /// Set once this host answered a forced-choice call only through its
    /// logprobs (`forced_choice`); such calls go that way from then on.
    forced_choice_by_logprobs: std::sync::atomic::AtomicBool,
}

/// Default request timeout for `RemoteApiProvider`. Matches the
/// local-inference path's `CHAT_TIMEOUT` (1800s / 30 min) so a remote
/// peer isn't artificially capped tighter than the same call would be
/// locally — a Phase 1 enrichment call that takes 3 minutes on a slow
/// CPU-bound peer (grammar masking is single-threaded) would time out
/// at the previous 120s default before the peer could return, even
/// though the peer was healthy and producing tokens.
///
/// Embed callers reuse this provider; their response is <1s in
/// practice so the long timeout never fires for them in the happy
/// path. Tests/customization can adjust via `with_timeout`.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

impl RemoteApiProvider {
    /// Texts per `/embeddings` request in [`InferenceProvider::embed_batch`].
    ///
    /// Sized to keep one request's payload modest while giving the
    /// daemon's embed slot several full multi-sequence decodes to chew
    /// on (it packs 16 sequences per decode). Larger batches stop paying
    /// — the slot's packing, not the request count, is the throughput
    /// bound past this point.
    pub const EMBED_BATCH_INPUTS: usize = 64;

    pub fn new(endpoint: &str, api_key: Option<String>, model_id: &str, context_size: u32) -> Self {
        Self::with_timeout(endpoint, api_key, model_id, context_size, DEFAULT_TIMEOUT)
    }

    /// Declare that `model_id` is a routing/attribution label, not a
    /// name the remote can serve. See the field docs on
    /// `model_id_is_placeholder`.
    ///
    /// Opt-in rather than inferred: a provider pointed at a real model
    /// must keep pinning its name on the wire, which is what the
    /// 2026-07-23 fast-slot fix (c8b0519b) exists to guarantee. Only the
    /// caller knows whether the id it supplied names anything.
    pub fn with_placeholder_model_id(mut self) -> Self {
        self.model_id_is_placeholder = true;
        self
    }

    /// One `/embeddings` call carrying every text as an array `input`.
    ///
    /// Rows come back with an `index` field; we sort by it rather than
    /// trusting arrival order, and verify the count matches so a partial
    /// response can never silently misalign embeddings with chunks —
    /// that would corrupt retrieval in a way no test downstream would
    /// catch.
    async fn embed_many_one_request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let admitted = self.outbound(Payload::Texts)?;
        let url = format!("{}/embeddings", self.endpoint.resolve().await?);
        let inputs: Vec<std::borrow::Cow<'_, str>> =
            texts.iter().map(|t| self.document_input(t)).collect();
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": inputs,
        });

        // Deliberately NOT routed through `send_honouring_shed`: this site's
        // refusal is a CAPABILITY verdict, not backpressure, and it returns
        // `NotImplemented` so a caller can fall back to per-item embedding.
        // Flattening that into `Inference` would be the same collapse this
        // client already refuses to make on a 503 body (§18.3).
        let req = admitted.post(&url).json(&body);

        let lap = sovereign_contracts::engine_state::Lap::start("client", "embed");
        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Batch embedding request failed: {e}")))?;
        lap.mark("headers");

        if !response.status().is_success() {
            return Err(Error::NotImplemented(format!(
                "Batch embedding not supported by this endpoint (status {})",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedData>,
        }
        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f32>,
            #[serde(default)]
            index: usize,
        }

        let parsed: EmbedResponse = response.json().await.map_err(|e| {
            Error::Inference(format!("Failed to parse batch embedding response: {e}"))
        })?;
        lap.mark("body parsed");

        if parsed.data.len() != texts.len() {
            return Err(Error::Inference(format!(
                "Batch embedding returned {} rows for {} inputs",
                parsed.data.len(),
                texts.len()
            )));
        }

        let mut rows = parsed.data;
        rows.sort_by_key(|d| d.index);
        Ok(rows.into_iter().map(|d| d.embedding).collect())
    }

    /// Construct with an explicit request timeout. Use for tests or
    /// for short-lived health probes where waiting 30 min on a
    /// hanging peer is wrong.
    pub fn with_timeout(
        endpoint: &str,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
        timeout: std::time::Duration,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self::with_client(endpoint, client, api_key, model_id, context_size)
    }

    /// Every constructor's one field list, so a new field has one default.
    fn assemble(
        client: reqwest::Client,
        endpoint: EndpointRef,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        Self {
            // Off: see the field docs — a peer shed is a routing signal, not a wait.
            wait_out_sheds: false,
            outbound: outbound::Outbound::new(client, api_key),
            endpoint,
            model_id: model_id.to_string(),
            model_id_is_placeholder: false,
            carries_turn_admission: false,
            context_size,
            // The embed query-instruction prefix is model-family knowledge
            // that this pure HTTP client no longer computes. Callers that
            // need it (the embed slot of `SplitInferenceProvider`) set it via
            // `with_query_instruction`; chat providers and document-embed
            // (`embed`, which ignores the prefix) leave it empty.
            query_instruction: String::new(),
            input_prep: None,
            structured_output_mode: StructuredOutputMode::default(),
            grammar: Default::default(),
            extra_params: None,
            json_schema_refused: std::sync::atomic::AtomicBool::new(false),
            forced_choice_by_logprobs: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Construct against a binding resolved on every call rather than a fixed
    /// address — a `terminal` node and its entry node.
    ///
    /// Everything else is `new`'s behaviour: only the endpoint is late, so a
    /// caller cannot acquire different request-building, streaming or envelope
    /// handling by choosing this door.
    pub fn dynamic(
        resolver: std::sync::Arc<dyn EndpointResolver>,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .unwrap_or_default();
        let endpoint = EndpointRef::Dynamic(resolver);
        Self::assemble(client, endpoint, api_key, model_id, context_size)
    }

    /// What this provider is bound to — an address, or the identity behind one.
    pub fn endpoint_ref(&self) -> &EndpointRef {
        &self.endpoint
    }

    /// Construct with a pre-built `reqwest::Client` and an explicit
    /// bearer token. Used by the mesh scheduler when routing to a
    /// pinned worker pod: the client carries a TLS pin to the pod's
    /// seed-derived cert and the bearer is the owner-signed
    /// `WorkerToken` the worker daemon's auth middleware validates.
    ///
    /// Equivalent to `new` in every other respect — request build,
    /// streaming, and OICP envelope handling are unchanged because
    /// they only consume `self.client` and `self.api_key`.
    pub fn with_client_and_bearer(
        endpoint: &str,
        client: reqwest::Client,
        bearer: String,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        Self::with_client(endpoint, client, Some(bearer), model_id, context_size)
    }

    /// Construct with a pre-built `reqwest::Client` (an egress-built one, say)
    /// and the bearer when the host wants one.
    pub fn with_client(
        endpoint: &str,
        client: reqwest::Client,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        let endpoint = EndpointRef::Static(endpoint.trim_end_matches('/').to_string());
        Self::assemble(client, endpoint, api_key, model_id, context_size)
    }

    /// Spell `structured_output` the way this provider's host accepts.
    pub fn with_structured_output_mode(mut self, mode: StructuredOutputMode) -> Self {
        self.structured_output_mode = mode;
        self
    }

    /// Merge these vendor fields into every request body, last.
    /// How this host takes a decode constraint: a daemon's extension
    /// fields (the default) or one llguidance `grammar`.
    pub fn with_grammar(
        mut self,
        grammar: sovereign_contracts::engine_config::GrammarSupport,
    ) -> Self {
        self.grammar = grammar;
        self
    }

    pub fn with_extra_params(mut self, extra: Option<serde_json::Value>) -> Self {
        self.extra_params = extra;
        self
    }

    /// Declare this endpoint the LAST RESORT, so a shed is waited out rather
    /// than reported. See [`Self::wait_out_sheds`] — do not set this on a peer.
    pub fn waiting_out_sheds(mut self) -> Self {
        self.wait_out_sheds = true;
        self
    }

    /// Set the query-side embedding instruction prefix (empty by default).
    /// Applied by `embed_query` so a remote query embedding is bit-identical
    /// to the embedded engine's. The prefix is model-family knowledge the
    /// caller resolves (from the OICP manifest's `EmbedModelInfo`, or —
    /// pre-v0.4 — from `ModelsManifest::embed_query_instruction`).
    pub fn with_query_instruction(mut self, query_instruction: String) -> Self {
        self.query_instruction = query_instruction;
        self
    }

    /// Prepare every embed input here, as the embed slot does in process:
    /// `prepare_document` for `embed`/`embed_batch`, `prepare_query` for
    /// `embed_query`. For a host that sends the text to the model as given.
    pub fn with_input_prep(
        mut self,
        quirks: sovereign_contracts::embed_quirks::EmbedQuirks,
    ) -> Self {
        self.input_prep = Some(quirks);
        self
    }

    /// One `/embeddings` call on `text` exactly as given: no preparation.
    async fn embed_as_given(&self, text: &str) -> Result<Vec<f32>> {
        let admitted = self.outbound(Payload::Texts)?;
        let url = format!("{}/embeddings", self.endpoint.resolve().await?);
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": text,
        });

        let response = self
            .send_honouring_shed(|| admitted.post(&url).json(&body), "Embedding request")
            .await?;

        #[derive(Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedData>,
        }
        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f32>,
        }

        let embed_response: EmbedResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse embedding response: {e}")))?;

        embed_response
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or(Error::Inference(
                "No embedding data in response".to_string(),
            ))
    }

    /// A document-side input as this host must receive it.
    fn document_input<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        match &self.input_prep {
            Some(q) => std::borrow::Cow::Owned(q.prepare_document(text)),
            None => std::borrow::Cow::Borrowed(text),
        }
    }

    fn build_request(&self, request: &CompletionRequest) -> Result<serde_json::Value> {
        self.build_request_in(request, self.effective_structured_output_mode())
    }

    fn build_request_in(
        &self,
        request: &CompletionRequest,
        mode: StructuredOutputMode,
    ) -> Result<serde_json::Value> {
        let messages = chat_wire::request_messages(request);

        // Pin the OpenAI `model` field when the caller asked for a
        // specific model — and, since 2026-07-23, ALSO for slot-routed
        // Medium/Slow requests, pinned to this provider's resolved
        // chat model. The previous behaviour (empty model + auto
        // latency envelope, "the daemon's local pick maps Normal and
        // Extended to the same primary slot") was wrong in practice:
        // the daemon's Priority-1 OICP routing claim-scores EVERY
        // loaded model against the envelope, its scheduler-side claims
        // are synthesized with a hardcoded 32k context (feasibility
        // gates never bind), and capability-profile affinities tie
        // across model sizes — so Normal-class traffic was routed by
        // plan-iteration order and consistently served by the FAST 4B
        // slot while callers believed they were on the primary
        // (observed 2026-07-23: all 496 enrichment calls of a
        // book-report run attributed to Qwen3.5-4B).
        //
        // Fast requests keep the empty-model + envelope form: a
        // fast-class pick is the desired outcome there, and the
        // FastShort overflow lane still engages daemon-side.
        // A placeholder id names nothing the remote can resolve, so the
        // Medium/Slow pin below must not fire for it — an unnamed mesh
        // dispatch stays unnamed on the wire and routes on the envelope
        // instead. Every provider pointed at a real model is unaffected
        // and still pins, which is the fast-slot guarantee.
        let pinnable = (!self.model_id_is_placeholder).then_some(self.model_id.as_str());
        let model_field = match request.model_id.as_deref() {
            Some(mid) => mid,
            None => match request.preferred_speed {
                Speed::Fast => "",
                Speed::Medium | Speed::Slow => pinnable.unwrap_or(""),
            },
        };
        let mut body = serde_json::json!({
            "model": model_field,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        // OICP envelope. The runtime's local `Speed` enum is mapped
        // to `latency_class` (v0.3 §2.2) — internal types stay off
        // the wire while the daemon's slot picker routes by the
        // protocol's standard signal. If the caller attached an
        // explicit `oicp`, we honor it as-is.
        //
        // Privacy: deliberately left at the protocol default
        // (`LocalOnly` per §3.1). We do NOT silently downgrade the
        // privacy contract here — that would violate ARCH_PRINCIPLES
        // §7 (privacy invariants must be structural). Privacy-aware
        // callers attach their own oicp envelope above. The daemon's
        // privacy gate is responsible for serving LocalOnly via
        // local_inference rather than rejecting it.
        // THE FORWARD. Everything reached through this client is a request
        // leaving this node for a peer, so this is where a hop is spent.
        // `decremented_for_forward` is the only place that spends one; do not
        // decrement by hand elsewhere (oicp-types::requirements).
        //
        // Without this the envelope crosses verbatim, the receiver re-runs its
        // own scheduler over an already-forwarded request, and A→B→C is
        // unbounded. The desktop avoids that structurally by handing peers its
        // raw provider (sovereign-desktop state.rs); the CLI daemon installs
        // the mesh-routing provider and had no equivalent until this.
        let oicp_val = if self.far_end() == FarEnd::ThirdParty {
            // A vendor does not speak OICP. The envelope is ours: admission
            // has already read the one field that mattered to it.
            None
        } else if self.far_end() == FarEnd::Origin {
            // Not a forward (`originating`): the caller's envelope or none.
            request
                .oicp
                .as_ref()
                .and_then(|o| serde_json::to_value(o).ok())
        } else if let Some(ref oicp) = request.oicp {
            serde_json::to_value(oicp.decremented_for_forward()).ok()
        } else if model_field.is_empty() {
            // Canonical Speed→LatencyClass map (SLOT_POLICY §8). Slow
            // derives Normal, not Extended (rule 4.4). Attached ONLY
            // when no model is pinned above: the daemon's Priority-1
            // OICP routing treats any envelope as an explicit routing
            // opinion and would override the pinned model name — the
            // 2026-07-23 fast-slot hijack described at `model_field`.
            let class = sovereign_contracts::slot_policy::speed_to_latency(request.preferred_speed);
            let mut req =
                sovereign_contracts::oicp::InferenceRequirements::new().with_latency_class(class);
            if let Some(n) = request.max_tokens {
                req = req.with_max_output_tokens(n as u32);
            }
            // Synthesized here, but still a forward: this request is on its
            // way to a peer exactly like the branch above, so it spends a hop
            // from the default budget too. Serializing `req` un-decremented
            // would leave this one path able to start an unbounded chain.
            serde_json::to_value(req.decremented_for_forward()).ok()
        } else {
            // A NAMED request with no envelope of its own — the thin-client
            // shape: an IDE or any OpenAI client that pins `model` and knows
            // nothing about OICP. It still crosses a hop, so it still spends
            // one, or the named path (`peer_inference::locate_named_model`)
            // has no hop count and two nodes with stale manifests can bounce
            // it between them forever.
            //
            // The envelope attached here carries ONLY the budget. Every
            // routing field stays absent on purpose: both `has_routing_signal`
            // (peer_inference.rs) and the daemon's Priority-1 gate
            // (routes_inference.rs:276-279) key on capability_hint /
            // latency_class / context_tokens / max_output_tokens, so a
            // budget-only envelope is invisible to both and cannot override
            // the pinned model name this branch exists to preserve — the
            // 2026-07-23 fast-slot hijack described at `model_field`.
            let budget = request
                .oicp
                .clone()
                .unwrap_or_default()
                .decremented_for_forward();
            debug_assert!(
                budget.capability_hint.is_none()
                    && budget.latency_class.is_none()
                    && budget.context_tokens.is_none()
                    && budget.max_output_tokens.is_none(),
                "a budget-only envelope must carry no routing signal"
            );
            serde_json::to_value(budget).ok()
        };
        if let Some(v) = oicp_val {
            body["oicp"] = v;
        }

        // `enable_thinking` → `chat_template_kwargs` (vLLM, llama-server;
        // daemon-side `inference_adapter::extract_enable_thinking`), and
        // `think_budget` in the daemon's and DeepSeek's spellings. Without the
        // budget the runtime's `think_budget: Some(0)` (FastFocused synthesis,
        // gap check, router) died at this boundary and every fast-slot answer
        // was truncated raw deliberation (2026-06-10 fabrication burn-down).
        chat_wire::write_thinking(&mut body, request.think_budget, request.enable_thinking);

        // Commonwealth extension: forward the caller-directed
        // stable-prefix declaration (bytes of the user prompt shared
        // byte-identically across sibling requests). The daemon's
        // inference_adapter carries it onto
        // `CompletionRequest.stable_prefix_len`; the engine uses it to
        // checkpoint/restore decode state at that boundary
        // (prefix_state.rs). Advisory — dropping it costs prefill
        // time, never correctness — but without this forward the
        // per-claim grounding gate loses its evidence-prefix reuse on
        // every daemon-routed (CLI `chat ask`) path.
        if let Some(n) = request.stable_prefix_len {
            body["stable_prefix_len"] = serde_json::json!(n);
        }

        // Forward the tool catalog + `tool_choice` so the daemon presents the
        // tools to the model in its chat template. Without this, a daemon-routed
        // agent/authoring loop sends `lark_grammar` (the call SHAPE) but the model
        // never sees the tools it's meant to call — so it emits a prose/markdown
        // description instead, the grammar's permitted plain-text branch lets it,
        // and the loop captures zero tool calls. (Proven 2026-06-24: workflow
        // authoring worked embedded but not attach-routed for exactly this reason.)
        // The embedded path receives `tools`/`tool_choice` directly; mirror that on
        // the wire. The daemon's inference_adapter rebuilds the tool-envelope grammar
        // from these, equivalent to the forwarded `lark_grammar`.
        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(
                    tools
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": t.name,
                                    "description": t.description,
                                    "parameters": t.parameters,
                                }
                            })
                        })
                        .collect(),
                );
            }
        }
        if let Some(tc) = &request.tool_choice {
            body["tool_choice"] = tc.clone();
        }

        // `structured_output` in this host's spelling (`chat_wire`). Without
        // it the schema was dropped at the HTTP boundary and the daemon's
        // grammar layer never saw it; the daemon unwraps `response_format` via
        // `inference_adapter::extract_response_format_schema`. After the tool
        // catalog, so a tool-mode host gets the schema beside the caller's tools.
        if let Some(schema) = &request.structured_output {
            chat_wire::write_structured_output(&mut body, schema, mode);
        }

        // The sampler and constraint fields the chat wire already carries
        // and serve's `build_completion_request` already reads. Unwritten,
        // each was dropped on every turn that crossed this client
        // (sovereign-serve tests/chat_round_trip.rs).
        if let Some(p) = request.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        if let Some(k) = request.top_k {
            body["top_k"] = serde_json::json!(k);
        }
        self.write_turn_admission(&mut body, request);
        if let Some(mode) = request.sampling_mode {
            body["sampling_mode"] = serde_json::json!(mode);
        }
        if let Some(prefix) = &request.assistant_prefix {
            body["assistant_prefix"] = serde_json::json!(prefix);
        }
        if let Some(prefix) = &request.cmd_prefix {
            body["cmd_prefix"] = serde_json::json!(prefix);
        }
        // The Lark grammar and the URL and evidence-id allow-lists, in this
        // host's spelling: a daemon's extension fields, or one llguidance
        // `grammar` (`[engine] grammar`). After the tools and the schema,
        // which decide what can ride beside them.
        chat_wire::write_constraints(&mut body, request, self.grammar)?;
        // Operator-set vendor fields, last, so they can override.
        if let (Some(extra), Some(obj)) = (
            self.extra_params.as_ref().and_then(|e| e.as_object()),
            body.as_object_mut(),
        ) {
            obj.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        Ok(body)
    }

    /// The daemon root: this provider's endpoint with any `/v1` suffix
    /// stripped. Routes mounted at the daemon root (not under `/v1`) — warmup,
    /// `/status`, and the OICP capabilities manifest — resolve from here. Two
    /// endpoint shapes appear in the codebase: callers like
    /// `chat_cmd/bootstrap` pass `http://host:9741/v1`; peer-inference callers
    /// pass `http://peer:9741`. Both resolve to the same root here.
    ///
    /// Async because the endpoint may be a binding rather than an address, and
    /// the ONE derivation of "root" for every caller — `SplitInferenceProvider`
    /// used to keep a second copy in a `status_url` field, which is two answers
    /// to one question (§10.6).
    pub(crate) async fn daemon_root(&self) -> Result<String> {
        let endpoint = self.endpoint.resolve().await?;
        Ok(endpoint
            .strip_suffix("/v1")
            .unwrap_or(&endpoint)
            .to_string())
    }

    async fn warmup_url(&self) -> Result<String> {
        Ok(format!(
            "{}/internal/inference/warmup",
            self.daemon_root().await?
        ))
    }

    /// Fetch the OICP capabilities manifest from a provider.
    /// Returns None if the provider doesn't support OICP (404 or parse failure).
    pub async fn fetch_oicp_manifest(&self) -> Option<ProviderManifest> {
        // `/oicp/v1/capabilities` is mounted at the daemon root, NOT under
        // `/v1` (same as warmup) — strip a `/v1` endpoint suffix so a caller
        // holding the OpenAI `/v1` URL still reaches it. Without this, a
        // `/v1`-shaped endpoint hit `…/v1/oicp/v1/capabilities` → 404 → None.
        let url = format!("{}/oicp/v1/capabilities", self.daemon_root().await.ok()?);

        let req = self.outbound(Payload::Probe).ok()?.get(&url);

        let response = req.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }

        response.json::<ProviderManifest>().await.ok()
    }
}

/// Fetch the OICP capabilities manifest from a daemon at `endpoint`, which may
/// be either the daemon root (`http://host:9741`) or the OpenAI `/v1` shape
/// (`http://host:9741/v1`) — both resolve, since the manifest lives at the
/// daemon root (see [`RemoteApiProvider::daemon_root`]). `bearer` is the
/// optional auth token for a non-loopback host.
///
/// Returns `None` on a v0.3 host (no `/oicp/v1/capabilities`) or any transport
/// or parse failure. Callers treat `None` as "degrade to v0.3 client defaults"
/// — never a hard error. This is the ergonomic entry a package uses to source
/// context length + the embed query-instruction prefix from the host's own
/// manifest (v0.4 §7 context discoverability, §4 embed completeness) rather
/// than compiling those values in.
pub async fn fetch_manifest(endpoint: &str, bearer: Option<String>) -> Option<ProviderManifest> {
    // A throwaway provider carries no model/context — `fetch_oicp_manifest`
    // only reads the endpoint, HTTP client, and auth header.
    RemoteApiProvider::new(endpoint, bearer, "", 0)
        .fetch_oicp_manifest()
        .await
}

// ─── OpenAI Response Types ───────────────────────────────────

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    oicp: Option<OicpResponseMeta>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    /// OpenAI-compatible finish_reason carried on the terminal
    /// choice — `"stop"` / `"length"` / `"content_filter"` /
    /// `"tool_calls"`. Parsed into [`FinishReason`] at the consume
    /// site so the desktop cutoff chip + non-streaming surfacing
    /// behave identically across local and remote providers.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    /// The server's NATIVE function-call array, when it chose that shape.
    ///
    /// A daemon handed a `tools` catalog rebuilds the tool-envelope grammar
    /// from it and may answer with `content: ""` and the call here instead of
    /// as `<tool_call>` text. This field was absent until 2026-09-08, so those
    /// responses reached every caller as an EMPTY completion and the loop that
    /// asked for the tool saw no call at all — silently, since the empty string
    /// is a valid answer. [`ChatMessage::as_text`] is where the two shapes
    /// become one.
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    function: WireToolFunction,
}

#[derive(Deserialize)]
struct WireToolFunction {
    name: String,
    /// OpenAI sends the arguments as a JSON-encoded STRING, not an object.
    #[serde(default)]
    arguments: Option<String>,
}

impl ChatMessage {
    /// The assistant turn as ONE text shape.
    ///
    /// Native `tool_calls` are re-emitted as the
    /// `<tool_call>{"name":..,"arguments":{..}}</tool_call>` envelope every
    /// tool loop in this workspace parses (`sovereign_core::tool_loop`), so a
    /// caller never has to ask which shape the server picked. Normalising HERE
    /// — at the wire boundary, in the one place the wire is read — is what
    /// keeps that a single protocol rather than a second one that happens to
    /// arrive over HTTP.
    ///
    /// Arguments arrive JSON-encoded as a string; they are re-parsed so the
    /// envelope carries a real object, matching what a model emitting the
    /// envelope directly would write. An unparseable argument string is passed
    /// through as a string rather than dropped — the loop's parser tolerates
    /// that shape, and dropping it would lose the call.
    fn as_text(&self) -> String {
        let prose = self.content.clone().unwrap_or_default();
        if self.tool_calls.is_empty() {
            return prose;
        }
        let mut out = prose;
        for call in &self.tool_calls {
            let args: serde_json::Value = match call.function.arguments.as_deref() {
                Some(raw) => serde_json::from_str(raw)
                    .unwrap_or_else(|_| serde_json::Value::String(raw.to_string())),
                None => serde_json::json!({}),
            };
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!(
                "<tool_call>{}</tool_call>",
                serde_json::json!({ "name": call.function.name, "arguments": args })
            ));
        }
        out
    }
}

#[derive(Deserialize)]
struct UsageInfo {
    #[serde(default)]
    total_tokens: usize,
    #[serde(default)]
    prompt_tokens: usize,
    /// Completion tokens generated. Distinct from `total_tokens -
    /// prompt_tokens` only when the server emits all three (some
    /// proxies don't); kept as the explicit source so the cutoff
    /// chip can read the authoritative split.
    #[serde(default)]
    completion_tokens: u32,
}

// ─── SSE Streaming Types ─────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// OpenAI emits a final post-DONE chunk carrying token usage on
    /// some servers (vLLM, recent llama.cpp). `None` on servers that
    /// don't emit it — the cutoff chip still works via finish_reason
    /// alone, just without the precise generated-token count.
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    /// Set on the terminal chunk (and only the terminal chunk in
    /// OpenAI-compliant servers). Parsed via
    /// [`FinishReason::from_openai_str`] at the consume site so a
    /// stray non-OpenAI string (server bug) round-trips to None and
    /// we synthesise a `Stop` rather than panic.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    content: Option<String>,
}

// ─── InferenceProvider Implementation ────────────────────────

#[async_trait]
impl InferenceProvider for RemoteApiProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let start = Instant::now();
        let admitted = self.outbound(Payload::Completion)?;
        let url = format!("{}/chat/completions", self.endpoint.resolve().await?);
        let labels = request.forced_choice_candidates();
        if let Some(labels) = labels
            .as_deref()
            .filter(|_| self.forced_choice_by_logprobs_known())
        {
            if let Some(text) = self
                .forced_choice_by_logprobs(&url, request, labels)
                .await?
            {
                return Ok(CompletionResponse {
                    text,
                    tokens_used: 0,
                    prompt_tokens: 0,
                    model_id: self.model_id.clone(),
                    latency_ms: start.elapsed().as_millis() as u64,
                    oicp_meta: None,
                    finish_reason: Some(FinishReason::Stop),
                    completion_tokens: Some(1),
                });
            }
        }
        let (response, mode) = self.send_chat(&admitted, &url, request).await?;

        let status = response.status().as_u16();
        let raw = response
            .text()
            .await
            .map_err(|e| Error::Inference(format!("Failed to read API response: {e}")))?;
        chat_wire::observe_wire_response(status, &raw);
        let chat_response: ChatCompletionResponse = serde_json::from_str(&raw)
            .map_err(|e| Error::Inference(format!("Failed to parse API response: {e}")))?;

        let first_choice = chat_response.choices.first();
        let text = first_choice
            .map(|c| c.message.answer(request, mode))
            .unwrap_or_default();
        // A host that sampled a label rather than answering with the map
        // (`forced_choice`): ask it once more through its logprobs.
        let text = match labels {
            Some(labels) if sovereign_contracts::oicp::forced_choice::parse(&text).is_none() => {
                self.forced_choice_by_logprobs(&url, request, &labels)
                    .await?
                    .unwrap_or(text)
            }
            _ => text,
        };
        let finish_reason = first_choice
            .and_then(|c| c.finish_reason.as_deref())
            .and_then(FinishReason::from_openai_str);

        let (tokens_used, prompt_tokens, completion_tokens) = chat_response
            .usage
            .map(|u| (u.total_tokens, u.prompt_tokens, Some(u.completion_tokens)))
            .unwrap_or((0, 0, None));

        let model_id = chat_response.model.unwrap_or_else(|| self.model_id.clone());

        if let Some(ref fr) = finish_reason {
            tracing::debug!(
                model = %model_id,
                finish_reason = %fr.as_openai_str(),
                completion_tokens = ?completion_tokens,
                "remote: chat_completions - finish_reason"
            );
        }

        Ok(CompletionResponse {
            text,
            tokens_used,
            prompt_tokens,
            model_id,
            latency_ms: start.elapsed().as_millis() as u64,
            oicp_meta: chat_response.oicp,
            finish_reason,
            completion_tokens,
        })
    }

    /// Typed-Finish streaming override. Parses the SSE
    /// `choices[].finish_reason` and `usage` fields and emits a
    /// terminal [`StreamFrame::Finish`] frame so peer-routed mesh
    /// streams surface real Length truncation instead of the trait
    /// default's synthetic `Stop`. Pairs with
    /// `InferenceRouter::complete_stream_with_id_and_finish`,
    /// which is what carries the typed frame all the way to the
    /// runtime's cutoff-chip wiring.
    ///
    /// Note on OpenAI SSE shapes: `finish_reason` typically lands on
    /// the last `delta`-bearing chunk (or one just before `[DONE]`).
    /// `usage` lands either on the same chunk (vLLM, recent llama.cpp)
    /// or in a separate post-DONE chunk on some servers. We
    /// accumulate both lazily and emit them on the terminal Finish
    /// frame regardless of which chunk carried them.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>> {
        use sovereign_contracts::types::{FinishReason, StreamFrame, StreamUsage};
        let admitted = self.outbound(Payload::Completion)?;
        let url = format!("{}/chat/completions", self.endpoint.resolve().await?);
        let mut body = self.build_request(request)?;
        body["stream"] = serde_json::json!(true);
        chat_wire::observe_wire_request(&body);

        let lap = sovereign_contracts::engine_state::Lap::start("client", "chat");
        let response = self
            .send_honouring_shed(
                || admitted.post(&url).json(&body),
                "Remote typed stream request",
            )
            .await?;
        lap.mark("headers");

        let status = response.status().as_u16();
        let byte_stream = response.bytes_stream();
        // Carry parser state across the byte-stream's filter_map by
        // streaming into a channel: parsing SSE line-by-line is
        // stateful (finish_reason + usage may land on any chunk
        // before [DONE]) and async-stream combinators can't carry
        // mutable state across yields cleanly. Channel-driven actor
        // keeps the parser straightforward.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);
        tokio::spawn(sovereign_contracts::engine_observe::carry_future(
            async move {
                use futures::StreamExt;
                let mut byte_stream = byte_stream;
                let mut buf = String::new();
                // The raw stream, kept only while a conformance sink is installed.
                let mut raw = sovereign_contracts::engine_observe::is_observing().then(String::new);
                let mut finish_reason: Option<FinishReason> = None;
                let mut usage: Option<StreamUsage> = None;
                'outer: while let Some(chunk) = byte_stream.next().await {
                    let Ok(bytes) = chunk else { continue };
                    lap.first("first byte");
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    if let Some(raw) = raw.as_mut() {
                        raw.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    // Process complete lines; leave the tail in buf for
                    // the next iteration so a chunk-split SSE line
                    // doesn't drop tokens.
                    while let Some(pos) = buf.find('\n') {
                        let line = buf[..pos].trim().to_string();
                        buf.drain(..=pos);
                        if line == "data: [DONE]" {
                            break 'outer;
                        }
                        let Some(data) = line.strip_prefix("data: ") else {
                            continue;
                        };
                        let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) else {
                            continue;
                        };
                        if let Some(u) = parsed.usage {
                            usage = Some(StreamUsage {
                                prompt_tokens: u.prompt_tokens as u32,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens as u32,
                            });
                        }
                        for choice in parsed.choices {
                            if let Some(text) = choice.delta.content {
                                lap.first("first frame parsed");
                                if !text.is_empty()
                                    && tx.send(StreamFrame::Token(text)).await.is_err()
                                {
                                    return;
                                }
                            }
                            if let Some(reason_str) = choice.finish_reason {
                                finish_reason = FinishReason::from_openai_str(&reason_str);
                                tracing::debug!(
                                    finish_reason = %reason_str,
                                    "remote: stream - terminal finish_reason captured"
                                );
                            }
                        }
                    }
                }
                if let Some(raw) = raw {
                    chat_wire::observe_wire_response(status, &raw);
                }
                let _ = tx
                    .send(StreamFrame::Finish {
                        reason: finish_reason.unwrap_or(FinishReason::Stop),
                        usage,
                    })
                    .await;
            },
        ));
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let admitted = self.outbound(Payload::Completion)?;
        let url = format!("{}/chat/completions", self.endpoint.resolve().await?);
        let mut body = self.build_request(request)?;
        body["stream"] = serde_json::json!(true);

        let response = self
            .send_honouring_shed(|| admitted.post(&url).json(&body), "Remote stream request")
            .await?;

        let byte_stream = response.bytes_stream();

        let token_stream = byte_stream.filter_map(|chunk| async move {
            let bytes = chunk.ok()?;
            let text = String::from_utf8_lossy(&bytes);

            let mut tokens = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line == "data: [DONE]" {
                    break;
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                        if let Some(content) =
                            chunk.choices.first().and_then(|c| c.delta.content.clone())
                        {
                            tokens.push(content);
                        }
                    }
                }
            }

            if tokens.is_empty() {
                None
            } else {
                Some(Ok(tokens.join("")))
            }
        });

        Ok(Box::pin(token_stream))
    }

    /// Dispatch requests concurrently via HTTP connection pooling.
    /// Against a server with `--parallel N`, N requests run simultaneously.
    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        let futures: Vec<_> = requests.iter().map(|req| self.complete(req)).collect();
        futures::future::join_all(futures)
            .await
            .into_iter()
            .collect()
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_as_given(&self.document_input(text)).await
    }

    /// The rerank kind's client method (`rerank.rs`).
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.rerank_over_route(query, docs).await
    }

    /// Embed many texts with ONE request per chunk of
    /// [`Self::EMBED_BATCH_INPUTS`], using the `/embeddings` endpoint's
    /// array `input` form.
    ///
    /// Without this override the trait default loops `embed()` — one
    /// HTTP round-trip and one single-sequence decode per text. Measured
    /// on the 2026-07-24 book-ingest arc: the daemon had served 8959
    /// consecutive embed calls at `sequences=1`, and a 301-chunk
    /// document spent 78s of a 149s ingest embedding, ~250ms per chunk.
    /// The server side was batch-capable the whole time
    /// (`routes_inference::embeddings` → `LocalInferenceService::
    /// embed_batch` → the embed slot's multi-sequence decode, 16
    /// sequences per decode); only the client never asked for it.
    ///
    /// Chunks are sent sequentially on purpose: the embed slot
    /// serializes on a single context lock, so overlapping requests
    /// would queue inside the daemon rather than add throughput.
    ///
    /// Falls back to the sequential default if the endpoint rejects an
    /// array payload — third-party OpenAI-compatible servers vary, and
    /// an ingest must not fail over a request-shape difference.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(Self::EMBED_BATCH_INPUTS) {
            match self.embed_many_one_request(chunk).await {
                Ok(vectors) => out.extend(vectors),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        inputs = chunk.len(),
                        "embed_batch: array request failed; falling back to per-text embed"
                    );
                    for text in chunk {
                        out.push(self.embed(text).await?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Embed a *query* with this model's query-side instruction prefix.
    ///
    /// The OpenAI `/embeddings` endpoint has no query/document distinction, so
    /// the prefix is applied client-side: prepend it, then embed via the same
    /// HTTP path. This makes the result bit-identical to the embedded engine's
    /// `embed_query_sync` (which prepends the same `query_instruction`). When
    /// the model declares no query instruction (chat / non-embedding ids), the
    /// prefix is empty and this is exactly `embed()` — no behaviour change.
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        if let Some(q) = &self.input_prep {
            return self.embed_as_given(&q.prepare_query(query)).await;
        }
        if self.query_instruction.is_empty() {
            return self.embed(query).await;
        }
        let prefixed = format!("{}{query}", self.query_instruction);
        self.embed(&prefixed).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: self.context_size as usize,
            supports_structured_output: false,
            relative_speed: Speed::Medium,
            relative_reasoning: Depth::Deep,
        }
    }

    /// The model this provider talks to.
    ///
    /// Was inheriting the trait default, `"unknown"` — while the id sat
    /// right there in `self.model_id`, and `capabilities()` two methods
    /// up was already reading its sibling field. The cost was not
    /// cosmetic: the inherited `complete_stream_with_id` stamps this
    /// onto every streamed response's provenance, and the mesh builds
    /// one of these per peer specifically so a turn can say where it
    /// went (`peer_inference::provider_for_peer`). A placeholder id is
    /// still the right answer here for exactly that reason — see
    /// `model_id_is_placeholder`, which governs the WIRE, not
    /// attribution.
    fn model_id_for(&self, _speed: Speed) -> String {
        self.model_id.clone()
    }

    /// Was inheriting `None` despite `context_size` being a field —
    /// the same oversight as `model_id_for`. `None` reads as "no window
    /// known", which switches the runtime's budget-aware compaction to
    /// its blind fallback.
    fn effective_context_size(&self) -> Option<u32> {
        Some(self.context_size)
    }

    /// POSTs to the daemon's loopback warmup endpoint
    /// (`/internal/inference/warmup`, see
    /// `commonwealth-api::routes_internal::mesh_admin::inference_warmup`).
    /// Used by the desktop's child-process supervisor path so a
    /// window-focus warmup flows over HTTP to the supervised daemon
    /// rather than into an in-process slot.
    ///
    /// Best-effort and silent on failure (network error, 4xx/5xx) —
    /// the trait's default impl is a no-op for the same reason: an
    /// unwarmed slot is a slow first turn, not a broken caller.
    async fn warmup_primary(&self) -> Result<()> {
        let Ok(url) = self.warmup_url().await else {
            tracing::debug!(
                binding = %self.endpoint.describe(),
                "RemoteApiProvider::warmup_primary: endpoint unresolved, treating as no-op"
            );
            return Ok(());
        };
        let req = self
            .outbound(Payload::Probe)?
            .post(&url)
            .json(&serde_json::json!({}));
        match req.send().await {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => {
                tracing::debug!(
                    status = %r.status(),
                    "RemoteApiProvider::warmup_primary: non-success, treating as no-op"
                );
                Ok(())
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "RemoteApiProvider::warmup_primary: transport error, treating as no-op"
                );
                Ok(())
            }
        }
    }
}

/// Wraps two [`RemoteApiProvider`]s — one per endpoint — and routes
/// `InferenceProvider` trait calls to the correct one. `RemoteApiProvider` is
/// constructed with a single `model_id` used for BOTH `/chat/completions` and
/// `/embeddings`; sending a chat model to the embeddings endpoint returns
/// non-embedding shapes (or errors). Keeping two instances and routing by
/// method keeps the daemon honest: the chat endpoint never sees an embed model
/// id, and vice versa.
///
/// This is the "talk to the daemon over HTTP, own no weights" provider for
/// **both** `sovereign chat` (the daemon-backed CLI) and the desktop's Attach
/// mode (a CLI daemon already owns the models on `:9741`). Promoted here from
/// `sovereign-cli-llm::chat_cmd::bootstrap` (2026-06-16) so the two callers
/// share one impl rather than diverging.
pub struct SplitInferenceProvider {
    chat: std::sync::Arc<RemoteApiProvider>,
    /// A daemon's or server's embed route, or, under a hosted engine, a model
    /// in this process ([`EngineEmbed::Local`]).
    embed: std::sync::Arc<dyn InferenceProvider>,
    chat_model_id: String,
    /// Kept so `embed_model_id()` can vouch for persisted embeddings
    /// (the T1 memory-embedding staleness guard) without a daemon
    /// round-trip.
    embed_model_id: String,
    /// Daemon-side chat slot context window, captured at construction (the same
    /// `SetupConfig.effective_context_size()` value the daemon's slot loader
    /// uses) so `effective_context_size` answers without a daemon round-trip —
    /// the runtime's budget-aware compaction arm reads it.
    context_size: u32,
    /// Where a turn built by this provider actually executes.
    ///
    /// STORED, not re-derived from the endpoint. It used to be answered by
    /// string-matching the chat endpoint's host for loopback, which was right
    /// only while every forwarder held a literal address. A terminal resolves
    /// its entry node through the mesh, and on an encrypted mesh that resolves
    /// to an iroh bridge on `127.0.0.1` — a loopback address whose far end is
    /// another machine. Sniffing it would report `ForwardsOnBox` and start
    /// honouring `local_only` for turns that leave the host, which is the exact
    /// inversion [`ServingLocus`] exists to prevent.
    ///
    /// So the party that knows answers: the builder of the provider. The
    /// address-shaped constructors keep deciding it by loopback test, because
    /// for them the address IS the whole truth.
    ///
    /// [`ServingLocus`]: sovereign_contracts::traits::ServingLocus
    locus: sovereign_contracts::traits::ServingLocus,
    /// serve's self-report, in the loopback mode only (`serve_loopback`).
    served: Option<sovereign_contracts::engine_state::ServedSelf>,
    /// A hosted engine's own models (`Self::engine` only). A forwarder holds
    /// nothing and reports nothing; a hosted engine serves its vendor model.
    hosted: Option<outbound::Hosted>,
}

mod chat_wire;
mod forced_choice;
mod outbound;
pub use outbound::{EngineEmbed, FarEnd, Payload, ThirdPartyRefusal};
mod shed;
pub use chat_wire::{openai_function_name, titled_schema, StructuredOutputMode};
mod loopback;
pub use loopback::endpoint_is_loopback;

impl SplitInferenceProvider {
    /// Build the daemon-backed pair from an explicit context window and embed
    /// query-instruction prefix.
    ///
    /// Both slots are declared LAST RESORT — this provider owns no weights, so
    /// the daemon on the far end is the only holder and a shed is waited out
    /// rather than reported. See [`RemoteApiProvider::waiting_out_sheds`].
    pub fn new(
        endpoint_v1: &str,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        embed_query_instruction: String,
    ) -> Self {
        Self::new_with_bearer(
            endpoint_v1,
            None,
            chat_model_id,
            embed_model_id,
            context_size,
            embed_query_instruction,
        )
    }

    /// Same, carrying an `Authorization: Bearer` on every outbound call.
    ///
    /// The bearer exists for the case where the daemon on the far end is
    /// **not this operator's** — a node that lent named models to a guest for
    /// a bounded window (`svrn mesh grant`). A local daemon needs none: a
    /// loopback caller is admitted before any bearer is read.
    ///
    /// Deliberately a second constructor rather than a `with_bearer(self)`
    /// builder: the key lives inside the two `RemoteApiProvider`s, so a
    /// post-hoc setter would have to rebuild both — and would then be a second
    /// site deciding shed-waiting and the query-instruction prefix. One body
    /// builds the pair; `new` is the no-bearer call of it.
    pub fn new_with_bearer(
        endpoint_v1: &str,
        bearer: Option<String>,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        embed_query_instruction: String,
    ) -> Self {
        // BOTH slots wait out a shed, and this is the ONE site that opts in
        // (ARCH §7 — structural, not remembered). This provider owns no
        // weights: the daemon on the other end of `endpoint_v1` is the only
        // holder there is, so "busy, come back in 32s" is the whole answer and
        // there is nowhere else to route. A peer provider is the opposite case
        // and stays OFF by default — see [`RemoteApiProvider::wait_out_sheds`].
        //
        // Putting it here rather than at the six call sites is what makes it
        // unforgettable: a new daemon-backed client gets the behaviour by
        // construction, and `provider_for_peer` cannot acquire it by accident
        // because it builds a bare `RemoteApiProvider`, never this.
        //
        // The failure it closes was measured: three sub-requests of ONE turn's
        // own fan-out, refused by their own host inside 17 ms against a
        // 32-second hint, with no other client on the machine (note
        // `bf432b4d`). The hint was computed, serialised, transported — and
        // dropped.
        let chat = std::sync::Arc::new(
            RemoteApiProvider::new(endpoint_v1, bearer.clone(), &chat_model_id, context_size)
                .waiting_out_sheds()
                .carrying_turn_admission(),
        );
        // The embed slot carries the query-instruction prefix so
        // `embed_query` stays bit-identical to the embedded engine. The chat
        // slot never embeds, so it leaves the prefix empty.
        let embed = std::sync::Arc::new(
            RemoteApiProvider::new(endpoint_v1, bearer, &embed_model_id, context_size)
                .with_query_instruction(embed_query_instruction)
                .waiting_out_sheds(),
        );
        Self {
            chat,
            embed,
            chat_model_id,
            embed_model_id,
            context_size,
            // The address is the whole truth for this constructor: a caller who
            // named `127.0.0.1` named this machine. See the field docs for why
            // the resolved-binding constructor is told instead of asked.
            locus: if endpoint_is_loopback(endpoint_v1) {
                sovereign_contracts::traits::ServingLocus::ForwardsOnBox
            } else {
                sovereign_contracts::traits::ServingLocus::ForwardsOffBox
            },
            served: None,
            hosted: None,
        }
    }

    /// Build against an entry node resolved through the mesh on every call —
    /// the `terminal` node's provider.
    ///
    /// `locus` is an argument because this constructor cannot work it out: the
    /// resolved address may be an iroh bridge on loopback whose far end is
    /// another machine. The caller knows which node it bound, so the caller
    /// says. A terminal always passes [`ServingLocus::ForwardsOffBox`].
    ///
    /// [`ServingLocus::ForwardsOffBox`]: sovereign_contracts::traits::ServingLocus::ForwardsOffBox
    /// `node_id_hex` is THIS node's mesh identity, stamped as `X-Node-Id` on
    /// every request both slots send. Without it the entry node admits a
    /// terminal's traffic as its OWN LOCAL traffic, with three consequences:
    /// it cannot ration the terminal at all (the ceiling, the foreground yield
    /// and the contribution pause are all keyed on that header), the
    /// terminal's usage never appears on the entry node's
    /// `/status.inference.peer_requests` so a fleet cannot see what its
    /// weightless nodes cost, and the obvious two-machine corroboration —
    /// fire an embedding on the terminal, watch the entry node's tally move —
    /// cannot pass. Measured 2026-08-31: RuggedFox's embeddings reached MAC
    /// and left no trace there beyond the terminal's own log line.
    ///
    /// `None` is honest rather than defaulted (§18.3): the caller could not
    /// determine this node's identity, the request goes out unstamped, and the
    /// caller logs it. Do not synthesise a placeholder — `parse_x_node_id`
    /// buckets an unparseable value under the zero node and still gates it,
    /// so a fake id would ration every terminal in the fleet as one peer.
    ///
    /// Safe for a terminal specifically because both slots are built
    /// `waiting_out_sheds()`: being newly rationable cannot strand a node
    /// whose far end is the only holder there is.
    pub fn resolved(
        resolver: std::sync::Arc<dyn EndpointResolver>,
        locus: sovereign_contracts::traits::ServingLocus,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        embed_query_instruction: String,
        node_id_hex: Option<String>,
    ) -> Self {
        // One helper for both slots, so a future third slot cannot acquire the
        // stamp on one path and forget it on the other (§10.6).
        fn stamp(p: RemoteApiProvider, node_id_hex: &Option<String>) -> RemoteApiProvider {
            match node_id_hex {
                Some(hex) => p.with_node_id(hex.clone()),
                None => p,
            }
        }
        // Both slots wait out a shed for the reason given in `new_with_bearer`:
        // this provider owns no weights, so the node on the far end is the only
        // holder there is and there is nowhere to route a refusal.
        let chat = std::sync::Arc::new(stamp(
            RemoteApiProvider::dynamic(
                std::sync::Arc::clone(&resolver),
                None,
                &chat_model_id,
                context_size,
            )
            .waiting_out_sheds(),
            &node_id_hex,
        ));
        let embed = std::sync::Arc::new(stamp(
            RemoteApiProvider::dynamic(resolver, None, &embed_model_id, context_size)
                .with_query_instruction(embed_query_instruction)
                .waiting_out_sheds(),
            &node_id_hex,
        ));
        Self {
            chat,
            embed,
            chat_model_id,
            embed_model_id,
            context_size,
            locus,
            served: None,
            hosted: None,
        }
    }

    /// Chat and embeddings on SEPARATE servers.
    ///
    /// [`Self::new_with_bearer`] points both halves at one base URL, which is
    /// right for a Sovereign daemon (it serves both from one `/v1`). It is the
    /// wrong shape for the usual third-party deployment: vLLM, SGLang and TGI
    /// each serve ONE model per process, so a node that both chats and
    /// retrieves is talking to two of them on two ports.
    ///
    /// The `/status` probe resolves from the CHAT endpoint — that is the host
    /// whose slot residency `primary_slot_status` is asking about — and is
    /// derived on demand from that provider's `daemon_root()` rather than
    /// stored, so there is one derivation of "the daemon root" rather than
    /// two (§10.6). A third-party server has no `/status`, so that probe
    /// simply returns `None`, which is the documented "cannot answer" and not
    /// an error.
    ///
    /// The serving LOCUS likewise follows the chat endpoint: a turn runs where
    /// chat goes, and the embed half may sit elsewhere without changing
    /// whether a prompt left this machine.
    pub fn new_split_endpoints(
        chat_endpoint_v1: &str,
        embed_endpoint_v1: &str,
        bearer: Option<String>,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        embed_query_instruction: String,
    ) -> Self {
        // Both halves wait out sheds for the same reason as the single-endpoint
        // constructor: this provider owns no weights, so the far end is the
        // only holder and there is nowhere else to route.
        let chat = std::sync::Arc::new(
            RemoteApiProvider::new(
                chat_endpoint_v1,
                bearer.clone(),
                &chat_model_id,
                context_size,
            )
            .waiting_out_sheds(),
        );
        let embed = std::sync::Arc::new(
            RemoteApiProvider::new(embed_endpoint_v1, bearer, &embed_model_id, context_size)
                .with_query_instruction(embed_query_instruction)
                .waiting_out_sheds(),
        );
        Self {
            chat,
            embed,
            chat_model_id,
            embed_model_id,
            context_size,
            // The CHAT endpoint decides, because that is where a turn runs —
            // the embed half can sit elsewhere without changing whether a
            // prompt left this machine. Address-derived like the other
            // operator-named constructors: whoever typed `127.0.0.1` named
            // this box. See the `locus` field for why the resolved-binding
            // constructor is told instead of asked.
            locus: if endpoint_is_loopback(chat_endpoint_v1) {
                sovereign_contracts::traits::ServingLocus::ForwardsOnBox
            } else {
                sovereign_contracts::traits::ServingLocus::ForwardsOffBox
            },
            served: None,
            hosted: None,
        }
    }

    /// Build from an OICP manifest (v0.4 §7 context discoverability): resolve
    /// the chat slot's context window from the advertised
    /// [`ProviderModel::context_tokens`] rather than a hardcoded default, so a
    /// client's budget-aware compaction matches the host's real window.
    ///
    /// Falls back to the historical 8192 when the manifest doesn't advertise
    /// the chat model's context (a v0.3 host, or a model absent from
    /// `/v1/models`) — never a hard failure.
    pub fn from_manifest(
        endpoint_v1: &str,
        manifest: &ProviderManifest,
        chat_model_id: String,
        embed_model_id: String,
    ) -> Self {
        Self::from_manifest_with_bearer(endpoint_v1, None, manifest, chat_model_id, embed_model_id)
    }

    /// [`Self::from_manifest`] carrying a bearer. See [`Self::new_with_bearer`]
    /// for why the credential is a constructor argument rather than a setter.
    pub fn from_manifest_with_bearer(
        endpoint_v1: &str,
        bearer: Option<String>,
        manifest: &ProviderManifest,
        chat_model_id: String,
        embed_model_id: String,
    ) -> Self {
        /// The pre-v0.4 client default, used when the host doesn't advertise a
        /// truthful `context_tokens` for the chat model.
        const V03_FALLBACK_CONTEXT: u32 = 8192;
        let context_size = manifest
            .models
            .iter()
            .find(|m| m.id == chat_model_id)
            .map(|m| m.context_tokens)
            .filter(|&c| c > 0)
            .unwrap_or(V03_FALLBACK_CONTEXT);
        // v0.4 §4: the embed model's query-instruction prefix is advertised in
        // the knowledge section; empty on a v0.3 host (or one without a
        // knowledge plane).
        let embed_query_instruction = manifest
            .knowledge
            .as_ref()
            .and_then(|k| k.embed_model.as_ref())
            .map(|e| e.query_instruction_prefix.clone())
            .unwrap_or_default();
        Self::new_with_bearer(
            endpoint_v1,
            bearer,
            chat_model_id,
            embed_model_id,
            context_size,
            embed_query_instruction,
        )
    }
}

#[async_trait]
impl InferenceProvider for SplitInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.chat.complete(&self.for_speed(request)).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.chat.complete_stream(&self.for_speed(request)).await
    }

    /// Stream with a TYPED terminal frame.
    ///
    /// Third instance of the same defect, found while auditing the
    /// first two: `self.chat` parses the real `finish_reason` and
    /// `usage` off the SSE wire, but without this forward the trait
    /// default wraps the UNTYPED `complete_stream` and appends a
    /// `Finish { reason: Stop, usage: None }` it never observed. The
    /// trait doc is explicit that silent truncation "is the bug this
    /// method exists to make impossible" — and inheriting the default
    /// reintroduced exactly that: a `max_tokens` cutoff rendered
    /// identically to a clean stop, and token accounting read `None`,
    /// on every streaming turn in Attach mode.
    ///
    /// `complete_stream_with_id_and_finish` needs no forward of its
    /// own: its default composes this method with `model_id_for`, and
    /// both are now honest here.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>> {
        if serve_loopback::wants_raw_completion(&self.served, request) {
            return self.chat.raw_completion_stream(request).await;
        }
        self.chat
            .complete_stream_with_finish(&self.for_speed(request))
            .await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        let requests: Vec<_> = requests
            .iter()
            .map(|r| self.for_speed(r).into_owned())
            .collect();
        self.chat.complete_batch(&requests).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed.embed(text).await
    }

    /// Route to the embed provider's batch path. Without this the trait
    /// default would loop `Self::embed` — the exact one-round-trip-per-
    /// chunk behaviour that made corpus ingest embed-bound.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed.embed_batch(texts).await
    }

    /// Route to the embed provider's `embed_query` so its model-specific
    /// query-instruction prefix is applied (the trait default would call
    /// `Self::embed`, the document path, silently dropping the prefix).
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed.embed_query(query).await
    }

    /// Rerank on the serving node, which is the chat endpoint's host.
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.chat.rerank_batch(query, docs).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        if let Some(served) = &self.served {
            return serve_loopback::model_id_for(served, speed);
        }
        if let Some(hosted) = &self.hosted {
            return hosted.model_id_for(speed, &self.chat_model_id);
        }
        // Only one chat slot over HTTP; the daemon's own engine maps the
        // request (Speed / max_tokens) to its loaded fast/primary slots.
        // Reporting the request model is the most honest client-side signal.
        self.chat_model_id.clone()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        match &self.hosted {
            Some(hosted) => hosted.slots.clone(),
            None => serve_loopback::resident_slots(&self.served),
        }
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        serve_loopback::edit_slot_info(&self.served)
    }

    fn load_extra_slot(&self, _: String, _: std::path::PathBuf, _: u32) -> Result<String> {
        Err(serve_loopback::slot_refusal(&self.served, "load"))
    }

    fn unload_extra_slot(&self, _: &str) -> Result<Option<String>> {
        Err(serve_loopback::slot_refusal(&self.served, "unload"))
    }

    fn code_model_id(&self) -> Option<String> {
        serve_loopback::code_model_id(&self.served)
    }

    fn compute_children(&self) -> Vec<sovereign_contracts::oicp::ComputeChildStatus> {
        serve_loopback::compute_children(&self.served)
    }

    fn embed_model_id(&self) -> String {
        self.embed_model_id.clone()
    }

    /// Forwarding, and WHERE to matters.
    ///
    /// On-box means the turn stays on this machine — the attach-mode desktop
    /// and the CLI chat bootstrap both point at their own daemon, and a
    /// `local_only` envelope is perfectly satisfiable that way. Off-box is a
    /// `terminal` bound to its entry node, and there `local_only` cannot be
    /// honoured by anyone: this process owns no weights and the only place the
    /// turn can run is another machine.
    ///
    /// Decided at construction — see the `locus` field for why asking the
    /// address here would invert the answer for a mesh-resolved binding.
    fn serving_locus(&self) -> sovereign_contracts::traits::ServingLocus {
        self.locus
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.chat.capabilities()
    }

    fn effective_context_size(&self) -> Option<u32> {
        Some(self.context_size)
    }

    /// Ask the daemon whether its deep-reasoning slot is loaded.
    ///
    /// This provider owns no weights, so the inherited default (read the
    /// sync `resident_slots()`, which is empty here) would answer `None`
    /// forever — and the caller would stay silent through the exact wait
    /// it exists to explain. The attach-mode desktop runs its Runtime
    /// in-process against this provider, so that silence was the bug:
    /// a 95s cold load with a frozen counter and no stated cause.
    ///
    /// Fails soft in every direction — unreachable daemon, non-200,
    /// unparseable body, no primary row — all yield `None`, i.e. "can't
    /// say", never a fabricated verdict and never a blocked turn. The
    /// timeout is deliberately tight: this runs on the critical path
    /// immediately before synthesis, and a narration frame is never
    /// worth delaying the answer it narrates.
    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        if let Some(served) = &self.served {
            return serve_loopback::primary_slot(served);
        }
        if let Some(hosted) = &self.hosted {
            return hosted.primary();
        }
        #[derive(Deserialize)]
        struct StatusBody {
            inference: StatusInference,
        }
        #[derive(Deserialize)]
        struct StatusInference {
            #[serde(default)]
            resident: Vec<StatusSlot>,
        }
        #[derive(Deserialize)]
        struct StatusSlot {
            role: String,
            #[serde(default)]
            model_id: String,
            #[serde(default)]
            resident: bool,
            #[serde(default)]
            size_bytes: Option<u64>,
            #[serde(default)]
            transitioning: bool,
        }

        // Derived from the chat provider's endpoint rather than a stored copy:
        // on a resolved binding there is no fixed address to have stored, and
        // two derivations of "the daemon root" is two answers to one question.
        let status_url = format!("{}/status", self.chat.daemon_root().await.ok()?);
        let resp = reqwest::Client::new()
            .get(&status_url)
            .timeout(std::time::Duration::from_millis(1500))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: StatusBody = resp.json().await.ok()?;
        let slot = body
            .inference
            .resident
            .into_iter()
            .find(|s| s.role == "primary")?;
        Some(ResidentSlot {
            role: slot.role,
            model_id: slot.model_id,
            resident: slot.resident,
            size_bytes: slot.size_bytes,
            transitioning: slot.transitioning,
            placement: None,
        })
    }

    /// Ask the daemon to load its deep-reasoning slot now.
    ///
    /// Same shape of bug as [`Self::primary_slot_status`] above, and it
    /// is worth stating plainly because this is the second instance:
    /// the trait's default `warmup_primary` is `Ok(())`, a SILENT
    /// no-op. A weight-less provider that doesn't override it therefore
    /// reports success while doing nothing, and the caller has no way
    /// to tell "warmed" from "never happened". The desktop's two warm
    /// triggers — window-focus and chat-mount — both ran through this
    /// provider in Attach mode, so both were dead: the app looked like
    /// it had warm-up wired end to end while every deep turn still paid
    /// the full cold load.
    ///
    /// `self.chat` already implements this correctly against the
    /// daemon's HTTP warmup route, so the fix is delegation, not new
    /// transport. Best-effort by the same reasoning as the rest of the
    /// warm path: an unwarmed slot is a slow first turn, never a
    /// blocked one. The embed provider is deliberately not warmed —
    /// the embed slot is eagerly loaded at daemon startup and never
    /// idle-unloaded, so it has nothing to warm.
    async fn warmup_primary(&self) -> Result<()> {
        self.chat.warmup_primary().await
    }
}

#[cfg(test)]
mod tests {
    use super::ChatCompletionResponse;

    /// The named failing input (ARCH §18.1): a daemon handed a `tools` catalog
    /// answers with `content: ""` and the call in `tool_calls`. Before
    /// 2026-09-08 that reached the caller as an EMPTY completion, so a tool
    /// loop that asked for the tool saw no call — measured on the knowledge
    /// gym's executor fixtures, 0 of 9 replays parsed a call and every
    /// `final_message excerpt` was blank.
    #[test]
    fn a_native_tool_call_arrives_as_the_one_envelope() {
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"",
            "tool_calls":[{"id":"call_1","type":"function","function":{
            "name":"knowledge_lookup","arguments":"{\"query\":\"mesh retry policy\"}"}}]},
            "finish_reason":"tool_calls"}]}"#;
        let parsed: ChatCompletionResponse =
            serde_json::from_str(body).expect("wire response parses");
        let text = parsed.choices.first().map(|c| c.message.as_text()).unwrap();
        // The property is that it ROUND-TRIPS into the loop's parser, not that
        // the string has a particular key order — asserting the rendered text
        // would pin serde_json's map ordering and pass or fail for a reason
        // that has nothing to do with the behaviour.
        let v: serde_json::Value = {
            let inner = text
                .split("<tool_call>")
                .nth(1)
                .and_then(|s| s.split("</tool_call>").next())
                .expect("envelope is closed");
            serde_json::from_str(inner).expect("envelope body is JSON")
        };
        assert_eq!(v["name"], "knowledge_lookup");
        assert_eq!(v["arguments"]["query"], "mesh retry policy");
    }

    #[test]
    fn prose_without_tool_calls_is_untouched() {
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant",
            "content":"Just an answer."},"finish_reason":"stop"}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.choices.first().map(|c| c.message.as_text()).unwrap(),
            "Just an answer."
        );
    }

    #[test]
    fn prose_and_a_native_call_both_survive() {
        // Some servers put the model's reasoning in `content` AND the call in
        // `tool_calls`. Dropping either half loses information the loop uses:
        // the prose becomes the transcript's "thinking", the call becomes the
        // dispatch.
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant",
            "content":"Let me check.","tool_calls":[{"id":"c","type":"function",
            "function":{"name":"a","arguments":"{\"k\":1}"}}]},"finish_reason":"tool_calls"}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(body).unwrap();
        let text = parsed.choices.first().map(|c| c.message.as_text()).unwrap();
        assert!(text.starts_with("Let me check."), "{text}");
        assert!(text.contains(r#""name":"a""#), "{text}");
    }

    #[test]
    fn unparseable_arguments_are_carried_not_dropped() {
        // Absence is reported, never defaulted (ARCH §18.3) — and a call whose
        // argument string is malformed is still a call the loop should see and
        // refuse on its own terms, not one the wire silently swallows.
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"",
            "tool_calls":[{"id":"c","type":"function",
            "function":{"name":"a","arguments":"{not json"}}]},"finish_reason":"tool_calls"}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(body).unwrap();
        let text = parsed.choices.first().map(|c| c.message.as_text()).unwrap();
        assert!(text.contains(r#""name":"a""#), "{text}");
        assert!(
            text.contains("not json"),
            "the raw arguments survive: {text}"
        );
    }

    /// A shed is retried; a FAILURE is not. This is the whole safety property
    /// of the retry, so it is the thing pinned.
    ///
    /// Named failing input (ARCH §18.1): key the retry on the 503 STATUS
    /// instead of on `retry_after_secs`, and case two starts retrying a
    /// genuine `backend_error` — turning a broken host into a slow one and
    /// hiding the break. That is the exact shape of the embed-slot failure
    /// that cost this project a session on 2026-08-26 (note `f4972e1b`).
    #[test]
    fn only_backpressure_is_retried_never_a_failure() {
        use reqwest::StatusCode;
        let shed = r#"{"error":"host busy: ~121875 ms predicted wait at queue position 1","reason":"local_queue_full","retry_after_secs":32}"#;
        assert_eq!(
            shed_retry_after(StatusCode::SERVICE_UNAVAILABLE, shed),
            Some(std::time::Duration::from_secs(32))
        );

        // A 503 that is NOT a shed — no `retry_after_secs`. Must not retry.
        let broken = r#"{"error":{"message":"embedding batch failed: Decode Error -3","type":"backend_error"}}"#;
        assert_eq!(
            shed_retry_after(StatusCode::SERVICE_UNAVAILABLE, broken),
            None
        );

        // The hint on a non-503 is not ours to honour.
        assert_eq!(
            shed_retry_after(StatusCode::INTERNAL_SERVER_ERROR, shed),
            None
        );

        // Not JSON at all, and an empty body — both are refusals, not delays.
        assert_eq!(
            shed_retry_after(StatusCode::SERVICE_UNAVAILABLE, "gateway timeout"),
            None
        );
        assert_eq!(shed_retry_after(StatusCode::SERVICE_UNAVAILABLE, ""), None);
    }

    /// The default is OFF, and that is the safety half.
    ///
    /// Named failing input (ARCH §18.1), and it is not hypothetical: shipping
    /// this ON by default on 2026-08-26 broke
    /// `chat_completion_e2e::a_yielding_peer_is_asked_once_not_once_per_turn`
    /// and `repeated_sheds_never_quarantine_a_healthy_peer` — a peer that
    /// yielded with `retry_after_secs=34` was re-dialled twice inside its own
    /// window. A peer shed is a ROUTING signal; only a last-resort endpoint
    /// may wait one out.
    #[test]
    fn a_provider_does_not_wait_out_sheds_unless_told_to() {
        let p = RemoteApiProvider::new("http://x", None, "m", 4096);
        assert!(
            !p.wait_out_sheds,
            "default must be OFF — a peer shed is a routing signal, not backpressure to sit on"
        );
        assert!(p.waiting_out_sheds().wait_out_sheds);
    }

    /// The hint is honoured, not obeyed. A `0` would spin; an hour would hang
    /// the caller the total cap exists to protect.
    #[test]
    fn the_retry_hint_is_clamped_at_both_ends() {
        use reqwest::StatusCode;
        let with = |n: u64| format!(r#"{{"reason":"local_queue_full","retry_after_secs":{n}}}"#);
        let d = |n: u64| shed_retry_after(StatusCode::SERVICE_UNAVAILABLE, &with(n)).unwrap();
        assert_eq!(d(0), std::time::Duration::from_secs(1));
        assert_eq!(d(32), std::time::Duration::from_secs(32));
        // The ceiling IS the total budget — no dead range above it.
        assert_eq!(d(9_999), SHED_TOTAL_WAIT_CAP);
        // So no single honoured hint can ever exceed the budget it spends.
        assert!(d(9_999) <= SHED_TOTAL_WAIT_CAP);
        assert!(d(32) <= SHED_TOTAL_WAIT_CAP);
    }
    use super::*;

    #[tokio::test]
    async fn fetch_manifest_degrades_to_none_on_unreachable_host() {
        // Contract: a v0.3 host (or any transport failure) yields None so the
        // caller falls back to v0.3 defaults rather than hard-erroring. Port 1
        // is reserved/unbound, so this never touches a real daemon.
        let m = fetch_manifest("http://127.0.0.1:1", None).await;
        assert!(m.is_none());
        // Same for the `/v1`-shaped endpoint — `daemon_root` strips the suffix
        // before joining `/oicp/v1/capabilities`, then still can't connect.
        let m = fetch_manifest("http://127.0.0.1:1/v1", None).await;
        assert!(m.is_none());
    }

    #[test]
    fn build_request_basic() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // Caller-specified `model_id` flows to the wire `model` field.
        // When `request.model_id = None` (default), the field is left
        // empty so the daemon's OICP slot picker decides — see the
        // doc comment on `build_request`.
        let request = CompletionRequest::new("Hello, world!").with_model_id("test-model");
        let body = provider.build_request(&request).unwrap();

        assert_eq!(body["model"], "test-model");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello, world!");
    }

    #[test]
    fn build_request_slow_pins_provider_model_fast_stays_slot_routed() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // Default (Slow) with model_id = None: pinned to the provider's
        // resolved chat model, and NO auto OICP envelope — the daemon's
        // Priority-1 envelope routing would override the pin (the
        // 2026-07-23 fast-slot hijack; see `build_request` docs).
        let request = CompletionRequest::new("Hello");
        let body = provider.build_request(&request).unwrap();
        assert_eq!(body["model"], "test-model");
        // The invariant this guards is "no ROUTING signal", not "no envelope".
        // Absence of the envelope used to be a sufficient proxy for it; since
        // every forward now carries a budget-only envelope to bound the named
        // path, the proxy no longer holds and the real property is asserted
        // directly. The pin itself is checked on the line above, and the four
        // fields below are exactly what `has_routing_signal` and the daemon's
        // Priority-1 gate read.
        for field in [
            "capability_hint",
            "latency_class",
            "context_tokens",
            "max_output_tokens",
        ] {
            assert!(
                body["oicp"].get(field).is_none(),
                "a pinned Slow request must carry no routing signal, leaked `{field}`"
            );
        }

        // Fast with model_id = None keeps the empty model + envelope
        // form so the daemon routes it to a fast-class slot.
        let fast = CompletionRequest::new("Hello").with_speed(Speed::Fast);
        let body = provider.build_request(&fast).unwrap();
        assert_eq!(body["model"], "");
        assert_eq!(body["oicp"]["latency_class"], "fast");
    }

    /// A request leaving this node for a peer must arrive with one forward
    /// spent. This is the regression guard for the A→B→C chain: before the
    /// budget existed the envelope crossed verbatim, so B re-ran its own
    /// scheduler over A's already-forwarded request and could send it on.
    #[test]
    fn forwarding_spends_a_hop_and_says_so_explicitly() {
        use sovereign_contracts::oicp::{InferenceRequirements, ShardingPrivacy};
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // A locally-originated request: envelope present, budget unstated.
        let env = InferenceRequirements::new().with_sharding(ShardingPrivacy::MeshAllowed);
        assert!(env.forward_budget.is_none(), "fixture starts unstated");
        assert_eq!(env.effective_forward_budget(), 1, "unstated means one hop");

        let body = provider
            .build_request(&CompletionRequest::new("hi").with_oicp(env))
            .unwrap();

        // Explicit zero, not omission. The receiver must be able to tell
        // "you are the last hop" from "nobody told me".
        assert_eq!(
            body["oicp"]["forward_budget"], 0,
            "the wire must carry an explicit spent budget, got {}",
            body["oicp"]
        );
    }

    /// The synthesized-envelope branch is a forward too. It is a separate
    /// code path, and one un-decremented path is enough to reopen the chain.
    #[test]
    fn a_synthesized_envelope_also_spends_its_hop() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // Fast + no model_id is the branch that builds an envelope from
        // scratch (see `build_request_slow_pins_provider_model_...`).
        let body = provider
            .build_request(&CompletionRequest::new("hi").with_speed(Speed::Fast))
            .unwrap();

        assert_eq!(
            body["oicp"]["latency_class"], "fast",
            "still the synth branch"
        );
        assert_eq!(
            body["oicp"]["forward_budget"], 0,
            "a synthesized envelope must not hand out a fresh budget"
        );
    }

    /// An already-forwarded request must not gain a hop by being forwarded
    /// again — the budget saturates at zero rather than wrapping.
    #[test]
    fn a_spent_budget_cannot_go_below_zero() {
        use sovereign_contracts::oicp::{InferenceRequirements, ShardingPrivacy};
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let spent = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_forward_budget(0);
        assert!(!spent.may_forward());

        let body = provider
            .build_request(&CompletionRequest::new("hi").with_oicp(spent))
            .unwrap();
        assert_eq!(
            body["oicp"]["forward_budget"], 0,
            "saturating, not wrapping"
        );
    }

    /// The thin-client shape: an IDE or any OpenAI client pins `model` and
    /// knows nothing about OICP. That request still crosses a hop and must
    /// still spend one — the named path never reaches `offload_verdict`, so a
    /// missing budget here leaves it with no hop bound at all.
    ///
    /// And the envelope minted to carry the budget must stay invisible to
    /// routing, or it re-opens the 2026-07-23 fast-slot hijack by overriding
    /// the very model name it was sent to preserve.
    /// A body whose 500th byte lands mid-character must not panic. The
    /// old `&body[..body.len().min(500)]` did, and it sat on the path
    /// that reports a peer's refusal — the worst possible place for one.
    #[test]
    fn an_error_excerpt_is_bounded_and_never_splits_a_character() {
        assert_eq!(super::error_excerpt("short"), "short");

        // 'é' is two bytes, so 251 of them put a character boundary
        // problem exactly at byte index 500.
        let body = "é".repeat(251);
        let excerpt = super::error_excerpt(&body);
        assert!(excerpt.len() <= 500);
        assert!(body.starts_with(excerpt));
        assert_eq!(excerpt.chars().count(), 250);
    }

    #[test]
    fn a_named_envelope_less_request_spends_a_hop_without_gaining_routing_signal() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let request = CompletionRequest::new("complete this").with_model_id("qwen-122b");
        assert!(request.oicp.is_none(), "thin clients send no envelope");
        let body = provider.build_request(&request).unwrap();

        // The name survives the hop — no silent substitution.
        assert_eq!(body["model"], "qwen-122b");

        // ...and the hop is now counted.
        assert_eq!(
            body["oicp"]["forward_budget"], 0,
            "a named forward must spend its hop, got {}",
            body["oicp"]
        );

        // The four fields both `has_routing_signal` and the daemon's
        // Priority-1 gate key on must all be absent.
        for field in [
            "capability_hint",
            "latency_class",
            "context_tokens",
            "max_output_tokens",
        ] {
            assert!(
                body["oicp"].get(field).is_none(),
                "budget-only envelope leaked routing signal `{field}`: {}",
                body["oicp"]
            );
        }
    }

    #[test]
    fn build_request_with_system() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let request = CompletionRequest {
            admission: None,
            prompt: "Hi".to_string(),
            system_message: Some("You are helpful.".to_string()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(100),
            temperature: Some(0.5),
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
        };

        let body = provider.build_request(&request).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.5);
    }

    #[test]
    fn build_request_with_oicp() {
        use sovereign_contracts::oicp::{CapabilityHint, InferenceRequirements, LatencyClass};

        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let request = CompletionRequest {
            admission: None,
            prompt: "Review this code".to_string(),
            system_message: None,
            preferred_speed: Speed::Slow,
            max_tokens: None,
            temperature: None,
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: Some(
                InferenceRequirements::new()
                    .with_hint(CapabilityHint::code())
                    .with_latency_class(LatencyClass::Normal)
                    .with_context_tokens(8192),
            ),
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
        };

        let body = provider.build_request(&request).unwrap();
        assert!(body.get("oicp").is_some());
        // v0.3: hint + latency class + sizing live at the top level
        // of the OICP envelope.
        assert_eq!(body["oicp"]["capability_hint"], "code");
        assert_eq!(body["oicp"]["latency_class"], "normal");
        assert_eq!(body["oicp"]["context_tokens"].as_u64(), Some(8192));
    }

    #[test]
    fn auth_header_present() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            Some("test-key-not-real".to_string()),
            "model",
            4096,
        );
        assert_eq!(
            provider.auth_header(),
            Some("Bearer test-key-not-real".to_string())
        );
    }

    #[test]
    fn auth_header_absent() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "model", 4096);
        assert_eq!(provider.auth_header(), None);
    }

    #[tokio::test]
    async fn warmup_url_strips_v1_suffix() {
        let provider = RemoteApiProvider::new("http://localhost:9741/v1", None, "model", 4096);
        assert_eq!(
            provider.warmup_url().await.unwrap(),
            "http://localhost:9741/internal/inference/warmup",
        );
    }

    #[tokio::test]
    async fn warmup_url_preserves_bare_host() {
        let provider = RemoteApiProvider::new("http://peer:9741", None, "mesh-peer", 32_768);
        assert_eq!(
            provider.warmup_url().await.unwrap(),
            "http://peer:9741/internal/inference/warmup",
        );
    }
}

// The attach-mode wire tests live in a sibling file: keeping them here put
// this file at its arch-gate ceiling (ARCH §3.1).
#[cfg(test)]
#[path = "attach_conformance_tests.rs"]
mod attach_conformance_tests;
