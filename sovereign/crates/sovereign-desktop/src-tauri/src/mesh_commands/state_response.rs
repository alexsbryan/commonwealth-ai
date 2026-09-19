use serde::{Deserialize, Serialize};

use sovereign_contracts::daemon_wire::{MemberStatus, MeshMember, MeshStatus, MeshStatusSummary};

// ── Serializable wrappers for MeshState ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStateResponse {
    pub status: MeshStatus,
    pub members: Vec<MeshMember>,
    pub corpora: Vec<sovereign_contracts::daemon_wire::MeshCorpus>,
    pub contribution: Option<sovereign_contracts::daemon_wire::ContributionSummary>,
    /// Client-API bearer token for remote peers/clients, rendered on
    /// the invite screen beside `status.join_key`. `None` for a
    /// loopback-only (unshared) daemon. Populated from the running
    /// daemon (local mode) or `/v1/mesh/status` (attach mode).
    #[serde(default)]
    pub client_token: Option<String>,
}

impl MeshStateResponse {
    /// Build a `MeshStateResponse` from the client's read of the flat
    /// `/v1/mesh/status` answer (`MeshStatusSummary` — the fields this
    /// app reads, pinned to the route's `StatusResponse` by
    /// `sovereign-mesh`'s `wire_view_drift` test). The UI surface
    /// (members list, online counts) is covered; rich fields that
    /// weren't surfaced over HTTP (contribution ledger, corpora shard
    /// plan) come back empty — they're populated on the daemon side
    /// and a future iteration can extend the HTTP shape to include them.
    ///
    /// Member status strings are parsed by the enum's OWN serde repr
    /// (`MemberStatus` is `rename_all = "lowercase"` in
    /// `sovereign_contracts::daemon_wire`) — one decider for the string set. Until 2026-09-09 this
    /// was a hand match with `_ => Offline`, so a daemon newer than the
    /// desktop (a new status variant) silently rendered its members as
    /// offline instead of surfacing the unknown string — the §18.3
    /// substitution. The parse now refuses (sv-surface rung 4).
    pub fn from_remote_status(remote: MeshStatusSummary) -> Result<Self, String> {
        use serde::de::IntoDeserializer;
        let members: Vec<MeshMember> = remote
            .members
            .into_iter()
            .map(|m| {
                Ok(MeshMember {
                    name: m.name,
                    node_id: m.node_id,
                    is_self: m.is_self,
                    status: MemberStatus::deserialize(m.status.as_str().into_deserializer())
                        .map_err(|e: serde::de::value::Error| {
                            format!("mesh member status {:?}: {e}", m.status)
                        })?,
                    vram_gb: m.vram_gb,
                    can_anchor: m.can_anchor,
                    contribution_level: 0,
                    contribution_label: String::new(),
                    addresses: m.addresses,
                    origins: m.origins,
                    node_pubkey: m.node_pubkey,
                    active: m.active,
                    hw_fingerprint: m.hw_fingerprint,
                    backend: m.backend,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            status: MeshStatus {
                name: remote.mesh_name.unwrap_or_default(),
                members_online: remote.members_online,
                members_total: remote.members_total,
                model_name: None,
                knowledge_corpora: Vec::new(),
                is_connected: remote.running,
                join_link: remote.join_link,
                join_key: remote.join_key,
                self_reachability: remote.self_reachability,
            },
            members,
            corpora: Vec::new(),
            contribution: None,
            client_token: remote.client_token,
        })
    }
}
