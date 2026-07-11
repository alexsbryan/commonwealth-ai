// SPDX-License-Identifier: AGPL-3.0-or-later
//! Response metadata (v0.3 §5.2) echoed on completions.

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::manifest::ProviderModel;

// -----------------------------------------------------------------
// Section 5.2 — Response Metadata
// -----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OicpResponseMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_quality: Option<MatchQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// v0.4 §6: fingerprint of the concrete model that produced this
    /// response — the same token as the resolved model's
    /// [`ProviderModel::fingerprint`]. Lets a client key model-dependent
    /// caches correctly across a model swap. Gated by the
    /// `model_fingerprint` feature; absent on v0.3 hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    Full,
    Partial,
    Degraded,
    Unmatched,
}
