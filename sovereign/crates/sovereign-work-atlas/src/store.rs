// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed facade over a [`PeerStore`].
//!
//! Hides the bytes/app_id/serde dance from callers. Every mutating
//! method routes through `Privacy::app_id()` so a Private record
//! cannot be written to the public namespace by accident.
//!
//! The store is the PORT, not the mesh (cw-lift 3b). It was
//! `commonwealth_state::MeshStore` until then, which meant a capability crate
//! could not be built, tested or run without the mesh substrate — even though
//! the four methods it calls (`get`/`set`/`delete`/`scan`) are the same four
//! a node with no peers answers correctly on its own. A daemon on a live mesh
//! passes the mesh adapter and every record gossips; a solo one passes
//! `SoloPeerStore` and the atlas still works, because replication to zero
//! peers is the identity function.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use kernel_types::NodeId;
use serde::Serialize;
use sovereign_contracts::peer::{PeerStore, PeerStoreError};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{
    ClaimRecord, ClaimTombstone, ObservationRecord, Privacy, SessionRecord, SymbolRef,
};

#[derive(Debug, Error)]
pub enum WorkAtlasError {
    #[error("intent must not be empty")]
    EmptyIntent,

    #[error("ttl_seconds {requested} exceeds configured max {max}")]
    TtlExceedsMax { requested: u64, max: u64 },

    #[error("claim {0} not found")]
    ClaimNotFound(Uuid),

    #[error("peer store error: {0}")]
    Store(#[from] PeerStoreError),

    #[error("serialize: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Identity scope for session deduplication. A new Session is created
/// when no existing one matches all three fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    pub node_id: NodeId,
    pub agent_session_token: Option<String>,
    pub repo_id: String,
}

/// Typed wrapper around a [`PeerStore`] for the work atlas.
#[derive(Clone)]
pub struct WorkAtlasStore {
    store: Arc<dyn PeerStore>,
    node_id: NodeId,
    /// Claims-rail receipt stamps (order `commons-fluency` fix 3b):
    /// `claim_id -> unix seconds` of THIS node's first local
    /// observation of a PEER-owned claim. Read side only — a claim's
    /// arrival is a local fact, so the stamp lives in a side map
    /// rather than in the gossiped record bytes (the origin must not
    /// receive its own claim back with a receipt it never gave).
    /// Dies at process restart, exactly like the claims themselves
    /// (the store is in-memory).
    received_at: Arc<Mutex<HashMap<Uuid, u64>>>,
}

// `PeerStore` is not a `Debug` bound — surface a placeholder shape so
// the work-atlas tools (which derive `Debug`) compile.
impl std::fmt::Debug for WorkAtlasStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkAtlasStore")
            .field("node_id", &self.node_id)
            .finish()
    }
}

