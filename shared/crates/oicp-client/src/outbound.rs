// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one way out of a [`RemoteApiProvider`]: who answers it ([`FarEnd`]),
//! what a send carries ([`Payload`]), and the admission rule between them.
//!
//! A third party is a hosted vendor the operator configured as this node's
//! `[engine]`, and that configuration is the release: completions go to it.
//! Texts to embed or rerank never do, so a corpus is never embedded off the
//! machine. [`Admitted`] is the only request builder a provider has, and
//! [`RemoteApiProvider::outbound`] the only way to get one.
//!
//! The core (the first half of this file) is pure and table-tested; the shell
//! below it owns the HTTP client.

use std::fmt;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::ServingLocus;

use crate::{RemoteApiProvider, SplitInferenceProvider};

/// Who answers this provider's requests. It decides how the OICP envelope
/// crosses and whether a request may be sent at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FarEnd {
    /// A mesh peer, the default. A request is a forward and spends a hop
    /// (`InferenceRequirements::decremented_for_forward`).
    Peer,
    /// The caller's own daemon, or a server on this machine. The request
    /// originates here: the caller's envelope crosses verbatim, none is
    /// synthesized, no hop is spent.
    Origin,
    /// A party outside the estate: a hosted vendor the operator configured.
    /// Completions go to it, without our envelope; texts to embed never do.
    ThirdParty,
}

/// What one send carries, as far as admission cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    /// A completion.
    Completion,
    /// Texts to embed or rerank.
    Texts,
    /// No caller text at all: the manifest read, warmup.
    Probe,
}

/// Why a third party was sent nothing: it was asked to embed or rerank text,
/// which stays on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThirdPartyRefusal;

impl FarEnd {
    /// May `payload` be sent to this far end? Everything may, except texts to
    /// a third party.
    pub fn admit(self, payload: Payload) -> std::result::Result<(), ThirdPartyRefusal> {
        match (self, payload) {
            (FarEnd::ThirdParty, Payload::Texts) => Err(ThirdPartyRefusal),
            _ => Ok(()),
        }
    }

    /// D1: an `[engine]` endpoint on this machine is a server the operator
    /// runs here; any other is a third party, because an address cannot say
    /// whose machine it is and the safe reading of silence is the stranger.
    /// An address that does not parse reads as the stranger too.
    pub fn of_engine_endpoint(endpoint: &str) -> FarEnd {
        if crate::endpoint_is_loopback(endpoint) {
            FarEnd::Origin
        } else {
            FarEnd::ThirdParty
        }
    }
}

impl Payload {
    fn describe(self) -> &'static str {
        match self {
            Payload::Completion => "a completion",
            Payload::Texts => "texts to embed or rerank",
            Payload::Probe => "a probe",
        }
    }
}

impl fmt::Display for ThirdPartyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "text is never sent to a third party to be embedded or reranked; \
             embeddings need a model on this machine (`[engine] embed_path`)"
        )
    }
}

// ─── The shell ───────────────────────────────────────────────────────────

/// A provider's HTTP half: the client, what every request is stamped with,
/// and who it reaches. Private to this module, so the only requests a
/// provider can build are [`Admitted`]'s.
pub(crate) struct Outbound {
    client: reqwest::Client,
    bearer: Option<String>,
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
    /// Opt-in because this provider also serves OpenAI, Ollama and
    /// bench endpoints, none of which are mesh peers and none of which
    /// should be told a node identity. Only the mesh routing layer knows
    /// it is talking to a peer — `peer_inference.rs::provider_for_peer`
    /// is the one caller that sets it.
    node_id: Option<String>,
    /// Who answers: a peer (the default), the caller's own daemon
    /// ([`RemoteApiProvider::originating`]), or a third party
    /// ([`RemoteApiProvider::third_party`]).
    far_end: FarEnd,
}

impl Outbound {
    pub(crate) fn new(client: reqwest::Client, bearer: Option<String>) -> Self {
        Self {
            client,
            bearer,
            node_id: None,
            far_end: FarEnd::Peer,
        }
    }
}

