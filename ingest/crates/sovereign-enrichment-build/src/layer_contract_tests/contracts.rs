// SPDX-License-Identifier: AGPL-3.0-or-later
//! C1-C6, one test each, every one over all three fixture shapes.

use super::*;
use corpus_engine::enrichment::atlas::resolve_records::QUOTE_LABEL;

/// The attributes a declared field, key, reference or derivation supplies,
/// per type: derived attributes (a declared path or fold fills them, references
/// among them), a metadata-sourced type's attributes other than the identity
/// keys a statement names it by (its `refs` among them), and the document
/// stamps. A reference no declaration fills is open: Pick asks it.
fn supplied(policies: &OntologyPolicies) -> BTreeSet<(String, String)> {
    use corpus_engine::enrichment::ontology::SourceDecl;
    let mut out = BTreeSet::new();
    for t in &policies.shape.types {
        let sourced = matches!(t.source, Some(SourceDecl::Metadata(_)));
        for a in &t.attributes {
            let keyed = t.identity.contains(&a.name);
            if a.derived.is_some() || (sourced && !keyed) {
                out.insert((t.name.clone(), a.name.clone()));
            }
        }
        for stamp in ["document_date", "document_thread", "document_id"] {
            out.insert((t.name.clone(), stamp.to_string()));
        }
    }
    out
}

/// The `(type, attribute)` a Choose, Point or Pick question asks, from its
/// rendered text.
pub(super) fn chosen(prompt: &ChatPrompt) -> (String, String) {
    let line = |tag: &str| {
        prompt
            .user
            .lines()
            .find_map(|l| l.strip_prefix(tag))
            .unwrap_or_else(|| panic!("a Choose question names its {tag}: {}", prompt.user))
            .to_string()
    };
    let ty = line("Type: ");
    let ty = ty.split(" (").next().unwrap().to_string();
    let attr = line("Attribute: ");
    let attr = attr.split([',', ' ']).next().unwrap().to_string();
    (ty, attr)
}

/// The phases that ask a field of a statement.
pub(super) const FIELD_PHASES: [&str; 3] = [
    "document_passes_choose",
    "document_passes_point",
    "document_passes_pick",
];

/// C1: a model question asks only what declared structure leaves open: no
/// question of a kind the plan does not generate, and no Choose of a value a
/// declared field, key, reference or derivation supplies.
#[tokio::test]
async fn c1_questions_ask_only_what_declared_structure_leaves_open() {
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let supplied = supplied(&f.policies());
        let run = run(&f).await;
        let mut kinds = BTreeMap::<String, usize>::new();
        for p in &run.prompts {
            let phase = p.phase_id.clone().unwrap_or_default();
            *kinds.entry(phase.clone()).or_default() += 1;
            assert!(
                matches!(
                    phase.as_str(),
                    "document_passes_locate"
                        | "document_passes_mention"
                        | "document_passes_choose"
                        | "document_passes_point"
                        | "document_passes_pick"
                        | "resolve_select"
                ),
                "{shape}: a question the plan does not generate: `{phase}`"
            );
            if FIELD_PHASES.contains(&phase.as_str()) {
                let asked = chosen(p);
                assert!(
                    !supplied.contains(&asked),
                    "{shape}: `{phase}` asked {asked:?}, which declared structure supplies"
                );
            }
        }
        assert!(
            kinds.contains_key("document_passes_locate"),
            "{shape}: {kinds:?}"
        );
    }
}

/// Each document's text with a code planted on every line, so any text of it
/// in a question is seen.
fn planted(f: &Fixture) -> Fixture {
    let (content, _) = f.fields();
    let mut out = f.clone();
    for (i, d) in out.documents.iter_mut().enumerate() {
        let body = d[&content].as_str().unwrap();
        let marked: Vec<String> = body
            .lines()
            .map(|l| {
                if l.trim().is_empty() {
                    l.to_string()
                } else {
                    format!("{l} (PQ{i}Z)")
                }
            })
            .collect();
        d.insert(content.clone(), Value::String(marked.join("\n")));
    }
    out
}

/// The planted codes of `0..n` documents `text` holds.
fn codes(text: &str, n: usize) -> Vec<usize> {
    (0..n)
        .filter(|i| text.contains(&format!("(PQ{i}Z)")))
        .collect()
}

/// A RESOLVE question's quote lines: `(line, quoted text)` for each line
/// carrying `QUOTE_LABEL`, its text the last string the line quotes.
fn quotes(user: &str) -> Vec<(&str, String)> {
    user.lines()
        .filter(|l| l.trim_start().starts_with(QUOTE_LABEL))
        .map(|l| {
            let at = l.rfind(": \"").expect("a quote line ends in its quote") + 2;
            (
                l,
                serde_json::from_str::<String>(&l[at..]).expect("a quoted string"),
            )
        })
        .collect()
}

