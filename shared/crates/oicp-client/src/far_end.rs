// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FarEnd`]: who answers a [`RemoteApiProvider`]'s requests, and the one
//! admission rule for a far end outside the estate.
//!
//! The release of a payload is the CLIENT's decision (an enrich run's
//! `--consent`, through `egress::verify`), declared on the OICP envelope as
//! `third_party_allowed`. Whether the endpoint is a third party is the
//! DAEMON's fact, fixed by whoever built the provider. The two meet here, at
//! the send, so no route (named, ranked, or a direct caller) can reach a
//! vendor without the declaration.

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::{InferenceRequirements, ShardingPrivacy};
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
    /// A party outside the estate: a hosted vendor. Only a request declaring
    /// `third_party_allowed` is sent; embeddings and rerank carry no envelope
    /// to declare it, so they never are. The envelope itself stays here.
    ThirdParty,
}

impl RemoteApiProvider {
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
        provider.far_end = FarEnd::ThirdParty;
        Ok(provider)
    }

    /// Who answers this provider's requests.
    pub fn far_end(&self) -> FarEnd {
        self.far_end
    }

    /// May `what` be sent? Every method that sends text calls this first.
    /// `oicp` is the request's envelope; `None` for a request that has none
    /// (embeddings, rerank) as well as for one that sent none.
    pub(crate) fn admit(&self, what: &str, oicp: Option<&InferenceRequirements>) -> Result<()> {
        if self.far_end != FarEnd::ThirdParty {
            return Ok(());
        }
        let declared = oicp.map(InferenceRequirements::sharding);
        let endpoint = self.endpoint.describe();
        if declared == Some(ShardingPrivacy::ThirdPartyAllowed) {
            tracing::debug!(
                target: "oicp_client",
                gate = "third_party_admission",
                %what,
                %endpoint,
                "far end: third party admitted, the envelope declares third_party_allowed"
            );
            return Ok(());
        }
        let declared = declared.map_or_else(
            || "no envelope".to_string(),
            |s| serde_json::to_value(s).map_or_else(|_| format!("{s:?}"), |v| v.to_string()),
        );
        tracing::debug!(
            target: "oicp_client",
            gate = "third_party_admission",
            %what,
            %endpoint,
            %declared,
            "far end: third party refused, nothing sent"
        );
        Err(Error::PermissionDenied(format!(
            "{what} to a third party ({endpoint}) refused before sending: only a request whose \
             OICP envelope declares `privacy.sharding = \"third_party_allowed\"` may go there, \
             and this one declares {declared}. The client releases its own payload (an enrich \
             run: `--consent <class>`). Embedding and rerank requests carry no envelope, so they \
             need a server on this machine (`[engine] embed_endpoint`)."
        )))
    }
}

impl SplitInferenceProvider {
    /// The pair behind `[engine] kind = "remote"`, and the one place an
    /// engine's far ends are decided. An endpoint on this machine is a server
    /// the operator runs here; any other is a third party, because an address
    /// cannot say whose machine it is and the safe reading of silence is the
    /// stranger. The locus follows the chat half, where a turn runs.
    ///
    /// Both halves wait out sheds: an engine is the only holder there is.
    pub fn engine(
        chat_endpoint_v1: &str,
        embed_endpoint_v1: &str,
        api_key: Option<String>,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        extra_params: Option<serde_json::Value>,
    ) -> Result<Self> {
        let half = |endpoint: &str, model: &str| -> Result<RemoteApiProvider> {
            let provider = if crate::endpoint_is_loopback(endpoint) {
                RemoteApiProvider::new(endpoint, api_key.clone(), model, context_size).originating()
            } else {
                RemoteApiProvider::third_party(endpoint, api_key.clone(), model, context_size)?
            };
            Ok(provider.waiting_out_sheds())
        };
        let chat = half(chat_endpoint_v1, &chat_model_id)?.with_extra_params(extra_params);
        let embed = half(embed_endpoint_v1, &embed_model_id)?;
        let locus = match chat.far_end() {
            FarEnd::ThirdParty => ServingLocus::ForwardsToThirdParty,
            FarEnd::Origin | FarEnd::Peer => ServingLocus::ForwardsOnBox,
        };
        tracing::info!(
            target: "oicp_client",
            chat = %chat_endpoint_v1,
            chat_far_end = ?chat.far_end(),
            embed = %embed_endpoint_v1,
            embed_far_end = ?embed.far_end(),
            ?locus,
            "engine pair built"
        );
        Ok(Self {
            chat: std::sync::Arc::new(chat),
            embed: std::sync::Arc::new(embed),
            chat_model_id,
            embed_model_id,
            context_size,
            locus,
            served: None,
        })
    }
}

#[cfg(test)]
#[path = "far_end_tests.rs"]
mod tests;
