// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed-query executor tests over a small synthetic hoard atlas: four hoards,
//! five mints, two coins, `holds_coins_of` relations, each atom cited from
//! its own chunk so a row's evidence can be read back exactly.

use super::*;
use crate::atlas_traversal::engine::AtlasView;
use crate::atlas_traversal::typed_check::signed_years;
use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Claim, Entity, Relation, SectionRange};
use crate::enrichment::ontology::OntologyPolicies;
use crate::taxonomy::{
    ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, RelationType,
};
use understanding_vocab::ontology::decl::{
    AttrDecl, AttrFamily, Force, OntologyTypeDecl, OntologyV1, TypeKind,
};

fn attr(name: &str, family: AttrFamily) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family,
        description: String::new(),
    }
}

fn policies() -> OntologyPolicies {
    let text = || AttrFamily::Text { values: vec![] };
    OntologyV1 {
        types: vec![
            OntologyTypeDecl {
                name: "hoard".into(),
                kind: TypeKind::Entity,
                attributes: vec![
                    attr("findspot", text()),
                    attr("buried", AttrFamily::Time { range: true }),
                ],
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "mint".into(),
                kind: TypeKind::Entity,
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "coin".into(),
                kind: TypeKind::Entity,
                attributes: vec![
                    attr("mint", AttrFamily::Ref { of: "mint".into() }),
                    attr("hoard", AttrFamily::Ref { of: "hoard".into() }),
                    attr(
                        "weight",
                        AttrFamily::Quantity {
                            unit: Some("g".into()),
                        },
                    ),
                ],
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "attribution".into(),
                kind: TypeKind::Claim,
                force: Some(Force::Assertive),
                subject: Some("coin".into()),
                attributes: vec![attr("proposed_date", AttrFamily::Time { range: true })],
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "holds_coins_of".into(),
                kind: TypeKind::Relation,
                from: Some("hoard".into()),
                to: Some("mint".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
    .into_policies()
}

fn ent(idx: usize, name: &str, ty: &str, attrs: &[(&str, &str)]) -> Entity {
    Entity {
        id: AtomId::entity(idx),
        canonical_name: name.into(),
        aliases: vec![],
        entity_type: EntityType::Other(ty.into()),
        first_appearance: ChunkRef::new(format!("sec_e{idx}"), None),
        description: String::new(),
        salience: 1.0 - idx as f32 / 100.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: attrs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect(),
        concept_kind: None,
    }
}

fn holds(idx: usize, hoard: usize, mint: usize) -> Relation {
    Relation {
        attributes: Default::default(),
        id: AtomId::relation(idx),
        label: "holds_coins_of".into(),
        participants: vec![AtomId::entity(hoard), AtomId::entity(mint)],
        relation_type: RelationType::Other("holds_coins_of".into()),
        evidence: vec![ChunkRef::new(format!("sec_r{idx}"), None)],
        section_range: SectionRange::point("sec_0001"),
        enrichment_depth: EnrichmentDepth::Extracted,
    }
}

/// Hoards 1-5, mints 11-15, coins 21-22.
fn atlas() -> (Vec<Entity>, Vec<Relation>, Vec<Claim>) {
    let mut miletus = ent(11, "Miletus", "mint", &[]);
    miletus.aliases = vec!["Milétos".into()];
    let entities = vec![
        ent(
            1,
            "Demanhur hoard",
            "hoard",
            &[
                ("findspot", "Egypt (vicinity of Demanhur)"),
                ("buried", "c. 318 B.C."),
            ],
        ),
        ent(
            2,
            "Kuft hoard",
            "hoard",
            &[("findspot", "Kuft, Egypt"), ("buried", "325-320 B.C.")],
        ),
        ent(3, "Asia Minor 1964", "hoard", &[("findspot", "Asia Minor")]),
        ent(
            4,
            "Sinan Pascha hoard",
            "hoard",
            &[("findspot", "unknown"), ("buried", "c. 317/6 B.C.")],
        ),
        // Buried across a wide span: starts before every other burial and
        // ends after them, so start-ranking and end-ranking disagree on it.
        ent(
            5,
            "Larnaca hoard",
            "hoard",
            &[("findspot", "Larnaca, Cyprus"), ("buried", "330-310 B.C.")],
        ),
        miletus,
        ent(12, "Sardes", "mint", &[]),
        ent(13, "Lampsacus", "mint", &[]),
        ent(14, "Sardes Serpent workshop", "mint", &[]),
        ent(15, "Abydus", "mint", &[]),
        ent(
            21,
            "Lampsacus stater",
            "coin",
            &[
                ("mint", "entity-0013"),
                ("hoard", "entity-0001"),
                ("weight", "8.4"),
            ],
        ),
        ent(
            22,
            "Sidonian stater",
            "coin",
            &[("mint", "Sidon"), ("hoard", "entity-0002")],
        ),
    ];
    let relations = vec![
        holds(1, 1, 11),
        holds(2, 1, 12),
        holds(3, 1, 13),
        holds(4, 2, 11),
        holds(5, 2, 14),
        holds(6, 3, 12),
        holds(7, 3, 13),
        holds(8, 4, 13),
    ];
    let claims = vec![
        attribution(1, "Struck at Lampsacus under Philip III", "c. 320 B.C."),
        attribution(2, "A later Sidonian issue", "306/5 B.C."),
    ];
    (entities, relations, claims)
}

fn attribution(idx: usize, content: &str, date: &str) -> Claim {
    Claim {
        attributes: [(
            "proposed_date".to_string(),
            serde_json::Value::String(date.into()),
        )]
        .into_iter()
        .collect(),
        subject: Some(AtomId::entity(21)),
        id: AtomId::claim(idx),
        content: content.into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Universal,
        evidence: vec![ChunkRef::new(format!("sec_c{idx}"), None)],
        attributed_to: None,
        confidence: Some(0.9),
        anchor: None,
        enrichment_depth: EnrichmentDepth::Extracted,
        quotable_excerpt: None,
        claim_kind: Some("attribution".into()),
        concession_outcome: None,
        evidence_kind: None,
    }
}

fn run(json: &str) -> TraversalResult {
    let vocab = policies();
    let (entities, relations, claims) = atlas();
    let query: TypedQuery = serde_json::from_str(json).expect("query parses");
    execute(
        &query,
        AtlasView {
            entities: &entities,
            events: &[],
            states: &[],
            relations: &relations,
            claims: &claims,
            questions: &[],
            configurations: &[],
            edges: &[],
            positions: &[],
            oppositions: &[],
            vocab: Some(&vocab),
        },
    )
}

fn names(r: &TraversalResult) -> Vec<String> {
    let mut n: Vec<String> = r
        .table
        .as_ref()
        .expect("a typed query answers with a table")
        .rows
        .iter()
        .map(|row| row.name.clone())
        .collect();
    n.sort();
    n
}

fn hoards_holding(mints: &[&str]) -> String {
    let rels: Vec<String> = mints
        .iter()
        .map(|m| {
            format!(
                r#"{{"relation":"holds_coins_of","other_type":"mint","other_name":"{m}","negate":false,"where":null}}"#
            )
        })
        .collect();
    format!(
        r#"{{"target_type":"hoard","filters":[],"relations":[{}],"aggregate":"none","aggregate_over":null}}"#,
        rels.join(",")
    )
}

/// Intersection: every relation constraint must hold, and the row cites the
/// hoard's own chunk plus each Relation atom that satisfied a constraint.
#[test]
fn intersection_rows_cite_the_relations_that_satisfied_them() {
    let r = run(&hoards_holding(&["Miletus", "Sardes"]));
    assert!(r.hit);
    assert_eq!(names(&r), ["Demanhur hoard"]);
    let row = &r.table.as_ref().unwrap().rows[0];
    assert_eq!(row.atom_id, "entity-0001");
    assert_eq!(row.evidence, ["sec_e1", "sec_r1", "sec_r2"]);
    assert_eq!(r.headline, "hoard: 1 match");
}

/// A named far end is the atom NAMED that, by canonical name or alias, case-
/// and diacritic-folded — never a longer name that contains it.
#[test]
fn a_named_far_end_matches_whole_names_and_aliases_only() {
    // "Sardes Serpent workshop" (held by Kuft) contains "Sardes" and must not
    // stand in for it.
    assert_eq!(
        names(&run(&hoards_holding(&["Sardes"]))),
        ["Asia Minor 1964", "Demanhur hoard"]
    );
    // An alias, in another case and with its accent dropped.
    assert_eq!(
        names(&run(&hoards_holding(&["MILETOS"]))),
        ["Demanhur hoard", "Kuft hoard"]
    );
}

/// A name no atom carries is reported, not silently answered as empty.
#[test]
fn an_unresolved_name_is_noted() {
    let r = run(&hoards_holding(&["Tarsus"]));
    assert!(
        r.hit,
        "a valid query over atoms that exist answers, with zero rows"
    );
    let table = r.table.unwrap();
    assert!(table.rows.is_empty());
    assert!(
        table
            .notes
            .iter()
            .any(|n| n.contains("'Tarsus' names no mint")),
        "{:?}",
        table.notes
    );
}

fn burial(op: &str, value: &str) -> String {
    format!(
        r#"{{"target_type":"hoard","filters":[{{"attribute":"buried","op":"{op}","value":{value},"negate":false}}],"relations":[],"aggregate":"none","aggregate_over":null}}"#
    )
}

/// A time range compares as an interval: `lt` when it ends before the bound,
/// `gt` when it starts after it, `eq` when it spans it. A hoard with no burial
/// date is set aside and counted, not treated as failing (or passing) it.
#[test]
fn time_filters_compare_signed_year_intervals() {
    // Kuft 325-320 B.C. ends at -320 < -319; Demanhur's -318 does not, and
    // Larnaca 330-310 B.C. ends after the bound however early it starts.
    assert_eq!(names(&run(&burial("lt", "-319"))), ["Kuft hoard"]);
    // Kuft spans -321: it is not wholly before it.
    assert!(names(&run(&burial("lt", "-321"))).is_empty());
    // Demanhur -318 and Sinan Pascha 317/6 B.C. start after -319.
    assert_eq!(
        names(&run(&burial("gt", "-319"))),
        ["Demanhur hoard", "Sinan Pascha hoard"]
    );
    // A text bound reads the same way.
    assert_eq!(
        names(&run(&burial("eq", "\"322 B.C.\""))),
        ["Kuft hoard", "Larnaca hoard"]
    );
    let notes = run(&burial("gt", "-319")).table.unwrap().notes;
    assert!(
        notes
            .iter()
            .any(|n| n == "1 hoard atom(s) set aside: buried unset or unreadable."),
        "{notes:?}"
    );
}

/// `negate` on a filter never turns an absent value into a yes.
#[test]
fn a_negated_filter_does_not_pass_an_unset_value() {
    let q = r#"{"target_type":"hoard","filters":[{"attribute":"buried","op":"lt","value":-319,"negate":true}],"relations":[]}"#;
    // Asia Minor 1964 has no burial date: unjudged, so absent from both answers.
    assert_eq!(
        names(&run(q)),
        ["Demanhur hoard", "Larnaca hoard", "Sinan Pascha hoard"]
    );
}

/// `contains` matches at a word start, folded; negation over a relation
/// excludes the hoards that hold the mint.
#[test]
fn region_filter_with_a_negated_relation() {
    let q = r#"{"target_type":"hoard",
        "filters":[{"attribute":"findspot","op":"contains","value":"egypt","negate":false}],
        "relations":[{"relation":"holds_coins_of","other_type":"mint","other_name":"Sardes","negate":true,"where":null}],
        "aggregate":"none","aggregate_over":null}"#;
    let r = run(q);
    assert_eq!(names(&r), ["Kuft hoard"]);
    assert!(
        r.table.unwrap().notes.iter().any(
            |n| n.starts_with("a negated relation is judged over the links this atlas records")
        ),
        "a negation must say it is closed-world"
    );
    let mid_word = r#"{"target_type":"hoard","filters":[{"attribute":"findspot","op":"contains","value":"gypt","negate":false}],"relations":[]}"#;
    assert!(names(&run(mid_word)).is_empty());
}

/// `count` answers in `matched`; the rows still list and cite the atoms.
#[test]
fn count_reports_matched() {
    let q =
        hoards_holding(&["Lampsacus"]).replace(r#""aggregate":"none""#, r#""aggregate":"count""#);
    let table = run(&q).table.unwrap();
    assert_eq!(table.matched, 3);
    assert_eq!(table.rows.len(), 3);
}

/// Depth 2: mints other than Miletus that share a hoard with it. The `where`
/// names the anchor on the far end; the row cites both hops.
#[test]
fn co_occurrence_through_a_where() {
    let q = r#"{"target_type":"mint",
        "filters":[{"attribute":"name","op":"eq","value":"Miletus","negate":true}],
        "relations":[{"relation":"holds_coins_of","other_type":"hoard","other_name":null,"negate":false,
            "where":{"filters":[],"relations":[{"relation":"holds_coins_of","other_type":"mint","other_name":"Miletus","negate":false}]}}],
        "aggregate":"none","aggregate_over":null}"#;
    let r = run(q);
    assert_eq!(
        names(&r),
        ["Lampsacus", "Sardes", "Sardes Serpent workshop"]
    );
    let sardes = r
        .table
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.name == "Sardes")
        .unwrap();
    // Sardes' own chunk, Demanhur->Sardes (r2), and Demanhur->Miletus (r1)
    // which qualified Demanhur inside the `where`.
    assert_eq!(sardes.evidence, ["sec_e12", "sec_r2", "sec_r1"]);
}

/// argmin ranks by an interval's START, argmax by its END; a hoard with no
/// readable burial is left out of the ranking and counted.
#[test]
fn argmin_and_argmax_over_a_time_range() {
    let q = |agg: &str| {
        format!(
            r#"{{"target_type":"hoard","filters":[],"relations":[],"aggregate":"{agg}","aggregate_over":"buried"}}"#
        )
    };
    // Larnaca (330-310) both starts first and ends last.
    assert_eq!(names(&run(&q("argmin"))), ["Larnaca hoard"]);
    let latest = run(&q("argmax"));
    assert_eq!(names(&latest), ["Larnaca hoard"]);
    let table = latest.table.unwrap();
    assert_eq!(table.matched, 5);
    // Without Larnaca, the earliest start is Kuft's and the latest end
    // Sinan Pascha's (317/6, read as 317 and 316).
    let not_larnaca = |agg: &str| {
        format!(
            r#"{{"target_type":"hoard","filters":[{{"attribute":"name","op":"eq","value":"Larnaca hoard","negate":true}}],"relations":[],"aggregate":"{agg}","aggregate_over":"buried"}}"#
        )
    };
    assert_eq!(names(&run(&not_larnaca("argmin"))), ["Kuft hoard"]);
    assert_eq!(names(&run(&not_larnaca("argmax"))), ["Sinan Pascha hoard"]);
    assert!(table
        .notes
        .iter()
        .any(|n| n.starts_with("1 matching hoard")));
}

/// argmax over a related type counts distinct linked atoms, inside the
/// `where` the query puts on that type.
#[test]
fn argmax_over_a_related_type_counts_links_in_scope() {
    let most_mints = r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"argmax","aggregate_over":"mint"}"#;
    assert_eq!(names(&run(most_mints)), ["Demanhur hoard"]);
    // Mints by how many EGYPTIAN hoards hold them: Miletus 2 (Demanhur, Kuft).
    let most_egyptian_hoards = r#"{"target_type":"mint","filters":[],
        "relations":[{"relation":"holds_coins_of","other_type":"hoard","other_name":null,"negate":false,
            "where":{"filters":[{"attribute":"findspot","op":"contains","value":"Egypt","negate":false}],"relations":[]}}],
        "aggregate":"argmax","aggregate_over":"hoard"}"#;
    assert_eq!(names(&run(most_egyptian_hoards)), ["Miletus"]);
}

