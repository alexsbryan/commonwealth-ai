// SPDX-License-Identifier: AGPL-3.0-or-later
//! The svrn daemon's one read of mesh membership (pb-mesh-exit-core).
//!
//! Every roster read outside the daemon's own mesh endpoint goes through
//! [`MembershipReader`]. Its first impl reads the in-process `Mesh` the
//! endpoint gossips into (`sovereign_mesh::membership::InProcessMembership`).
//! At the flip (pb-mesh-exit-transport) cw-rails' `GET /v1/mesh/status` fills
//! it instead, so the flip swaps one reader rather than rewriting every site
//! in the same commit as the behaviour change. It lives here, beside
//! [`MemberDto`], so serve's discovery and ranking consume the same port.
//!
//! [`MembershipEntry`] is the typed form of a `/v1/mesh/status` member row —
//! [`MemberDto`]'s `node_id`, `name`, `status` and `active` — plus what that
//! row lacks and the sites read: when the member was last heard, whether it
//! can be dialled at all, what it advertises ([`NodeCapabilities`]), and a
//! dial handle. The dial
//! handle is the reader's own type because the transport that dials owns its
//! shape; this crate cannot name it and must not copy it. No field is
//! defaulted: a reader that cannot fill one does not produce the entry.
//!
//! [`MemberDto`]: crate::daemon_wire::mesh::MemberDto

use async_trait::async_trait;
use kernel_types::NodeId;
use oicp_types::capabilities::NodeCapabilities;
use oicp_types::FederatedMeshDescriptor;

use crate::daemon_wire::mesh::MemberStatus;

/// One mesh member, as svrn reads it.
#[derive(Debug, Clone)]
pub struct MembershipEntry<D> {
    /// The member's node id.
    pub node_id: NodeId,
    /// The member's display name.
    pub name: String,
    /// Liveness as the roster judges it.
    pub status: MemberStatus,
    /// Not a tombstone: a departed member stays in the roster with
    /// `active: false`.
    pub active: bool,
    /// Unix seconds this node last heard the member's gossip.
    pub last_seen: u64,
    /// The member has at least one path a dial could take (an address, or an
    /// endpoint key with a relay or direct address).
    pub dialable: bool,
    /// What the member advertises.
    pub capabilities: NodeCapabilities,
    /// How to reach the member, in the reader's transport's own shape.
    pub dial: D,
}

/// The members that can hold a shard of the shared model now: present
/// (Online or Busy) with an anchor record that says they can anchor. THE one
/// roster decision behind the shared-model host election, read by Fabric's
/// status and by the discovery loop that elects (pb-serve-distributes).
pub fn eligible_anchors<D>(members: &[MembershipEntry<D>]) -> Vec<NodeId> {
    members
        .iter()
        .filter(|m| matches!(m.status, MemberStatus::Online | MemberStatus::Busy))
        .filter(|m| m.capabilities.anchor.as_ref().is_some_and(|a| a.can_anchor))
        .map(|m| m.node_id)
        .collect()
}

/// The members a router may rank as inference venues: not `self_id`, present
/// (Online or Busy), and dialable. THE one roster decision behind ranking,
/// applied to the svrn daemon's roster and to a standalone serve's cw-rails
/// roster alike (pb-serve-ranks).
pub fn inference_peers<D>(
    members: Vec<MembershipEntry<D>>,
    self_id: NodeId,
) -> Vec<MembershipEntry<D>> {
    members
        .into_iter()
        .filter(|m| m.node_id != self_id)
        .filter(|m| matches!(m.status, MemberStatus::Online | MemberStatus::Busy))
        .filter(|m| m.dialable)
        .collect()
}

