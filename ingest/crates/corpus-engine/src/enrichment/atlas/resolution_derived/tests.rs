// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::enrichment::atlas::atoms::AtomId;
use crate::enrichment::atlas::project_source_atoms;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const DECLARATION: &str = r#"
[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "email", type = "text" }, { name = "employer", type = "ref", of = "company" }]
identity = ["email"]
source = { metadata = ["from", "to", "cc"], attributes = { email = "address" }, refs = { employer = { of = "company", reader = "domain" } } }

[[types]]
name = "company"
kind = "entity"
attributes = [{ name = "domain", type = "text" }]
identity = ["domain"]
source = { metadata = ["from", "to", "cc"], attributes = { domain = "domain" } }

[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"
identity_bar = 0.5
attributes = [{ name = "counterparty", type = "ref", of = "company", derived = "party_of_deal" }]

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "deal"
attributes = [{ name = "party", type = "ref", of = "company", derived = "party_of_message" }]

[change]
document = { date = "date", thread = "thread" }

[[sets]]
id = "ours"
type = "company"
where = { domain = { suffix = "enron.com" } }

[[paths]]
id = "outside_parties"
path = "document / (from | to | cc) / employer [!ours]"

[[folds]]
id = "party_of_message"
by = "first"
from = ["outside_parties"]

[[folds]]
id = "party_of_deal"
by = "most"
from = ["^subject / party"]
"#;

fn policies() -> OntologyPolicies {
    OntologyBlock {
        version: 1,
        body: toml::from_str(DECLARATION).unwrap(),
    }
    .policies()
    .unwrap()
}

fn row(
    id: u64,
    key: &str,
    date: &str,
    from: &str,
    to: &str,
    cc: &str,
    body: &str,
) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: body.into(),
        title: None,
        url: None,
        metadata_raw: Some(
            json!({"date": date, "thread": key, "from": from, "to": to, "cc": cc}).to_string(),
        ),
        source_doc_id: Some(key.into()),
    }
}

/// m1, m2: the customer and us (m2 copies a colleague at a subsidiary); m3:
/// two outside companies; m4: internal only.
fn documents() -> SectionDocuments {
    let rows = [
        row(
            1,
            "m1",
            "2001-01-01T10:00:00Z",
            "Ann Lee <ann@city.gov>",
            "kim.ward@enron.com",
            "",
            "We can offer gas at Malin.",
        ),
        row(
            2,
            "m2",
            "2001-01-02T10:00:00Z",
            "kim.ward@enron.com",
            "Ann Lee <ann@city.gov>",
            "bob@ees.enron.com",
            "The Malin deal is agreed.",
        ),
        row(
            3,
            "m3",
            "2001-01-03T10:00:00Z",
            "kim.ward@enron.com",
            "carl@pge.com, dana@sdge.com",
            "",
            "Two offers for power.",
        ),
        row(
            4,
            "m4",
            "2001-01-04T10:00:00Z",
            "kim.ward@enron.com",
            "ed@enron.com",
            "",
            "Internal note on Malin.",
        ),
    ];
    SectionDocuments::from_chunk_rows(
        [
            ("s1", &[1u64, 2][..]),
            ("s2", &[3u64][..]),
            ("s3", &[4u64][..]),
        ],
        &rows,
    )
}

fn claim(id: &str, section: &str, anchor: &str) -> Claim {
    serde_json::from_value(json!({
        "id": id, "content": anchor, "discourse_act": "assert", "epistemic_status": "confident",
        "scope": "universal", "evidence": [{"chunk_id": section, "passage_preview": anchor}],
        "anchor": anchor, "claim_kind": "stage_update", "enrichment_depth": "extracted",
    }))
    .unwrap()
}