/// A ref attribute is a relation spelled `<type>.<attr>`, in either direction;
/// a ref holding a name no atom carries still answers a constraint naming it.
#[test]
fn ref_attributes_link_both_ways() {
    let coin_of = |mint: &str| {
        format!(
            r#"{{"target_type":"coin","filters":[],"relations":[{{"relation":"coin.mint","other_type":"mint","other_name":"{mint}","negate":false,"where":null}}]}}"#
        )
    };
    let lampsacus = run(&coin_of("Lampsacus"));
    assert_eq!(names(&lampsacus), ["Lampsacus stater"]);
    assert_eq!(names(&run(&coin_of("Sidon"))), ["Sidonian stater"]);
    let with_coins = r#"{"target_type":"hoard","filters":[],"relations":[{"relation":"coin.hoard","other_type":"coin","other_name":null,"negate":false,"where":null}]}"#;
    let r = run(with_coins);
    assert_eq!(names(&r), ["Demanhur hoard", "Kuft hoard"]);
    let demanhur = &r.table.unwrap().rows[0];
    // The coin that holds the ref is the citation.
    assert_eq!(demanhur.evidence, ["sec_e1", "sec_e21"]);
}

/// A filtered attribute the atom does not carry shows as null in its row.
#[test]
fn rows_show_named_attributes_the_atom_lacks_as_null() {
    let q = r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"argmin","aggregate_over":"buried"}"#;
    let larnaca = &run(q).table.unwrap().rows[0];
    assert_eq!(larnaca.attributes["buried"], "330-310 B.C.");
    let listing = r#"{"target_type":"hoard","filters":[{"attribute":"findspot","op":"contains","value":"Asia","negate":false}],"relations":[],"aggregate":"argmax","aggregate_over":"buried"}"#;
    // Asia Minor 1964 has no burial: unscored, so no winner, and a note.
    let t = run(listing).table.unwrap();
    assert!(t.rows.is_empty());
    assert_eq!(t.matched, 1);
    let tally = r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"tally","aggregate_over":"buried"}"#;
    let rows = run(tally).table.unwrap().rows;
    let asia = rows.iter().find(|r| r.name == "Asia Minor 1964").unwrap();
    assert_eq!(asia.attributes["buried"], serde_json::Value::Null);
}

