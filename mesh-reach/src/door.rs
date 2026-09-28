// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reach door's wire: `GET /v1/mesh/reach?peer=<name|id>&class=<class>` on
//! cw-rails' loopback API. commonwealth-rails answers it and `RailsTransport`
//! reads it — one definition, two speakers.
//!
//! The answer is what cw-rails' own `PeerTransport` yields for that peer and
//! class, best first: loopback iroh bridges, or IP-overlay addresses when the
//! class falls back to the overlay. A refusal is a non-2xx status with
//! `{"error": "<why>"}`, never an empty list served as a success.

use serde::{Deserialize, Serialize};

use crate::PeerEndpoint;

/// Where the door is mounted on cw-rails' loopback API.
pub const REACH_PATH: &str = "/v1/mesh/reach";

/// The door's query string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachQuery {
    /// A member name, or a node-id prefix of at least 4 hex characters (the
    /// full `NodeId::to_hex` is the exact one).
    pub peer: String,
    /// A `TrafficClass::as_str` name.
    pub class: String,
}

/// The door's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    /// The member's roster name.
    pub peer: String,
    /// Its full `NodeId::to_hex`.
    pub node_id: String,
    /// The class resolved, as `TrafficClass::as_str` names it.
    pub class: String,
    /// Best first, as cw-rails' transport ordered them. Never empty: no
    /// endpoint is a refusal.
    pub endpoints: Vec<PeerEndpoint>,
}