impl<D> MembershipEntry<D> {
    /// This member as a venue a router ranks, reached at `base_urls` (its
    /// Inference-class endpoints, each a `/v1` prefix). The load signals are
    /// the ones the member gossips, with when they were heard. A mesh peer is
    /// never a pinned pod: those come from `PinnedWorkerEndpointSource`.
    pub fn inference_venue(&self, base_urls: Vec<String>) -> crate::venue::InferenceVenue {
        crate::venue::InferenceVenue {
            node_id: self.node_id,
            name: self.name.clone(),
            base_urls,
            system_ram_gb: self.capabilities.hardware.system_ram_gb,
            benchmark: self.capabilities.benchmark.clone(),
            current_in_flight: self.capabilities.current_in_flight,
            inference_availability: Some(self.capabilities.inference_availability),
            gossip_last_seen_unix: self.last_seen,
            pinned_transport: false,
        }
    }
}

/// The one read of mesh membership. See the module docs.
#[async_trait]
pub trait MembershipReader: Send + Sync {
    /// The dial handle each entry carries.
    type Dial: Clone + Send + Sync + 'static;

    /// The mesh's display name.
    async fn mesh_name(&self) -> String;

    /// The other meshes this one federates with, as the OICP manifest
    /// advertises them.
    async fn federated_meshes(&self) -> Vec<FederatedMeshDescriptor>;

    /// Every member the roster holds, this node and tombstones included, in
    /// the roster's own order.
    async fn members(&self) -> Vec<MembershipEntry<Self::Dial>>;

    /// The member with this id, if the roster holds one.
    async fn member(&self, id: NodeId) -> Option<MembershipEntry<Self::Dial>> {
        self.members().await.into_iter().find(|m| m.node_id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u128, status: MemberStatus, anchor: serde_json::Value) -> MembershipEntry<()> {
        let capabilities = serde_json::from_value(serde_json::json!({
            "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                         "total_storage_gb": 0, "free_storage_gb": 0},
            "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                          "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                          "available_for_mesh": true},
            "hosted_corpora": [], "reported_at": 0,
            "anchor": anchor
        }))
        .expect("capabilities");
        MembershipEntry {
            node_id: NodeId::from_u128(id),
            name: format!("n{id}"),
            status,
            active: true,
            last_seen: 0,
            dialable: true,
            capabilities,
            dial: (),
        }
    }

    /// Present members that say they can anchor, and no one else: an absent
    /// anchor, a consumer's `can_anchor: false` and an offline anchor are
    /// each left out. Failing input: drop either filter.
    #[test]
    fn eligible_anchors_are_present_members_that_can_anchor() {
        let yes = serde_json::json!({"can_anchor": true, "vram_gb": 8});
        let roster = vec![
            member(1, MemberStatus::Online, yes.clone()),
            member(2, MemberStatus::Busy, yes.clone()),
            member(3, MemberStatus::Offline, yes),
            member(
                4,
                MemberStatus::Online,
                serde_json::json!({"can_anchor": false, "vram_gb": 8}),
            ),
            member(5, MemberStatus::Online, serde_json::Value::Null),
        ];
        assert_eq!(
            eligible_anchors(&roster),
            vec![NodeId::from_u128(1), NodeId::from_u128(2)]
        );
    }

    /// A router ranks the present, dialable members that are not itself:
    /// this node, an offline member and an undialable one are each left out.
    /// Failing input: drop any of the three filters.
    #[test]
    fn inference_peers_are_present_dialable_members_other_than_this_node() {
        let none = serde_json::Value::Null;
        let mut undialable = member(4, MemberStatus::Online, none.clone());
        undialable.dialable = false;
        let roster = vec![
            member(1, MemberStatus::Online, none.clone()),
            member(2, MemberStatus::Busy, none.clone()),
            member(3, MemberStatus::Offline, none.clone()),
            undialable,
            member(5, MemberStatus::Online, none),
        ];
        let ids: Vec<NodeId> = inference_peers(roster, NodeId::from_u128(5))
            .into_iter()
            .map(|m| m.node_id)
            .collect();
        assert_eq!(ids, vec![NodeId::from_u128(1), NodeId::from_u128(2)]);
    }
}
