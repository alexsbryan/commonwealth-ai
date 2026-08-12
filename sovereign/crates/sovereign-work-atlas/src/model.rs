// SPDX-License-Identifier: AGPL-3.0-or-later
//! Domain types for the work atlas.
//!
//! Privacy enforcement leans on [`Privacy::app_id`] being a `const fn`
//! returning a hardcoded literal — see WORK_ATLAS.md §Privacy. A
//! caller cannot construct a `work-atlas-private` app_id from runtime
//! data; the violation is impossible to express.

use std::path::PathBuf;

use commonwealth_core::ids::NodeId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// MeshStore namespace for Public records. Gossips across the mesh.
pub const APP_ID_PUBLIC: &str = "work-atlas";

/// MeshStore namespace for Private records. Excluded from gossip via
/// `GOSSIP_EXCLUDED_APP_IDS` — cannot leak by construction.
pub const APP_ID_PRIVATE: &str = "work-atlas-private";

/// Who created a session. Humans get one ambient session per
/// workstation in Phase 1 (a CLI-synthesized session); Agents get
/// per-MCP-token sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Human,
    Agent,
}

impl AgentKind {
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// Visibility of a session and everything attributed to it.
///
/// Encoded structurally: `app_id()` returns the MeshStore namespace,
/// and Private's namespace is in `GOSSIP_EXCLUDED_APP_IDS`. There is
/// no third value, and the mapping is `const fn` so the compiler can
/// verify there is no runtime path that selects an arbitrary string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    Public,
    Private,
}

impl Privacy {
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    pub const fn app_id(&self) -> &'static str {
        match self {
            Self::Public => APP_ID_PUBLIC,
            Self::Private => APP_ID_PRIVATE,
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

/// One row per active session — a human at a workstation or an agent
/// holding a connection to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: Uuid,
    pub node_id: NodeId,
    pub agent_kind: AgentKind,
    /// Agent's native session token from `X-Agent-Session`, or a
    /// per-connection synthetic (`conn:<mcp_session>`), or a CLI
    /// synthetic (`cli:<node_id>`). Used to deduplicate sessions and
    /// to exclude the caller's own session from query results.
    pub agent_session_token: Option<String>,
    /// SHA-256 of the canonicalized origin remote URL. Required —
    /// session creation hard-fails if the repo has no origin.
    pub repo_id: String,
    /// Local absolute path to the repo root. Not used for cross-node
    /// matching; included for human-readable display.
    pub repo_root: PathBuf,
    pub current_branch: Option<String>,
    pub privacy: Privacy,
    pub created_at: u64,
    pub last_activity_at: u64,
}

/// A live, explicit claim on a scope. Drops on TTL, drops on explicit
/// release, drops if the parent session drops. No history kept — see
/// the spec's point-in-time invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub claim_id: Uuid,
    pub session_id: Uuid,
    /// Non-empty. Empty intent is rejected at the write surface.
    pub intent: String,
    pub symbol_refs: Vec<SymbolRef>,
    pub declared_at: u64,
    pub ttl_expires_at: u64,
    /// Owning node, embedded in the claim itself (order
    /// `commons-fluency` fix 1). Before this field, a reader resolved
    /// the node through the session record, which replicates slower
    /// than the claim — a peer could read "held, by whom-unknown" for
    /// minutes after a take. The claim is the one canonical carrier.
    ///
    /// `Option` + `#[serde(default)]` is wire-compat: claims written
    /// by an older binary lack the field, and a new reader must not
    /// drop them on deserialization. `None` means "writer predates the
    /// field" — readers fall back to session resolution (named
    /// fallback, logged; never silently substituted).
    #[serde(default)]
    pub node_id: Option<NodeId>,
    /// Claims-rail receipt (order `commons-fluency` fix 3b): when THIS
    /// node first observed the claim. Stamped on the READ path only —
    /// the first local observation of a claim owned by a PEER. Never
    /// set by the origin, never gossiped: a receipt is a local fact,
    /// so it lives in a per-process side map (`WorkAtlasStore`) and
    /// this field is filled in on the way out of the store.
    ///
    /// `None` means one of: this is the origin's own claim (no
    /// receipt needed), the claim predates embedded node_id and
    /// cannot be attributed, or this node has not observed it yet.
    ///
    /// `skip_serializing_if` keeps stored/gossiped bytes identical to
    /// pre-receipt claims; `#[serde(default)]` keeps older binaries'
    /// claims parseable in both directions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
}

