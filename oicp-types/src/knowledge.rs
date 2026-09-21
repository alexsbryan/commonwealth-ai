// SPDX-License-Identifier: AGPL-3.0-or-later
//! Knowledge search API (v0.3 §6) and the landscape-digest surface
//! (§6.5).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::manifest::EmbedModelInfo;

// -----------------------------------------------------------------
// Section 6 — Knowledge Search API
// -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchRequest {
    /// Pre-computed query embedding. OPTIONAL as of v0.4: when empty,
    /// the HOST embeds `query_text` with its advertised
    /// [`EmbedModelInfo::query_instruction_prefix`] — the OICP contract
    /// is thin-client (the host owns the embed model), so a client need
    /// only send text. Mesh peers still pre-embed and send this to
    /// avoid re-embedding on every hop; when present it is used as-is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_embedding: Vec<f32>,
    /// The query text. `query` is accepted as an alias — it is the
    /// natural OICP thin-client field name; `query_text` is retained
    /// for the mesh-internal shape.
    #[serde(default, alias = "query")]
    pub query_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpora: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl KnowledgeSearchRequest {
    /// The default result limit per §6.1 when `limit` is omitted.
    pub const DEFAULT_LIMIT: u32 = 20;

    /// Effective result limit, applying the §6.1 default of 20.
    pub fn effective_limit(&self) -> u32 {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeSearchResponse {
    pub results: Vec<KnowledgeResult>,
    pub corpora_searched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unavailable: Vec<String>,
    /// The subset of `corpora_unavailable` that NO live member advertises —
    /// named apart so "nobody hosts it" reads differently from "its host is
    /// offline". Every entry here is also in `corpora_unavailable`, so a
    /// reader that predates this field still sees the corpus as missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unhosted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_chunks_searched: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub corpus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub score: f32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Stable LanceDB row id for the chunk on the producing peer.
    /// Lets the desktop's reading surface deref a citation back to
    /// the source chunk (see ENRICHMENT_V2 / glass-box reading
    /// surface plan). `None` for synthetic chunks (atlas-virtual,
    /// local-doc) and for older peers that haven't been upgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// Document grouping key for "elsewhere in this document"
    /// lookups and for chunk-neighbor ordering. `None` when the
    /// extractor didn't tag chunks with a document id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
    /// The custody class the SERVING index recorded, in the wire spelling
    /// `kernel_types::Custody::as_str` defines (`public-web` | `personal` |
    /// `peer`). `None` means the serving side recorded none — which is
    /// different from `unknown` and must stay different: the requester joins
    /// this with its own "arrived from another node" fact and refuses on
    /// absence.
    ///
    /// Deliberately a `String` and not `kernel_types::Custody`. `oicp-types`
    /// is pinned to ZERO internal dependencies (the `[[package_leaf]]` budget
    /// in `quality/ARCH_LAYERS.toml`) so the protocol crate stays liftable by
    /// a third party; the wire spelling is the contract and
    /// `Custody::parse_wire` is its one parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody: Option<String>,
    /// `leaf` | `summary` — whether the serving index vouched for this as
    /// source text or as prose ABOUT source text
    /// (`kernel_types::Grain::as_str`).
    ///
    /// Added 2026-08-26. Before it the requester could not tell a peer-served
    /// RAPTOR rollup from a peer-served passage, so it had to treat every mesh
    /// hit as unciteable content built in-process. `None` from a peer that
    /// predates this field, and the requester must read absence as `summary`,
    /// the refusing value — a rollup wrongly marked `leaf` becomes quotable,
    /// which is the direction that fabricates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
    /// The member name of the peer that served this passage, stamped by the
    /// requester's fan-out. `None` for a locally-served hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
    /// That peer's node id, stamped beside `peer_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_node_id: Option<String>,
}

// -----------------------------------------------------------------
// Corpus shard advertisement (mesh gossip capability report)
// -----------------------------------------------------------------
//
// Named by BOTH sides — `commonwealth-core` builds the capability report
// that carries these, `sovereign-mesh` fills them in from the local index,
// and `sovereign-tools`' atlas peer-advice rule reads them — so the schema
// lives in the protocol leaf rather than in either side's crate
// (quality/ARCH_LAYERS.toml package closures). Moved from
// `commonwealth_core::knowledge` 2026-09-21; no field changed.

/// A contiguous range of chunk IDs within a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    /// First chunk ID (inclusive).
    pub start_id: u64,
    /// Last chunk ID (exclusive).
    pub end_id: u64,
}

impl ChunkRange {
    pub fn new(start_id: u64, end_id: u64) -> Self {
        debug_assert!(start_id < end_id, "empty chunk range: {start_id}..{end_id}");
        Self { start_id, end_id }
    }

    pub fn count(&self) -> u64 {
        self.end_id - self.start_id
    }
}

/// Information about a corpus shard hosted on a node.
/// Used in capability reports.
///
/// ## Phase: canonical-sync surface (Phase 6 of the resilience track)
///
/// Three new fields drive the mesh's self-healing canonical sync:
///
/// - `chunk_count`: how many chunks this peer's canonical contains.
///   Compared by the auto-recover path to pick the healthier peer
///   when several have a canonical for the same id.
/// - `canonical_fingerprint`: blake3 of the sorted content_hash list
///   for the canonical at this peer. Two peers with byte-identical
///   chunks arrive at the same string. The puller validates this
///   value against the file it actually downloaded so a poisoned
///   tarball fails closed.
/// - `total_shards` + `processed_shards`: lets a peer compute its
///   coverage ratio (`processed / total`) for sharded corpora.
///   Auto-recover compares ratios — not raw chunk counts — to pick
///   the most-complete peer, which is robust to legitimate corpus
///   updates that shrink the chunk set.
///
/// All three are `Option`/`Vec`-defaulted so older peers (whose
/// gossip blobs predate this struct) deserialize cleanly. A peer
/// missing the fields just opts out of the new sync paths until
/// it upgrades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusShardInfo {
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
    pub last_updated: u64,
    /// Total chunks in this peer's canonical (or partition).
    /// Defaults to 0 for older peers; auto-recover treats `0` as
    /// "unknown" rather than "empty" — peers with a fingerprint
    /// but no chunk_count are eligible to pull from but not
    /// rankable by count.
    #[serde(default)]
    pub chunk_count: u64,
    /// Stable content fingerprint for the canonical. See
    /// `corpus_engine::IndexInfo::canonical_fingerprint` for the
    /// algorithm. `None` for partitions and for canonicals that
    /// haven't been stamped yet (the daemon's lazy-stamp pass on
    /// next start fills these in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_fingerprint: Option<String>,
    /// Total source shards this corpus expects (e.g. 38 for the
    /// canonical Wikipedia ingest). `None` for non-sharded corpora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_shards: Option<usize>,
    /// Source shards this peer's canonical (or partition) has
    /// processed. The auto-recover path takes the union of these
    /// across peers to compute coverage ratios.
    #[serde(default)]
    pub processed_shards: Vec<usize>,
    // ── Atlas advertisement (Phase C1) ──────────────────────────
    //
    // These three fields let a peer joining the mesh decide
    // whether to pull this corpus's atlas instead of running
    // local Tier-2 enrichment. All three default to 0 / `None` so
    // older peers (and peers whose corpus has no atlas yet)
    // serialise + deserialise cleanly with no protocol break.
    /// Total atoms (entities + events + …) in this peer's
    /// `<corpus>/atlas/atoms.json`. `0` means "no atlas yet" or
    /// "older peer that doesn't advertise atlas state."
    #[serde(default)]
    pub atlas_atom_count: u64,
    /// Entities at `enrichment_depth = "extracted"` (Tier-2
    /// enriched). The mesh ranks atlases by this — a peer with a
    /// higher count has done more deep-extraction work and is the
    /// preferred atlas source for fresh nodes.
    #[serde(default)]
    pub atlas_tier2_count: u64,
    /// SHA-256 of `atoms.json` (hex). Receipt the puller validates
    /// against after fetching a peer's atlas — a corrupted /
    /// poisoned transfer fails closed. `None` = no atlas or
    /// fingerprint not yet stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_fingerprint: Option<String>,
}

