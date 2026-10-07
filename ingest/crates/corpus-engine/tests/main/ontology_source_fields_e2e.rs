// SPDX-License-Identifier: AGPL-3.0-or-later
//! `source = { metadata = … }` end to end: a recipe names the document fields
//! a type comes from and the reader for each attribute; chunk rows (the
//! build's LanceDB loader's own type) carry the fields; projection makes one
//! atom per identity value; Phase-1 sketches resolve through the shipped
//! resolver, and a model atom carrying a projected atom's identity value
//! merges into it.
//!
//! Two fixtures, one code path, different field names: mail-like `sender` /
//! `recipients` read as address lists, and an issue tracker's `author` read
//! whole. Neither is the mail extractor's spelling, and the attribute names
//! (`mailbox`, `site`, `login`) are not the readers' names: nothing but the
//! declaration says which field is which.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::{Entity, SignalKind};
use corpus_engine::enrichment::atlas::{
    project_source_atoms, resolve_entities_and_events_with, ResolutionOutput, ResolutionPolicy,
    SectionDocuments, SourceProjection,
};
use corpus_engine::enrichment::ontology::{validate_block, OntologyPolicies};
use corpus_engine::enrichment::pipeline::atlas::{
    EnrichmentDepth, EntitySketch, EntityType, SectionExtraction,
};
use corpus_engine::enrichment::pipeline::types::PhaseFailureKind;
use corpus_engine::types::EmbedFn;
use corpus_engine::Recipe;
use corpus_index::index::EnrichmentChunkRow;

use super::ontology_recipe::{load_err, policies_of, recipe_with_ontology};

const MAIL: &str = r#"version = 1
[[enrichment.ontology.types]]
name = "person"
kind = "entity"
attributes = [{ name = "mailbox", type = "text" }, { name = "full_name", type = "text" }]
identity = ["mailbox"]
source = { metadata = ["sender", "recipients"], attributes = { mailbox = "address", full_name = "display_name" } }
[[enrichment.ontology.types]]
name = "company"
kind = "entity"
attributes = [{ name = "site", type = "text" }]
identity = ["site"]
source = { metadata = ["sender", "recipients"], attributes = { site = "domain" }, exclude = ["freemail.example"] }"#;

const ISSUES: &str = r#"version = 1
[[enrichment.ontology.types]]
name = "contributor"
kind = "entity"
attributes = [{ name = "login", type = "text" }]
identity = ["login"]
source = { metadata = ["author"], attributes = { login = "value" } }"#;

fn fake_embed() -> EmbedFn {
    Arc::new(move |s: &str| {
        let n = s.len() as f32;
        Box::pin(async move { Ok(vec![n, 1.0, 0.0]) })
    })
}

fn row(id: u64, doc: &str, meta: serde_json::Value) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: format!("body of {doc}"),
        title: None,
        url: None,
        metadata_raw: Some(meta.to_string()),
        source_doc_id: Some(doc.into()),
    }
}

fn sketch(name: &str, ty: &str, attrs: serde_json::Value) -> EntitySketch {
    EntitySketch {
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::from_str_repr(ty),
        description: format!("{name}, as the model read it."),
        defining_quote: None,
        anchor: String::new(),
        attributes: attrs.as_object().cloned().unwrap_or_default(),
    }
}

fn section(id: &str, entities: Vec<EntitySketch>) -> SectionExtraction {
    SectionExtraction {
        section_id: id.into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_introduced: entities,
        entities_developed: Vec::new(),
        relations_introduced: Vec::new(),
        relations_developed: Vec::new(),
        events: Vec::new(),
        claims: Vec::new(),
        questions_raised: Vec::new(),
        argument_reconstructions: Vec::new(),
        type_extension: None,
        type_extensions: Vec::new(),
    }
}

/// Project, then resolve the sketches the way `atlas_resolve.rs` does.
async fn project_and_resolve(
    policies: &OntologyPolicies,
    documents: &SectionDocuments,
    sections: Vec<SectionExtraction>,
) -> (SourceProjection, ResolutionOutput) {
    let projection = project_source_atoms(documents, policies, "fixture").expect("projects");
    let policy = ResolutionPolicy::new(policies);
    let out = resolve_entities_and_events_with(
        &sections,
        &fake_embed(),
        &policy,
        projection.atoms.clone(),
    )
    .await
    .expect("3a resolves");
    (projection, out)
}

fn attr<'a>(e: &'a Entity, key: &str) -> Option<&'a str> {
    e.attributes.get(key).and_then(|v| v.as_str())
}

