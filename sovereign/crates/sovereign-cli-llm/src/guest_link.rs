// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest half of an ephemeral mesh link: `<root>/guest.json`.
//!
//! `svrn mesh use <sovereign://guest/…>` writes this file; `svrn chat`
//! consults it and, when it holds a live link, points its daemon base and its
//! `Authorization: Bearer` at the issuing node instead of the local daemon.
//!
//! # Why a file and not a flag
//!
//! The alternative is `svrn chat ask --daemon http://host:9741 --token …` on
//! every invocation. That is not the shape of the feature: a guest was *lent*
//! a machine for a bounded window, and the window — not the invocation — is
//! what should decide where the request goes. Persisting once and expiring on
//! its own is what makes "for two hours" true without anyone remembering it.
//!
//! # Where the bearer actually goes
//!
//! Two shapes, and [`open_route`] is the only thing that turns one into a URL.
//! A link minted against a plaintext client API carries `url` and nothing
//! else. A link minted on an ENCRYPTED mesh carries `dial` as well, because
//! that mesh forces its client API loopback-only and the sole ingress is the
//! iroh acceptor — `url` then names the lender without being reachable. When
//! `dial` is present it IS the path: [`open_route`] tunnels, and a tunnel
//! that fails is reported rather than downgraded to the plaintext address a
//! mesh asking for encryption never meant to serve (§18.3).
//!
//! # Expiry is reported, never assumed away
//!
//! [`load_live`] returns `None` for an expired link **and says so on stderr**.
//! Silently falling back to the local daemon would answer the guest's question
//! with a different machine's model and never mention it — the §18.3 failure
//! this whole feature was built to avoid. The ISSUING NODE is still the
//! authority (its store rejects an expired token regardless); this check only
//! spares a round-trip and produces a message that names the real cause.
//!
//! # 0600, tmp-then-rename
//!
//! The file holds a live bearer. Same posture as its siblings under the same
//! root — `node_key`, `client-token`, `join_key.secret`
//! (`commonwealth_transport::identity`).

use std::io;

use sovereign_cli_shared::dirs::sovereign_root;

// The TYPE and its file format live in `sovereign-core`, because the DAEMON
// reads this file too: `svrn chat ask` is a surface, so a guest's turn runs on
// their own daemon and it is the daemon that must resolve a granted model id
// to the lender (`sovereign_mesh::guest_lender`). A daemon cannot depend on a
// CLI crate, and two copies of the format would be the §10.6 failure.
//
// What stays here is the CLI's own half: the well-known root, and the stderr
// wording a person reads. A library must not print to stderr.
pub use sovereign_core::guest_link::{
    forget_in, load_in, load_live_in, now_secs, path_in, save_in, GuestLink, GUEST_LINK_FILE,
};

pub fn path() -> std::path::PathBuf {
    path_in(&sovereign_root())
}

pub fn save(link: &GuestLink) -> io::Result<()> {
    save_in(&sovereign_root(), link)
}

pub fn load() -> Option<GuestLink> {
    load_in(&sovereign_root())
}

/// The accessor every CLI consumer should use: present AND live.
///
/// An expired one returns `None` **loudly**. The core accessor is silent by
/// design (a daemon has no stderr worth writing to), so the wording lives
/// here, where there is a person to read it.
pub fn load_live(now_secs: u64) -> Option<GuestLink> {
    let link = load_in(&sovereign_root())?;
    if link.is_live(now_secs) {
        return Some(link);
    }
    eprintln!(
        "The guest link for {} expired. Ask for a fresh one (`svrn mesh grant` \
         on their side), or `svrn mesh use --forget` to go back to your own daemon.",
        link.url
    );
    None
}

