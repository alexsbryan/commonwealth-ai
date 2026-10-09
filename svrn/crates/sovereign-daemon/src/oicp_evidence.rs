// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OICP v0.5 evidence extension's shared half (`cmnwlth/docs/oicp-v0.5.md`
//! §2-§3): a stored text's record as the wire carries it, the published reason
//! for each way a corpus cannot answer, and the corpora a caller may read.
//! The text route ([`crate::routes_oicp_text`]) reads these, so none is
//! spelled a second time.

use corpus_index::index::{DocumentRecord, TextAbsence};
use oicp_types::evidence::{reasons, Document, SourceRef};
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

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::Sha256Hash;

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
