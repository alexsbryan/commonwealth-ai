// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn's admission id on the chat wire (pb-svrn-dials-serve, phase-b-27).
//!
//! An admitted turn's model calls carry `CompletionRequest.admission`, which
//! lets the slot queue park them where it would shed fresh load. When the
//! engine is in another process (`serve`), the id has to cross the wire or
//! every call of an admitted turn arrives as fresh load. It crosses as the
//! `turn_admission` extension field, and only from a daemon-backed client
//! (`SplitInferenceProvider::new_with_bearer`'s chat half): a third-party
//! engine (`engine_factory`'s `RemoteApiProvider::new` and
//! `new_split_endpoints`) and a mesh peer (`provider_for_peer`) never receive
//! it. The receiving listener honours it only from a caller on its own host
//! (`sovereign_serving_host::turn_admission`).

use sovereign_contracts::types::CompletionRequest;

use crate::RemoteApiProvider;

impl RemoteApiProvider {
    /// Write the turn's admission id onto the chat wire. Only for a client
    /// whose far end is a Sovereign daemon or `serve`.
    pub(crate) fn carrying_turn_admission(mut self) -> Self {
        self.carries_turn_admission = true;
        self
    }

    pub(crate) fn write_turn_admission(
        &self,
        body: &mut serde_json::Value,
        request: &CompletionRequest,
    ) {
        let Some(admission) = request.admission.as_ref() else {
            return;
        };
        if !self.carries_turn_admission {
            tracing::debug!(target: "oicp_client", turn = %admission, "turn admission not sent: this client does not carry it");
            return;
        }
        tracing::debug!(target: "oicp_client", turn = %admission, "turn admission sent on the chat wire");
        body["turn_admission"] = serde_json::json!(admission.id());
    }
}