/// C2, narrowed 2026-10-10: a READ turn holds one document's text. A RESOLVE
/// turn holds its own document's text plus, for its candidates, their
/// statements' cited lines, each on a line marked as quoted from another
/// document and no wider than the lines its statement cites; no other
/// document's text. That a quote never becomes a citation is
/// `resolve_records::tests::a_cite_copied_from_a_candidates_quote_is_refused_never_a_citation`:
/// the default path asks the forced choice, which cites nothing, and no
/// fixture record holds two documents, so a check here could not fail.
#[tokio::test]
async fn c2_a_read_turn_holds_one_document_and_a_resolve_turn_adds_only_marked_quotes() {
    for shape in SHAPES {
        let f = planted(&Fixture::load(shape));
        let n = f.documents.len();
        let (content, _) = f.fields();
        let lines: Vec<Vec<String>> = f
            .documents
            .iter()
            .map(|d| {
                d[&content]
                    .as_str()
                    .unwrap()
                    .lines()
                    .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect()
            })
            .collect();
        let run = run(&f).await;
        let (mut asked_resolve, mut quoted) = (0, 0);
        for p in &run.prompts {
            let phase = p.phase_id.as_deref().unwrap_or("-");
            if phase != "resolve_select" {
                let held = codes(&format!("{}\n{}", p.system, p.user), n);
                assert!(
                    held.len() <= 1,
                    "{shape}: a `{phase}` question holds the text of documents {held:?}:\n{}",
                    p.user
                );
                continue;
            }
            asked_resolve += 1;
            let (_, rest) = p.user.split_once("<<<\n").expect("the asked document");
            let (own_text, _) = rest.split_once("\n>>>").expect("the asked document ends");
            let own = codes(own_text, n);
            assert_eq!(
                own.len(),
                1,
                "{shape}: the asked document is one: {}",
                p.user
            );
            let outside: String = p
                .user
                .lines()
                .filter(|l| !l.trim_start().starts_with(QUOTE_LABEL))
                .collect::<Vec<_>>()
                .join("\n");
            let others: Vec<usize> = codes(&outside, n)
                .into_iter()
                .filter(|i| !own.contains(i))
                .collect();
            assert!(
                others.is_empty(),
                "{shape}: a RESOLVE question holds unmarked text of documents {others:?}:\n{}",
                p.user
            );
            for (line, quote) in quotes(&p.user) {
                quoted += 1;
                // The reader cites whole lines, so a quote bounded to them is
                // a run of whole lines of one other document, never a window.
                let whole_lines = (0..n).filter(|j| !own.contains(j)).any(|j| {
                    (0..lines[j].len())
                        .any(|a| (a..lines[j].len()).any(|b| lines[j][a..=b].join(" ") == quote))
                });
                assert!(
                    whole_lines,
                    "{shape}: a quote not bounded to another document's cited lines: {line}\n{}",
                    p.user
                );
            }
        }
        assert!(
            asked_resolve > 0,
            "{shape}: no RESOLVE question was asked, so C2 judged none"
        );
        assert!(
            quoted > 0,
            "{shape}: no RESOLVE question quoted a candidate, so the quote bound judged none"
        );
    }
}

/// A precision as C3 requires it: declared or estimated with a number in
/// 0..=1, or said to be unmeasured.
fn says_its_precision(by: &Value) -> bool {
    by["source"].as_str().is_some_and(|s| !s.is_empty())
        && match &by["precision"] {
            Value::String(s) => s == "unmeasured",
            Value::Object(o) if o.len() == 1 => {
                let (k, v) = o.iter().next().unwrap();
                matches!(k.as_str(), "declared" | "estimated")
                    && v.as_f64().is_some_and(|p| (0.0..=1.0).contains(&p))
            }
            _ => false,
        }
}

/// C3: every link and every value carries its source and that source's
/// precision, saying whether it is declared or estimated (or unmeasured).
#[tokio::test]
async fn c3_every_link_and_value_carries_its_source_and_precision() {
    let mut judged = Vec::new();
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let run = run(&f).await;
        let (mut links, mut values) = (0, 0);
        for d in run.lines(DECISIONS_FILE) {
            for o in d["outcomes"].as_array().unwrap() {
                let decided = &o["outcome"]["decided"];
                let links_a_record =
                    decided.is_object() && decided["decision"].as_str() != Some("opened");
                if links_a_record {
                    links += 1;
                    let by = o["by"].as_array();
                    assert!(
                        by.is_some_and(|b| !b.is_empty() && b.iter().all(says_its_precision)),
                        "{shape}: a link without its sources' precisions: {o}"
                    );
                }
            }
        }
        for v in run.lines(DERIVED_FILE) {
            values += 1;
            assert!(says_its_precision(&v["by"]), "{shape}: {v}");
        }
        for a in run.atoms() {
            let fields = &a["data"]["attributes"]["__document_read_fields"];
            for (_, field) in fields.as_object().into_iter().flatten() {
                if field["status"] == "supported" {
                    values += 1;
                    assert!(says_its_precision(&field["by"]), "{shape}: {field}");
                }
            }
        }
        // A fixture of two documents gives the estimator one pair, so on
        // news nothing links (campaign E2): C3 could not judge it. That is
        // said, never passed silently; the run must still have judged the
        // other shapes.
        if links + values == 0 {
            eprintln!("C3 could not judge {shape}: no link and no value to judge");
        } else {
            judged.push(shape);
        }
    }
    assert!(judged.len() >= 2, "C3 judged only {judged:?}");
}

