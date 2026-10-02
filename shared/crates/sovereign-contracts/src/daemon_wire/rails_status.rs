// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails' `GET /v1/mesh/status`, read as svrn's [`MeshStatusSummary`].
//!
//! Since pb-mesh-exit-transport cw-rails is the node's one mesh endpoint and
//! its status is the one mesh view; svrn's own `/v1/mesh/status` answers 410
//! naming it. cw-rails speaks the roster in its own terms (the gossiped
//! record: `node_id` in display form, `node_id_hex`, whole `capabilities`),
//! and svrn's clients key on full hex and read the derived hardware columns.
//! [`RailsMeshStatus`] parses cw-rails' document and `From` is THE one
//! projection onto the svrn wire, moved whole from sovereign-mesh's
//! `MeshState::from_membership` when its last caller (the daemon's status
//! route) went.

use oicp_types::capabilities::{ComputeType, NodeCapabilities};
use serde::Deserialize;

use super::mesh::{KnownMeshDto, MemberDto, MeshStatusSummary, SelfReachability};

/// cw-rails' status document, the fields svrn's view reads.
#[derive(Debug, Deserialize)]
pub struct RailsMeshStatus {
    #[serde(default)]
    mesh: Option<RailsMeshName>,
    #[serde(default)]
    members: Vec<RailsMember>,
    /// `null` when cw-rails' store did not read (`meshes_absent` names why);
    /// the summary then lists none, as a daemon without persistence did.
    #[serde(default)]
    meshes: Option<Vec<KnownMeshDto>>,
    #[serde(default)]
    join_link: Option<String>,
    /// Kept raw: a watchdog snapshot this build cannot read is `None` in the
    /// summary, never a parse failure of the whole view.
    #[serde(default)]
    self_reachability: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RailsMeshName {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RailsMember {
    name: String,
    node_id_hex: String,
    /// `commonwealth_core::mesh::NodeStatus`, snake_case: the same four words
    /// [`super::mesh::MemberStatus`] spells.
    status: String,
    #[serde(default)]
    is_self: bool,
    #[serde(default)]
    capabilities: Option<NodeCapabilities>,
    #[serde(default)]
    node_pubkey: Option<String>,
}

impl From<RailsMeshStatus> for MeshStatusSummary {
    fn from(s: RailsMeshStatus) -> Self {
        let mut members: Vec<MemberDto> = s.members.into_iter().map(member).collect();
        members.sort_by(|a, b| b.is_self.cmp(&a.is_self)); // Self first
        let members_online = members
            .iter()
            .filter(|m| matches!(m.status.as_str(), "online" | "busy"))
            .count();
        let self_reachability = s
            .self_reachability
            .and_then(|v| serde_json::from_value::<SelfReachability>(v).ok());
        MeshStatusSummary {
            running: true,
            meshes: s.meshes.unwrap_or_default(),
            mesh_name: s.mesh.map(|m| m.name),
            members_online,
            members_total: members.len(),
            members,
            join_key: None,
            join_link: s.join_link,
            client_token: None,
            self_reachability,
            node_class: String::new(),
            entry_node: None,
            iroh_transport: Vec::new(),
        }
    }
}

/// One roster row as svrn's wire spells it. The node id is full hex, never
/// the display form: the desktop joins members against per-peer
/// contributions and preferences keyed on full hex, and the display form
/// once blanked every per-member panel (the pin below).
fn member(m: RailsMember) -> MemberDto {
    let caps = m.capabilities.as_ref();
    let gpus = caps.map(|c| c.hardware.gpus.as_slice()).unwrap_or(&[]);
    MemberDto {
        node_id: m.node_id_hex,
        name: m.name,
        is_self: m.is_self,
        status: m.status,
        vram_gb: gpus.iter().map(|g| g.vram_gb).sum(),
        can_anchor: caps
            .and_then(|c| c.anchor.as_ref())
            .is_some_and(|a| a.can_anchor),
        // cw-rails publishes dial info, not the socket addresses the retired
        // IP path gossiped; `--addr-only` reports the absence.
        addresses: Vec::new(),
        origins: caps.map(|c| c.origins.clone()).unwrap_or_default(),
        node_pubkey: m.node_pubkey,
        // cw-rails lists no tombstone.
        active: true,
        // Derived from the hardware this member already gossips. A member
        // advertising no GPU gets `None` rather than a fingerprint of an
        // empty set, so "we don't know this machine" stays distinguishable
        // from "we know it has nothing".
        hw_fingerprint: caps.filter(|_| !gpus.is_empty()).map(|c| {
            kernel_types::hardware_fingerprint(
                c.hardware.cpu_cores,
                c.hardware.system_ram_gb,
                &gpus
                    .iter()
                    .map(|g| {
                        (
                            g.name.clone(),
                            g.vram_gb,
                            compute_type_label(g.compute_type).to_string(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        }),
        backend: gpus
            .first()
            .map(|g| compute_type_label(g.compute_type).to_string()),
    }
}

/// Wire label for a GPU's compute backend.
///
/// Spelled out here rather than derived from `Debug` because it is part of a
/// measurement cache key: a `Debug` rename would silently invalidate every
/// recorded throughput number on every machine. These strings are a contract.
fn compute_type_label(ct: ComputeType) -> &'static str {
    match ct {
        ComputeType::Cuda => "cuda",
        ComputeType::Rocm => "rocm",
        ComputeType::Metal => "metal",
        ComputeType::Vulkan => "vulkan",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The member key is `node_id_hex`, not cw-rails' display `node_id`
    /// (`"node-"` + 8 bytes): the desktop's per-member joins land only on
    /// full hex. Failing input: map `node_id` instead.
    #[test]
    fn a_member_is_keyed_by_full_hex() {
        let doc = serde_json::json!({
            "mesh": { "id": "m", "name": "Home" },
            "members": [
                { "name": "b", "node_id": "node-44ae7614", "node_id_hex": "44ae76142b0c3c723051ff98f043104a",
                  "status": "online", "is_self": false },
                { "name": "a", "node_id": "node-0000", "node_id_hex": "00", "status": "offline", "is_self": true }
            ],
            "meshes": null,
        });
        let s: MeshStatusSummary = serde_json::from_value::<RailsMeshStatus>(doc)
            .expect("parses")
            .into();
        assert_eq!(s.mesh_name.as_deref(), Some("Home"));
        assert_eq!((s.members_online, s.members_total), (1, 2));
        assert!(s.members[0].is_self, "self first");
        let b = &s.members[1];
        assert_eq!(b.node_id, "44ae76142b0c3c723051ff98f043104a");
        assert!(!b.node_id.starts_with("node-"));
        assert_eq!(
            b.hw_fingerprint, None,
            "no GPU advertised: unknown, not empty"
        );
    }
}
