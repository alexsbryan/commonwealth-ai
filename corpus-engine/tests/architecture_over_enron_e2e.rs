//! End-to-end exercise of the architecture-over-Enron substrate
//! (Phases 1-4) on a synthetic mini-corpus.
//!
//! This test stands in for the full Enron run that Phase 5 will
//! eventually drive once the corpus is in hand. It threads:
//!
//! - **Phase 1**: asset store + described-asset dispatcher
//! - **Phase 2**: email RFC-5322 extractor (synthetic message-id +
//!   attachment payload)
//! - **Phase 4**: multi-origin entity reconciliation across three
//!   origins (email body, attached column-aware spreadsheet, judge-
//!   replacement-by-policy)
//! - **Phase 3 scorer**: B³ + pairwise-F1 over the reconciled
//!   clustering against ground truth
//!
//! The synthetic corpus is intentionally small — the test verifies
//! the *plumbing*, not the actual entity-resolution quality at scale.

use std::sync::Arc;

use corpus_engine::asset_store::{AssetStore, FilesystemAssetStore};
use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, Provenance, SignalKind};
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use corpus_engine::enrichment::reconciliation::{reconcile, ReconciliationPolicy};

fn ent(
    id: &str,
    name: &str,
    et: EntityType,
    extractor: &str,
    doc: &str,
    signal: SignalKind,
) -> Entity {
    Entity {
        id: AtomId::from_raw(id),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: et,
        first_appearance: ChunkRef::new("sec-001", None),
        description: String::new(),
        defining_quote: None,
        salience: 0.5,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Provenance::new(extractor, doc, signal),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

#[test]
fn synthetic_three_origin_corpus_collapses_surface_forms() {
    // Three origins, same canonical person, distinct surface forms:
    //   - Email body (LLM batch) → "Ken Lay"
    //   - Email header (email_rfc5322) → "Kenneth Lay" with email
    //   - Column-aware (spread) → "Kenneth L. Lay"
    let mut email_atom = ent(
        "entity-001",
        "Kenneth Lay",
        EntityType::Person,
        "email_rfc5322",
        "msg-1",
        SignalKind::EmailHeader,
    );
    email_atom.aliases.push("klay@enron.com".into());

    let mut body_atom = ent(
        "entity-002",
        "Ken Lay",
        EntityType::Person,
        "llm_batch",
        "msg-1",
        SignalKind::LlmBatch,
    );
    // The email-body extractor sees the same email address inline in
    // the message — set the alias so the email-header signal fires
    // alongside name_similarity for the cross-origin merge.
    body_atom.aliases.push("klay@enron.com".into());

    let mut col_atom = ent(
        "entity-003",
        "Kenneth L. Lay",
        EntityType::Person,
        "column_aware",
        "spread.xlsx",
        SignalKind::ColumnHeader,
    );
    col_atom.aliases.push("klay@enron.com".into());

    // And a control: Jeff Skilling from one origin only — should
    // pass through as a singleton.
    let skilling = ent(
        "entity-004",
        "Jeff Skilling",
        EntityType::Person,
        "email_rfc5322",
        "msg-2",
        SignalKind::EmailHeader,
    );

    let entities = vec![email_atom, body_atom, col_atom, skilling];

    let policy = ReconciliationPolicy::default();
    let outcome = reconcile(entities, &policy);

    // Substrate assertions:
    // - Three Ken Lay surface forms collapse into one canonical
    //   entity (cross-origin merge, ≥2 distinct signals fired:
    //   name_similarity + email_header where applicable).
    // - Jeff Skilling stays as a singleton.
    assert_eq!(
        outcome.entities.len(),
        2,
        "expected 1 canonical Lay + 1 singleton Skilling; got {} entities",
        outcome.entities.len()
    );
    let lay = outcome
        .entities
        .iter()
        .find(|e| e.canonical_name.contains("Lay"))
        .expect("Lay canonical entity must collapse");
    assert_eq!(
        lay.surface_forms.len(),
        3,
        "all three Lay surface forms must collapse under one canonical id"
    );
    let signal_names: Vec<&str> = lay.signals_fired.iter().map(|s| s.as_str()).collect();
    assert!(
        signal_names.contains(&"name_similarity"),
        "name_similarity should fire: {signal_names:?}"
    );
    assert!(
        signal_names.contains(&"email_header"),
        "email_header should fire on the shared klay@enron.com alias: {signal_names:?}"
    );

    // Multi-origin merge writes an oplog entry per collapsed cluster.
    assert_eq!(
        outcome.oplog_entries.len(),
        1,
        "one oplog entry per merge cluster (Lay); Skilling is a singleton, no merge"
    );
    let merge_entry = &outcome.oplog_entries[0];
    assert_eq!(merge_entry.inputs.len(), 3);
}

// Note: the B³ pre-vs-tuned floor delta test lives in
// `sovereign-eval/tests/enron_floor_delta.rs` — that crate owns the
// scorer + the dependency direction (sovereign-eval depends on
// corpus-engine, not the other way around).

#[test]
fn asset_store_and_dispatcher_e2e_with_email_attachment() {
    // Exercise Phase 1's asset store + dispatcher and Phase 2's
    // email extractor's attachment-dispatch path on synthetic data.
    use corpus_engine::extractors::described_asset::AssetSubExtractorRegistry;
    use corpus_engine::extractors::email_rfc5322::{
        EmailAssetDispatch, EmailExtractor, EmailExtractorConfig,
    };
    use corpus_engine::extractors::Extractor;

    let dir = tempfile::tempdir().unwrap();
    let inbox = dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let email_with_attachment = "From: alice@enron.com\r\n\
To: bob@enron.com\r\n\
Subject: see attached counterparty list\r\n\
Date: Tue, 28 May 2026 09:00:00 -0500\r\n\
Message-ID: <attach1@enron.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=BOUNDARY\r\n\
\r\n\
--BOUNDARY\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See the attached.\r\n\
--BOUNDARY\r\n\
Content-Type: text/csv; name=\"counterparties.csv\"\r\n\
Content-Disposition: attachment; filename=\"counterparties.csv\"\r\n\
\r\n\
Counterparty,Notes\r\nDynegy,gas\r\nEl Paso,gas\r\n\
--BOUNDARY--\r\n";
    std::fs::write(inbox.join("1.eml"), email_with_attachment).unwrap();

    let store: Arc<dyn AssetStore> =
        Arc::new(FilesystemAssetStore::new(dir.path().join("assets")).unwrap());
    let dispatch = EmailAssetDispatch {
        store: store.clone(),
        registry: AssetSubExtractorRegistry::defaults(),
        asset_atoms_sidecar: dir.path().join("atlas/asset_atoms.jsonl"),
        asset_edges_sidecar: dir.path().join("atlas/asset_edges.jsonl"),
    };
    let extractor =
        EmailExtractor::new(EmailExtractorConfig::default()).with_asset_dispatch(dispatch.clone());

    let docs: Vec<_> = extractor
        .extract(&inbox)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(docs.len(), 1);

    // Phase 1 asset store has the attachment.
    let entries = store.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].original_filename.as_deref(),
        Some("counterparties.csv")
    );

    // Phase 2 wrote the Attaches edge sidecar.
    let edges = std::fs::read_to_string(&dispatch.asset_edges_sidecar).unwrap();
    assert!(edges.contains("Attaches"));
    // The Asset atom sidecar carries the filename (the edge schema
    // doesn't — it's a typed pointer at the atom).
    let atom_sidecar = std::fs::read_to_string(&dispatch.asset_atoms_sidecar).unwrap();
    assert!(atom_sidecar.contains("counterparties.csv"));
}
