use super::*;

/// The pre-registered table, row by row (`EPISTEMIC_INDEX.md` §2.2).
/// Failing input: change any seed, edge, hop count or budget below.
#[test]
fn defaults_are_the_spec_table() {
    let n = NavigationPolicy::default();
    let t = n.walk(QuestionKind::Thematic);
    // `Summary` joined this row in ei-7a (2026-09-04) — a repair of a
    // spec inconsistency, not a tuning move: §3's port table says the
    // walk reaches Summary nodes and §2.2's table predated the kind.
    // The other four rows, the hops and the budget are untouched.
    assert_eq!(
        t.seed.kinds,
        vec![AtomType::Configuration, AtomType::Entity, AtomType::Summary]
    );
    assert_eq!(t.seed.entity_types, vec![EntityType::Concept]);
    // ei-5c: `Configures` joins the edge list so a walk seeded on a
    // Configuration can leave it, and `Summary` gets its own seed quota so
    // it cannot take a leaf's slot.
    assert_eq!(
        t.walk,
        vec![
            EdgeType::Involves,
            EdgeType::Tension,
            EdgeType::Grounds,
            EdgeType::Configures
        ]
    );
    assert_eq!(
        t.seed.budgets,
        BTreeMap::from([(AtomType::Summary, SUMMARY_SEED_BUDGET)])
    );
    // The thematic row COMPOSES both summary sources, atoms first. Failing
    // input: drop `Raptor` and a corpus that keeps its summaries in
    // `raptor_summaries.lance` and has not been projected supplies none.
    assert_eq!(
        t.summary_sources,
        vec![SummarySource::Atoms, SummarySource::Raptor]
    );
    assert_eq!((t.hops, t.budget), (2, DEFAULT_BUDGET));

    let t = n.walk(QuestionKind::Trajectory);
    assert_eq!(t.seed.kinds, vec![AtomType::Entity, AtomType::State]);
    assert_eq!(t.walk, vec![EdgeType::Transition, EdgeType::Causes]);
    assert_eq!(t.hops, 2);

    let t = n.walk(QuestionKind::Tension);
    assert_eq!(t.seed.kinds, vec![AtomType::Claim, AtomType::Position]);
    assert_eq!(t.walk, vec![EdgeType::Tension, EdgeType::OpposesIn]);
    assert_eq!(t.hops, 1);

    let t = n.walk(QuestionKind::Enumeration);
    assert!(t.seed.declared && t.seed.kinds.is_empty() && t.walk.is_empty());
    assert_eq!(t.hops, 0);

    let t = n.walk(QuestionKind::Lookup);
    // Event and Relation joined Entity 2026-09-09 — see `WalkPolicy::lookup`.
    // A point fact in a section-extracted corpus lives in something that
    // HAPPENED or a way two entities STAND as often as in a named entity,
    // and until this row named them `seed_population` built a table
    // without them and no walk could reach one.
    assert_eq!(
        t.seed.kinds,
        vec![AtomType::Entity, AtomType::Event, AtomType::Relation]
    );
    assert_eq!(t.walk, vec![EdgeType::Involves]);
    assert_eq!(t.hops, 1);

    assert_eq!(n.rows().count(), QuestionKind::ALL.len());

    // Only the thematic row declares a quota. A quota on every row would
    // be a cap on seeding in general; this field exists to hold ONE kind
    // out of the leaf race, and the other four rows seed only leaf kinds.
    for (kind, w) in n.rows() {
        if kind == QuestionKind::Thematic {
            continue;
        }
        assert!(
            w.seed.budgets.is_empty(),
            "{kind:?} declares a seed quota; only thematic should"
        );
    }
    assert!(WalkPolicy::unfiltered().seed.budgets.is_empty());

    // Only the row that asks for whole-work summaries composes any source.
    // The unfiltered row is the status quo ante and carries `Atoms` alone:
    // an abstention must not widen the evidence.
    for (kind, w) in n.rows() {
        if kind == QuestionKind::Thematic {
            continue;
        }
        assert!(
            w.summary_sources.is_empty(),
            "{kind:?} composes a summary source; only thematic should"
        );
    }
    // The unfiltered row composes EVERY source. It is the status quo ante
    // written down, and the injector it stands in for ran on every
    // question regardless of kind — so an abstention that composed less
    // than a classified walk would be a narrowing, not a neutral choice.
    // Failing input, measured: with `[Atoms]` here, 15 of 21 SEP questions
    // abstain and the thematic row's `raptor` source never runs at all.
    assert_eq!(
        WalkPolicy::unfiltered().summary_sources,
        SummarySource::ALL.to_vec()
    );
}

