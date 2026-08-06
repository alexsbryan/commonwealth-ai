// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client requirements schema (v0.3 §3): what a request needs from an
//! inference call.

use serde::{Deserialize, Serialize};

use crate::capability::{CapabilityHint, LatencyClass};
use crate::version::OICP_VERSION;

// -----------------------------------------------------------------
// Section 3 — Client Requirements Schema
// -----------------------------------------------------------------

/// What a client needs from an inference call (§3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequirements {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// §3.2: capability hint for specialization-aware routing.
    /// Absent → scheduler treats as `general` per §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hint: Option<CapabilityHint>,
    /// §3.2: latency class the request needs. Absent → scheduler
    /// treats as [`LatencyClass::Normal`] per §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_class: Option<LatencyClass>,
    /// §3.2: actual context length of the request. Used by the
    /// scheduler as a hard feasibility gate against each claim's
    /// `max_context` (§6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// §3.2: expected output length. Used by the scheduler as a
    /// hard feasibility gate against each claim's `max_output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,

    /// How many further mesh forwards this request may take.
    ///
    /// The envelope is forwarded across a hop essentially verbatim, so
    /// without this a receiving node re-runs its own scheduler over an
    /// already-forwarded request and may forward it again — A→B→C→…, with
    /// nothing bounding the chain. This field is the bound: the *sending*
    /// side decrements it (see [`Self::decremented_for_forward`]) and a
    /// node holding zero must serve the request itself or refuse.
    ///
    /// Absent means "not stated", which resolves to
    /// [`DEFAULT_FORWARD_BUDGET`] — one hop, matching the topology the
    /// mesh was always documented to have. Absent deliberately does NOT
    /// mean zero: every locally-originated request builds its envelope
    /// without setting this, and reading absence as "may not offload"
    /// would silently disable mesh routing entirely.
    ///
    /// **Mixed-version caveat.** A peer running a build without this
    /// field forwards `None`, which the receiver reads as a full budget.
    /// The bound therefore holds between updated nodes and degrades to
    /// today's unbounded behaviour when an old node is the forwarder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_budget: Option<u8>,
}

/// Mesh forwards permitted when the envelope does not say.
///
/// One. The originator may hand the request to exactly one peer, and that
/// peer serves it. Every operational doc already describes this topology
/// (`RUN_A_BIGGER_MODEL.md`, `RUN_GLM_5_2_ON_THE_MESH.md`: "one machine is
/// the host — the one you talk to"), and the desktop already enforces it
/// structurally by handing peers its raw provider. This makes the CLI
/// daemon agree.
pub const DEFAULT_FORWARD_BUDGET: u8 = 1;

impl Default for InferenceRequirements {
    fn default() -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            privacy: None,
            request_id: None,
            capability_hint: None,
            latency_class: None,
            context_tokens: None,
            max_output_tokens: None,
            // Absent, not zero: a fresh envelope is a locally-originated
            // request, which is entitled to the default budget.
            forward_budget: None,
        }
    }
}

impl InferenceRequirements {
    /// New empty requirements at the current OICP version.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set the sharding privacy. Allocates `privacy` if absent.
    pub fn with_sharding(mut self, sharding: ShardingPrivacy) -> Self {
        self.privacy = Some(PrivacyRequirements { sharding });
        self
    }

    /// Builder: set the request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Builder: set the capability hint.
    pub fn with_hint(mut self, hint: CapabilityHint) -> Self {
        self.capability_hint = Some(hint);
        self
    }

    /// Builder: set the latency class.
    pub fn with_latency_class(mut self, class: LatencyClass) -> Self {
        self.latency_class = Some(class);
        self
    }

    /// Builder: set the actual context length.
    pub fn with_context_tokens(mut self, tokens: u32) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// Builder: set the expected output length.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// §8: effective hint, defaulting to `general` when absent.
    pub fn effective_hint(&self) -> CapabilityHint {
        self.capability_hint
            .clone()
            .unwrap_or_else(CapabilityHint::general)
    }

    /// §8: effective latency class, defaulting to `Normal`.
    pub fn effective_latency_class(&self) -> LatencyClass {
        self.latency_class.unwrap_or(LatencyClass::Normal)
    }

    /// Effective sharding privacy, defaulting to `LocalOnly` per §3.1.
    pub fn sharding(&self) -> ShardingPrivacy {
        self.privacy
            .as_ref()
            .map(|p| p.sharding)
            .unwrap_or_default()
    }

    /// Builder: set the forward budget explicitly.
    pub fn with_forward_budget(mut self, hops: u8) -> Self {
        self.forward_budget = Some(hops);
        self
    }

    /// Forwards still permitted, resolving absence to
    /// [`DEFAULT_FORWARD_BUDGET`].
    pub fn effective_forward_budget(&self) -> u8 {
        self.forward_budget.unwrap_or(DEFAULT_FORWARD_BUDGET)
    }

    /// Whether this request may still be handed to a peer.
    pub fn may_forward(&self) -> bool {
        self.effective_forward_budget() > 0
    }

