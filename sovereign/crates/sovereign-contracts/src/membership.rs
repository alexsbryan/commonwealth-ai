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