/// C3 for the values identity rests on: RESOLVE says where each necessary
/// value it used came from (a declared field, the reader's Choose, its own
/// READ), and one a model chose never forbids a candidate outright. News, its
/// `what` chosen by the reader, the oracle spreading its Choose over values so
/// two documents' statements disagree, or there would be nothing to forbid.
#[tokio::test]
async fn c3_a_necessary_value_says_its_source_and_a_readers_never_forbids() {
    let f = Fixture::load("news");
    let store = tempfile::tempdir().unwrap();
    let run = run_choosing(&f, Asker::Daemon, store.path(), Choose::Spread).await;
    let picks: BTreeSet<usize> = run
        .prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_passes_choose"))
        .filter(|p| chosen(p).1 == "what")
        .map(|p| {
            let labels = p.response_schema.as_ref().unwrap()["enum"]
                .as_array()
                .unwrap()
                .len();
            spread_pick(p, labels)
        })
        .collect();
    assert!(
        picks.len() > 1,
        "news: the reader chose one value for every statement: {picks:?}"
    );
    let (mut by, mut vetoed) = (BTreeMap::<String, u64>::new(), 0);
    for d in run.lines(DECISIONS_FILE) {
        vetoed += d["vetoed"].as_u64().unwrap();
        let necessary = d["necessary"].as_object().unwrap_or_else(|| {
            panic!("news: RESOLVE does not say where its necessary values came from: {d}")
        });
        for (origin, n) in necessary {
            *by.entry(origin.clone()).or_default() += n.as_u64().unwrap();
        }
    }
    assert!(
        by.get("reader").copied().unwrap_or(0) > 0,
        "news: no value from the reader: {by:?}"
    );
    assert_eq!(
        by.get("supplied").copied().unwrap_or(0),
        0,
        "news: a model's choice labelled supplied: {by:?}"
    );
    assert_eq!(
        vetoed, 0,
        "news: a reader-chosen value forbade a candidate outright ({by:?})"
    );
}

/// What C4 and C6 compare: per decided type, the records by their names and
/// the sets of statements (document and anchor) they hold, each with the
/// derived values of its attributes. Atom and claim ids are left out: claim ids count in
/// build order, record ids hash the type's name.
fn records(run: &Run, policies: &OntologyPolicies) -> BTreeSet<String> {
    records_named(run, policies, &|n: &str| n.to_string())
}

/// [`records`] with every declared name passed through `name`, and a value
/// that names an atom shown as that atom's name: an atom id hashes its type's
/// name, so a renamed recipe mints other ids for the same particulars.
fn records_named(
    run: &Run,
    policies: &OntologyPolicies,
    name: &dyn Fn(&str) -> String,
) -> BTreeSet<String> {
    let decided: BTreeSet<&str> = policies
        .shape
        .types
        .iter()
        .filter(|t| t.identity_criterion.is_some() && t.source.is_none())
        .map(|t| t.name.as_str())
        .collect();
    let atoms = run.atoms();
    let shown: BTreeMap<String, String> = atoms
        .iter()
        .filter_map(|a| {
            let d = &a["data"];
            let label = d["canonical_name"].as_str().or(d["description"].as_str())?;
            Some((d["id"].as_str()?.to_string(), label.to_string()))
        })
        .collect();
    let mut held: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for a in &atoms {
        if a["atom_type"] != "Claim" {
            continue;
        }
        let d = &a["data"];
        if let Some(subject) = d["subject"].as_str() {
            let doc = d["attributes"]["document_id"].as_str().unwrap_or("-");
            let anchor = d["anchor"].as_str().unwrap_or("-");
            held.entry(subject.to_string())
                .or_default()
                .insert(format!("{doc}: {anchor}"));
        }
    }
    let derived: Vec<Value> = run.lines(DERIVED_FILE);
    atoms
        .iter()
        .filter_map(|a| {
            let d = &a["data"];
            let ty = d["entity_type"].as_str().or(d["event_type"].as_str())?;
            decided.contains(ty).then_some(())?;
            let id = d["id"].as_str()?;
            let values: BTreeSet<String> = derived
                .iter()
                .filter(|v| v["atom"] == id)
                .map(|v| {
                    let values: Vec<String> = v["values"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|x| {
                            let x = x.as_str().map_or_else(|| x.to_string(), str::to_string);
                            shown.get(&x).cloned().unwrap_or(x)
                        })
                        .collect();
                    format!(
                        "{}={values:?}",
                        name(v["attribute"].as_str().unwrap_or("-"))
                    )
                })
                .collect();
            // A record's name is its opening statement's words: which
            // statement opens it is the order-sensitive part (the clock).
            let named = d["canonical_name"].as_str().or(d["description"].as_str());
            Some(format!(
                "{} {named:?} {:?} {values:?}",
                name(ty),
                held.get(id)
            ))
        })
        .collect()
}

