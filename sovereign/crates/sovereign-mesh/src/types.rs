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
    /// Track W: this founder's own iroh reachability (relay-homed?,
    /// discoverable?, plus the self-heal watchdog's recovery history). `None`
    /// when iroh isn't running. Populated from the running daemon (local mode)
    /// or `/v1/mesh/status` (attach mode); drives the desktop's
    /// "Reachable / Reconnecting" indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_reachability: Option<crate::daemon::SelfReachability>,
}

/// A member of the mesh, as shown in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMember {
    pub name: String,
    pub node_id: String,
    pub is_self: bool,
    pub status: MemberStatus,
    /// Advertised total GPU VRAM (GB) summed across the member's GPUs — the
    /// planning input for `svrn mesh plan --from-mesh`.
    #[serde(default)]
    pub vram_gb: u32,
    /// Advertises itself as a shared-model anchor (an eligible tensor-split worker).
    #[serde(default)]
    pub can_anchor: bool,
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
    /// Stable hash of this member's advertised hardware
    /// (`sovereign_core::mesh_measurements::hardware_fingerprint`).
    ///
    /// Part of the measurement cache key: a measured throughput number is only
    /// valid on the hardware it was measured on, so a machine change has to
    /// break the key rather than quietly serve the old number. `None` for a
    /// peer running a daemon that predates this field, which `mesh plan` treats
    /// as "not measured" rather than substituting a placeholder — one shared
    /// default would collide every unidentified host into a single key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_fingerprint: Option<u64>,
    /// GPU compute backend as advertised (`cuda` | `rocm` | `metal` | `vulkan`).
    ///
    /// Displayed beside a measurement so the reader knows which stack produced
    /// it. The same silicon driven through a different backend runs at a
    /// materially different rate, so this is also folded into
    /// [`MeshMember::hw_fingerprint`] — it annotates *and* discriminates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
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
    /// Founder's iroh dial string — present when the invite carries a
    /// no-VPN connect path (either mesh kind). `encrypted` below says
    /// which join posture it implies; `None` ⇒ legacy IP/mDNS join.
    #[serde(default)]
    pub iroh_dial: Option<String>,
    /// True iff the invite is for an ENCRYPTED mesh (`iroh=` param —
    /// fail-closed key-dialed join). False with `iroh_dial` present
    /// means a plaintext mesh reachable over iroh (`dial=` param —
    /// prefer-iroh join, IP/mDNS fallback).
    #[serde(default)]
    pub encrypted: bool,
    /// Unix-seconds TTL after which the invite is rejected (display).
    #[serde(default)]
    pub expires_at: Option<u64>,
}