/// The composition is DATA: a corpus declares which sources it keeps its
/// summaries in, and in what order. Failing input: drop
/// `#[serde(default)]` from `summary_sources`, or rename a variant's wire
/// spelling away from the one an author writes.
#[test]
fn the_summary_sources_are_a_declared_ordered_list() {
    let text = serde_json::to_string(&NavigationPolicy::default()).unwrap();
    assert!(
        text.contains(r#""summary_sources":["atoms","raptor"]"#),
        "the composition must write its own wire spellings, in order: {text}"
    );

    // A corpus that keeps summaries ONLY in the RAPTOR table says so.
    let declared = serde_json::json!({
        "thematic": { "seed": { "kinds": ["Summary"] }, "walk": [], "hops": 1, "budget": 6,
                      "summary_sources": ["raptor"] }
    });
    let n: NavigationPolicy = serde_json::from_value(declared).unwrap();
    assert_eq!(n.thematic.summary_sources, vec![SummarySource::Raptor]);

    // A row that omits the key composes NOTHING rather than defaulting to
    // both — an omitted list is a list, and guessing here would give a
    // corpus a source it never declared.
    let silent = serde_json::json!({
        "thematic": { "seed": { "kinds": ["Summary"] }, "walk": [], "hops": 1, "budget": 6 }
    });
    let n: NavigationPolicy = serde_json::from_value(silent).unwrap();
    assert!(n.thematic.summary_sources.is_empty());

    for src in SummarySource::ALL {
        assert_eq!(
            serde_json::to_string(&src).unwrap().trim_matches('"'),
            src.as_str()
        );
    }
}

/// A quota is data, so it round-trips as data: a map may set its own
/// (a corpus whose readers want more or fewer summaries) and a map that
/// says nothing gets the pre-registered one. Failing input: drop
/// `#[serde(default)]` from `budgets`, or serialise the key as anything
/// but the on-disk `atom_type` tag.
#[test]
fn a_seed_quota_is_declared_data_not_a_constant() {
    let text = serde_json::to_string(&NavigationPolicy::default()).unwrap();
    assert!(
        text.contains("\"budgets\":{\"Summary\":8}"),
        "the quota must write the on-disk atom_type tag: {text}"
    );

    let declared = serde_json::json!({
        "thematic": { "seed": { "kinds": ["Summary"], "budgets": { "Summary": 2 } },
                      "walk": ["Involves"], "hops": 1, "budget": 6 }
    });
    let n: NavigationPolicy = serde_json::from_value(declared).unwrap();
    assert_eq!(n.thematic.seed.budgets.get(&AtomType::Summary), Some(&2));

    // A row that omits `budgets` has none — the pre-ei-5c behaviour, which
    // is what makes the displacement fixture's negative arm expressible.
    let silent = serde_json::json!({
        "thematic": { "seed": { "kinds": ["Summary"] }, "walk": [], "hops": 1, "budget": 6 }
    });
    let n: NavigationPolicy = serde_json::from_value(silent).unwrap();
    assert!(n.thematic.seed.budgets.is_empty());
}

/// A JSON document with no `navigation` key — every `ontology.json`
/// written before this section existed — reads as the default table, and
/// a document that carries one row keeps that row and defaults the rest.
/// Failing input: drop `#[serde(default = …)]` from any field.
#[test]
fn json_round_trips_with_defaults() {
    let absent: NavigationPolicy = serde_json::from_str("{}").unwrap();
    assert_eq!(absent, NavigationPolicy::default());

    let one_row = serde_json::json!({
        "tension": { "seed": { "kinds": ["Claim"] }, "walk": ["Tension"], "hops": 2, "budget": 3 }
    });
    let n: NavigationPolicy = serde_json::from_value(one_row).unwrap();
    assert_eq!(n.tension.seed.kinds, vec![AtomType::Claim]);
    assert_eq!((n.tension.hops, n.tension.budget), (2, 3));
    assert_eq!(n.thematic, WalkPolicy::thematic());
    assert_eq!(n.lookup, WalkPolicy::lookup());

    let text = serde_json::to_string(&n).unwrap();
    let back: NavigationPolicy = serde_json::from_str(&text).unwrap();
    assert_eq!(back, n);
}

/// The wire spellings are the ones the atoms and edges already carry —
/// PascalCase `atom_type` / `edge_type` tags, snake_case entity types and
/// question kinds. Failing input: a `rename_all` on any of the four enums.
#[test]
fn rows_are_written_in_the_on_disk_tags() {
    let text = serde_json::to_string(&NavigationPolicy::default()).unwrap();
    for tag in [
        "\"Configuration\"",
        "\"Entity\"",
        "\"concept\"",
        "\"Involves\"",
        "\"OpposesIn\"",
        "\"thematic\"",
        "\"enumeration\"",
    ] {
        assert!(text.contains(tag), "{tag} missing from {text}");
    }
    for k in QuestionKind::ALL {
        assert_eq!(
            serde_json::to_string(&k).unwrap().trim_matches('"'),
            k.as_str()
        );
    }
}