impl WorkAtlasStore {
    pub fn new(store: Arc<dyn PeerStore>, node_id: NodeId) -> Self {
        Self {
            store,
            node_id,
            received_at: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    // ── Sessions ──────────────────────────────────────────────────────────

    /// Find an existing session matching `identity` and `privacy`, or
    /// create one. Idempotent on `(node_id, agent_session_token,
    /// repo_id)` — the same caller invoking twice returns the same
    /// session_id.
    pub fn ensure_session(
        &self,
        identity: SessionIdentity,
        privacy: Privacy,
        agent_kind: crate::model::AgentKind,
        repo_root: PathBuf,
        current_branch: Option<String>,
    ) -> Result<SessionRecord, WorkAtlasError> {
        if let Some(existing) = self.find_session_by_identity(&identity, privacy)? {
            // Bump last_activity_at; otherwise leave the record alone.
            let mut updated = existing;
            updated.last_activity_at = now_secs();
            self.put_session(&updated)?;
            return Ok(updated);
        }
        let now = now_secs();
        let session = SessionRecord {
            session_id: Uuid::new_v4(),
            node_id: self.node_id,
            agent_kind,
            agent_session_token: identity.agent_session_token.clone(),
            repo_id: identity.repo_id.clone(),
            repo_root,
            current_branch,
            privacy,
            created_at: now,
            last_activity_at: now,
        };
        self.put_session(&session)?;
        tracing::debug!(
            session_id = %session.session_id,
            privacy = privacy.id(),
            agent_kind = session.agent_kind.id(),
            repo_id = %short_hash(&session.repo_id),
            "work_atlas:session_created"
        );
        Ok(session)
    }

    fn find_session_by_identity(
        &self,
        identity: &SessionIdentity,
        privacy: Privacy,
    ) -> Result<Option<SessionRecord>, WorkAtlasError> {
        let app_id = privacy.app_id();
        for entry in self.store.scan(app_id, "session:")? {
            let rec: SessionRecord = serde_json::from_slice(&entry.value)?;
            if rec.node_id == identity.node_id
                && rec.repo_id == identity.repo_id
                && rec.agent_session_token == identity.agent_session_token
            {
                return Ok(Some(rec));
            }
        }
        Ok(None)
    }

    pub fn put_session(&self, rec: &SessionRecord) -> Result<(), WorkAtlasError> {
        let key = format!("session:{}", rec.session_id);
        write_record(
            self.store.as_ref(),
            rec.privacy.app_id(),
            &key,
            rec,
            self.node_id,
        )
    }

    pub fn get_session(&self, session_id: Uuid) -> Result<Option<SessionRecord>, WorkAtlasError> {
        for app_id in [Privacy::Public.app_id(), Privacy::Private.app_id()] {
            let key = format!("session:{session_id}");
            if let Some(entry) = self.store.get(app_id, &key)? {
                let rec: SessionRecord = serde_json::from_slice(&entry.value)?;
                return Ok(Some(rec));
            }
        }
        Ok(None)
    }

    pub fn delete_session(
        &self,
        session_id: Uuid,
        privacy: Privacy,
    ) -> Result<bool, WorkAtlasError> {
        let key = format!("session:{session_id}");
        Ok(self.store.delete(privacy.app_id(), &key)?)
    }

    /// Scan all sessions across both privacy namespaces.
    pub fn scan_sessions(&self) -> Result<Vec<SessionRecord>, WorkAtlasError> {
        let mut out = Vec::new();
        for app_id in [Privacy::Public.app_id(), Privacy::Private.app_id()] {
            for entry in self.store.scan(app_id, "session:")? {
                let rec: SessionRecord = serde_json::from_slice(&entry.value)?;
                out.push(rec);
            }
        }
        Ok(out)
    }

    // ── Claims ────────────────────────────────────────────────────────────

    /// Write a claim. The caller is responsible for non-empty intent
    /// and TTL clamping — the tool layer enforces both.
    pub fn put_claim(
        &self,
        parent_privacy: Privacy,
        rec: &ClaimRecord,
    ) -> Result<(), WorkAtlasError> {
        if rec.intent.trim().is_empty() {
            return Err(WorkAtlasError::EmptyIntent);
        }
        let key = format!("claim:{}", rec.claim_id);
        write_record(
            self.store.as_ref(),
            parent_privacy.app_id(),
            &key,
            rec,
            self.node_id,
        )
    }

    pub fn release_claim(&self, claim_id: Uuid) -> Result<bool, WorkAtlasError> {
        // Try both namespaces; the claim could be in either.
        let key = format!("claim:{claim_id}");
        let mut removed = false;
        for app_id in [Privacy::Public.app_id(), Privacy::Private.app_id()] {
            if self.store.delete(app_id, &key)? {
                removed = true;
            }
        }
        Ok(removed)
    }

    /// Claims-rail receipt (fix 3b): stamp `received_at` on the first
    /// local observation of a PEER-owned claim. Own claims
    /// (node_id == self) and unattributable claims (node_id `None` —
    /// writer predates fix 1) are never stamped: a receipt only means
    /// something when the origin is known to be elsewhere. Idempotent —
    /// the first observation wins and later reads keep that stamp.
    fn stamp_received_at(&self, rec: &mut ClaimRecord) {
        // Stamp only claims owned by a KNOWN peer. Own claims need no
        // receipt; unattributable claims (node_id `None` — writer
        // predates fix 1) must not get one either, because the stamp
        // would falsely imply the origin is elsewhere.
        if rec.node_id == Some(self.node_id) || rec.node_id.is_none() {
            return;
        }
        let mut map = self
            .received_at
            .lock()
            .expect("work-atlas receipt map poisoned");
        let stamp = *map.entry(rec.claim_id).or_insert_with(now_secs);
        rec.received_at = Some(stamp);
    }

    /// Find the claim and its app_id without consuming.
    pub fn get_claim(
        &self,
        claim_id: Uuid,
    ) -> Result<Option<(Privacy, ClaimRecord)>, WorkAtlasError> {
        for (privacy, app_id) in [
            (Privacy::Public, Privacy::Public.app_id()),
            (Privacy::Private, Privacy::Private.app_id()),
        ] {
            let key = format!("claim:{claim_id}");
            if let Some(entry) = self.store.get(app_id, &key)? {
                let mut rec: ClaimRecord = serde_json::from_slice(&entry.value)?;
                self.stamp_received_at(&mut rec);
                return Ok(Some((privacy, rec)));
            }
        }
        Ok(None)
    }

    /// Scan all claims under one privacy namespace.
    pub fn scan_claims(&self, privacy: Privacy) -> Result<Vec<ClaimRecord>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self.store.scan(privacy.app_id(), "claim:")? {
            let mut rec: ClaimRecord = serde_json::from_slice(&entry.value)?;
            self.stamp_received_at(&mut rec);
            out.push(rec);
        }
        Ok(out)
    }

    /// Write an eviction tombstone (GC only). Public namespace —
    /// abandonment evidence must gossip like the claim did.
    pub fn put_tombstone(&self, rec: &ClaimTombstone) -> Result<(), WorkAtlasError> {
        let key = format!("claim-tombstone:{}", rec.claim_id);
        write_record(
            self.store.as_ref(),
            Privacy::Public.app_id(),
            &key,
            rec,
            self.node_id,
        )
    }

    /// Delete a tombstone by claim id. Used by GC's retention sweep.
    /// Idempotent.
    pub fn delete_tombstone(&self, claim_id: Uuid) -> Result<bool, WorkAtlasError> {
        let key = format!("claim-tombstone:{claim_id}");
        Ok(self.store.delete(Privacy::Public.app_id(), &key)?)
    }

    /// Scan every tombstone in the Public namespace.
    pub fn scan_tombstones(&self) -> Result<Vec<ClaimTombstone>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self
            .store
            .scan(Privacy::Public.app_id(), "claim-tombstone:")?
        {
            let rec: ClaimTombstone = serde_json::from_slice(&entry.value)?;
            out.push(rec);
        }
        Ok(out)
    }

