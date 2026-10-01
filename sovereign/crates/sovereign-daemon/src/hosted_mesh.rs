// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's mesh, as a distribution composes it into this process
//! (pb-mesh-exit-transport; phase-b-80 fork 1). cw-rails is the node's one
//! mesh endpoint: svrn reads its roster and reaches peers through its reach
//! door, and neither accepts nor dials a peer itself. svrn cannot name the
//! roster reader (it is serve's, sovereign-serve rails_mesh.rs `RailsRoster`),
//! so the distribution builds both ports for the rails base svrn resolves and
//! hands them here, the swap `sovereign-stock` already makes for serve. svrn
//! alone (no [`HostedMesh`]) reads no roster: [`MeshAccess::absent`] names that
//! at boot, and every membership read answers an empty roster.

use std::sync::Arc;

use async_trait::async_trait;
use mesh_reach::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};
use sovereign_contracts::membership::{MembershipReader, NoMembership};

/// The trace target of the absence below.
const TARGET: &str = "mesh";

/// The two ports svrn reads the mesh through: cw-rails' roster, and the
/// transport that resolves a peer's origin through cw-rails' reach door.
#[derive(Clone)]
pub struct MeshAccess {
    pub membership: Arc<dyn MembershipReader<Dial = PeerContact>>,
    pub transport: Arc<dyn PeerTransport>,
}

impl MeshAccess {
    /// svrn composed without a roster reader: an empty roster and a transport
    /// that resolves no peer, each naming why in its trace.
    pub fn absent() -> Self {
        tracing::warn!(
            target: TARGET,
            "no mesh: this binary composes no cw-rails roster reader (svrn alone), so svrn \
             sees no peer; run the stock binary (`svrn daemon`) for a node on a mesh"
        );
        Self {
            membership: Arc::new(NoMembership::default()),
            transport: Arc::new(NoReach),
        }
    }
}

type Compose = Box<dyn FnOnce(&str) -> MeshAccess + Send>;

/// The distribution's composition of the node's mesh, run once at boot with
/// cw-rails' API base (`rails_client::resolve_rails_base`, the one reader).
pub struct HostedMesh {
    compose: Compose,
}

impl HostedMesh {
    pub fn new(compose: impl FnOnce(&str) -> MeshAccess + Send + 'static) -> Self {
        Self {
            compose: Box::new(compose),
        }
    }

    /// Build the ports for `rails_base`.
    pub fn compose(self, rails_base: &str) -> MeshAccess {
        (self.compose)(rails_base)
    }
}

/// The transport of a svrn composed with no mesh: no peer resolves.
#[derive(Debug)]
pub(crate) struct NoReach;

#[async_trait]
impl PeerTransport for NoReach {
    fn name(&self) -> &'static str {
        "none"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        tracing::debug!(
            target: TARGET,
            peer = %peer.node_id,
            class = class.as_str(),
            "no mesh: no reach door composed, the peer resolves to nothing"
        );
        Vec::new()
    }
}