/// The base URL every request under `link` must be sent to — opening a mesh
/// tunnel first when the link names an iroh endpoint.
///
/// **The one decider.** Nothing else turns a [`GuestLink`] into an address:
/// a second reader that took `link.url` when a `dial` was present would send
/// a bearer in plaintext to a mesh that closed its plaintext ingress on
/// purpose.
///
/// The tunnel is parked in a process-lifetime slot rather than returned,
/// because dropping it shuts the local port and every caller here holds the
/// URL for as long as the process runs. Repeat calls reuse the one tunnel —
/// a second dial would bind a second endpoint for the same lender.
pub async fn open_route(link: &GuestLink) -> Result<String, String> {
    let Some(dial) = link.dial.as_deref() else {
        return Ok(link.url.clone());
    };
    if let Some(open) = TUNNEL.get() {
        return Ok(open.base_url().to_string());
    }
    // The guest's OWN iroh posture, not the lender's: a node that severed n0
    // discovery must not be put back on it by accepting a lend.
    let (relay_urls, discovery) = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| (c.iroh.relay_urls.clone(), c.iroh.discovery.clone()))
        .unwrap_or_default();
    let tunnel =
        sovereign_mesh::guest_tunnel::GuestTunnel::open(dial, relay_urls, discovery.as_deref())
            .await
            .map_err(|e| {
                format!(
                    "could not reach {} over the mesh tunnel: {e}\n\
             The link names an iroh endpoint, which means the lending node's \
             plaintext API is closed (an encrypted mesh). There is no plaintext \
             fallback — ask for a fresh link, or ask them to check `svrn mesh status`.",
                    link.url
                )
            })?;
    Ok(TUNNEL.get_or_init(|| tunnel).base_url().to_string())
}

/// The one live tunnel this process holds. See [`open_route`].
static TUNNEL: std::sync::OnceLock<sovereign_mesh::guest_tunnel::GuestTunnel> =
    std::sync::OnceLock::new();

pub fn forget() -> io::Result<bool> {
    forget_in(&sovereign_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(exp: u64) -> GuestLink {
        GuestLink {
            token: "deadbeef".into(),
            url: "http://box:9741".into(),
            dial: None,
            expires_at: exp,
            summary: Some("models: big-27b".into()),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(9_000)).unwrap();
        assert_eq!(load_in(dir.path()).unwrap(), link(9_000));
    }

    #[test]
    fn an_expired_link_is_not_live() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(100)).unwrap();
        // Present on disk...
        assert!(load_in(dir.path()).is_some());
        // ...and still refused, rather than quietly used.
        assert!(load_live_in(dir.path(), 101).is_none());
        assert!(load_live_in(dir.path(), 99).is_some());
    }

    #[test]
    fn remaining_goes_none_at_the_boundary() {
        let l = link(100);
        assert_eq!(l.remaining_secs(40), Some(60));
        assert_eq!(l.remaining_secs(100), None);
        assert_eq!(l.remaining_secs(101), None);
    }

    #[test]
    fn forget_is_idempotent_and_absence_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!forget_in(dir.path()).unwrap());
        save_in(dir.path(), &link(9_000)).unwrap();
        assert!(forget_in(dir.path()).unwrap());
        assert!(!forget_in(dir.path()).unwrap());
        assert!(load_in(dir.path()).is_none());
    }

    /// A `guest.json` written before `dial` existed must still read — the
    /// field is additive, and a guest who accepted a link an hour ago should
    /// not have to accept it again.
    #[test]
    fn a_file_without_the_dial_field_reads_as_no_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path_in(dir.path()),
            r#"{"token":"t","url":"http://box:9741","expires_at":9000}"#,
        )
        .unwrap();
        let link = load_in(dir.path()).expect("the older shape is still a link");
        assert!(link.dial.is_none());
        assert_eq!(link.url, "http://box:9741");
    }

    /// A link with no dial routes to its own `url` — the plaintext case, and
    /// the arm that must not need a network to resolve.
    #[tokio::test]
    async fn a_link_without_a_dial_routes_straight_to_its_url() {
        assert_eq!(open_route(&link(9_000)).await.unwrap(), "http://box:9741");
    }

    /// A corrupt file must not read as "no link" without saying so, and must
    /// not read as a link either.
    #[test]
    fn a_corrupt_file_is_ignored_not_deserialized_into_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_in(dir.path()), "{not json").unwrap();
        assert!(load_in(dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_holding_a_bearer_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(9_000)).unwrap();
        let mode = std::fs::metadata(path_in(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "guest.json holds a live bearer");
    }
}
