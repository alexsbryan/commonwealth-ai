// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OICP v0.5 evidence extension's shared half (`cmnwlth/docs/oicp-v0.5.md`
//! §2-§3): a stored text's record as the wire carries it, the published reason
//! for each way a corpus cannot answer, the corpora a caller may read, and
//! search hits carrying their document. The text route
//! ([`crate::routes_oicp_text`]) and both knowledge routes read these, so none
//! spells them a second time.

use std::collections::{BTreeSet, HashMap};

use corpus_index::index::{CorpusIndex, DocumentRecord, TextAbsence};
use corpus_index::types::ScoredChunk;
use kernel_types::Sha256Hash;
use oicp_types::evidence::{reasons, Document, SourceRef};
use oicp_types::KnowledgeResult;
use sovereign_contracts::principal::Principal;
use sovereign_core::context::PrincipalScope;

use crate::state::AppState;

/// A stored text's record on the wire. `Err` only when the stored metadata is
/// not JSON, which the writer never stores: a corrupt record, never read as
/// "none declared".
pub(crate) fn wire_document(rec: &DocumentRecord) -> Result<Document, String> {
    let metadata = match &rec.metadata {
        None => None,
        Some(m) => Some(serde_json::from_str(m).map_err(|e| {
            format!(
                "the record of {} ({}) holds metadata that is not JSON: {e}",
                rec.text_sha256, rec.source_id
            )
        })?),
    };
    Ok(Document {
        text_sha256: rec.text_sha256.to_hex(),
        extractor: rec.extractor.clone(),
        source: SourceRef {
            id: rec.source_id.clone(),
            sha256: rec.source_sha256.map(|s| s.to_hex()),
        },
        metadata,
    })
}

/// The published reason (v0.5 §2.5) for a corpus that cannot answer.
pub(crate) fn absence_reason(absence: TextAbsence) -> &'static str {
    match absence {
        TextAbsence::NotHeld => reasons::TEXT_NOT_HELD,
        TextAbsence::TextsNotStored => reasons::TEXTS_NOT_STORED,
        TextAbsence::TextNotStored => reasons::TEXT_NOT_STORED,
    }
}

/// The corpora this caller may read on the OICP surface, decided by the
/// turn's own decider: [`PrincipalScope`] over the daemon's corpus resolver
/// (`KeyedOwners` on a keyed daemon, the local owner otherwise), keyed the way
/// [`crate::api_keys::Caller::scope`] keys a conversation. A keyed daemon
/// with no resolver attributes nobody, so its callers read nothing.
pub(crate) fn read_scope(state: &AppState, principal: Option<&Principal>) -> PrincipalScope {
    let id = match principal {
        Some(Principal::Asserted { sub, .. }) => {
            crate::api_keys::Caller::Keyed { sub: sub.clone() }.scope("")
        }
        _ => String::new(),
    };
    let node = &state.inner.node;
    let scope = match node.corpus_principal.as_deref() {
        None if node.named_client_tokens.is_keyed() => {
            tracing::warn!(
                "oicp read scope: a keyed daemon holds no corpus resolver; nothing is readable"
            );
            PrincipalScope::Unresolved
        }
        resolver => PrincipalScope::from_resolver(resolver, &id),
    };
    tracing::debug!(principal = ?principal.map(Principal::label), ?scope, "oicp read scope");
    scope
}

/// The record a hit names: the hit's own source's, at its lowest ordinal, or
/// the lowest `(source.id, ordinal)` when no record is its source's.
fn record_for<'a>(
    records: &'a [DocumentRecord],
    source_doc_id: Option<&str>,
) -> Option<&'a DocumentRecord> {
    let own = records
        .iter()
        .filter(|r| Some(r.source_id.as_str()) == source_doc_id)
        .min_by_key(|r| r.ordinal);
    own.or_else(|| {
        records
            .iter()
            .min_by(|a, b| (&a.source_id, a.ordinal).cmp(&(&b.source_id, b.ordinal)))
    })
}