impl CorpusShardInfo {
    /// Coverage ratio for sharded corpora — `processed_shards.len()
    /// / total_shards`. Returns `None` for non-sharded corpora
    /// (`total_shards.is_none()`) and for the degenerate case
    /// `total_shards = 0`. Used by `auto_recover` to pick the
    /// most-complete peer.
    pub fn coverage_ratio(&self) -> Option<f64> {
        let total = self.total_shards?;
        if total == 0 {
            return None;
        }
        Some(self.processed_shards.len() as f64 / total as f64)
    }
}

// -----------------------------------------------------------------
// Section 6.5 — Knowledge Landscape Digest API
// -----------------------------------------------------------------
//
// The daemon-side `KnowledgeViewManager` exposes its assembled
// digests via `POST /v1/knowledge/landscape_digest`, so an attached
// desktop (which does NOT construct its own manager — see
// `AppState::is_attach_mode`) can splice the same prompt blocks the
// daemon would. Wire shape mirrors the existing
// `LandscapeDigest` type in `sovereign-core::types`; we redefine it
// here to keep `oicp-types` a leaf crate with no upstream Sovereign
// deps. The receiving side maps between the two.

/// One assembled landscape-digest block (e.g. personal-knowledge,
/// conversation-history, cross-view, relational, strategic). The
/// `body` is markdown ready to splice; the `view_id` lets clients
/// dedupe / re-order if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LandscapeDigestEntry {
    pub view_id: String,
    pub body: String,
}

