// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail's listener — the fourth bind of the client router.
//!
//! # Why it is a bind and not a route on `:9741`
//!
//! `client_auth` admits a loopback caller on the operator bind **before it
//! reads a bearer**, and a deployed ring app is a process on this machine. An
//! app pointed at `:9741` would therefore arrive as an OPERATOR with its
//! grant ignored: the namespace scoping on `Scope::Rails` would be
//! decorative, and a guard nobody can watch fail is not a guard (ARCH §18.1).
//! This bind carries `ClientAuthPolicy::UNTRUSTED_LOOPBACK`, so the grant is
//! the only way in — and the refusal is observable
//! (`rail_e2e::on_the_rail_bind_a_loopback_caller_without_a_grant_is_refused`).
//!
//! # Why a FIXED port
//!
//! The peer and guest binds take ephemeral loopback ports because the iroh
//! acceptor that forwards to them learns the address at bind time. Nothing
//! forwards to this one: `svrn ring dev` is a separate process that dials it
//! with only the config to go on. `commonwealth_core::config::rail_port` is
//! the ONE derivation, so a second daemon on a non-default client port gets
//! its own rail rather than a silently unreachable one (ARCH §10.6).

use std::net::SocketAddr;

use tracing::warn;

/// Where this daemon's rail listens, given its client port. Loopback only —
/// in M0 a ring app runs on the same machine, so there is nothing to
/// advertise and nothing to firewall.
pub fn rail_addr(client_port: u16) -> SocketAddr {
    SocketAddr::from((
        [127u8, 0, 0, 1],
        commonwealth_core::config::rail_port(client_port),
    ))
}

/// Bind it, or warn and carry on. A daemon whose rail did not bind serves
/// everything else exactly as before; only ring apps are affected, and they
/// find out immediately because `svrn ring dev` refuses to start against a
/// rail it cannot reach.
pub async fn bind(addr: SocketAddr) -> Option<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => Some(l),
        Err(e) => {
            warn!("rail listener could not bind {addr} ({e}) — ring apps disabled");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rail_sits_beside_the_client_port_and_never_leaves_loopback() {
        assert_eq!(rail_addr(9741).to_string(), "127.0.0.1:9743");
        // A second daemon on its own client port gets its own rail rather
        // than colliding on a hardcoded one.
        assert_eq!(rail_addr(19741).to_string(), "127.0.0.1:19743");
        assert!(rail_addr(9741).ip().is_loopback());
    }
}
