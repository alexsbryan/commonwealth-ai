// SPDX-License-Identifier: AGPL-3.0-or-later
//! The in-process [`MembershipReader`]: membership read from the `Mesh` this
//! daemon's own endpoint gossips into (pb-mesh-exit-core).
//!
//! It is the port's first impl and the one every daemon builds today
//! ([`crate::fabric::FabricPart::new`]). The flip (pb-mesh-exit-transport)
//! replaces it with cw-rails' roster; until then reading cw-rails would read
//! its solo roster on a daemon-founded mesh (d8704a23f).

use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_core::mesh::{MemberRecord, Mesh};
use commonwealth_transport::{peer_contact, PeerContact};
use kernel_types::NodeId;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};
use tokio::sync::RwLock;

use crate::state::member_status;

/// Membership read from the in-process roster.
pub struct InProcessMembership {
    mesh: Arc<RwLock<Mesh>>,
}

impl InProcessMembership {
    /// A reader over the roster lock Fabric holds.
    pub fn new(mesh: Arc<RwLock<Mesh>>) -> Self {
        Self { mesh }
    }
}

/// One roster row as the port's entry. The dial handle is
/// `commonwealth_transport::peer_contact`'s, the one decision about which
/// record fields may influence dialling.
fn entry(member: &MemberRecord) -> MembershipEntry<PeerContact> {
    MembershipEntry {
        node_id: member.node_id,
        name: member.name.clone(),
        status: member_status(member.status),
        active: member.is_active(),
        dialable: member.is_dialable(),
        capabilities: member.capabilities.clone(),
        dial: peer_contact(member),
    }
}

#[async_trait]
impl MembershipReader for InProcessMembership {
    type Dial = PeerContact;

    async fn mesh_name(&self) -> String {
        self.mesh.read().await.name.clone()
    }

    async fn members(&self) -> Vec<MembershipEntry<PeerContact>> {
        let mesh = self.mesh.read().await;
        let members: Vec<_> = mesh.members.values().map(entry).collect();
        tracing::trace!(
            members = members.len(),
            "membership: in-process roster read"
        );
        members
    }

    async fn member(&self, id: NodeId) -> Option<MembershipEntry<PeerContact>> {
        self.mesh.read().await.members.get(&id).map(entry)
    }
}