/// Request body for `POST /v1/knowledge/landscape_digest`. All
/// fields are optional — the simplest valid request is `{}`,
/// equivalent to "give me the unconstrained digest set with no
/// active-skill privacy filter and no in-conversation context."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestRequest {
    /// Active skill id. Today this is informational only; reserved
    /// for v2 skill-tiered digest work. The daemon does NOT
    /// introspect it for privacy gating — see
    /// `active_is_local_only` for that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_skill: Option<String>,
    /// Caller-resolved "the active skill has privacy = local_only"
    /// flag. The desktop has the canonical skill registry and
    /// computes this against `SkillRegistry::local_only_skill_ids`;
    /// the daemon trusts the flag and applies it directly. This
    /// design keeps the daemon out of the skill-registry business
    /// while preserving the splice-time privacy filter (a
    /// `local_only` session must NOT receive
    /// conversational/institutional/cross-view blocks).
    #[serde(default)]
    pub active_is_local_only: bool,
    /// In-conversation message contents. Drives the "this entity is
    /// already on screen, don't re-introduce it" predicate in the
    /// relational/strategic blocks. Empty = no in-conv suppression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_messages: Vec<String>,
}

/// Response shape — a flat list of digests in the order the daemon
/// would have spliced them. The desktop calls
/// `ConversationContext::set_landscape_digests` with the converted
/// list and the runtime treats it identically to a locally-spliced
/// payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestResponse {
    pub digests: Vec<LandscapeDigestEntry>,
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_range_count() {
        let r = ChunkRange::new(0, 1000);
        assert_eq!(r.count(), 1000);
    }

    #[test]
    fn chunk_range_serde_roundtrip() {
        let r = ChunkRange::new(500, 1500);
        let json = serde_json::to_string(&r).unwrap();
        let back: ChunkRange = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn corpus_shard_info_serde_roundtrips_atlas_fields() {
        let info = CorpusShardInfo {
            corpus_id: "wikipedia".into(),
            chunk_range: None,
            is_replica: false,
            last_updated: 0,
            chunk_count: 1_000_000,
            canonical_fingerprint: Some("abc123".into()),
            total_shards: Some(38),
            processed_shards: vec![0, 1, 2],
            atlas_atom_count: 51_280,
            atlas_tier2_count: 612,
            atlas_fingerprint: Some(
                "7c3f8e9b1f0a2d3c4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d".into(),
            ),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: CorpusShardInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.atlas_atom_count, 51_280);
        assert_eq!(back.atlas_tier2_count, 612);
        assert!(back.atlas_fingerprint.unwrap().starts_with("7c3f"));
    }

    /// A blob from an older peer (pre-C1) won't carry the atlas
    /// fields. Deserialize must succeed with zero counts and `None`
    /// fingerprint so the upgrade window is graceful.
    #[test]
    fn corpus_shard_info_back_compat_without_atlas_fields() {
        let json = r#"{
            "corpus_id": "wikipedia",
            "chunk_range": null,
            "is_replica": false,
            "last_updated": 0,
            "chunk_count": 0,
            "processed_shards": []
        }"#;
        let back: CorpusShardInfo = serde_json::from_str(json).unwrap();
        assert_eq!(back.atlas_atom_count, 0);
        assert_eq!(back.atlas_tier2_count, 0);
        assert!(back.atlas_fingerprint.is_none());
    }

    #[test]
    fn knowledge_result_legacy_json_deserialises_with_none_chunk_id() {
        // Older peers (pre reading-surface plumbing) emit
        // KnowledgeResult JSON without the chunk_id / source_doc_id
        // fields. Verify they deserialise cleanly to None so
        // wire-compat is preserved across mixed-version meshes.
        let legacy = r#"{
            "content": "Alyosha Karamazov is a novice",
            "title": "The Brothers Karamazov",
            "corpus_id": "brothers_karamazov",
            "url": null,
            "score": 0.87,
            "metadata": {}
        }"#;
        let parsed: KnowledgeResult = serde_json::from_str(legacy).expect("deserialise");
        assert_eq!(parsed.chunk_id, None);
        assert_eq!(parsed.source_doc_id, None);
        assert_eq!(parsed.corpus_id, "brothers_karamazov");
        // A peer that predates the provenance fields says NOTHING about them,
        // and the requester must read that as absence — never as a class.
        // `acquired_from_peer` turns both `None`s into the refusing value
        // (`Custody::Unknown` through the join, `Grain::Summary`), so an
        // un-upgraded peer's hits stay exactly as unquotable as they were.
        assert_eq!(parsed.custody, None);
        assert_eq!(parsed.grain, None);

        // And a forward-compat round-trip preserves every field.
        let modern = KnowledgeResult {
            content: "passage".into(),
            title: Some("title".into()),
            corpus_id: "bk".into(),
            url: None,
            score: 0.5,
            metadata: Default::default(),
            chunk_id: Some(42),
            source_doc_id: Some("bk-ch01".into()),
            custody: Some("public-web".into()),
            grain: Some("summary".into()),
            peer_name: None,
            peer_node_id: None,
        };
        let json = serde_json::to_string(&modern).unwrap();
        let back: KnowledgeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chunk_id, Some(42));
        assert_eq!(back.source_doc_id.as_deref(), Some("bk-ch01"));
        assert_eq!(back.custody.as_deref(), Some("public-web"));
        assert_eq!(back.grain.as_deref(), Some("summary"));

        // An OLD peer reading a NEW payload must not choke on the additions —
        // the other half of interop, and the half a `skip_serializing_if`
        // does not prove on its own.
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct LegacyShape {
            content: String,
            corpus_id: String,
            score: f32,
        }
        let old_reader: LegacyShape =
            serde_json::from_str(&json).expect("a new payload still parses as the old shape");
        assert_eq!(old_reader.corpus_id, "bk");

        // A recorded-nothing stamp stays ABSENT on the wire rather than
        // becoming the string "unknown" — the two mean different things to
        // the join on the far side.
        let silent = KnowledgeResult {
            custody: None,
            grain: None,
            ..modern
        };
        let json = serde_json::to_string(&silent).unwrap();
        assert!(
            !json.contains("custody"),
            "absence must not be serialised: {json}"
        );
        assert!(
            !json.contains("grain"),
            "absence must not be serialised: {json}"
        );
    }

    #[test]
    fn knowledge_search_thin_client_shape_deserializes() {
        // OICP v0.4 §6.1: a thin client sends only `query` — no embedding,
        // and the OICP field name `query` (not `query_text`).
        let req: KnowledgeSearchRequest =
            serde_json::from_value(serde_json::json!({"query": "stoic virtue", "limit": 3}))
                .unwrap();
        assert_eq!(req.query_text, "stoic virtue");
        assert!(req.query_embedding.is_empty(), "host embeds when absent");
        assert_eq!(req.effective_limit(), 3);
    }

    #[test]
    fn knowledge_search_mesh_shape_still_deserializes() {
        // The mesh-internal shape (pre-embedded, `query_text`) is unchanged.
        let req: KnowledgeSearchRequest = serde_json::from_value(serde_json::json!({
            "query_embedding": [0.1, 0.2, 0.3],
            "query_text": "stoic virtue",
        }))
        .unwrap();
        assert_eq!(req.query_embedding, vec![0.1, 0.2, 0.3]);
        assert_eq!(req.query_text, "stoic virtue");
    }

    #[test]
    fn knowledge_search_empty_embedding_omitted_from_wire() {
        // An absent embedding must not serialize as `query_embedding: []`.
        let req = KnowledgeSearchRequest {
            query_embedding: Vec::new(),
            query_text: "q".into(),
            corpora: None,
            limit: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("query_embedding").is_none());
    }
}
