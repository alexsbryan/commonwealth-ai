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
    /// is pinned to ZERO internal dependencies (`boundary_gate::
    /// allowed_leaf_deps`) so the protocol crate stays liftable by a third
    /// party; the wire spelling is the contract and `Custody::parse_wire` is
    /// its one parser.
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