/// The document each hit was cut from, by chunk id (v0.5 §3), read through
/// the chunk's text name. A hit without one — a pre-v4 chunk, a corpus that
/// keeps no texts — is absent from the map, and why is traced.
async fn hit_documents(
    index: &CorpusIndex,
    corpus_id: &str,
    hits: &[ScoredChunk],
) -> HashMap<u64, Document> {
    let mut out = HashMap::new();
    let ids: Vec<u64> = hits.iter().filter_map(|h| h.chunk_id).collect();
    if ids.is_empty() {
        return out;
    }
    let names = match index.chunk_text_sha256s(&ids).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(corpus = corpus_id, error = %e, "knowledge hits: chunk text names unread; hits carry no document");
            return out;
        }
    };
    let wanted: Vec<Sha256Hash> = names
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let records = match index.documents_for(&wanted).await {
        Ok(Ok(r)) => r,
        Ok(Err(absence)) => {
            tracing::debug!(
                corpus = corpus_id,
                reason = absence_reason(absence),
                "knowledge hits: no records; hits carry no document"
            );
            return out;
        }
        Err(e) => {
            tracing::warn!(corpus = corpus_id, error = %e, "knowledge hits: records unread; hits carry no document");
            return out;
        }
    };
    for hit in hits {
        let Some(id) = hit.chunk_id else { continue };
        let Some(name) = names.get(&id) else {
            tracing::debug!(
                corpus = corpus_id,
                chunk = id,
                "knowledge hits: the chunk names no text"
            );
            continue;
        };
        let Some(rec) = records
            .get(name)
            .and_then(|recs| record_for(recs, hit.source_doc_id.as_deref()))
        else {
            tracing::warn!(corpus = corpus_id, chunk = id, %name, "knowledge hits: the chunk names a text with no record");
            continue;
        };
        match wire_document(rec) {
            Ok(d) => {
                out.insert(id, d);
            }
            Err(e) => {
                tracing::error!(corpus = corpus_id, chunk = id, error = %e, "knowledge hits: record not served")
            }
        }
    }
    tracing::debug!(
        corpus = corpus_id,
        hits = hits.len(),
        with_document = out.len(),
        "knowledge hits: documents read"
    );
    out
}

/// A local index's search hits as OICP results, each carrying the record of
/// the text it was cut from (v0.5 §3). The one projection both knowledge
/// routes serve, the client route and the peer route alike, so a peer's hits
/// keep their metadata exactly as local ones do. The v0.2 `metadata` map
/// stays empty: `document.metadata` supersedes it.
pub(crate) async fn knowledge_results(
    index: &CorpusIndex,
    corpus_id: &str,
    hits: Vec<ScoredChunk>,
) -> Vec<KnowledgeResult> {
    let mut documents = hit_documents(index, corpus_id, &hits).await;
    hits.into_iter()
        .map(|r| KnowledgeResult {
            // Provenance the SERVING index stamped, forwarded rather than
            // dropped (TOPOLOGY §10 rung 9.1). `stamped_custody` and not
            // `custody` so "this index recorded no class" stays ABSENT on the
            // wire rather than becoming the string "unknown" — the requester
            // joins absence into a refusal.
            custody: r
                .provenance
                .stamped_custody()
                .map(|c| c.as_str().to_string()),
            grain: Some(r.provenance.grain().as_str().to_string()),
            document: r.chunk_id.and_then(|id| documents.remove(&id)),
            content: r.content,
            title: r.title,
            corpus_id: corpus_id.to_string(),
            url: r.url,
            score: r.score,
            metadata: HashMap::new(),
            chunk_id: r.chunk_id,
            source_doc_id: r.source_doc_id,
            peer_name: None,
            peer_node_id: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(source_id: &str, ordinal: u32) -> DocumentRecord {
        DocumentRecord {
            text_sha256: Sha256Hash::of_str("t"),
            extractor: "plaintext@1".into(),
            source_id: source_id.into(),
            source_sha256: None,
            ordinal,
            metadata: Some(r#"{"b":1,"a":[2]}"#.into()),
            text_stored: true,
        }
    }

    #[test]
    fn a_hit_names_its_own_sources_record() {
        let recs = [rec("b", 0), rec("a", 3), rec("a", 1)];
        assert_eq!(record_for(&recs, Some("b")), Some(&recs[0]));
        assert_eq!(
            record_for(&recs, Some("a")),
            Some(&recs[2]),
            "lowest ordinal"
        );
        assert_eq!(
            record_for(&recs, Some("z")),
            Some(&recs[2]),
            "lowest (source, ordinal)"
        );
    }

    #[test]
    fn a_record_goes_on_the_wire_as_stored() {
        let d = wire_document(&rec("a", 0)).unwrap();
        assert_eq!(d.source.sha256, None, "a record inside a file of many");
        assert_eq!(
            serde_json::to_string(&d.metadata).unwrap(),
            r#"{"b":1,"a":[2]}"#,
            "key order and bytes kept"
        );
        let mut bad = rec("a", 0);
        bad.metadata = Some("{".into());
        assert!(wire_document(&bad).unwrap_err().contains("not JSON"));
    }

    #[test]
    fn each_absence_has_its_published_reason() {
        assert_eq!(absence_reason(TextAbsence::NotHeld), "text not held");
        assert_eq!(
            absence_reason(TextAbsence::TextsNotStored),
            "texts not stored"
        );
        assert_eq!(
            absence_reason(TextAbsence::TextNotStored),
            "text not stored"
        );
    }
}