/// Names the vocabulary does not hold are refused with a reason, never
/// skipped; a malformed aggregate does not parse.
#[test]
fn undeclared_names_are_refused() {
    for (q, why) in [
        (
            r#"{"target_type":"coins"}"#,
            "'coins' is not a declared type",
        ),
        (
            r#"{"target_type":"holds_coins_of"}"#,
            "a typed query answers over entity or claim types",
        ),
        (
            r#"{"target_type":"hoard","filters":[{"attribute":"mint","op":"eq","value":"x"}]}"#,
            "'mint' is not an attribute of hoard",
        ),
        (
            r#"{"target_type":"coin","filters":[{"attribute":"mint","op":"eq","value":"x"}]}"#,
            "constrain it as the relation 'coin.mint'",
        ),
        (
            r#"{"target_type":"hoard","filters":[{"attribute":"findspot","op":"lt","value":"x"}]}"#,
            "cannot be compared by Lt",
        ),
        (
            r#"{"target_type":"coin","relations":[{"relation":"holds_coins_of","other_type":"mint"}]}"#,
            "joins hoard and mint, not coin and mint",
        ),
        (
            r#"{"target_type":"hoard","aggregate":"argmax","aggregate_over":"findspot"}"#,
            "'findspot' is a text attribute",
        ),
    ] {
        let r = run(q);
        assert!(!r.hit, "{q} should be refused");
        assert!(r.headline.contains(why), "{q}: {}", r.headline);
    }
    for bad in [
        r#"{"target_type":"hoard","aggregate":"argmax"}"#,
        r#"{"target_type":"hoard","aggregate":"count","aggregate_over":"buried"}"#,
        r#"{"target_type":"hoard","relations":[{"relation":"holds_coins_of","other_type":"mint","where":{"relations":[{"relation":"holds_coins_of","other_type":"hoard","where":null}]}}]}"#,
        r#"{"target_type":"hoard","aggregate_by":"x"}"#,
    ] {
        assert!(
            serde_json::from_str::<TypedQuery>(bad).is_err(),
            "{bad} should not parse"
        );
    }
}