    /// All tombstones whose `symbol_refs` match `scope` — the
    /// abandonment evidence for `resource_may_i`'s expired verdict.
    pub fn list_tombstones_for_scope(
        &self,
        scope: &str,
        match_mode: ScopeMatch,
    ) -> Result<Vec<ClaimTombstone>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self
            .store
            .scan(Privacy::Public.app_id(), "claim-tombstone:")?
        {
            let rec: ClaimTombstone = serde_json::from_slice(&entry.value)?;
            if rec
                .symbol_refs
                .iter()
                .any(|sr| matches_scope(sr, scope, match_mode))
            {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// All Public claims whose `symbol_refs` match `scope`.
    ///
    /// `match_mode`:
    /// - `"symbol"` → exact SCIP symbol id match
    /// - `"file"`   → file_path equality or prefix
    pub fn list_claims_for_scope(
        &self,
        scope: &str,
        match_mode: ScopeMatch,
    ) -> Result<Vec<ClaimRecord>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self.store.scan(Privacy::Public.app_id(), "claim:")? {
            let mut rec: ClaimRecord = serde_json::from_slice(&entry.value)?;
            self.stamp_received_at(&mut rec);
            if rec
                .symbol_refs
                .iter()
                .any(|sr| matches_scope(sr, scope, match_mode))
            {
                out.push(rec);
            }
        }
        Ok(out)
    }

    // ── Observations ──────────────────────────────────────────────────────

    /// Build the store key for an observation. Embeds `session_id`
    /// + `file_path` so the (session, path) tuple is the natural
    /// primary key. Path separators are kept verbatim — `scan` uses
    /// the full prefix `observation:<session_id>:`.
    pub fn observation_key(session_id: Uuid, file_path: &std::path::Path) -> String {
        format!("observation:{}:{}", session_id, file_path.to_string_lossy())
    }

    /// Upsert an observation. The caller decides the parent session's
    /// privacy — the same write goes to `work-atlas-private` for
    /// Private sessions and is structurally barred from gossip.
    pub fn put_observation(
        &self,
        parent_privacy: Privacy,
        rec: &ObservationRecord,
    ) -> Result<(), WorkAtlasError> {
        let key = Self::observation_key(rec.session_id, &rec.file_path);
        write_record(
            self.store.as_ref(),
            parent_privacy.app_id(),
            &key,
            rec,
            self.node_id,
        )
    }

    /// Fetch a single observation. Tries both privacy namespaces.
    pub fn get_observation(
        &self,
        session_id: Uuid,
        file_path: &std::path::Path,
    ) -> Result<Option<(Privacy, ObservationRecord)>, WorkAtlasError> {
        let key = Self::observation_key(session_id, file_path);
        for (privacy, app_id) in [
            (Privacy::Public, Privacy::Public.app_id()),
            (Privacy::Private, Privacy::Private.app_id()),
        ] {
            if let Some(entry) = self.store.get(app_id, &key)? {
                let rec: ObservationRecord = serde_json::from_slice(&entry.value)?;
                return Ok(Some((privacy, rec)));
            }
        }
        Ok(None)
    }

    /// Delete one observation. Idempotent.
    pub fn delete_observation(
        &self,
        privacy: Privacy,
        session_id: Uuid,
        file_path: &std::path::Path,
    ) -> Result<bool, WorkAtlasError> {
        let key = Self::observation_key(session_id, file_path);
        Ok(self.store.delete(privacy.app_id(), &key)?)
    }

    /// All Public observations whose `file_path` matches `scope` under
    /// `match_mode`. Phase 2 observations are file-level: `symbol`
    /// mode matches when `file_path` equals `scope`; `file` mode
    /// supports prefix matching.
    pub fn list_observations_for_scope(
        &self,
        scope: &str,
        match_mode: ScopeMatch,
    ) -> Result<Vec<ObservationRecord>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self.store.scan(Privacy::Public.app_id(), "observation:")? {
            let rec: ObservationRecord = serde_json::from_slice(&entry.value)?;
            let path = rec.file_path.to_string_lossy();
            let hit = match match_mode {
                ScopeMatch::Symbol => path == scope,
                ScopeMatch::File => path == scope || path.starts_with(scope),
            };
            if hit {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// Scan every observation under one privacy namespace. Used by GC
    /// for cascade eviction.
    pub fn scan_observations(
        &self,
        privacy: Privacy,
    ) -> Result<Vec<ObservationRecord>, WorkAtlasError> {
        let mut out = Vec::new();
        for entry in self.store.scan(privacy.app_id(), "observation:")? {
            let rec: ObservationRecord = serde_json::from_slice(&entry.value)?;
            out.push(rec);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMatch {
    Symbol,
    File,
}

fn matches_scope(sr: &SymbolRef, scope: &str, mode: ScopeMatch) -> bool {
    match mode {
        ScopeMatch::Symbol => {
            // Prefer the SCIP-resolved symbol. When the writer didn't
            // resolve (Phase 1: SCIP resolution deferred — `scip_symbol`
            // is `None`), fall back to exact equality on the
            // `file_path` slot, which the writer used as a stringly
            // store for the user's input. This keeps "declare X then
            // query X finds it" working in Phase 1; Phase 2 plus SCIP
            // resolution promotes the comparison to graph-distance.
            if let Some(sym) = sr.scip_symbol.as_deref() {
                return sym == scope;
            }
            sr.file_path.to_string_lossy() == scope
        }
        ScopeMatch::File => {
            let path = sr.file_path.to_string_lossy();
            path == scope || path.starts_with(scope)
        }
    }
}

fn write_record<T: Serialize>(
    store: &dyn PeerStore,
    app_id: &str,
    key: &str,
    rec: &T,
    origin: NodeId,
) -> Result<(), WorkAtlasError> {
    let bytes = serde_json::to_vec(rec)?;
    store.set(app_id, key, Bytes::from(bytes), origin)?;
    Ok(())
}

use sovereign_core::time::unix_now_u64 as now_secs;

fn short_hash(s: &str) -> &str {
    &s[..12.min(s.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentKind;
    use sovereign_contracts::peer::{PeerStore, SoloPeerStore};

    fn mk_store() -> WorkAtlasStore {
        let mesh = Arc::new(SoloPeerStore::new());
        WorkAtlasStore::new(mesh as Arc<dyn PeerStore>, NodeId::from_u128(1))
    }

    fn sample_session(privacy: Privacy) -> SessionRecord {
        SessionRecord {
            session_id: Uuid::new_v4(),
            node_id: NodeId::from_u128(1),
            agent_kind: AgentKind::Agent,
            agent_session_token: Some("conn:abc".into()),
            repo_id: "a".repeat(64),
            repo_root: PathBuf::from("/tmp/x"),
            current_branch: Some("main".into()),
            privacy,
            created_at: 0,
            last_activity_at: 0,
        }
    }

    /// ARCH §7.2 invariant pin: a Private session lands ONLY in the
    /// private namespace. If `Privacy::app_id()` is ever changed to
    /// pick the wrong namespace for Private, this fails.
    #[test]
    fn private_session_writes_only_to_private_app_id() {
        let s = mk_store();
        let priv_rec = sample_session(Privacy::Private);
        s.put_session(&priv_rec).unwrap();

        // Public namespace must NOT contain the private record.
        let public_hits = s.store.scan("work-atlas", "session:").unwrap();
        assert!(
            public_hits.is_empty(),
            "private record leaked to public namespace"
        );

        // Private namespace MUST contain it.
        let private_hits = s.store.scan("work-atlas-private", "session:").unwrap();
        assert_eq!(private_hits.len(), 1);
    }

    /// The other half of the mirror test in `peer_preferences`. If
    /// `Privacy::Private.app_id()` ever drifts away from the literal
    /// listed in `GOSSIP_EXCLUDED_APP_IDS`, this fails.
    #[test]
    fn private_app_id_matches_gossip_exclusion_list() {
        use commonwealth_state::peer_preferences::is_gossip_excluded;
        assert!(is_gossip_excluded(Privacy::Private.app_id()));
        assert!(!is_gossip_excluded(Privacy::Public.app_id()));
    }

    #[test]
    fn put_claim_rejects_empty_intent() {
        let s = mk_store();
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            intent: "   ".into(),
            symbol_refs: vec![],
            declared_at: 0,
            ttl_expires_at: 0,
            node_id: Some(NodeId::from_u128(1)),
            received_at: None,
        };
        let res = s.put_claim(Privacy::Public, &claim);
        assert!(matches!(res, Err(WorkAtlasError::EmptyIntent)));
    }

    #[test]
    fn ensure_session_is_idempotent_on_identity_triple() {
        let s = mk_store();
        let identity = SessionIdentity {
            node_id: NodeId::from_u128(1),
            agent_session_token: Some("conn:a".into()),
            repo_id: "r".repeat(64),
        };
        let first = s
            .ensure_session(
                identity.clone(),
                Privacy::Public,
                AgentKind::Agent,
                PathBuf::from("/tmp/x"),
                None,
            )
            .unwrap();
        let second = s
            .ensure_session(
                identity,
                Privacy::Public,
                AgentKind::Agent,
                PathBuf::from("/tmp/x"),
                None,
            )
            .unwrap();
        assert_eq!(first.session_id, second.session_id);
    }

    #[test]
    fn release_claim_clears_both_namespaces() {
        let s = mk_store();
        let claim_id = Uuid::new_v4();
        let claim = ClaimRecord {
            claim_id,
            session_id: Uuid::new_v4(),
            intent: "tuning".into(),
            symbol_refs: vec![],
            declared_at: 0,
            ttl_expires_at: u64::MAX,
            node_id: Some(NodeId::from_u128(1)),
            received_at: None,
        };
        s.put_claim(Privacy::Public, &claim).unwrap();
        assert!(s.release_claim(claim_id).unwrap());
        // Idempotent: second release is a no-op (returns false).
        assert!(!s.release_claim(claim_id).unwrap());
    }

    #[test]
    fn list_claims_for_scope_matches_symbol_id() {
        let s = mk_store();
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            intent: "tuning".into(),
            symbol_refs: vec![SymbolRef {
                scip_symbol: Some("Module::ingest".into()),
                file_path: PathBuf::from("src/m.rs"),
                scip_was_fresh: true,
            }],
            declared_at: 0,
            ttl_expires_at: u64::MAX,
            node_id: Some(NodeId::from_u128(1)),
            received_at: None,
        };
        s.put_claim(Privacy::Public, &claim).unwrap();
        let hits = s
            .list_claims_for_scope("Module::ingest", ScopeMatch::Symbol)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let misses = s
            .list_claims_for_scope("Module::other", ScopeMatch::Symbol)
            .unwrap();
        assert!(misses.is_empty());
    }
}