fn of_type<'a>(out: &'a ResolutionOutput, ty: &str) -> Vec<&'a Entity> {
    out.entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == ty)
        .collect()
}

fn mailbox() -> SectionDocuments {
    let rows = vec![
        row(
            1,
            "m1",
            serde_json::json!({ "sender": "Ann Lee <ann@acme.org>",
                "recipients": "bob@beta.com, Carol <carol@freemail.example>" }),
        ),
        row(
            2,
            "m2",
            serde_json::json!({ "sender": "bob@beta.com", "recipients": "\"Lee, Ann\" <ANN@acme.org>" }),
        ),
        // A recipients field with no address in it: recorded, never guessed.
        row(
            3,
            "m3",
            serde_json::json!({ "sender": "Ann Lee <ann@acme.org>", "recipients": "undisclosed-recipients:;" }),
        ),
        row(4, "m4", serde_json::json!({ "recipients": "bob@beta.com" })),
    ];
    SectionDocuments::from_chunk_rows(
        [("sec_00001", &[1u64, 2][..]), ("sec_00002", &[3u64, 4][..])],
        &rows,
    )
}

#[tokio::test]
async fn mail_fields_project_people_and_companies_and_model_atoms_merge_on_the_key() {
    let policies = policies_of(MAIL);
    let sections = vec![section(
        "sec_00001",
        vec![
            sketch(
                "Ann Lee",
                "person",
                serde_json::json!({ "mailbox": "ann@acme.org" }),
            ),
            sketch(
                "Robert Bee",
                "person",
                serde_json::json!({ "mailbox": "Bob@Beta.com" }),
            ),
            sketch(
                "Dave Dee",
                "person",
                serde_json::json!({ "mailbox": "dave@delta.org" }),
            ),
            sketch(
                "Acme Corp",
                "company",
                serde_json::json!({ "site": "acme.org" }),
            ),
            sketch(
                "Freemail",
                "company",
                serde_json::json!({ "site": "freemail.example" }),
            ),
        ],
    )];
    let (projection, out) = project_and_resolve(&policies, &mailbox(), sections).await;

    let people = projection
        .report
        .types
        .get("person")
        .expect("person projected");
    let companies = projection
        .report
        .types
        .get("company")
        .expect("company projected");
    assert_eq!(people.projected, 3, "{people:?}");
    assert_eq!(companies.projected, 2, "{companies:?}");
    assert_eq!(companies.excluded, 1, "carol's freemail domain");
    assert_eq!(people.absent.get("sender"), Some(&1), "{people:?}");
    assert_eq!(people.unreadable, 1, "{people:?}");
    let reasons: Vec<&str> = projection
        .report
        .failures
        .iter()
        .map(|f| f.reason.as_str())
        .collect();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("`recipients`") && r.contains("holds no address")),
        "{reasons:?}"
    );
    assert!(projection
        .report
        .failures
        .iter()
        .all(|f| f.kind == PhaseFailureKind::UnreadableDocumentField));

    // One atom per identity value, the excluded domain nowhere.
    let mut sites: Vec<&str> = of_type(&out, "company")
        .iter()
        .filter_map(|e| attr(e, "site"))
        .collect();
    sites.sort_unstable();
    assert_eq!(sites, ["acme.org", "beta.com", "freemail.example"]);
    let freemail: Vec<&Entity> = of_type(&out, "company")
        .into_iter()
        .filter(|e| attr(e, "site") == Some("freemail.example"))
        .collect();
    assert_eq!(freemail.len(), 1);
    assert_eq!(
        freemail[0].provenance.signal_kind,
        SignalKind::LlmBatch,
        "only the model's atom: the excluded domain projected nothing"
    );

    let people_out = of_type(&out, "person");
    let by_mailbox = |m: &str| -> Vec<&Entity> {
        people_out
            .iter()
            .copied()
            .filter(|e| attr(e, "mailbox") == Some(m))
            .collect()
    };
    let ann = by_mailbox("ann@acme.org");
    assert_eq!(
        ann.len(),
        1,
        "the model's Ann merged into the projected one"
    );
    let ann = ann[0];
    assert_eq!(ann.provenance.signal_kind, SignalKind::DocumentField);
    assert_eq!(
        ann.canonical_name, "Ann Lee",
        "the most frequent display name"
    );
    assert!(
        ann.aliases.iter().any(|a| a == "Lee, Ann"),
        "{:?}",
        ann.aliases
    );
    assert_eq!(ann.first_appearance.chunk_id, "sec_00001");
    assert_eq!(ann.provenance.source_doc_id, "m1");
    assert_eq!(
        ann.attributes.get("document_count"),
        Some(&serde_json::json!(3))
    );
    assert_eq!(ann.description, "Ann Lee, as the model read it.");
    assert_eq!(ann.enrichment_depth, EnrichmentDepth::Structural);

    // Folded case-insensitively on the identity fold. No header names Bob, so
    // the model's name does and the bare address stays reachable as an alias:
    // a read name, then a model's, then the key.
    let bob = by_mailbox("bob@beta.com");
    assert_eq!(bob.len(), 1, "{bob:?}");
    assert_eq!(bob[0].canonical_name, "Robert Bee");
    assert!(
        bob[0].aliases.iter().any(|a| a == "bob@beta.com"),
        "{:?}",
        bob[0].aliases
    );
    assert!(
        !bob[0].aliases.iter().any(|a| a == "Robert Bee"),
        "the name is not also its own alias: {:?}",
        bob[0].aliases
    );
    assert!(
        people_out
            .iter()
            .all(|e| attr(e, "mailbox") != Some("Bob@Beta.com")),
        "the model atom is gone, not kept beside its merge"
    );
    let carol = by_mailbox("carol@freemail.example");
    assert_eq!(carol[0].canonical_name, "Carol");
    // A model atom whose key no document carries is left alone.
    let dave = by_mailbox("dave@delta.org");
    assert_eq!(dave.len(), 1);
    assert_eq!(dave[0].provenance.signal_kind, SignalKind::LlmBatch);

    assert_eq!(out.sources.projected, 5);
    assert_eq!(out.sources.merged.get("person"), Some(&2));
    assert_eq!(out.sources.merged.get("company"), Some(&1));
    assert!(out.sources.refused.is_empty(), "{:?}", out.sources.refused);
    // Ann's header named her; Bob and Acme took their model atom's name.
    assert_eq!(out.sources.named.get("person"), Some(&1));
    assert_eq!(out.sources.named.get("company"), Some(&1));
    let acme: Vec<&Entity> = of_type(&out, "company")
        .into_iter()
        .filter(|e| attr(e, "site") == Some("acme.org"))
        .collect();
    assert_eq!(acme.len(), 1);
    assert_eq!(acme[0].canonical_name, "Acme Corp");
    assert!(acme[0].aliases.iter().any(|a| a == "acme.org"));
    let beta: Vec<&Entity> = of_type(&out, "company")
        .into_iter()
        .filter(|e| attr(e, "site") == Some("beta.com"))
        .collect();
    assert_eq!(
        beta[0].canonical_name, "beta.com",
        "no model atom folded in: the key names it"
    );

    // Identity from essence: the same value is the same id on every run.
    let again = project_source_atoms(&mailbox(), &policies, "fixture").unwrap();
    let ids = |p: &SourceProjection| p.atoms.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&projection), ids(&again));
    let lines = projection.report.summary_lines(&out.sources);
    assert!(
        lines.iter().any(
            |l| l.starts_with("source person ← sender, recipients: 3 atom(s)")
                && l.contains("2 model atom(s) merged on mailbox (0 refused, 1 named an atom)")
        ),
        "{lines:?}"
    );
}