/// The K2 reference shape round-trips: what parses serialises back to the
/// same JSON value.
#[test]
fn the_wire_shape_round_trips() {
    for q in [
        r#"{"target_type":"hoard","filters":[{"attribute":"buried","op":"lt","value":-310,"negate":false}],"relations":[{"relation":"holds_coins_of","other_type":"mint","other_name":"Tarsus","negate":false,"where":null}],"aggregate":"none","aggregate_over":null}"#,
        r#"{"target_type":"mint","filters":[],"relations":[{"relation":"holds_coins_of","other_type":"hoard","other_name":null,"negate":false,"where":{"filters":[],"relations":[{"relation":"holds_coins_of","other_type":"mint","other_name":"Tarsus","negate":false}]}}],"aggregate":"argmax","aggregate_over":"hoard"}"#,
        r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"argmin","aggregate_over":"buried"}"#,
        r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"tally","aggregate_over":"findspot"}"#,
    ] {
        let parsed: TypedQuery = serde_json::from_str(q).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(q).unwrap());
    }
}

#[test]
fn years_read_signed_with_span_shorthand() {
    assert_eq!(signed_years("c. 317/6 B.C."), [-317.0, -316.0]);
    assert_eq!(signed_years("1914/15"), [1914.0, 1915.0]);
    assert_eq!(signed_years("325-320 BCE"), [-325.0, -320.0]);
    assert_eq!(signed_years("~1905"), [1905.0]);
    assert!(signed_years("third century B.C.").is_empty());
    assert!(signed_years("4th century").is_empty());
    // "bc" inside a word is not an era.
    assert_eq!(signed_years("abc 300"), [300.0]);
}

