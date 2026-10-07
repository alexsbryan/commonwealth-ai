// SPDX-License-Identifier: AGPL-3.0-or-later
//! `change.document` end to end: a recipe names the metadata fields that
//! place a document; chunk rows — the build's LanceDB loader's own type —
//! put two messages in ONE section; Phase-1 sketches resolve through the
//! shipped resolver; every claim comes out stamped from ITS document, and
//! one that cannot be placed comes out unstamped and counted.
//!
//! The fixture's field names (`sent`, `conversation`, `msg`) are
//! deliberately not the mail extractor's: nothing but the declaration may
//! say which field is the date.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::Claim;
use corpus_engine::enrichment::atlas::{
    resolve_entities_and_events_with, resolve_step_3b_with, stamp_claim_documents,
    DocumentStampReport, ResolutionPolicy, SectionDocuments,
};
use corpus_engine::enrichment::ontology::OntologyPolicies;
use corpus_engine::enrichment::pipeline::atlas::{
    ClaimSketch, DiscourseAct, EnrichmentDepth, EpistemicStatus, SectionExtraction,
};
use corpus_engine::enrichment::pipeline::types::PhaseFailureKind;
use corpus_engine::types::EmbedFn;
use corpus_index::index::EnrichmentChunkRow;

use super::ontology_recipe::policies_of;

const DECLARED: &str = r#"version = 1
[[enrichment.ontology.types]]
name = "deal"
kind = "entity"
[[enrichment.ontology.types]]
name = "deal_act"
kind = "claim"
force = "commissive"
subject = "deal"
[enrichment.ontology.change]
document = { date = "sent", thread = "conversation", id = "msg" }"#;

fn fake_embed() -> EmbedFn {
    Arc::new(move |s: &str| {
        let n = s.len() as f32;
        Box::pin(async move { Ok(vec![n, 1.0, 0.0]) })
    })
}

fn row(id: u64, doc: &str, content: &str, meta: serde_json::Value) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: content.into(),
        title: Some("RE: June gas".into()),
        url: None,
        metadata_raw: Some(meta.to_string()),
        source_doc_id: Some(doc.into()),
    }
}

fn meta(sent: &str, conversation: &str, msg: &str) -> serde_json::Value {
    serde_json::json!({ "sent": sent, "conversation": conversation, "msg": msg })
}

fn act(content: &str, anchor: &str) -> ClaimSketch {
    ClaimSketch {
        content: content.into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        attributed_to: None,
        quotable_excerpt: None,
        anchor: anchor.into(),
        claim_kind: Some("deal_act".into()),
        subject: None,
        scope: None,
        attributes: Default::default(),
    }
}

fn section(id: &str, claims: Vec<ClaimSketch>) -> SectionExtraction {
    SectionExtraction {
        section_id: id.into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_introduced: Vec::new(),
        entities_developed: Vec::new(),
        relations_introduced: Vec::new(),
        relations_developed: Vec::new(),
        events: Vec::new(),
        claims,
        questions_raised: Vec::new(),
        argument_reconstructions: Vec::new(),
        type_extension: None,
        type_extensions: Vec::new(),
    }
}

/// Resolve the sketches the way `atlas_resolve.rs` does, then stamp the
/// claims from `documents` under the recipe's own declaration.
async fn resolve_and_stamp(
    policies: &OntologyPolicies,
    sections: Vec<SectionExtraction>,
    documents: &SectionDocuments,
) -> (Vec<Claim>, DocumentStampReport) {
    let policy = ResolutionPolicy::new(policies);
    let step_3a = resolve_entities_and_events_with(&sections, &fake_embed(), &policy, Vec::new())
        .await
        .expect("3a resolves");
    let step_3b = resolve_step_3b_with(&sections, &step_3a.entities, &step_3a.events, &policy)
        .expect("3b resolves");
    let mut claims = step_3b.claims;
    let decl = policies
        .change
        .document
        .as_ref()
        .expect("the recipe declares change.document");
    let report = stamp_claim_documents(&mut claims, documents, decl);
    (claims, report)
}

fn stamp<'a>(c: &'a Claim, attr: &str) -> Option<&'a str> {
    c.attributes.get(attr).and_then(|v| v.as_str())
}

/// Two messages of one thread-by-subject in ONE section, plus a section
/// holding one message. Message A spans two chunks.
fn mailbox() -> SectionDocuments {
    let a = meta(
        "Mon, 14 May 2001 16:39:00 -0700 (PDT)",
        "<root-1@x>",
        "<a@x>",
    );
    let b = meta("2001-05-15T09:05:00Z", "<root-2@x>", "<b@x>");
    let c = meta("2001-05-16", "<root-3@x>", "<c@x>");
    let rows = vec![
        row(
            10,
            "mail-a",
            "We offer 5,000 MMBtu at Henry Hub for June.",
            a.clone(),
        ),
        row(11, "mail-a", "Delivery starts\non the first.", a),
        row(12, "mail-b", "We accept the offer at Henry Hub.", b),
        row(20, "mail-c", "Confirmed for the record.", c),
    ];
    SectionDocuments::from_chunk_rows(
        [
            ("sec_00001", &[10u64, 11, 12][..]),
            ("sec_00002", &[20u64][..]),
        ],
        &rows,
    )
}