/// Proof that this send was admitted, and the only way to build its request.
///
/// Every request carries the same headers, from one body, because a send
/// that forgets the node stamp fails SILENTLY and in the safe-looking
/// direction: it still succeeds, admitted on the far side as though the
/// peer's user had typed it (no pause, no yield, no ceiling).
pub(crate) struct Admitted<'a> {
    outbound: &'a Outbound,
}

impl Admitted<'_> {
    pub(crate) fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.stamped(self.outbound.client.post(url))
    }

    pub(crate) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.stamped(self.outbound.client.get(url))
    }

    fn stamped(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(auth) = self.outbound.auth_header() {
            req = req.header("Authorization", auth);
        }
        if let Some(ref id) = self.outbound.node_id {
            req = req.header("X-Node-Id", id);
        }
        req
    }
}

impl Outbound {
    fn auth_header(&self) -> Option<String> {
        self.bearer.as_ref().map(|k| format!("Bearer {k}"))
    }
}

impl RemoteApiProvider {
    /// Admit `payload` to this provider's far end, and hand back the builder
    /// its requests go out on. Decided once per logical call: a shed retry
    /// reuses the admission.
    pub(crate) fn outbound(&self, payload: Payload) -> Result<Admitted<'_>> {
        let far_end = self.outbound.far_end;
        let verdict = far_end.admit(payload);
        if far_end == FarEnd::ThirdParty || verdict.is_err() {
            let endpoint = self.endpoint.describe();
            let what = payload.describe();
            match verdict {
                Ok(()) => tracing::debug!(
                    target: "oicp_client",
                    gate = "third_party_admission",
                    %what,
                    %endpoint,
                    "far end: third party admitted"
                ),
                Err(refusal) => {
                    tracing::debug!(
                        target: "oicp_client",
                        gate = "third_party_admission",
                        %what,
                        %endpoint,
                        %refusal,
                        "far end: third party refused, nothing sent"
                    );
                    return Err(Error::PermissionDenied(format!(
                        "{what} to a third party ({endpoint}) refused before sending: {refusal}"
                    )));
                }
            }
        }
        Ok(Admitted {
            outbound: &self.outbound,
        })
    }

    /// A provider whose far end is a third party, on a client built at the
    /// egress boundary (`egress::model_client`, the F26 census's one
    /// construction site for clients that carry payloads off the estate).
    pub fn third_party(
        endpoint: &str,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
    ) -> Result<Self> {
        let client = sovereign_contracts::egress::model_client(crate::DEFAULT_TIMEOUT)
            .map_err(|e| Error::Inference(format!("egress client for {endpoint}: {e}")))?;
        let mut provider = Self::with_client(endpoint, client, api_key, model_id, context_size);
        provider.outbound.far_end = FarEnd::ThirdParty;
        Ok(provider)
    }

    /// Requests through this provider ORIGINATE here (an enrich run calling
    /// its own daemon, an engine on this machine), they are not forwards for a
    /// peer: the caller's OICP envelope crosses verbatim, no hop is spent and
    /// none is synthesized. A forward spends one, and an unstated budget is one
    /// hop, so a forwarded enrich call would reach the daemon unable to use a
    /// peer.
    pub fn originating(mut self) -> Self {
        self.outbound.far_end = FarEnd::Origin;
        self
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
    pub fn with_node_id(mut self, node_id_hex: impl Into<String>) -> Self {
        self.outbound.node_id = Some(node_id_hex.into());
        self
    }

    /// Who answers this provider's requests.
    pub fn far_end(&self) -> FarEnd {
        self.outbound.far_end
    }

    #[cfg(test)]
    pub(crate) fn auth_header(&self) -> Option<String> {
        self.outbound.auth_header()
    }
}