#[tokio::test]
async fn an_issue_author_field_read_whole_projects_contributors() {
    let policies = policies_of(ISSUES);
    let rows = vec![
        row(
            1,
            "issue-1",
            serde_json::json!({ "author": "charliermarsh", "kind": "issue" }),
        ),
        row(2, "comment-2", serde_json::json!({ "author": "zanieb" })),
        row(
            3,
            "comment-3",
            serde_json::json!({ "author": "charliermarsh" }),
        ),
        row(4, "comment-4", serde_json::json!({ "author": ["konstin"] })),
        row(5, "comment-5", serde_json::json!({ "author": true })),
    ];
    let docs = SectionDocuments::from_chunk_rows(
        [
            ("sec_00001", &[1u64, 2, 3][..]),
            ("sec_00002", &[4u64, 5][..]),
        ],
        &rows,
    );
    let sections = vec![section(
        "sec_00001",
        vec![sketch(
            "Charlie Marsh",
            "contributor",
            serde_json::json!({ "login": "charliermarsh" }),
        )],
    )];
    let (projection, out) = project_and_resolve(&policies, &docs, sections).await;

    let mut logins: Vec<(&str, &str)> = of_type(&out, "contributor")
        .iter()
        .map(|e| (attr(e, "login").unwrap(), e.canonical_name.as_str()))
        .collect();
    logins.sort_unstable();
    assert_eq!(
        logins,
        [
            ("charliermarsh", "Charlie Marsh"),
            ("konstin", "konstin"),
            ("zanieb", "zanieb")
        ]
    );
    let charlie = of_type(&out, "contributor")
        .into_iter()
        .find(|e| attr(e, "login") == Some("charliermarsh"))
        .unwrap();
    assert!(
        charlie.aliases.iter().any(|a| a == "charliermarsh"),
        "{:?}",
        charlie.aliases
    );
    assert_eq!(
        charlie.attributes.get("document_count"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(out.sources.merged.get("contributor"), Some(&1));
    let r = projection.report.types.get("contributor").unwrap();
    assert_eq!(
        r.unreadable, 1,
        "a boolean author is recorded, not read: {r:?}"
    );
    assert!(
        projection.report.failures[0]
            .reason
            .contains("holds a boolean"),
        "{:?}",
        projection.report.failures
    );
}

// ── The declaration: refused at load, or by validate ────────────────────────

fn person_with_source(source: &str) -> String {
    format!(
        r#"version = 1
[[enrichment.ontology.types]]
name = "person"
kind = "entity"
attributes = [{{ name = "email", type = "text" }}]
identity = ["email"]
source = {source}"#
    )
}

fn validate(body: &str) -> corpus_engine::enrichment::ontology::OntologyValidation {
    let recipe = Recipe::from_toml(&recipe_with_ontology(body)).expect("loads");
    validate_block(recipe.ontology_block().unwrap())
}

#[test]
fn an_unknown_reader_is_refused_at_load() {
    let err = load_err(&person_with_source(
        r#"{ metadata = ["from"], attributes = { email = "adress" } }"#,
    ));
    assert!(
        err.contains("adress") && err.contains("display_name"),
        "{err}"
    );
}

#[test]
fn an_unknown_key_in_a_metadata_source_is_refused_at_load() {
    let err = load_err(&person_with_source(
        r#"{ metadata = ["from"], attributes = { email = "address" }, exclude_domains = ["aol.com"] }"#,
    ));
    assert!(err.contains("exclude_domains"), "{err}");
}

#[test]
fn a_source_naming_both_forms_or_neither_is_refused() {
    let both = load_err(&person_with_source(
        r#"{ file = "people.csv", metadata = ["from"], attributes = { email = "address" } }"#,
    ));
    assert!(both.contains("both"), "{both}");
    let neither = load_err(&person_with_source(
        r#"{ attributes = { email = "address" } }"#,
    ));
    assert!(neither.contains("neither"), "{neither}");
}

#[test]
fn validate_refuses_an_undeclared_source_attribute_and_an_unread_identity() {
    let v = validate(&person_with_source(
        r#"{ metadata = ["from"], attributes = { mail = "address" } }"#,
    ));
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("`mail` is not a declared attribute of `person`")),
        "{:?}",
        v.errors
    );
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("identity key `email` is not a source attribute")),
        "{:?}",
        v.errors
    );
}