    /// This envelope as it should appear on the wire to a peer: one forward
    /// spent, written explicitly.
    ///
    /// **The single place a budget is spent.** Callers must not decrement by
    /// hand — an outbound path that forgets is indistinguishable at the
    /// receiver from a legitimate full budget, which is exactly the
    /// unbounded chain this field exists to stop.
    ///
    /// Writing the value explicitly (rather than leaving it absent when it
    /// happens to equal the default) is what makes the receiver's reading
    /// unambiguous: `Some(0)` says "you are the last hop", where `None`
    /// could only ever mean "nobody told me".
    pub fn decremented_for_forward(&self) -> Self {
        let mut next = self.clone();
        next.forward_budget = Some(self.effective_forward_budget().saturating_sub(1));
        next
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyRequirements {
    #[serde(default)]
    pub sharding: ShardingPrivacy,
}

/// Whether the provider may distribute inference across multiple
/// nodes (§3.1).
///
/// Default is `LocalOnly`. The spec calls this out explicitly:
/// "privacy is the default, not something the client has to
/// remember to request." Clients that want distributed inference
/// must opt in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardingPrivacy {
    #[default]
    LocalOnly,
    MeshAllowed,
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_default_is_local_only() {
        let req = InferenceRequirements::default();
        assert_eq!(req.oicp_version, OICP_VERSION);
        assert_eq!(req.sharding(), ShardingPrivacy::LocalOnly);
        assert_eq!(req.effective_hint(), CapabilityHint::general());
        assert_eq!(req.effective_latency_class(), LatencyClass::Normal);
    }

    #[test]
    fn requirements_builders_compose() {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Fast)
            .with_context_tokens(16_000)
            .with_max_output_tokens(2_000)
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_request_id("test-req");
        assert_eq!(req.effective_hint(), CapabilityHint::code());
        assert_eq!(req.effective_latency_class(), LatencyClass::Fast);
        assert_eq!(req.context_tokens, Some(16_000));
        assert_eq!(req.max_output_tokens, Some(2_000));
        assert_eq!(req.sharding(), ShardingPrivacy::MeshAllowed);
        assert_eq!(req.request_id.as_deref(), Some("test-req"));
    }

    #[test]
    fn requirements_round_trip_minimal() {
        let req = InferenceRequirements::new();
        let json = serde_json::to_string(&req).unwrap();
        let back: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back.oicp_version, OICP_VERSION);
        assert!(back.capability_hint.is_none());
        assert!(back.latency_class.is_none());
    }

    #[test]
    fn requirements_serialize_in_spec_shape() {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(8_000)
            .with_max_output_tokens(1_500)
            .with_sharding(ShardingPrivacy::MeshAllowed);
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["oicp_version"], OICP_VERSION);
        assert_eq!(value["capability_hint"], "code");
        assert_eq!(value["latency_class"], "normal");
        assert_eq!(value["context_tokens"], 8_000);
        assert_eq!(value["max_output_tokens"], 1_500);
        assert_eq!(value["privacy"]["sharding"], "mesh_allowed");
    }

    /// Absence must resolve to a usable budget, not to zero. Every envelope
    /// minted before this field existed omits it, and so does every fresh
    /// locally-originated one — reading absence as "may not forward" would
    /// disable mesh routing across the whole fleet on upgrade.
    #[test]
    fn an_unstated_budget_is_one_hop_not_zero() {
        let fresh = InferenceRequirements::new();
        assert!(fresh.forward_budget.is_none());
        assert_eq!(fresh.effective_forward_budget(), DEFAULT_FORWARD_BUDGET);
        assert_eq!(DEFAULT_FORWARD_BUDGET, 1);
        assert!(fresh.may_forward());
    }

    /// A peer on an older build sends no field at all. That must deserialize,
    /// and must read as a full budget rather than failing or blocking.
    #[test]
    fn an_envelope_from_a_build_without_the_field_still_loads() {
        let old = serde_json::json!({
            "oicp_version": "0.4",
            "privacy": { "sharding": "mesh_allowed" },
            "latency_class": "normal",
        });
        let env: InferenceRequirements = serde_json::from_value(old).unwrap();
        assert!(env.forward_budget.is_none(), "absent stays absent");
        assert!(env.may_forward(), "an old peer's request is still routable");
    }

    /// Spending is explicit and saturating: `Some(0)` on the wire says "you
    /// are the last hop", which omission could never say, and a spent budget
    /// cannot wrap back around to a large one.
    #[test]
    fn spending_a_hop_is_explicit_and_saturates() {
        let one = InferenceRequirements::new();
        let sent = one.decremented_for_forward();
        assert_eq!(sent.forward_budget, Some(0), "written, not omitted");
        assert!(!sent.may_forward());

        // The receiver forwarding again must not manufacture budget.
        let again = sent.decremented_for_forward();
        assert_eq!(again.forward_budget, Some(0), "saturating_sub, not wrapping");

        // A larger budget spends one at a time.
        let three = InferenceRequirements::new().with_forward_budget(3);
        assert_eq!(three.decremented_for_forward().forward_budget, Some(2));
    }

    /// Spending a hop must not disturb anything else in the envelope — the
    /// privacy contract in particular travels unchanged.
    #[test]
    fn spending_a_hop_preserves_the_rest_of_the_envelope() {
        let env = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_latency_class(LatencyClass::Extended)
            .with_request_id("req-42")
            .with_context_tokens(8_000);
        let sent = env.decremented_for_forward();

        assert_eq!(sent.sharding(), env.sharding());
        assert_eq!(sent.effective_latency_class(), env.effective_latency_class());
        assert_eq!(sent.request_id, env.request_id);
        assert_eq!(sent.context_tokens, env.context_tokens);
        assert_eq!(sent.oicp_version, env.oicp_version);
    }
}
