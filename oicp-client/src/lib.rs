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
const SHED_MAX_ATTEMPTS: u32 = 3;

/// Total time this client will spend WAITING on sheds for one logical call.
///
/// A cap rather than an unbounded honour of the hint: a host predicting a
/// two-minute wait should hand control back to the caller, which can decide
/// to route elsewhere, rather than have its client block silently.
const SHED_TOTAL_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(90);

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
fn shed_retry_after(status: reqwest::StatusCode, body: &str) -> Option<std::time::Duration> {
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
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
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
    /// This node's id, lowercase hex, stamped as `X-Node-Id` on every
    /// outbound request when set. `None` = the request presents as
    /// LOCAL traffic to the receiving daemon.
    ///
    /// This field is the whole of M5 piece 3, and it is a policy
    /// control, not plumbing: `commonwealth-api`'s admission layer
    /// gates exclusively on the presence of this header
    /// (`admission.rs:125`). Absent, a peer's chat completion is
    /// admitted as if the user themselves had typed it — bypassing
    /// the operator's pause, the foreground yield, and the
    /// `max_peer_inflight` ceiling (default 1). Present, all three
    /// arm.
    ///
    /// Opt-in for the same reason `model_id_is_placeholder` is: this
    /// provider also serves OpenAI, Ollama and bench endpoints, none
    /// of which are mesh peers and none of which should be told a
    /// node identity. Only the mesh routing layer knows it is talking
    /// to a peer — `peer_inference.rs::provider_for_peer` is the one
    /// caller that sets it.
    node_id: Option<String>,
    context_size: u32,
    /// Query-side instruction prefix for this model, resolved once at
    /// construction from the bundled manifest (empty for chat / non-embedding
    /// models). The embedded engine applies this in `embed_query_sync`; the
    /// remote `/embeddings` API has no query/document distinction, so the
    /// client prepends it before sending — making the remote query-embedding
    /// path bit-identical to the embedded one. See
    /// `ModelsManifest::embed_query_instruction`.
    query_instruction: String,
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
        let url = format!("{}/embeddings", self.endpoint);
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": texts,
        });

        // Deliberately NOT routed through `send_honouring_shed`: this site's
        // refusal is a CAPABILITY verdict, not backpressure, and it returns
        // `NotImplemented` so a caller can fall back to per-item embedding.
        // Flattening that into `Inference` would be the same collapse this
        // client already refuses to make on a 503 body (§18.3).
        let req = self.stamped(self.client.post(&url).json(&body));

        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Batch embedding request failed: {e}")))?;

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

        Self {
            // Off: see the field docs — a peer shed is a routing signal, not a wait.
            wait_out_sheds: false,
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key,
            model_id: model_id.to_string(),
            model_id_is_placeholder: false,
            node_id: None,
            context_size,
            // The embed query-instruction prefix is model-family knowledge
            // that this pure HTTP client no longer computes. Callers that
            // need it (the embed slot of `SplitInferenceProvider`) set it via
            // `with_query_instruction`; chat providers and document-embed
            // (`embed`, which ignores the prefix) leave it empty.
            query_instruction: String::new(),
        }
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
        Self {
            // Off: see the field docs — a peer shed is a routing signal, not a wait.
            wait_out_sheds: false,
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key: Some(bearer),
            model_id: model_id.to_string(),
            model_id_is_placeholder: false,
            node_id: None,
            context_size,
            // The embed query-instruction prefix is model-family knowledge
            // that this pure HTTP client no longer computes. Callers that
            // need it (the embed slot of `SplitInferenceProvider`) set it via
            // `with_query_instruction`; chat providers and document-embed
            // (`embed`, which ignores the prefix) leave it empty.
            query_instruction: String::new(),
        }
    }

    /// Identify this node to the remote as a MESH PEER, by stamping
    /// `X-Node-Id: <hex>` on everything this provider sends.
    ///
    /// Call this only when the remote is a Commonwealth daemon and
    /// the traffic really is peer traffic. It changes how the far
    /// side treats the request: peer-tagged inference is subject to
    /// the operator's pause, the foreground yield, and the
    /// `max_peer_inflight` ceiling, any of which can answer `503` +
    /// `Retry-After` in ~10 ms instead of serving. That refusal is
    /// the point — see `MESH_N4_TOPOLOGY.md` §M5 — but it means an
    /// unconsidered call here turns served requests into shed ones.
    ///
    /// The hex encoding is what `commonwealth-api`'s
    /// `parse_x_node_id` expects; an unparseable value is not
    /// ignored, it buckets under the zero node and is still gated.
    /// Declare this endpoint the LAST RESORT, so a shed is waited out rather
    /// than reported. See [`Self::wait_out_sheds`] — do not set this on a peer.
    pub fn waiting_out_sheds(mut self) -> Self {
        self.wait_out_sheds = true;
        self
    }

    pub fn with_node_id(mut self, node_id_hex: impl Into<String>) -> Self {
        self.node_id = Some(node_id_hex.into());
        self
    }

    /// Apply the headers EVERY outbound request from this provider
    /// carries: bearer auth, and the mesh identity when this provider
    /// was built for a peer.
    ///
    /// One body, deliberately, because a new outbound method that
    /// forgets the stamp fails SILENTLY and in the safe-looking
    /// direction: the request still succeeds, it is simply admitted
    /// on the far side as though the peer's user had typed it — no
    /// pause, no yield, no ceiling. Nothing in a test or a log would
    /// distinguish that from correct behaviour, so the invariant is
    /// made structural rather than remembered (ARCH §7). Seven call
    /// sites hand-maintained the auth half before this existed.
    /// Send, and come back when the host asks us to.
    ///
    /// THE ONE place this client waits out backpressure (ARCH §10.6). `build`
    /// re-creates the request per attempt rather than cloning, so a body
    /// stream cannot be consumed by a failed try.
    ///
    /// Returns the FIRST success, or the last refusal. A refusal that is not a
    /// shed returns immediately and untouched — see [`shed_retry_after`].
    async fn send_honouring_shed<F>(
        &self,
        build: F,
        what: &'static str,
    ) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut waited = std::time::Duration::ZERO;
        let mut attempt = 0u32;
        loop {
            let response = build()
                .send()
                .await
                .map_err(|e| Error::Inference(format!("{what} failed: {e}")))?;
            if response.status().is_success() {
                return Ok(response);
            }
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            attempt += 1;

            let shed = if self.wait_out_sheds {
                shed_retry_after(status, &body)
            } else {
                // Not our shed to wait out: report it so the caller can route
                // elsewhere. Peers depend on this — see `wait_out_sheds`.
                None
            };
            let Some(delay) = shed else {
                // Not backpressure — a real failure. Surface it as it arrived.
                return Err(Error::Inference(format!(
                    "{what} returned {status}: {}",
                    error_excerpt(&body)
                )));
            };
            if attempt >= SHED_MAX_ATTEMPTS || waited + delay > SHED_TOTAL_WAIT_CAP {
                // Out of budget. Report the shed AS a shed — the caller needs
                // to know this was "busy", not "broken", to decide whether to
                // route elsewhere (§18.3).
                return Err(Error::Inference(format!(
                    "{what} shed by the host after {attempt} attempt(s), \
                     {}s waited: {}",
                    waited.as_secs(),
                    error_excerpt(&body)
                )));
            }
            tracing::info!(
                target: "oicp_client",
                what,
                attempt,
                delay_ms = delay.as_millis() as u64,
                waited_ms = waited.as_millis() as u64,
                "shed — honouring the host's Retry-After and coming back"
            );
            tokio::time::sleep(delay).await;
            waited += delay;
        }
    }

    fn stamped(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req;
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        if let Some(ref id) = self.node_id {
            req = req.header("X-Node-Id", id);
        }
        req
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

    fn build_request(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut messages = Vec::new();

        if let Some(ref system) = request.system_message {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        messages.push(serde_json::json!({
            "role": "user",
            "content": &request.prompt,
        }));

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
        let oicp_val = if let Some(ref oicp) = request.oicp {
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

        // Forward the per-request `enable_thinking` toggle as
        // `chat_template_kwargs: { enable_thinking: <bool> }` —
        // the convention vLLM and llama-server both accept on
        // OpenAI-compatible endpoints. Without this the daemon
        // falls through to its hardcoded default
        // (`embedded.rs::apply_chat_template_oaicompat` historically
        // pinned `enable_thinking: false`). With it set explicitly
        // by the caller, the relational/witness path can flip
        // thinking ON so the chat template wraps the model's
        // planning trace in `<think>...</think>` — and the
        // post-process `strip_think_blocks` (eval-side runner +
        // production runtime where wired) can drop it cleanly.
        // Daemon-side unwrap: `inference_adapter::extract_enable_thinking`.
        if let Some(enable) = request.enable_thinking {
            body["chat_template_kwargs"] = serde_json::json!({
                "enable_thinking": enable,
            });
        }

        // Forward `think_budget` as the Commonwealth extension field the
        // daemon's `resolve_think_budget` reads. Without this the
        // runtime's `think_budget: Some(0)` (FastFocused synthesis, gap
        // check, router — every "don't think, just answer" call) dies at
        // the HTTP boundary and the engine-side thinking suppression in
        // `format_prompt` never engages: the chat template pre-opens
        // `<think>` and the model spends its whole `max_tokens` budget
        // on CoT (2026-06-10 fabrication burn-down — chaos honesty 0.45,
        // every fast-slot KQ answer was truncated raw deliberation).
        if let Some(tb) = request.think_budget {
            body["think_budget"] = serde_json::json!(tb);
        }

        // Forward `structured_output` to the daemon as the OpenAI
        // `response_format: {type: "json_schema", json_schema: {...}}`
        // envelope. Without this, the schema is dropped at the HTTP
        // boundary and the daemon's grammar-constraint layer never
        // sees it (silent fallback to free-form sampling). The daemon
        // unwraps it back into `request.structured_output` via
        // `inference_adapter::extract_response_format_schema`.
        if let Some(schema) = &request.structured_output {
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "structured",
                    "schema": schema,
                },
            });
        }

        // Forward `lark_grammar` to the daemon as a sovereign-specific
        // extension field. The daemon's inference_adapter unwraps it
        // back onto CompletionRequest.lark_grammar; embedded.rs then
        // compiles it via llguidance and constrains decoding.
        //
        // Why this exists separately from `response_format`: lark
        // grammars are strictly more expressive than JSON-Schema
        // (regex tokens, recursion, custom productions) and the
        // OpenAI envelope has no slot for free-form lark. The
        // structured_output → response_format path remains the
        // canonical route for schema-shaped output; this is the
        // escape hatch for non-schema constraints like
        // `(entity ("," entity)*)?` per-line lists or strict
        // BREAK/CONTINUE alternations.
        //
        // The wire field name `lark_grammar` mirrors the
        // CompletionRequest field exactly — chosen for symmetry
        // with the in-process path so daemon-side debugging
        // doesn't need a translation table.
        if let Some(grammar) = &request.lark_grammar {
            body["lark_grammar"] = serde_json::json!(grammar);
        }

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

        body
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| format!("Bearer {k}"))
    }

    /// The daemon root: `self.endpoint` with any `/v1` suffix stripped. Routes
    /// mounted at the daemon root (not under `/v1`) — warmup and the OICP
    /// capabilities manifest — resolve from here. Two endpoint shapes appear in
    /// the codebase: callers like `chat_cmd/bootstrap` pass
    /// `http://host:9741/v1`; peer-inference callers pass `http://peer:9741`.
    /// Both resolve to the same root here.
    fn daemon_root(&self) -> &str {
        self.endpoint.strip_suffix("/v1").unwrap_or(&self.endpoint)
    }

    fn warmup_url(&self) -> String {
        format!("{}/internal/inference/warmup", self.daemon_root())
    }

    /// Fetch the OICP capabilities manifest from a provider.
    /// Returns None if the provider doesn't support OICP (404 or parse failure).
    pub async fn fetch_oicp_manifest(&self) -> Option<ProviderManifest> {
        // `/oicp/v1/capabilities` is mounted at the daemon root, NOT under
        // `/v1` (same as warmup) — strip a `/v1` endpoint suffix so a caller
        // holding the OpenAI `/v1` URL still reaches it. Without this, a
        // `/v1`-shaped endpoint hit `…/v1/oicp/v1/capabilities` → 404 → None.
        let url = format!("{}/oicp/v1/capabilities", self.daemon_root());

        let req = self.stamped(self.client.get(&url));

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
        let url = format!("{}/chat/completions", self.endpoint);
        let body = self.build_request(request);

        let response = self
            .send_honouring_shed(
                || self.stamped(self.client.post(&url).json(&body)),
                "Remote API request",
            )
            .await?;

        let chat_response: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse API response: {e}")))?;

        let first_choice = chat_response.choices.first();
        let text = first_choice
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
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
    /// `MeshInferenceProvider::complete_stream_with_id_and_finish`,
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
        let url = format!("{}/chat/completions", self.endpoint);
        let mut body = self.build_request(request);
        body["stream"] = serde_json::json!(true);

        let response = self
            .send_honouring_shed(
                || self.stamped(self.client.post(&url).json(&body)),
                "Remote typed stream request",
            )
            .await?;

        let byte_stream = response.bytes_stream();
        // Carry parser state across the byte-stream's filter_map by
        // streaming into a channel: parsing SSE line-by-line is
        // stateful (finish_reason + usage may land on any chunk
        // before [DONE]) and async-stream combinators can't carry
        // mutable state across yields cleanly. Channel-driven actor
        // keeps the parser straightforward.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut byte_stream = byte_stream;
            let mut buf = String::new();
            let mut finish_reason: Option<FinishReason> = None;
            let mut usage: Option<StreamUsage> = None;
            'outer: while let Some(chunk) = byte_stream.next().await {
                let Ok(bytes) = chunk else { continue };
                buf.push_str(&String::from_utf8_lossy(&bytes));
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
                            if !text.is_empty() && tx.send(StreamFrame::Token(text)).await.is_err()
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
            let _ = tx
                .send(StreamFrame::Finish {
                    reason: finish_reason.unwrap_or(FinishReason::Stop),
                    usage,
                })
                .await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let url = format!("{}/chat/completions", self.endpoint);
        let mut body = self.build_request(request);
        body["stream"] = serde_json::json!(true);

        let response = self
            .send_honouring_shed(
                || self.stamped(self.client.post(&url).json(&body)),
                "Remote stream request",
            )
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
        let url = format!("{}/embeddings", self.endpoint);
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": text,
        });

        let response = self
            .send_honouring_shed(
                || self.stamped(self.client.post(&url).json(&body)),
                "Embedding request",
            )
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
        let url = self.warmup_url();
        let req = self.stamped(self.client.post(&url).json(&serde_json::json!({})));
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
    embed: std::sync::Arc<RemoteApiProvider>,
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
    /// Absolute URL of the daemon's `/status`, derived once from the `/v1`
    /// endpoint. Sole consumer is [`Self::primary_slot_status`]: this
    /// provider holds no weights, so the only honest answer to "is the
    /// deep slot cold?" comes from the node that does.
    status_url: String,
}

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
            RemoteApiProvider::new(endpoint_v1, None, &chat_model_id, context_size)
                .waiting_out_sheds(),
        );
        // The embed slot carries the query-instruction prefix so
        // `embed_query` stays bit-identical to the embedded engine. The chat
        // slot never embeds, so it leaves the prefix empty.
        let embed = std::sync::Arc::new(
            RemoteApiProvider::new(endpoint_v1, None, &embed_model_id, context_size)
                .with_query_instruction(embed_query_instruction)
                .waiting_out_sheds(),
        );
        Self {
            chat,
            embed,
            chat_model_id,
            embed_model_id,
            context_size,
            // `endpoint_v1` is the OpenAI-shaped base (".../v1"); `/status`
            // is its sibling, not a child. Trim exactly one trailing "/v1"
            // (and any trailing slash) rather than string-replacing, so a
            // host whose path legitimately contains "v1" elsewhere survives.
            status_url: format!(
                "{}/status",
                endpoint_v1
                    .trim_end_matches('/')
                    .trim_end_matches("/v1")
                    .trim_end_matches('/')
            ),
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
        Self::new(
            endpoint_v1,
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
        self.chat.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.chat.complete_stream(request).await
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
        self.chat.complete_stream_with_finish(request).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.chat.complete_batch(requests).await
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

    fn model_id_for(&self, _speed: Speed) -> String {
        // Only one chat slot over HTTP; the daemon's own engine maps the
        // request (Speed / max_tokens) to its loaded fast/primary slots.
        // Reporting the request model is the most honest client-side signal.
        self.chat_model_id.clone()
    }

    fn embed_model_id(&self) -> String {
        self.embed_model_id.clone()
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

        let resp = reqwest::Client::new()
            .get(&self.status_url)
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
        let body = provider.build_request(&request);

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
        let body = provider.build_request(&request);
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
        let body = provider.build_request(&fast);
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

        let body = provider.build_request(&CompletionRequest::new("hi").with_oicp(env));

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
        let body = provider.build_request(&CompletionRequest::new("hi").with_speed(Speed::Fast));

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

        let body = provider.build_request(&CompletionRequest::new("hi").with_oicp(spent));
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
        let body = provider.build_request(&request);

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

        let body = provider.build_request(&request);
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

        let body = provider.build_request(&request);
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

    #[test]
    fn warmup_url_strips_v1_suffix() {
        let provider = RemoteApiProvider::new("http://localhost:9741/v1", None, "model", 4096);
        assert_eq!(
            provider.warmup_url(),
            "http://localhost:9741/internal/inference/warmup",
        );
    }

    #[test]
    fn warmup_url_preserves_bare_host() {
        let provider = RemoteApiProvider::new("http://peer:9741", None, "mesh-peer", 32_768);
        assert_eq!(
            provider.warmup_url(),
            "http://peer:9741/internal/inference/warmup",
        );
    }

    // ═══════════════════════════════════════════════════════════════
    // ATTACH-MODE CONFORMANCE
    //
    // `SplitInferenceProvider` is the provider the SHIPPED desktop
    // runs: the supervisor spawns a child daemon and the app rewrites
    // its own boot mode to Attach, so this wrapper — not the embedded
    // engine — serves virtually every real user.
    //
    // The recurring defect on it is never a wrong implementation. It is
    // a MISSING one. `InferenceProvider` defaults 21 of its 25 methods,
    // and 16 of those defaults return `Ok(())` / `None` / `vec![]` /
    // `"unknown"` — plausible answers that actually mean "I don't
    // know". Inherit one by accident and the wrapper reports success
    // while doing nothing, forwarding to no one, with the correct
    // implementation sitting one field away on `self.chat`. Rust emits
    // no diagnostic: the code compiles exactly as written. Three
    // shipped bugs came from this — `primary_slot_status`,
    // `warmup_primary`, `complete_stream_with_finish`.
    //
    // So these assert WIRE BEHAVIOUR, which is the only thing a silent
    // no-op cannot fake: a method that falls through to the default
    // never opens a socket, and a synthesised terminal frame never
    // carries the wire's own values. Asserting the URL-building helper
    // instead is what let `warmup_primary` ship broken — that test
    // passed on the one sound link of a three-link chain.
    // ═══════════════════════════════════════════════════════════════

    /// A one-shot loopback daemon: serves `responses` in order, then
    /// hands back every request line it saw.
    ///
    /// Raw TCP on purpose — `oicp-client` is a contract crate with no
    /// dev-dependencies, and pulling an HTTP mock framework in to
    /// exercise four verbs is a worse trade than this much `std`.
    struct MockDaemon {
        port: u16,
        server: std::thread::JoinHandle<Vec<String>>,
    }

    impl MockDaemon {
        fn serving(responses: Vec<String>) -> Self {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let mut seen = Vec::new();
                for body in responses {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let mut buf = [0u8; 8192];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    // The WHOLE head, not just the request line —
                    // `request_lines` narrows it back down. Headers
                    // are behaviour too: `X-Node-Id` decides whether
                    // the receiving daemon treats a request as peer
                    // traffic, and a request line cannot show it.
                    seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
                    let _ = stream.write_all(body.as_bytes());
                    let _ = stream.flush();
                }
                seen
            });
            Self { port, server }
        }

        /// The Attach-mode provider, pointed at this mock.
        fn attach_provider(&self) -> SplitInferenceProvider {
            SplitInferenceProvider::new(
                &format!("http://127.0.0.1:{}/v1", self.port),
                "chat-model".to_string(),
                "embed-model".to_string(),
                8192,
                String::new(),
            )
        }

        /// Request lines observed, in order. Consumes the mock.
        fn request_lines(self) -> Vec<String> {
            self.request_heads()
                .into_iter()
                .map(|head| head.lines().next().unwrap_or("").to_string())
                .collect()
        }

        /// Full request heads (request line + headers), in order.
        /// Consumes the mock.
        fn request_heads(self) -> Vec<String> {
            self.server.join().unwrap()
        }

        /// A plain `RemoteApiProvider` pointed at this mock.
        fn provider(&self) -> RemoteApiProvider {
            RemoteApiProvider::new(
                &format!("http://127.0.0.1:{}/v1", self.port),
                None,
                "chat-model",
                8192,
            )
        }
    }

    fn http_ok(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// What the daemon emits when its slot queue sheds — the shape
    /// `commonwealth-api::admission::shed_response` renders, header and
    /// all. `shed_retry_after` reads `retry_after_secs` out of the BODY;
    /// the header is here because the real response carries it and a
    /// fixture that drops it would let a body-only parser pass on a
    /// response no daemon sends.
    fn http_shed(retry_after_secs: u64) -> String {
        let body = format!(
            r#"{{"error":"host busy","reason":"local_queue_full","retry_after_secs":{retry_after_secs}}}"#
        );
        format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
             Retry-After: {retry_after_secs}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// What a real daemon emits for a TRUNCATED generation: two tokens,
    /// `finish_reason: "length"`, and a usage record. Truncation is the
    /// case that matters — a synthesised terminal frame reports it as a
    /// clean stop, and the caller cannot tell the difference.
    fn sse_truncated_generation() -> String {
        [
            r#"data: {"choices":[{"delta":{"content":"held "},"finish_reason":null}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"token"},"finish_reason":"length"}],"usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}}"#,
            "",
            "data: [DONE]",
            "",
        ]
        .join("\n")
    }

    fn a_request() -> CompletionRequest {
        let mut request = CompletionRequest::default();
        request.prompt = "anything".into();
        request
    }

    #[tokio::test]
    async fn attach_warmup_reaches_the_daemon() {
        let daemon = MockDaemon::serving(vec![http_ok("application/json", r#"{"latency_ms":0}"#)]);

        daemon.attach_provider().warmup_primary().await.unwrap();

        assert_eq!(
            daemon.request_lines(),
            vec!["POST /internal/inference/warmup HTTP/1.1"],
            "the trait default is a silent Ok(()) — the ONLY proof of a real \
             warm-up is that a request left the process"
        );
    }

    #[tokio::test]
    async fn attach_slot_status_asks_the_node_that_owns_the_weights() {
        let body = r#"{"inference":{"resident":[
            {"role":"fast","model_id":"fast-4b","resident":true},
            {"role":"primary","model_id":"deep-35b","resident":false,
             "size_bytes":18525200896,"transitioning":false}]}}"#;
        let daemon = MockDaemon::serving(vec![http_ok("application/json", body)]);

        let slot = daemon
            .attach_provider()
            .primary_slot_status()
            .await
            .expect("this provider owns no weights, so it must ASK — the default reads its own empty resident_slots() and answers None forever");

        assert_eq!(slot.model_id, "deep-35b");
        assert!(
            !slot.resident,
            "a COLD primary is the whole point of the call; the cold row must \
             survive the round-trip verbatim"
        );
        assert_eq!(slot.size_bytes, Some(18_525_200_896));
        assert_eq!(daemon.request_lines(), vec!["GET /status HTTP/1.1"]);
    }

    #[tokio::test]
    async fn attach_streaming_reports_the_wires_finish_reason_not_a_synthesised_stop() {
        let daemon = MockDaemon::serving(vec![http_ok(
            "text/event-stream",
            &sse_truncated_generation(),
        )]);
        let provider = daemon.attach_provider();

        let mut stream = provider
            .complete_stream_with_finish(&a_request())
            .await
            .unwrap();
        let (mut text, mut finish) = (String::new(), None);
        while let Some(frame) = stream.next().await {
            match frame {
                StreamFrame::Token(t) => text.push_str(&t),
                StreamFrame::Finish { reason, usage } => finish = Some((reason, usage)),
                StreamFrame::Error(e) => panic!("unexpected stream error: {e}"),
            }
        }

        assert_eq!(text, "held token");
        let (reason, usage) = finish.expect("every stream must end with a terminal frame");
        assert_eq!(
            reason,
            FinishReason::Length,
            "the trait default appends Finish{{Stop}} it never observed, making a \
             max_tokens truncation indistinguishable from a clean finish"
        );
        assert_eq!(
            usage.map(|u| u.total_tokens),
            Some(9),
            "the default drops usage entirely, so token accounting reads None"
        );
    }

    #[tokio::test]
    async fn attach_streaming_with_id_names_the_model_and_keeps_the_finish_reason() {
        // The composed default (`complete_stream_with_id_and_finish`)
        // is only as honest as the two methods it calls. It needs no
        // forward of its own — but that is a CONSEQUENCE of the other
        // two being right, so it is pinned rather than assumed.
        let daemon = MockDaemon::serving(vec![http_ok(
            "text/event-stream",
            &sse_truncated_generation(),
        )]);
        let provider = daemon.attach_provider();

        let (mut stream, model_id) = provider
            .complete_stream_with_id_and_finish(&a_request())
            .await
            .unwrap();
        assert_eq!(
            model_id, "chat-model",
            "\"unknown\" here poisons the provenance of every streamed response"
        );

        let mut finish = None;
        while let Some(frame) = stream.next().await {
            if let StreamFrame::Finish { reason, .. } = frame {
                finish = Some(reason);
            }
        }
        assert_eq!(finish, Some(FinishReason::Length));
    }

    /// The methods answered from the provider's own fields. No daemon:
    /// reaching the network here would itself be the bug.
    #[test]
    fn attach_answers_from_its_own_state_without_the_unknown_sentinels() {
        let provider = SplitInferenceProvider::new(
            "http://127.0.0.1:1/v1",
            "chat-model".to_string(),
            "embed-model".to_string(),
            8192,
            String::new(),
        );

        assert_eq!(provider.model_id_for(Speed::Slow), "chat-model");
        assert_eq!(provider.model_id_for(Speed::Fast), "chat-model");
        assert_eq!(
            provider.embed_model_id(),
            "embed-model",
            "\"unknown\" is the documented 'cannot verify' sentinel — returning it \
             here would silently disable the persisted-embedding staleness guard"
        );
        assert_eq!(provider.effective_context_size(), Some(8192));
    }

    /// A LEDGER of the defaults this provider still inherits — not an
    /// endorsement of them. Each line is a known gap in Attach mode,
    /// recorded so it is countable instead of invisible.
    ///
    /// If you implement one of these, this test FAILS. That failure is
    /// the point: delete the line, and the gap is gone from the ledger
    /// too. A silent gap is what produced the three bugs above.
    #[tokio::test]
    async fn attach_remaining_gaps_are_recorded_not_forgotten() {
        let provider = SplitInferenceProvider::new(
            "http://127.0.0.1:1/v1",
            "chat-model".to_string(),
            "embed-model".to_string(),
            8192,
            String::new(),
        );

        // Heuristic, not the daemon's real BPE vocab: Attach and Local
        // therefore budget context differently for the same text.
        assert_eq!(provider.count_tokens("12345678"), 2);
        // The Settings "you can raise ctx to N" ceiling is absent.
        assert_eq!(provider.n_ctx_train_for_primary(), None);
        // Extras loaded on the daemon are invisible to the desktop.
        assert!(provider.extras_inventory().is_empty());
        // Honest here, unlike the others: this provider genuinely holds
        // no slots. It is `primary_slot_status` that must not be
        // derived from it — see the dedicated test above.
        assert!(provider.resident_slots().is_empty());
    }

    #[test]
    fn capabilities_url_resolves_at_daemon_root_for_both_endpoint_shapes() {
        // `/oicp/v1/capabilities` is mounted at the daemon root, so a `/v1`
        // endpoint (chat-bootstrap shape) must strip it — otherwise the fetch
        // hits `…/v1/oicp/v1/capabilities` (404) and manifest-driven context
        // silently falls back to the hardcoded default.
        let with_v1 = RemoteApiProvider::new("http://host:9741/v1", None, "m", 4096);
        let bare = RemoteApiProvider::new("http://host:9741", None, "m", 4096);
        assert_eq!(with_v1.daemon_root(), "http://host:9741");
        assert_eq!(bare.daemon_root(), "http://host:9741");
        assert_eq!(
            format!("{}/oicp/v1/capabilities", with_v1.daemon_root()),
            "http://host:9741/oicp/v1/capabilities"
        );
    }

    // ── M5 piece 3: the peer identity stamp ────────────────────
    //
    // Wire assertions, per this module's own doctrine above: the
    // failure being guarded is a header that is silently ABSENT, and
    // an absent header changes nothing a caller can observe. The
    // request still succeeds. It is simply admitted on the far side
    // as local traffic, bypassing the operator's pause, the
    // foreground yield and the `max_peer_inflight` ceiling. Only the
    // bytes on the socket can tell the two apart.

    #[tokio::test]
    async fn a_node_stamped_provider_identifies_itself_on_every_request() {
        let daemon = MockDaemon::serving(vec![http_ok(
            "application/json",
            r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
        )]);
        let provider = daemon.provider().with_node_id("00c0ffee");

        let _ = provider.complete(&a_request()).await;

        let head = daemon.request_heads().remove(0).to_ascii_lowercase();
        assert!(
            head.contains("x-node-id: 00c0ffee"),
            "the chat completion must carry the node id; head was:\n{head}"
        );
    }

    #[tokio::test]
    async fn an_unstamped_provider_sends_no_identity_at_all() {
        // The control, and the reason the test above is a gate: this
        // provider is what a bench, an Ollama user or an OpenAI
        // endpoint gets, and none of them are mesh peers. Stamping
        // unconditionally would tell every third-party endpoint a
        // node identity it has no business holding.
        let daemon = MockDaemon::serving(vec![http_ok(
            "application/json",
            r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
        )]);

        let _ = daemon.provider().complete(&a_request()).await;

        let head = daemon.request_heads().remove(0).to_ascii_lowercase();
        assert!(
            !head.contains("x-node-id"),
            "an unstamped provider must present as local traffic; head was:\n{head}"
        );
    }

    /// The stamp lives in ONE body (`stamped`) precisely so a new
    /// outbound method cannot quietly ship without it. This pins the
    /// streaming path, which is the one the product's chat actually
    /// uses — non-streaming passing tells you nothing about it.
    #[tokio::test]
    async fn the_streaming_path_is_stamped_too() {
        let daemon = MockDaemon::serving(vec![http_ok("text/event-stream", "data: [DONE]\n\n")]);
        let provider = daemon.provider().with_node_id("00c0ffee");

        let _ = provider.complete_stream(&a_request()).await;

        let head = daemon.request_heads().remove(0).to_ascii_lowercase();
        assert!(
            head.contains("x-node-id: 00c0ffee"),
            "the streaming completion must carry the node id; head was:\n{head}"
        );
    }

    /// The two halves of the shed policy, in one test, because they are one
    /// decision seen from two sides — and shipping the retry ON by default on
    /// 2026-08-26 proved that half of it alone is a mesh regression.
    ///
    /// A WIRE assertion for the same reason the stamp tests above are: a
    /// provider that quietly gave up and one that quietly waited both return
    /// an `Err` to the caller. Only the socket count separates them.
    ///
    /// Named failing input (ARCH §18.1): drop `.waiting_out_sheds()` from
    /// either slot in `SplitInferenceProvider::new` and half A fails with one
    /// request line instead of two; add it to `provider_for_peer` and half B
    /// hangs the mock's second accept, which is `chat_completion_e2e`'s
    /// `a_yielding_peer_is_asked_once_not_once_per_turn` restated at this
    /// layer.
    #[tokio::test]
    async fn the_daemon_backed_slot_waits_out_a_shed_and_a_bare_provider_reports_it() {
        // A. The last resort. Scripted shed-then-answer: the client only ever
        //    sees the answer if it comes back for it.
        let daemon = MockDaemon::serving(vec![
            http_shed(1),
            http_ok(
                "application/json",
                r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
            ),
        ]);
        let served = daemon.attach_provider().complete(&a_request()).await;
        assert!(
            served.is_ok(),
            "the daemon-backed slot owns no weights and has nowhere to route: a shed \
             is a wait, not a verdict — got {served:?}"
        );
        assert_eq!(
            daemon.request_lines().len(),
            2,
            "the wait has to reach the socket; one request line means the hint was \
             computed, transported and dropped (note `bf432b4d`)"
        );

        // B. The control, and the invariant the default protects. ONE response
        //    scripted, so a provider that re-dialled would block the mock's
        //    second `accept` — the failure is a hang, which is louder than a
        //    wrong count and is the point.
        let peer = MockDaemon::serving(vec![http_shed(1)]);
        let refused = peer.provider().complete(&a_request()).await;
        let err = refused.expect_err("a bare provider must surface the shed, not absorb it");
        assert!(
            err.to_string().contains("503"),
            "a peer shed is a ROUTING signal and must arrive intact so the cascade \
             tries elsewhere — got {err}"
        );
        assert_eq!(
            peer.request_lines().len(),
            1,
            "a peer is asked ONCE; re-dialling inside its own retry window is the \
             failed-hop tax MESH_SCALE §9.1.1 measures"
        );
    }
}