#[test]
fn validate_prints_where_a_sourced_type_comes_from() {
    let v = validate(&person_with_source(
        r#"{ metadata = ["from", "to", "cc"], attributes = { email = "address" }, exclude = ["aol.com"] }"#,
    ));
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.notes.iter().any(|n| n
            == "source: person ← document fields from, to, cc (email: address) — one atom per \
                identity value, model atoms with that value merge into it; 1 value(s) excluded"),
        "{:?}",
        v.notes
    );
}

// ── refs: a role read from the same mailbox, linked to another sourced type ──

/// MAIL with Contact -> Account declared: a person's `works_at` is the company
/// atom its own address's domain keys. The attribute is not called `employer`:
/// only the declaration says which attribute links to what.
const MAIL_REFS: &str = r#"version = 1
[[enrichment.ontology.types]]
name = "person"
kind = "entity"
attributes = [{ name = "mailbox", type = "text" }, { name = "works_at", type = "ref", of = "company" }]
identity = ["mailbox"]
source = { metadata = ["sender", "recipients"], attributes = { mailbox = "address" }, refs = { works_at = { of = "company", reader = "domain" } } }
[[enrichment.ontology.types]]
name = "company"
kind = "entity"
attributes = [{ name = "site", type = "text" }]
identity = ["site"]
source = { metadata = ["sender", "recipients"], attributes = { site = "domain" }, exclude = ["freemail.example"] }"#;

