// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's guest listener as an origin in cw-rails' table
//! (pb-mesh-exit-transport).
//!
//! cw-rails is the node's one mesh endpoint, so a `sovereign://guest/…`
//! bearer reaches this daemon through it: on `cwth/guest/0`, and as the
//! fallback of serve's `cwth/client/0` registration
//! (`Admit::MembersElse`, sovereign-serve rails_mesh.rs), which is where a
//! non-member dialling the client protocol is sent. Both land on the guest
//! bind of the client router (`crate::client_surface::ClientSurface::Guest`),
//! whose auth layer reads the bearer and never trusts the loopback hop.

use oicp_types::origin::{Admit, Framing, OriginRegistration};
use tracing::info;

// The peer origin's TTL and renew cadence: one pair for svrn's registrations.
use crate::peer_origin::{ORIGIN_RENEW_EVERY, ORIGIN_TTL_SECS};

/// The trace target of every event here.
pub const TRACE_TARGET: &str = "guest_origin";

/// `cwth/guest/0`, whole, at the guest listener's port, for anyone: the
/// credential is the bearer the listener reads, not the dialer's key.
pub fn registration(guest_port: u16) -> OriginRegistration {
    OriginRegistration {
        alpn: String::from_utf8_lossy(mesh_reach::alpn::GUEST_ALPN).into_owned(),
        prefixes: Vec::new(),
        port: guest_port,
        admit: Admit::Any,
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: None,
        namespaces: Vec::new(),
    }
}

/// The registration loop; dropping it stops the renewals and cw-rails lets
/// the claim lapse within [`ORIGIN_TTL_SECS`].
pub struct GuestOriginHandle {
    register: tokio::task::JoinHandle<()>,
}

impl Drop for GuestOriginHandle {
    fn drop(&mut self) {
        self.register.abort();
    }
}

/// Register [`registration`] with cw-rails at `rails_base` and keep it
/// registered while the handle lives.
pub fn spawn(rails_base: String, guest_port: u16) -> GuestOriginHandle {
    info!(target: TRACE_TARGET, rails = %rails_base, guest_port,
          "guest origin: registering svrn's guest listener with cw-rails on cwth/guest/0");
    let register = tokio::spawn(sovereign_turn_client::rails_origins::keep_registered(
        rails_base,
        registration(guest_port),
        ORIGIN_TTL_SECS,
        ORIGIN_RENEW_EVERY,
    ));
    GuestOriginHandle { register }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guest door admits anyone on its own protocol; a registration that
    /// admitted members only would refuse every guest at cw-rails. Failing
    /// input: `Admit::Members(..)`.
    #[test]
    fn the_guest_door_admits_anyone_on_the_guest_protocol() {
        let r = registration(4242);
        assert_eq!(r.alpn, "cwth/guest/0");
        assert!(matches!(r.admit, Admit::Any));
        assert_eq!(r.port, 4242);
        assert!(r.prefixes.is_empty());
    }
}
