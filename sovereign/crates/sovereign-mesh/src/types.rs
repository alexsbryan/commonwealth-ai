// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI-friendly types for mesh status, member info, and contributions.

use serde::{Deserialize, Serialize};

/// Mesh status as shown in the UI sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatus {
    pub name: String,
    pub members_online: usize,
    pub members_total: usize,
    pub model_name: Option<String>,
    pub knowledge_corpora: Vec<String>,
    pub is_connected: bool,
    /// `sovereign://join/...` invite for the active mesh. `None` when
    /// the daemon resumed a mesh from before the cached-plaintext
    /// feature shipped — the UI hides the share card and offers
    /// "Rotate" to recover an inviteable link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_link: Option<String>,
    /// Bare `cwth-XXXX-XXXX-XXXX` — the link's payload, exposed so
    /// users can paste into chat clients that mangle deep-link URLs.
    /// Same `None` semantics as `join_link`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_key: Option<String>,
}

/// A member of the mesh, as shown in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMember {
    pub name: String,
    pub node_id: String,
    pub is_self: bool,
    pub status: MemberStatus,
    pub contribution_level: u8,     // 0-5 bar chart
    pub contribution_label: String, // "Top contributor", "Mostly uses, that's ok!"
    /// Tailnet (or other reachable) addresses for this member, as
    /// known to the local daemon's gossip view. Surfaced for
    /// operator use — see `sovereign mesh status` for the
    /// human-readable rendering and `--addr` for scripting. Empty
    /// when the member hasn't advertised a routable address yet
    /// (most often during a fresh join, before the first gossip
    /// round, or when the member crashed without graceful shutdown).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    Online,
    Busy,
    Away,
    Offline,
}

/// User-friendly contribution summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionSummary {
    pub compute_hours_contributed: f64,
    pub compute_hours_used: f64,
    pub storage_hosted_gb: f64,
    pub bandwidth_served_gb: f64,
    pub is_net_contributor: bool,
    pub summary_text: String, // "You're a net contributor. Thank you!"
}

/// Corpus available to add to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshCorpus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub article_count: String, // "6.8M articles"
    pub download_size: String, // "22 GB"
    pub status: CorpusStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusStatus {
    Available,
    Installing { percent: f32, node: String },
    Installed,
    SharedByPeer { peer_name: String },
}

/// Join confirmation info shown when a user taps a deep link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinConfirmation {
    pub mesh_name: String,
    pub invited_by: Option<String>,
    pub join_key: String,
    pub relay_hint: Option<String>,
    /// Founder's iroh dial string — present iff this is an ENCRYPTED
    /// mesh invite. Its presence tells the joiner (and the desktop
    /// preview) "this join will be encrypted, dialed by the founder's
    /// key." `None` ⇒ legacy/plaintext join.
    #[serde(default)]
    pub iroh_dial: Option<String>,
    /// Unix-seconds TTL after which the invite is rejected (display).
    #[serde(default)]
    pub expires_at: Option<u64>,
}