fn deal(id: &str) -> Entity {
    serde_json::from_value(json!({
        "id": id, "canonical_name": id, "entity_type": "deal", "first_appearance": {"chunk_id": "s1"},
        "description": "", "salience": 0.0, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

struct Build {
    entities: Vec<Entity>,
    claims: Vec<Claim>,
    participants: Participants,
    lines: Vec<DerivedValue>,
}

impl Build {
    fn new(docs: &SectionDocuments, p: &OntologyPolicies) -> Self {
        let projection = project_source_atoms(docs, p, "c").unwrap();
        Self {
            entities: projection.atoms,
            claims: vec![
                claim("claim-0001", "s1", "offer gas at Malin"),
                claim("claim-0002", "s1", "Malin deal is agreed"),
                claim("claim-0003", "s2", "Two offers for power"),
                claim("claim-0004", "s3", "Internal note on Malin"),
            ],
            participants: projection.participants,
            lines: Vec::new(),
        }
    }

    fn derive(
        &mut self,
        docs: &SectionDocuments,
        p: &OntologyPolicies,
        stage: DeriveStage,
    ) -> DerivedReport {
        let (mut events, mut states, mut relations) = (Vec::new(), Vec::new(), Vec::new());
        let (mut ars, mut positions, mut oppositions, mut edges) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut trajectories = BTreeMap::new();
        let mut atoms = BuildAtoms {
            entities: &mut self.entities,
            events: &mut events,
            states: &mut states,
            relations: &mut relations,
            claims: &mut self.claims,
            argument_reconstructions: &mut ars,
            positions: &mut positions,
            oppositions: &mut oppositions,
            edges: &mut edges,
            trajectories: &mut trajectories,
        };
        let lines = &mut self.lines;
        derive_attributes(&mut atoms, docs, &self.participants, p, stage, &mut |v| {
            lines.push(v.clone())
        })
        .unwrap()
    }

    fn company(&self, domain: &str) -> String {
        let e = self
            .entities
            .iter()
            .find(|e| e.attributes.get("domain").and_then(Value::as_str) == Some(domain));
        e.unwrap_or_else(|| panic!("no company {domain}"))
            .id
            .as_str()
            .to_string()
    }

    fn line(&self, atom: &str) -> &DerivedValue {
        self.lines.iter().rev().find(|l| l.atom == atom).unwrap()
    }
}

#[test]
fn a_claim_takes_its_message_party_and_a_record_the_party_its_claims_agree_on() {
    let (docs, p) = (documents(), policies());
    let mut b = Build::new(&docs, &p);
    let city = b.company("city.gov");

    let before = b.derive(&docs, &p, DeriveStage::BeforeResolve);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        ["stage_update.party"],
        "the deal waits for RESOLVE"
    );
    let party = |b: &Build, c: &str| {
        b.claims
            .iter()
            .find(|x| x.id.as_str() == c)
            .unwrap()
            .attributes
            .get("party")
            .cloned()
    };
    assert_eq!(party(&b, "claim-0001"), Some(json!(city)));
    // the subsidiary (ees.enron.com) is ours by the suffix, dropped and kept on the line
    assert_eq!(party(&b, "claim-0002"), Some(json!(city)));
    let ees = b.company("ees.enron.com");
    assert!(
        b.line("claim-0002").excluded.contains(&ees),
        "{:?}",
        b.line("claim-0002")
    );
    // two outside companies decide nothing; an internal message reaches none
    assert_eq!(party(&b, "claim-0003"), None);
    assert!(
        matches!(&b.line("claim-0003").outcome, DerivedOutcome::Ambiguous { values } if values.len() == 2)
    );
    assert_eq!(party(&b, "claim-0004"), None);
    assert_eq!(b.line("claim-0004").outcome, DerivedOutcome::Nothing);
    let t = &before["stage_update.party"];
    assert_eq!(
        (
            t.atoms,
            t.outcomes["decided"],
            t.outcomes["ambiguous"],
            t.outcomes["nothing"]
        ),
        (4, 2, 1, 1)
    );

    // RESOLVE's records: claims 1 and 2 one deal, claim 3 another.
    b.entities.extend([deal("entity-d1"), deal("entity-d2")]);
    for (c, d) in [
        ("claim-0001", "entity-d1"),
        ("claim-0002", "entity-d1"),
        ("claim-0003", "entity-d2"),
    ] {
        b.claims
            .iter_mut()
            .find(|x| x.id.as_str() == c)
            .unwrap()
            .subject = Some(AtomId::from_raw(d));
    }
    let after = b.derive(&docs, &p, DeriveStage::AfterResolve);
    assert_eq!(after.keys().collect::<Vec<_>>(), ["deal.counterparty"]);
    let d1 = b
        .entities
        .iter()
        .find(|e| e.id.as_str() == "entity-d1")
        .unwrap();
    assert_eq!(d1.attributes.get("counterparty"), Some(&json!(city)));
    match &b.line("entity-d1").outcome {
        DerivedOutcome::Decided { documents, .. } => assert_eq!(documents, &["m1", "m2"]),
        other => panic!("{other:?}"),
    }
    assert!(
        serde_json::to_value(b.line("entity-d1"))
            .unwrap()
            .get("protocol")
            .is_none(),
        "legacy derived-decision lines keep their existing wire shape"
    );
    assert_eq!(b.line("entity-d2").outcome, DerivedOutcome::Nothing);
}

fn cands(pairs: &[(&str, &str)]) -> BTreeMap<String, BTreeSet<Option<String>>> {
    let mut m: BTreeMap<String, BTreeSet<Option<String>>> = BTreeMap::new();
    for (v, d) in pairs {
        m.entry(v.to_string())
            .or_default()
            .insert(Some(d.to_string()));
    }
    m
}

#[test]
fn each_fold_function_decides_or_names_its_absence() {
    let clock: HashMap<&str, String> = [
        ("d1", "2001-01-01"),
        ("d2", "2001-02-01"),
        ("d3", "2001-03-01"),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let refined = cands(&[("x", "d1")]);
    let read = cands(&[("y", "d2")]);
    let two = cands(&[("y", "d2"), ("z", "d3")]);
    let none = cands(&[]);

    // first: priority order; a later input's different value is superseded, an ambiguous input falls through
    assert!(
        matches!(decide(FoldBy::First, &[refined.clone(), read.clone()], &clock),
        DerivedOutcome::Decided { values, input: Some(0), superseded, .. } if values == ["x"] && superseded == ["y"])
    );
    assert!(
        matches!(decide(FoldBy::First, &[two.clone(), read.clone()], &clock),
        DerivedOutcome::Decided { values, input: Some(1), .. } if values == ["y"])
    );
    assert!(matches!(
        decide(FoldBy::First, &[none.clone(), two.clone()], &clock),
        DerivedOutcome::Ambiguous { .. }
    ));
    assert_eq!(
        decide(FoldBy::First, &[none.clone()], &clock),
        DerivedOutcome::Nothing
    );
    // agree
    assert!(matches!(
        decide(
            FoldBy::Agree,
            &[read.clone(), cands(&[("y", "d3")])],
            &clock
        ),
        DerivedOutcome::Decided { .. }
    ));
    assert!(matches!(
        decide(FoldBy::Agree, &[refined.clone(), read.clone()], &clock),
        DerivedOutcome::Conflict { .. }
    ));
    // most: distinct documents; a tie decides nothing
    assert!(
        matches!(decide(FoldBy::Most, &[cands(&[("x", "d1"), ("x", "d2"), ("y", "d3")])], &clock),
        DerivedOutcome::Decided { values, .. } if values == ["x"])
    );
    assert!(matches!(
        decide(FoldBy::Most, &[two.clone()], &clock),
        DerivedOutcome::Tie { .. }
    ));
    // earliest / latest by the documents' clock; undated values cannot be ordered
    assert!(
        matches!(decide(FoldBy::Latest, &[cands(&[("x", "d1"), ("y", "d3")])], &clock),
        DerivedOutcome::Decided { values, .. } if values == ["y"])
    );
    assert!(
        matches!(decide(FoldBy::Earliest, &[cands(&[("x", "d1"), ("y", "d3")])], &clock),
        DerivedOutcome::Decided { values, .. } if values == ["x"])
    );
    assert_eq!(
        decide(FoldBy::Latest, &[cands(&[("x", "undated")])], &clock),
        DerivedOutcome::Unordered
    );
    // all: the set
    assert!(
        matches!(decide(FoldBy::All, &[two.clone(), refined.clone()], &clock),
        DerivedOutcome::Decided { values, .. } if values == ["x", "y", "z"])
    );
}

/// Equal text is not the same occurrence: two messages of one section hold
/// the same line, and the claim each was read from reaches the message its
/// evidence names, so each takes its own message's party. Located by its
/// words alone, either would land in both and reach neither.
#[test]
fn a_claim_reaches_its_own_message_when_another_holds_the_same_words() {
    let line = "The Malin deal is agreed.";
    let rows = [
        row(
            1,
            "m1",
            "2001-01-01T10:00:00Z",
            "Ann Lee <ann@city.gov>",
            "kim.ward@enron.com",
            "",
            line,
        ),
        row(
            2,
            "m2",
            "2001-01-02T10:00:00Z",
            "kim.ward@enron.com",
            "carl@pge.com",
            "",
            line,
        ),
    ];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64, 2][..])], &rows);
    let p = policies();
    let mut b = Build::new(&docs, &p);
    b.claims = ["m1", "m2"]
        .iter()
        .map(|key| {
            let mut c = claim(&format!("claim-{key}"), "s1", line);
            c.evidence[0].source_doc_id = Some(key.to_string());
            c
        })
        .collect();
    b.derive(&docs, &p, DeriveStage::BeforeResolve);
    let party = |c: &str| {
        let claim = b.claims.iter().find(|x| x.id.as_str() == c).unwrap();
        claim.attributes.get("party").cloned()
    };
    assert_eq!(party("claim-m1"), Some(json!(b.company("city.gov"))));
    assert_eq!(party("claim-m2"), Some(json!(b.company("pge.com"))));
}
