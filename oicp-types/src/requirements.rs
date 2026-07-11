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
}

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
}
