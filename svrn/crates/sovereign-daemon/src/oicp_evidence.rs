// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OICP v0.5 evidence extension's shared half (`cmnwlth/docs/oicp-v0.5.md`
//! §2-§3): a stored text's record as the wire carries it, the published reason
//! for each way a corpus cannot answer, the corpora a caller may read, and
//! search hits carrying their document. The text route
//! ([`crate::routes_oicp_text`]), the align route ([`crate::routes_oicp_align`])
//! and both knowledge routes read these, so none spells them a second time.

use std::collections::{BTreeSet, HashMap};

use corpus_index::index::{CorpusIndex, DocumentRecord, TextAbsence};
use corpus_index::types::{IndexInfo, ScoredChunk};
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

/// Which installed corpora a caller's kind may read: one half of
/// [`readable_corpora`]. Never wider than what that caller's
/// `/v1/knowledge/search` reads, since a stored text is the same words as the
/// chunks cut from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorpusReach {
    /// Every installed corpus: the owner's own processes, and a client
    /// presenting a credential. A keyed daemon's grant narrows a credential
    /// through the other half, [`PrincipalScope`].
    Every,
    /// The corpora whose recipe declares `query_sharing`: a mesh member reads
    /// what was declared shared with the mesh, as a federated search does.
    Shared,
    /// Nothing: a guest, or a caller whose identity could not be verified.
    Nothing,
}

impl CorpusReach {
    /// The reach of the request's attached principal. `None` (no principal
    /// attached: the route was mounted outside `client_auth`) reaches nothing.
    ///
    /// `Anonymous` reaches every corpus because `client_auth` admits a caller
    /// that presented nothing only when it is a local process on a listener
    /// that trusts one (a remote one is a 401 before any handler), and that
    /// is the owner's own tool: the desktop, `svrn`, a local OICP client.
    pub(crate) fn of(principal: Option<&Principal>) -> Self {
        match principal {
            Some(Principal::LocalOwner { .. })
            | Some(Principal::Anonymous)
            | Some(Principal::RemoteClient { .. })
            | Some(Principal::Asserted { .. }) => Self::Every,
            Some(Principal::Member { .. }) => Self::Shared,
            Some(Principal::Guest { .. }) | Some(Principal::Unverified) | None => Self::Nothing,
        }
    }

    /// Whether this reach admits `info`'s corpus.
    pub(crate) fn admits(self, info: &IndexInfo) -> bool {
        match self {
            Self::Every => true,
            Self::Shared => info.query_sharing,
            Self::Nothing => false,
        }
    }
}

/// The turn's own decider over this caller: [`PrincipalScope`] over the
/// daemon's corpus resolver (`KeyedOwners` on a keyed daemon, the local owner
/// otherwise), keyed the way [`crate::api_keys::Caller::scope`] keys a
/// conversation. A keyed daemon with no resolver attributes nobody, so its
/// callers read nothing. The other half of [`readable_corpora`].
fn read_scope(state: &AppState, principal: Option<&Principal>) -> PrincipalScope {
    let id = match principal {
        Some(Principal::Asserted { sub, .. }) => {
            crate::api_keys::Caller::Keyed { sub: sub.clone() }.scope("")
        }
        _ => String::new(),
    };
    let node = &state.inner.node;
    match node.corpus_principal.as_deref() {
        None if node.named_client_tokens.is_keyed() => {
            tracing::warn!(
                "oicp read scope: a keyed daemon holds no corpus resolver; nothing is readable"
            );
            PrincipalScope::Unresolved
        }
        resolver => PrincipalScope::from_resolver(resolver, &id),
    }
}

/// The installed corpora this caller may read on the OICP surface, sorted and
/// deduplicated: the one answer the text, align and knowledge routes read. A
/// corpus is readable only when both deciders admit it: the caller's kind
/// ([`CorpusReach`]: a member reaches the `query_sharing` corpora, a guest or
/// an unverified caller nothing) and the turn's own scope ([`read_scope`]:
/// keyed attribution and the corpus grant).
pub(crate) fn readable_corpora(
    state: &AppState,
    principal: Option<&Principal>,
    installed: &[IndexInfo],
) -> Vec<String> {
    let reach = CorpusReach::of(principal);
    let scope = read_scope(state, principal);
    let mut readable: Vec<String> = installed
        .iter()
        .filter(|i| reach.admits(i) && scope.admits(&i.corpus_id))
        .map(|i| i.corpus_id.clone())
        .collect();
    readable.sort();
    readable.dedup();
    tracing::debug!(
        principal = ?principal.map(Principal::label),
        ?reach,
        ?scope,
        installed = installed.len(),
        readable = readable.len(),
        "oicp read scope"
    );
    readable
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

    #[test]
    fn reach_is_the_principal_s_and_nothing_wider() {
        let owner = Principal::LocalOwner { sub_identity: None };
        assert_eq!(CorpusReach::of(Some(&owner)), CorpusReach::Every);
        assert_eq!(
            CorpusReach::of(Some(&Principal::Anonymous)),
            CorpusReach::Every,
            "an anonymous caller past client_auth is a local process"
        );
        let member = Principal::Member {
            node_id: kernel_types::NodeId::from_u128(7),
        };
        assert_eq!(CorpusReach::of(Some(&member)), CorpusReach::Shared);
        let guest = Principal::Guest { grant: "g".into() };
        assert_eq!(CorpusReach::of(Some(&guest)), CorpusReach::Nothing);
        assert_eq!(CorpusReach::of(None), CorpusReach::Nothing);
        assert_eq!(
            CorpusReach::of(Some(&Principal::Unverified)),
            CorpusReach::Nothing
        );
    }
}