#[tokio::test]
async fn a_ref_links_a_person_to_the_company_its_address_domain_keys_and_survives_resolution() {
    let policies = policies_of(MAIL_REFS);
    let sections = vec![section(
        "sec_00001",
        vec![sketch(
            "Ann Lee",
            "person",
            serde_json::json!({ "mailbox": "ann@acme.org", "works_at": "Acme Corporation" }),
        )],
    )];
    let (projection, out) = project_and_resolve(&policies, &mailbox(), sections).await;
    let report = projection
        .report
        .types
        .get("person")
        .expect("person projected");
    assert_eq!(
        report.refs_linked.get("works_at"),
        Some(&2),
        "ann and bob link: {report:?}"
    );
    assert_eq!(
        report.refs_unlinked.get("works_at"),
        Some(&1),
        "carol's freemail links nothing: {report:?}"
    );

    let company = |site: &str| {
        of_type(&out, "company")
            .into_iter()
            .find(|e| attr(e, "site") == Some(site))
            .map(|e| e.id.as_str().to_string())
            .expect("company projected")
    };
    let person = |mailbox: &str| {
        of_type(&out, "person")
            .into_iter()
            .find(|e| attr(e, "mailbox") == Some(mailbox))
            .expect("person projected")
    };
    // the model's raw name does not displace the linked id: attributes merge first-wins
    assert_eq!(
        attr(person("ann@acme.org"), "works_at"),
        Some(company("acme.org").as_str())
    );
    assert_eq!(
        attr(person("bob@beta.com"), "works_at"),
        Some(company("beta.com").as_str())
    );
    assert_eq!(attr(person("carol@freemail.example"), "works_at"), None);
}

/// With no `exclude` declared, an address at a mailbox provider names no
/// company: `dan@hotmail.com` and `eve@email.msn.com` (a provider's subdomain)
/// project none, and their people link to none. Read off the atoms, not only
/// off the report the projection writes about itself.
#[tokio::test]
async fn an_address_at_a_mailbox_provider_names_no_company_with_no_exclude_declared() {
    let policies = policies_of(&MAIL_REFS.replace(r#", exclude = ["freemail.example"]"#, ""));
    let rows = vec![row(
        1,
        "m1",
        serde_json::json!({ "sender": "Ann Lee <ann@acme.org>",
            "recipients": "Dan <dan@hotmail.com>, eve@email.msn.com" }),
    )];
    let documents = SectionDocuments::from_chunk_rows([("sec_00001", &[1u64][..])], &rows);
    let (projection, out) =
        project_and_resolve(&policies, &documents, vec![section("sec_00001", vec![])]).await;
    let companies = projection
        .report
        .types
        .get("company")
        .expect("company projected");
    assert_eq!(
        companies.providers, 2,
        "dan's and eve's domains: {companies:?}"
    );
    assert_eq!(companies.excluded, 0, "nothing declared: {companies:?}");
    let sites: Vec<&str> = of_type(&out, "company")
        .into_iter()
        .filter_map(|e| attr(e, "site"))
        .collect();
    assert_eq!(sites, ["acme.org"]);
    let works_at = |mailbox: &str| {
        of_type(&out, "person")
            .into_iter()
            .find(|e| attr(e, "mailbox") == Some(mailbox))
            .and_then(|e| attr(e, "works_at"))
    };
    assert!(works_at("ann@acme.org").is_some());
    assert_eq!(works_at("dan@hotmail.com"), None);
    assert_eq!(works_at("eve@email.msn.com"), None);
}

#[test]
fn validate_refuses_a_ref_on_a_non_ref_attribute_and_a_target_without_a_source() {
    let v = validate(&MAIL_REFS.replace(
        r#"{ name = "works_at", type = "ref", of = "company" }"#,
        r#"{ name = "works_at", type = "text" }"#,
    ));
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("ref `works_at` is not a declared `ref` attribute of `person`")),
        "{:?}",
        v.errors
    );
    let v = validate(&MAIL_REFS.replace(
        r#", exclude = ["freemail.example"] }"#,
        r#" }"#,
    ).replace(
        r#"source = { metadata = ["sender", "recipients"], attributes = { site = "domain" } }"#,
        "",
    ));
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("links to `company`, which must declare a metadata source")),
        "{:?}",
        v.errors
    );
}

#[test]
fn an_unknown_key_in_a_ref_is_refused_at_load() {
    let err = load_err(&MAIL_REFS.replace(
        r#"reader = "domain" }"#,
        r#"reader = "domain", via = "x" }"#,
    ));
    assert!(err.contains("via"), "{err}");
}