/// A hosted engine's own models: its vendor models as resident slots, so the
/// manifest, aliases and status see a node that serves them, and the model a
/// fast turn goes to.
pub(crate) struct Hosted {
    pub(crate) slots: Vec<sovereign_contracts::traits::ResidentSlot>,
    fast_model_id: Option<String>,
}

impl Hosted {
    fn new(chat_model_id: &str, fast_model_id: Option<String>, local_embed: Option<&str>) -> Self {
        let slot = |role: &str, model_id: &str| sovereign_contracts::traits::ResidentSlot {
            role: role.to_string(),
            model_id: model_id.to_string(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        };
        let mut slots = vec![slot("primary", chat_model_id)];
        slots.extend(fast_model_id.as_deref().map(|id| slot("fast", id)));
        slots.extend(local_embed.map(|id| slot("embed", id)));
        Self {
            slots,
            fast_model_id,
        }
    }

    pub(crate) fn model_id_for(
        &self,
        speed: sovereign_contracts::types::Speed,
        chat: &str,
    ) -> String {
        match (&self.fast_model_id, speed) {
            (Some(fast), sovereign_contracts::types::Speed::Fast) => fast.clone(),
            _ => chat.to_string(),
        }
    }

    /// The model id a slot alias names here: `primary`, `fast`, `embed` and
    /// their keys in `SLOT_ALIAS_POLICY` (`commonwealth/primary`, ...).
    /// `None` for anything that is not an alias of a slot this engine holds.
    pub(crate) fn alias_target(&self, name: &str) -> Option<&str> {
        self.slots
            .iter()
            .find(|s| {
                sovereign_contracts::venue::resolution_alias_keys(&s.role)
                    .iter()
                    .any(|k| k == name)
            })
            .map(|s| s.model_id.as_str())
    }

    pub(crate) fn primary(&self) -> Option<sovereign_contracts::traits::ResidentSlot> {
        self.slots.iter().find(|s| s.role == "primary").cloned()
    }
}

/// Where a remote engine's embeddings run.
pub enum EngineEmbed {
    /// An embedding server at this `/v1` base. A third party is never sent
    /// texts, so for a hosted engine this must be a server on this machine.
    Remote {
        /// The server's `/v1` base.
        endpoint_v1: String,
        /// The model it embeds with.
        model_id: String,
        /// The embed family's input preparation, when the server does none
        /// of its own (`[engine] embed_inputs = "client"`). `None` sends text
        /// as given: right for a Sovereign daemon, whose embed slot prepares.
        input_prep: Option<sovereign_contracts::embed_quirks::EmbedQuirks>,
    },
    /// A model in this process (`[engine] embed_path`): the small embedding
    /// GGUF, so a hosted engine embeds on this machine with no second server.
    Local {
        /// The loaded model.
        provider: std::sync::Arc<dyn sovereign_contracts::traits::InferenceProvider>,
        /// Its id, vouched for on persisted embeddings.
        model_id: String,
    },
}

impl SplitInferenceProvider {
    /// The pair behind `[engine] kind = "remote"`, and the one place an
    /// engine's far ends are decided ([`FarEnd::of_engine_endpoint`]). The
    /// locus follows the chat half, where a turn runs.
    ///
    /// Both remote halves wait out sheds: an engine is the only holder there is.
    pub fn engine(
        chat_endpoint_v1: &str,
        embed: EngineEmbed,
        api_key: Option<String>,
        chat_model_id: String,
        fast_model_id: Option<String>,
        context_size: u32,
        extra_params: Option<serde_json::Value>,
        structured_output: crate::StructuredOutputMode,
        grammar: sovereign_contracts::engine_config::GrammarSupport,
    ) -> Result<Self> {
        let half = |endpoint: &str, model: &str| -> Result<RemoteApiProvider> {
            let provider = match FarEnd::of_engine_endpoint(endpoint) {
                FarEnd::ThirdParty => {
                    RemoteApiProvider::third_party(endpoint, api_key.clone(), model, context_size)?
                }
                FarEnd::Origin | FarEnd::Peer => {
                    RemoteApiProvider::new(endpoint, api_key.clone(), model, context_size)
                        .originating()
                }
            };
            Ok(provider.waiting_out_sheds())
        };
        let chat = half(chat_endpoint_v1, &chat_model_id)?
            .with_extra_params(extra_params)
            .with_structured_output_mode(structured_output)
            .with_grammar(grammar);
        let local_embed = matches!(embed, EngineEmbed::Local { .. });
        let (embed, embed_model_id, embed_at): (
            std::sync::Arc<dyn sovereign_contracts::traits::InferenceProvider>,
            String,
            String,
        ) = match embed {
            EngineEmbed::Remote {
                endpoint_v1,
                model_id,
                input_prep,
            } => {
                let prepared_here = input_prep.is_some();
                let half = match input_prep {
                    Some(quirks) => half(&endpoint_v1, &model_id)?.with_input_prep(quirks),
                    None => half(&endpoint_v1, &model_id)?,
                };
                let at = format!(
                    "{endpoint_v1} ({:?}, inputs prepared {})",
                    half.far_end(),
                    if prepared_here {
                        "here"
                    } else {
                        "by the server"
                    }
                );
                (std::sync::Arc::new(half), model_id, at)
            }
            EngineEmbed::Local { provider, model_id } => {
                (provider, model_id, "this process".to_string())
            }
        };
        let locus = match chat.far_end() {
            FarEnd::ThirdParty => ServingLocus::ForwardsToThirdParty,
            FarEnd::Origin | FarEnd::Peer => ServingLocus::ForwardsOnBox,
        };
        tracing::info!(
            target: "oicp_client",
            chat = %chat_endpoint_v1,
            chat_far_end = ?chat.far_end(),
            fast = ?fast_model_id,
            ?structured_output,
            embed = %embed_at,
            %embed_model_id,
            ?locus,
            "engine pair built"
        );
        let hosted = Hosted::new(
            &chat_model_id,
            fast_model_id,
            local_embed.then_some(embed_model_id.as_str()),
        );
        Ok(Self {
            chat: std::sync::Arc::new(chat),
            embed,
            hosted: Some(hosted),
            chat_model_id,
            embed_model_id,
            context_size,
            locus,
            served: None,
        })
    }
}

impl SplitInferenceProvider {
    /// A turn that names no model, pinned to the hosted engine's model for
    /// its speed. A daemon picks a slot for an empty `model`; a vendor
    /// rejects one, and an unnamed fast turn goes out empty
    /// (`build_request_in`'s `model_field`).
    pub(crate) fn for_speed<'a>(
        &self,
        request: &'a sovereign_contracts::types::CompletionRequest,
    ) -> std::borrow::Cow<'a, sovereign_contracts::types::CompletionRequest> {
        match &self.hosted {
            Some(hosted) if request.model_id.is_none() => {
                let model = hosted.model_id_for(request.preferred_speed, &self.chat_model_id);
                tracing::debug!(
                    target: "oicp_client",
                    speed = ?request.preferred_speed,
                    %model,
                    "hosted engine: an unnamed turn pinned to the model for its speed"
                );
                let mut pinned = request.clone();
                pinned.model_id = Some(model);
                std::borrow::Cow::Owned(pinned)
            }
            Some(hosted) => match request
                .model_id
                .as_deref()
                .and_then(|m| hosted.alias_target(m))
            {
                // A slot role named by its alias (`primary`, `fast`, ...):
                // the remote serves model ids, not this node's role names.
                Some(model) => {
                    tracing::debug!(
                        target: "oicp_client",
                        alias = ?request.model_id,
                        %model,
                        "hosted engine: a slot alias resolved to the model id the remote serves"
                    );
                    let mut pinned = request.clone();
                    pinned.model_id = Some(model.to_string());
                    std::borrow::Cow::Owned(pinned)
                }
                None => std::borrow::Cow::Borrowed(request),
            },
            None => std::borrow::Cow::Borrowed(request),
        }
    }
}

#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