/// C4: the records are the same in any document order.
#[tokio::test]
async fn c4_records_are_the_same_in_any_document_order() {
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let mut reversed = f.clone();
        reversed.documents.reverse();
        let mut rotated = f.clone();
        rotated.documents.rotate_left(1);
        let base = records(&run(&f).await, &f.policies());
        assert!(!base.is_empty(), "{shape}: no record to compare");
        for (order, g) in [("reversed", reversed), ("rotated", rotated)] {
            assert_eq!(
                records(&run(&g).await, &g.policies()),
                base,
                "{shape}: the records change when the documents are {order}"
            );
        }
    }
}

/// C5: a run replays without the model: every answer recorded, and a replay
/// with no model writes the run's records byte for byte.
#[tokio::test]
async fn c5_a_run_replays_without_the_model() {
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let store = tempfile::tempdir().unwrap();
        let recorded = run_with(&f, Asker::Daemon, store.path()).await;
        let replayed = run_with(&f, Asker::Replay, store.path()).await;
        // The replay's own oracle is never asked: the store answered.
        assert!(
            replayed.prompts.is_empty(),
            "{shape}: the replay reached the model"
        );
        for (name, bytes) in &recorded.files {
            assert_eq!(
                &replayed.files[name], bytes,
                "{shape}: {name} differs on replay"
            );
        }
        assert!(!recorded.files["atoms.json"].is_empty());
    }
}

/// Every type and attribute name of a recipe, renamed: `t<i>` and `a<i>`.
pub(super) fn renamed(f: &Fixture) -> (Fixture, BTreeMap<String, String>) {
    let p = f.policies();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for (i, t) in p.shape.types.iter().enumerate() {
        names.insert(t.name.clone(), format!("t{i}x"));
    }
    let mut n = 0;
    for t in &p.shape.types {
        for a in &t.attributes {
            if !names.contains_key(&a.name) {
                names.insert(a.name.clone(), format!("a{n}x"));
                n += 1;
            }
        }
    }
    // Longest first, whole quoted names and path steps only.
    let mut order: Vec<(&String, &String)> = names.iter().collect();
    order.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    // The declaration only: `[corpus]` and the rest name no type.
    let at = f.recipe.find("[enrichment.ontology]").unwrap();
    let (head, mut recipe) = (f.recipe[..at].to_string(), f.recipe[at..].to_string());
    for (from, to) in &order {
        for (a, b) in [
            (format!("\"{from}\""), format!("\"{to}\"")),
            (format!("{from} = "), format!("{to} = ")),
            (format!("{{ {from} "), format!("{{ {to} ")),
            (format!("/ {from} "), format!("/ {to} ")),
            (format!("/ {from}\""), format!("/ {to}\"")),
            (format!("[{from}]"), format!("[{to}]")),
        ] {
            recipe = recipe.replace(&a, &b);
        }
    }
    (
        Fixture {
            recipe: head + &recipe,
            documents: f.documents.clone(),
        },
        names,
    )
}

/// C6 (ONTOLOGY_METHOD invariant 1): renaming every type and attribute changes
/// nothing but the names.
#[tokio::test]
async fn c6_renaming_every_type_and_attribute_changes_only_the_names() {
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let (g, names) = renamed(&f);
        assert_ne!(f.recipe, g.recipe);
        let base = records(&run(&f).await, &f.policies());
        assert!(!base.is_empty(), "{shape}: no record to compare");
        let back: BTreeMap<String, String> =
            names.iter().map(|(k, v)| (v.clone(), k.clone())).collect();
        let original = |n: &str| back.get(n).cloned().unwrap_or_else(|| n.to_string());
        let other = records_named(&run(&g).await, &g.policies(), &original);
        assert_eq!(other, base, "{shape}: renaming changed more than the names");
    }
}