/// Durable trace of a TTL-evicted claim (order `commons-fluency`
/// fix 2). Written by GC at eviction and retained for
/// `EXPIRED_TOMBSTONE_TTL_SECS`, so `resource_may_i` can answer
/// "this scope WAS taken and the taker never released — abandoned N
/// minutes ago" for longer than one 60s sweep. Deliberately distinct
/// from `free` (explicit release or never taken).
///
/// Lives in the Public namespace (key prefix `claim-tombstone:`) so
/// the abandonment evidence gossips to peers like the claim did.
/// Tombstones age out on their own retention clock; the idle-session
/// cascade does NOT create them (that path is a session drop, where
/// the spec's point-in-time invariant says records disappear).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimTombstone {
    pub claim_id: Uuid,
    pub session_id: Uuid,
    /// The evicted claim's owner, if the claim carried one (newer
    /// writers embed it; older writers left it absent).
    pub node_id: Option<NodeId>,
    /// Non-empty. Mirrored from the evicted claim for evidence.
    pub intent: String,
    pub symbol_refs: Vec<SymbolRef>,
    /// When the claim's TTL expired (the claim's own clock).
    pub ttl_expires_at: u64,
    /// When GC evicted the claim and wrote this tombstone — the
    /// abandonment moment readers report against.
    pub evicted_at: u64,
}

/// A scope reference inside a claim. SCIP symbol IDs are preferred;
/// when resolution fails, the path-only fallback is recorded with a
/// degraded confidence indicator at read time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRef {
    pub scip_symbol: Option<String>,
    pub file_path: PathBuf,
    /// SCIP graph staleness at write time. Stale references must not
    /// contribute to Active grade at read time.
    pub scip_was_fresh: bool,
}

/// Forward-compatible. Phase 1 defines the shape; no writer exists
/// yet — that lands in `sovereign-work-atlas-observer` (Phase 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationRecord {
    pub session_id: Uuid,
    pub file_path: PathBuf,
    pub source: ObservationSource,
    pub first_observed_at: u64,
    pub last_observed_at: u64,
    pub event_count: u64,
    pub symbol_refs: Vec<SymbolRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    CodeWatcherEdit,
    ToolCallInspect,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ARCH §7.1 / §7.2: structural-invariant pin. If anyone refactors
    // Privacy::app_id() into something that can return a runtime
    // string, this fails before the PR lands.
    #[test]
    fn privacy_app_id_returns_hardcoded_literals() {
        assert_eq!(Privacy::Public.app_id(), "work-atlas");
        assert_eq!(Privacy::Private.app_id(), "work-atlas-private");
    }

    #[test]
    fn privacy_id_roundtrip() {
        for v in [Privacy::Public, Privacy::Private] {
            assert_eq!(Privacy::from_id(v.id()), Some(v));
        }
        assert_eq!(Privacy::from_id("unknown"), None);
    }

    #[test]
    fn agent_kind_id_roundtrip() {
        for v in [AgentKind::Human, AgentKind::Agent] {
            assert_eq!(AgentKind::from_id(v.id()), Some(v));
        }
        assert_eq!(AgentKind::from_id("robot"), None);
    }

    #[test]
    fn session_record_serde_roundtrip() {
        let rec = SessionRecord {
            session_id: Uuid::nil(),
            node_id: NodeId::from_u128(1),
            agent_kind: AgentKind::Human,
            agent_session_token: Some("cli:node-deadbeef".into()),
            repo_id: "a".repeat(64),
            repo_root: PathBuf::from("/tmp/x"),
            current_branch: Some("main".into()),
            privacy: Privacy::Public,
            created_at: 1,
            last_activity_at: 2,
        };
        let bytes = serde_json::to_vec(&rec).unwrap();
        let back: SessionRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.session_id, rec.session_id);
        assert_eq!(back.repo_id, rec.repo_id);
        assert_eq!(back.privacy, rec.privacy);
    }
}
