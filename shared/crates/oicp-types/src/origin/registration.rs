// SPDX-License-Identifier: AGPL-3.0-or-later
//! The origin registration wire: what a program posts to its node's mesh
//! endpoint (`POST /v1/mesh/origins`) and what it is handed back. Spoken by
//! the endpoint (cw-rails) and by every program that registers an origin
//! with it (svrn, serve), which cannot see each other, so it lives in this
//! leaf beside `OriginKind` (pb-serve-distributes-standalone; moved from
//! `commonwealth_media::origins`, which re-exports it).

use serde::{Deserialize, Serialize};

use crate::capabilities::NodeCapabilities;

/// Who may reach a registered origin — decided once per connection on the
/// dialer's verified key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admit {
    /// Any dialer. For an origin that authenticates for itself: a guest door
    /// reading its bearer, the join route a non-member must reach to become
    /// one.
    Any,
    /// Members, with the verified identity; a member outside a non-empty list
    /// is refused. `[]` is every member.
    Members(Vec<String>),
    /// Members reach this origin; any other dialer is sent to the origin
    /// registered on the named ALPN (a guest door), or closed if none is.
    MembersElse(String),
    /// This node's own processes reach it on loopback, and no dialer ever
    /// does: it is listed, never advertised and never forwarded
    /// (`forward_for` answers no route). An execute origin a donor on this
    /// node finds through the listing (pb-work-donor).
    Local,
}

/// How a dial's bytes reach the origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// HTTP/1.1: every request head carries the verified identity and this
    /// registration's tie.
    #[default]
    Http,
    /// A byte splice, for a protocol that is not HTTP (a ggml rpc-server).
    /// It carries no identity, so admission is its only guard.
    Bytes,
}

/// The body of `POST /v1/mesh/origins`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginRegistration {
    /// The protocol members dial, e.g. `cwth/client/0`.
    pub alpn: String,
    /// For `cwth/http/0` only: the path prefixes this origin answers.
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// The loopback port the origin listens on.
    pub port: u16,
    /// Who may reach it.
    pub admit: Admit,
    /// How a dial's bytes reach it.
    #[serde(default)]
    pub framing: Framing,
    /// How long the claim holds unrenewed; the endpoint's default when absent.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// What this origin's program declares about the node. The endpoint
    /// merges it over the hardware it measures itself: a declared GPU's VRAM
    /// and `storage_remaining_bytes` adjust that measurement and are not
    /// gossiped as declared.
    #[serde(default)]
    pub claims: Option<NodeCapabilities>,
    /// Ring namespaces this program writes on its own behalf. No
    /// `roster.json` may narrow them while the endpoint runs.
    #[serde(default)]
    pub namespaces: Vec<String>,
}

/// What a successful registration hands back. The tie is shown here once and
/// never listed again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginClaim {
    /// The claim a renew or release names.
    pub claim_id: String,
    /// The secret the endpoint sends on every forward to this origin.
    pub tie: String,
    /// The slots (ALPNs or path prefixes) the claim holds.
    pub slots: Vec<String>,
    /// Seconds until the claim lapses unless renewed.
    pub expires_in_secs: u64,
}