/// A declared CLAIM type is listed like an entity type: its claims are the
/// rows, named by their content and cited by their own evidence.
#[test]
fn a_claim_type_lists_its_claims() {
    let q = r#"{"target_type":"attribution","filters":[{"attribute":"proposed_date","op":"lt","value":-310,"negate":false}],"relations":[]}"#;
    let r = run(q);
    assert_eq!(names(&r), ["Struck at Lampsacus under Philip III"]);
    assert_eq!(r.claims.len(), 1);
    assert_eq!(r.table.unwrap().rows[0].evidence, ["sec_c1"]);
}

/// The brief renders the table: one cited line per row, absent attributes as
/// `(unset)`, a count line, and the notes.
#[test]
fn the_brief_is_a_cited_table() {
    use crate::atlas_traversal::brief::assemble_brief;
    let text = assemble_brief(&run(&hoards_holding(&["Miletus", "Sardes"]))).to_text();
    assert!(
        text.contains(
            "- [extracted] Demanhur hoard — buried=c. 318 B.C.; findspot=Egypt (vicinity of Demanhur) [sec_e1, sec_r1, sec_r2]"
        ),
        "{text}"
    );
    assert!(text.contains("\ncount: 1\n"), "{text}");

    let tally = r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"tally","aggregate_over":"buried"}"#;
    let text = assemble_brief(&run(tally)).to_text();
    assert!(
        text.contains(
            "- [extracted] Asia Minor 1964 — buried=(unset); findspot=Asia Minor [sec_e3]"
        ),
        "{text}"
    );

    let best = r#"{"target_type":"hoard","filters":[],"relations":[],"aggregate":"argmax","aggregate_over":"buried"}"#;
    let text = assemble_brief(&run(best)).to_text();
    assert!(text.contains("\ncount: 1 of 5 that match\n"), "{text}");
    assert!(
        text.contains("note: 1 matching hoard atom(s) carry no readable buried"),
        "{text}"
    );
}
