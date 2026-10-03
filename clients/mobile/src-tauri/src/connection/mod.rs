// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client-owned connection state: the HOST_CONNECTION record (SQLite)
//! and the CREDENTIAL token (keychain).

pub mod keychain;
pub mod store;

pub use keychain::{CredentialStore, DevFileCredentialStore};
pub use store::HostConnection;

/// How a `HostConnection.tailnet_address` is interpreted. One kind
/// today; a future dial-by-key transport (iroh) adds its own without
/// reshaping the table — the address column is opaque per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// MagicDNS name or overlay IP + port, reached as plain
    /// `http://<addr>` over the tailnet.
    Tailnet,
    /// Dial-by-key pairing string `<endpoint-id-hex>@<relay-url>`
    /// (from the host's `GET /status` → `iroh.dial`), reached through
    /// a localhost iroh bridge — no VPN required. See
    /// `crate::iroh_bridge`.
    Iroh,
}

impl EndpointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EndpointKind::Tailnet => "tailnet",
            EndpointKind::Iroh => "iroh",
        }
    }

    /// Loud on unknown kinds: a row written by a NEWER app build
    /// must fail visibly here, not be silently dialed as a tailnet
    /// address that can't work.
    pub fn parse(s: &str) -> crate::error::Result<Self> {
        match s {
            "tailnet" => Ok(EndpointKind::Tailnet),
            "iroh" => Ok(EndpointKind::Iroh),
            other => Err(crate::error::Error::Other(format!(
                "unknown host endpoint_kind '{other}' — this app build is too old for this host entry"
            ))),
        }
    }
}
