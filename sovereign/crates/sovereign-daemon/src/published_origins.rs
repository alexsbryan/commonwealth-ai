// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's published apps and offer origin, in cw-rails' tables
//! (pb-mesh-exit-transport; director phase-b-81 (3)).
//!
//! cw-rails is the node's one mesh endpoint, so what the daemon's acceptor
//! served from `[iroh.apps]` and `[iroh] offer_origin` is registered with it
//! instead, and the allow lists ride the registration as `media_allow` rides
//! the media origin's: each `[iroh.apps]` entry is an app claim carrying
//! `[iroh] app_allow`, and `offer_origin` is a `cwth/offer/0` origin admitting
//! `Admit::Members([iroh] offer_allow)`. The config does not move and
//! `rails.toml` gains no key. Only loopback origins forward (cw-rails forwards
//! to `127.0.0.1:<port>`); any other entry is named and not published.

use std::net::SocketAddr;

use oicp_types::origin::{Admit, Framing, OriginRegistration};
use sovereign_contracts::setup_config::IrohSection;
use tracing::{info, warn};

use crate::peer_origin::{ORIGIN_RENEW_EVERY, ORIGIN_TTL_SECS};

/// The trace target of every event here.
pub const TRACE_TARGET: &str = "published_origins";

/// The port of a loopback `host:port`, or why it is not one.
fn loopback_port(what: &str, addr: &str) -> Result<u16, String> {
    let parsed: SocketAddr = addr
        .trim()
        .parse()
        .map_err(|e| format!("{what} = {addr:?} is not a host:port ({e})"))?;
    if !parsed.ip().is_loopback() {
        return Err(format!(
            "{what} = {addr:?} is not loopback; cw-rails forwards only to this host"
        ));
    }
    Ok(parsed.port())
}

/// `[iroh] offer_origin` as a `cwth/offer/0` registration for the members
/// `offer_allow` names (empty = every member; a non-member never).
pub fn offer_registration(
    offer_origin: &str,
    allow: &[String],
) -> Result<OriginRegistration, String> {
    Ok(OriginRegistration {
        alpn: String::from_utf8_lossy(mesh_reach::alpn::OFFER_ALPN).into_owned(),
        prefixes: Vec::new(),
        port: loopback_port("[iroh] offer_origin", offer_origin)?,
        admit: Admit::Members(allow.to_vec()),
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: None,
        namespaces: Vec::new(),
    })
}

/// Each `[iroh.apps]` entry as `(name, port)`; an entry that is not a
/// loopback `host:port` is named and left out.
pub fn app_ports(iroh: &IrohSection) -> Vec<(String, u16)> {
    iroh.apps
        .iter()
        .filter_map(
            |(name, addr)| match loopback_port(&format!("[iroh.apps] {name}"), addr) {
                Ok(port) => Some((name.clone(), port)),
                Err(e) => {
                    warn!(target: TRACE_TARGET, app = %name, error = %e,
                          "published origins: this app is not published");
                    None
                }
            },
        )
        .collect()
}

/// The publish and register loops; dropping it stops the renewals and
/// cw-rails lets each claim lapse at its TTL.
pub struct PublishedOriginsHandle {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for PublishedOriginsHandle {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Publish every `[iroh.apps]` entry and register `[iroh] offer_origin` with
/// cw-rails at `rails_base`, kept while the handle lives. `None` when the
/// config publishes neither.
pub fn spawn(rails_base: String, iroh: &IrohSection) -> Option<PublishedOriginsHandle> {
    let mut tasks = Vec::new();
    for (name, port) in app_ports(iroh) {
        info!(target: TRACE_TARGET, app = %name, port, allow = ?iroh.app_allow,
              "published origins: publishing an [iroh.apps] entry with cw-rails");
        tasks.push(tokio::spawn(
            sovereign_turn_client::rails_origins::keep_published(
                rails_base.clone(),
                name,
                port,
                iroh.app_allow.clone(),
                ORIGIN_TTL_SECS,
                ORIGIN_RENEW_EVERY,
            ),
        ));
    }
    if let Some(origin) = iroh.offer_origin.as_deref() {
        match offer_registration(origin, &iroh.offer_allow) {
            Ok(reg) => {
                info!(target: TRACE_TARGET, port = reg.port, allow = ?iroh.offer_allow,
                      "published origins: registering the offer origin with cw-rails on cwth/offer/0");
                tasks.push(tokio::spawn(
                    sovereign_turn_client::rails_origins::keep_registered(
                        rails_base,
                        reg,
                        ORIGIN_TTL_SECS,
                        ORIGIN_RENEW_EVERY,
                    ),
                ));
            }
            Err(e) => warn!(target: TRACE_TARGET, error = %e,
                            "published origins: the offer origin is not registered"),
        }
    }
    (!tasks.is_empty()).then_some(PublishedOriginsHandle { tasks })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `offer_allow` rides the registration as members-only with the list;
    /// a registration that admitted anyone would publish a household's
    /// inventory to every holder of an invite. Failing input: `Admit::Any`,
    /// or the list dropped.
    #[test]
    fn the_offer_origin_admits_the_members_offer_allow_names() {
        let r = offer_registration("127.0.0.1:8710", &["Mira".into()]).unwrap();
        assert_eq!(r.alpn, "cwth/offer/0");
        assert_eq!(r.port, 8710);
        assert!(matches!(&r.admit, Admit::Members(a) if a == &vec!["Mira".to_string()]));
        assert!(r.prefixes.is_empty());
    }

    /// cw-rails forwards to `127.0.0.1:<port>` only, so a routable or
    /// unparsable origin is refused by name rather than registered on a
    /// port that would reach something else.
    #[test]
    fn a_non_loopback_origin_is_refused_by_name() {
        let e = offer_registration("192.168.1.9:8710", &[]).unwrap_err();
        assert!(e.contains("not loopback"), "{e}");
        let e = offer_registration("nonsense", &[]).unwrap_err();
        assert!(e.contains("not a host:port"), "{e}");
        let iroh = IrohSection {
            apps: [
                ("chores".to_string(), "127.0.0.1:5000".to_string()),
                ("far".to_string(), "10.0.0.2:5000".to_string()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(app_ports(&iroh), vec![("chores".to_string(), 5000)]);
    }
}
