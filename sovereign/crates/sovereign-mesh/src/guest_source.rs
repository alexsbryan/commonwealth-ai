// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh-side wiring for the serving host's guest-lookup ports.
//!
//! `StoredGuestLink` (now in `sovereign-serving-host`) resolves a granted id
//! through two ports — `GuestLinkReader` and `GuestTunnelOpener` — because
//! neither the holder's link file (`sovereign_core::guest_link`, the holder's
//! credential) nor the tunnel (`crate::guest_tunnel`, Fabric reach) is the
//! serving package's to name (`sovereign/SERVING_BOUNDARY.md` "The five
//! entries" (a)). This module implements both over the mesh's own readers,
//! and carries the factory the daemon installs on its provider.
//!
//! The link reader takes no path by default ON PURPOSE: it resolves the same
//! `svrnmesh_root()` the CLI writes `guest.json` to. A root passed in is a
//! root that can disagree with the writer's — the 2026-08-28 silent dead
//! route (`StoredGuestLink::new`'s docs).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_serving_host::guest_lender::{
    GuestLenderSource, GuestLinkReader, GuestTunnelHandle, GuestTunnelOpener, LiveGuestLink,
    StoredGuestLink,
};

/// Reads `<root>/guest.json` through `sovereign_core::guest_link`.
#[derive(Debug)]
pub struct GuestLinkFileReader {
    root: PathBuf,
}

impl GuestLinkFileReader {
    /// The production root: the SSOT `svrn mesh use` writes to.
    pub fn new() -> Self {
        Self::new_in(sovereign_contracts::rebrand::svrnmesh_root())
    }

    /// Test-only escape hatch. Production must use [`Self::new`].
    pub fn new_in(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Default for GuestLinkFileReader {
    fn default() -> Self {
        Self::new()
    }
}

impl GuestLinkReader for GuestLinkFileReader {
    fn live_link(&self) -> Option<LiveGuestLink> {
        let link = sovereign_core::guest_link::load_live_in(
            &self.root,
            sovereign_core::guest_link::now_secs(),
        )?;
        Some(LiveGuestLink {
            token: link.token,
            url: link.url,
            dial: link.dial,
        })
    }
}

/// Opens the mesh tunnel through `crate::guest_tunnel::GuestTunnel`.
#[derive(Debug, Default)]
pub struct MeshTunnelOpener;

impl GuestTunnelHandle for crate::guest_tunnel::GuestTunnel {
    fn base_url(&self) -> &str {
        crate::guest_tunnel::GuestTunnel::base_url(self)
    }
}

#[async_trait]
impl GuestTunnelOpener for MeshTunnelOpener {
    async fn open(
        &self,
        dial: &str,
        relay_urls: Vec<String>,
        discovery: Option<String>,
    ) -> Result<Arc<dyn GuestTunnelHandle>, String> {
        let tunnel =
            crate::guest_tunnel::GuestTunnel::open(dial, relay_urls, discovery.as_deref()).await?;
        Ok(Arc::new(tunnel))
    }
}

/// The guest source the daemon installs on its mesh provider.
pub fn stored_guest_source() -> Arc<dyn GuestLenderSource> {
    Arc::new(StoredGuestLink::new(
        Arc::new(GuestLinkFileReader::new()),
        Arc::new(MeshTunnelOpener),
    ))
}

/// The same, reading a caller-supplied root. Test-only.
pub fn stored_guest_source_in(root: PathBuf) -> Arc<dyn GuestLenderSource> {
    Arc::new(StoredGuestLink::new(
        Arc::new(GuestLinkFileReader::new_in(root)),
        Arc::new(MeshTunnelOpener),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE regression the `root` field exists for, and the one the unit tests
    /// were structurally blind to before the port: the production root must be
    /// the SAME file `svrn mesh use` writes. It was built from the daemon's
    /// `cfg.data.dir` for one afternoon — `~/.sovereign` while the CLI wrote
    /// `~/.svrnmesh` — so every lookup found nothing, silently.
    #[test]
    fn the_default_root_is_the_one_the_cli_writes_to() {
        assert_eq!(
            GuestLinkFileReader::new().root,
            sovereign_contracts::rebrand::svrnmesh_root(),
            "the daemon must read guest.json where `svrn mesh use` put it; a \
             configurable data dir is NOT that place"
        );
        assert_eq!(
            sovereign_core::guest_link::path_in(&GuestLinkFileReader::new().root)
                .file_name()
                .unwrap(),
            "guest.json"
        );
    }

    /// An absent `guest.json` is the overwhelmingly common case and must be a
    /// cheap `None`, never an error or a panic.
    #[test]
    fn an_absent_guest_file_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let reader = GuestLinkFileReader::new_in(dir.path().to_path_buf());
        assert!(reader.live_link().is_none());
    }

    /// An EXPIRED link must not resolve — the window is the guest's own half
    /// of the contract, checked before any network call.
    #[test]
    fn an_expired_link_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        sovereign_core::guest_link::save_in(
            dir.path(),
            &sovereign_core::guest_link::GuestLink {
                token: "t".into(),
                // Unroutable on purpose: if expiry were not checked FIRST this
                // would hang or error rather than returning a clean None.
                url: "http://127.0.0.1:1".into(),
                dial: None,
                expires_at: 1,
                summary: None,
            },
        )
        .unwrap();
        let reader = GuestLinkFileReader::new_in(dir.path().to_path_buf());
        assert!(reader.live_link().is_none());
    }

    /// The planted POSITIVE: a live link is projected to exactly the fields
    /// the resolver reads. Without it the reader could answer `None` for every
    /// input and the absent/expired tests would still pass.
    #[test]
    fn a_live_link_is_projected_to_the_dispatch_fields() {
        let dir = tempfile::tempdir().unwrap();
        sovereign_core::guest_link::save_in(
            dir.path(),
            &sovereign_core::guest_link::GuestLink {
                token: "tok".into(),
                url: "http://lender:9741".into(),
                dial: Some("beef@127.0.0.1:9999".into()),
                expires_at: sovereign_core::guest_link::now_secs() + 3_600,
                summary: Some("a-model".into()),
            },
        )
        .unwrap();
        let link = GuestLinkFileReader::new_in(dir.path().to_path_buf())
            .live_link()
            .expect("a live link must resolve");
        assert_eq!(link.token, "tok");
        assert_eq!(link.url, "http://lender:9741");
        assert_eq!(link.dial.as_deref(), Some("beef@127.0.0.1:9999"));
    }
}
