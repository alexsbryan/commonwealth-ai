// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a node SERVES to the mesh — the local origins behind its acceptor.
//!
//! Lives in this leaf for the reason `TenantId` does (`tenant.rs`): the
//! roster gossips it (`commonwealth_core::capabilities::NodeCapabilities::
//! origins`), the daemon answers it on `/v1/mesh/status` (`members[].origins`),
//! and a thin client parses that answer. `commonwealth-core` and `sovereign-*`
//! cannot see each other (`quality/ARCH_LAYERS.toml`), so while the enum was
//! defined in commonwealth-core every wire shape carrying it was pinned above
//! the contract layer — `sovereign_mesh::mesh_http::MemberDto` could not move
//! to `sovereign_contracts::daemon_wire` for one closed set of one variant.
//! `oicp-types` is the serde-only crate both already depend on. Moved
//! 2026-09-11 (sv-surface svt-3); `commonwealth_core::capabilities::OriginKind`
//! re-exports it, so no gossip site changed.
//!
//! NOT `kernel-types`: its header splits identity/provenance from federation
//! and it already defines a different `Origin`. This is federation vocabulary.

use serde::{Deserialize, Serialize};

/// A kind of local origin a node can serve to members over the mesh. A
/// closed set (ARCH §2): every kind has one ALPN and one acceptor route, so a
/// new kind is a new variant beside a new route, never a string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// An HTTP media origin (`[iroh] media_origin`), served on `MEDIA_ALPN`.
    Media,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gossip bytes are the enum's serde repr, and a rename here is a
    /// roster-wide wire break with nothing red anywhere — so the repr is
    /// pinned, not assumed.
    #[test]
    fn origin_kind_wire_repr_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&OriginKind::Media).unwrap(),
            "\"media\""
        );
        assert_eq!(
            serde_json::from_str::<OriginKind>("\"media\"").unwrap(),
            OriginKind::Media
        );
        assert!(serde_json::from_str::<OriginKind>("\"Media\"").is_err());
    }
}