#[tokio::test]
async fn each_claim_is_stamped_from_its_own_document() {
    let policies = policies_of(DECLARED);
    let sections = vec![
        section(
            "sec_00001",
            vec![
                act(
                    "A offers gas at Henry Hub.",
                    "We offer 5,000 MMBtu at Henry Hub",
                ),
                act("B accepts.", "We accept the offer"),
                // Second chunk of message A, anchor re-wrapped by the model.
                act("Delivery from the first.", "Delivery starts on the\nfirst."),
            ],
        ),
        // One document in the section: no anchor is needed to place it.
        section("sec_00002", vec![act("C confirms.", "")]),
    ];
    let (claims, report) = resolve_and_stamp(&policies, sections, &mailbox()).await;
    assert_eq!(claims.len(), 4);

    let expect = [
        ("2001-05-14T23:39:00Z", "<root-1@x>", "<a@x>"),
        ("2001-05-15T09:05:00Z", "<root-2@x>", "<b@x>"),
        ("2001-05-14T23:39:00Z", "<root-1@x>", "<a@x>"),
        ("2001-05-16", "<root-3@x>", "<c@x>"),
    ];
    for (c, (date, thread, id)) in claims.iter().zip(expect) {
        assert_eq!(stamp(c, "document_date"), Some(date), "{}", c.content);
        assert_eq!(stamp(c, "document_thread"), Some(thread), "{}", c.content);
        assert_eq!(stamp(c, "document_id"), Some(id), "{}", c.content);
    }
    assert_eq!(report.claims, 4);
    assert_eq!(report.located, 4);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.stamped.get("document_date"), Some(&4));
}

/// The planted failures. An anchor in no document, an anchor in BOTH
/// documents, and a date that is not a date: none may be guessed, each is
/// counted.
#[tokio::test]
async fn an_unplaceable_claim_or_unreadable_date_is_counted_not_stamped() {
    let policies = policies_of(DECLARED);
    let mut docs_rows = vec![row(
        30,
        "mail-d",
        "Counter at 4.10.",
        meta("the day after tomorrow", "<root-4@x>", "<d@x>"),
    )];
    docs_rows.push(row(
        12,
        "mail-b",
        "We accept the offer at Henry Hub.",
        meta("2001-05-15T09:05:00Z", "<root-2@x>", "<b@x>"),
    ));
    docs_rows.push(row(
        10,
        "mail-a",
        "We offer 5,000 MMBtu at Henry Hub for June.",
        meta("2001-05-14", "<root-1@x>", "<a@x>"),
    ));
    let docs = SectionDocuments::from_chunk_rows(
        [("sec_00001", &[10u64, 12][..]), ("sec_00003", &[30u64][..])],
        &docs_rows,
    );
    let sections = vec![
        section(
            "sec_00001",
            vec![
                act("Nobody wrote this.", "a sentence neither message holds"),
                act("Henry Hub is named.", "at Henry Hub"),
            ],
        ),
        section("sec_00003", vec![act("D counters.", "Counter at 4.10.")]),
        // A section the manifest never listed.
        section("sec_00009", vec![act("Orphan.", "anything")]),
    ];
    let (claims, report) = resolve_and_stamp(&policies, sections, &docs).await;

    for c in &claims[..2] {
        assert!(
            c.attributes.keys().all(|k| !k.starts_with("document_")),
            "an unplaceable claim carries no stamp: {} {:?}",
            c.content,
            c.attributes
        );
    }
    assert!(claims[3]
        .attributes
        .keys()
        .all(|k| !k.starts_with("document_")));
    // Placed, so thread and id land — but the date that does not parse does not.
    assert_eq!(stamp(&claims[2], "document_date"), None);
    assert_eq!(stamp(&claims[2], "document_thread"), Some("<root-4@x>"));
    assert_eq!(stamp(&claims[2], "document_id"), Some("<d@x>"));

    assert_eq!(report.located, 1);
    assert_eq!(report.count(PhaseFailureKind::UnresolvedClaimDocument), 3);
    assert_eq!(report.count(PhaseFailureKind::UnreadableDocumentField), 1);
    let reasons: Vec<&str> = report.failures.iter().map(|f| f.reason.as_str()).collect();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("in none of section `sec_00001`'s 2 documents")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.contains("lands in 2 documents")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("holds no document metadata")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("neither RFC 2822 nor ISO 8601")),
        "{reasons:?}"
    );
    assert!(
        report.summary().contains("1 of 4 claim(s) located"),
        "{}",
        report.summary()
    );
}

#[test]
fn an_unknown_key_in_change_document_refuses_at_load() {
    let err = super::ontology_recipe::load_err(
        r#"version = 1
[enrichment.ontology.change]
document = { date = "sent", thred = "conversation" }"#,
    );
    assert!(err.contains("thred"), "{err}");
}
