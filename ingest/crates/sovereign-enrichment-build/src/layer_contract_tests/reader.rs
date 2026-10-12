// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader's own passes on the default path, beside C1-C6: each pass
//! ONTOLOGY_METHOD §Reading specifies is asked on the fixtures, and what it
//! must never ask is never asked.

use super::*;

/// The line a Locate question asks about.
pub(super) fn asked_line(prompt: &ChatPrompt) -> &str {
    prompt
        .user
        .rsplit_once("\n\nLine ")
        .and_then(|(_, rest)| rest.split_once('"'))
        .and_then(|(_, rest)| rest.split_once("\"\n"))
        .map(|(text, _)| text)
        .expect("a Locate question names its line")
}

/// Prefill: every question about a document opens with its declared facts.
#[tokio::test]
async fn every_question_opens_with_its_documents_declared_facts() {
    for shape in SHAPES {
        let run = run(&Fixture::load(shape)).await;
        for p in run
            .prompts
            .iter()
            .filter(|p| p.phase_id.as_deref() != Some("resolve_select"))
        {
            assert!(
                p.user.starts_with("Declared facts of this document:\n"),
                "{shape}: {}",
                p.user
            );
        }
    }
}

/// The Locate questions of `run`.
pub(super) fn locates(run: &Run) -> Vec<&ChatPrompt> {
    run.prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_passes_locate"))
        .collect()
}

/// The questions of `run` that ask about one line (Locate's and Mention's,
/// both naming it as `asked_line` reads): where a fixture whose every kind is
/// an act shows which lines are asked at all.
pub(super) fn line_questions(run: &Run) -> Vec<&ChatPrompt> {
    run.prompts
        .iter()
        .filter(|p| {
            matches!(
                p.phase_id.as_deref(),
                Some("document_passes_locate" | "document_passes_mention")
            )
        })
        .collect()
}

/// Whether `prompt` is about the document whose declared id is `id`.
pub(super) fn in_doc(prompt: &ChatPrompt, id: &str) -> bool {
    prompt.user.contains(&format!("\nid: {id}\n"))
}

/// Line classes: the mail fixture's reply carries a line of the message it
/// answers under a mark, and it is never asked there (mail's one claim kind is
/// an act, so the line questions are Mention's), while it is asked in the
/// message it comes from. The greeting its author opens two messages with is
/// asked in both: repetition alone is no evidence. No citation marks the
/// carried line.
#[tokio::test]
async fn a_quoted_line_is_never_asked_and_a_repeated_one_is() {
    let run = run(&Fixture::load("mail")).await;
    let locate = line_questions(&run);
    let quoted = "The offer stands for thirty days; tell us if you would like to go ahead.";
    assert!(
        locate
            .iter()
            .any(|p| in_doc(p, "q2@alderpine.example") && asked_line(p) == quoted),
        "the quoted line is asked where it was written"
    );
    let cites: Vec<&ChatPrompt> = run
        .prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_passes_cite"))
        .collect();
    assert!(cites.iter().any(|p| in_doc(p, "q3@birchhall.example")));
    for p in cites.iter().filter(|p| in_doc(p, "q3@birchhall.example")) {
        assert!(
            p.user.contains(&format!("\n> {quoted}\n")),
            "the carried line is shown, unmarked: {}",
            p.user
        );
    }
    for p in &locate {
        let line = asked_line(p);
        assert!(
            !(in_doc(p, "q3@birchhall.example") && line.ends_with(quoted)),
            "a quote reached Locate: {line}"
        );
    }
    for doc in ["q1@birchhall.example", "q3@birchhall.example"] {
        assert!(
            locate
                .iter()
                .any(|p| in_doc(p, doc) && asked_line(p) == "Hello Tom,"),
            "{doc}: the repeated greeting was never asked"
        );
    }
}

/// Review 2026-10-10 (note 2347f4c6), on the default path: a tracker act
/// written in the same words in two threads, and again in its own thread
/// after a reopen, is asked in each document and each becomes a placed
/// statement of its own document.
#[tokio::test]
async fn a_repeated_act_is_read_and_placed_in_every_document() {
    let mut f = Fixture::load("issues");
    let closed = "Closed this issue as resolved in the latest release.";
    for (id, thread, at, body) in [
        ("event-101-closed", 101, "2026-02-11T09:00:00Z", closed),
        ("event-102-closed", 102, "2026-02-12T09:00:00Z", closed),
        (
            "event-101-reopened",
            101,
            "2026-02-13T09:00:00Z",
            "Reopened this issue after a report from another user.",
        ),
        (
            "event-101-closed-again",
            101,
            "2026-02-14T09:00:00Z",
            closed,
        ),
    ] {
        let doc = json!({"id": id, "thread": thread, "kind": "event", "author": "jon",
            "author_association": "MEMBER", "created_at": at, "body": body});
        f.documents.push(doc.as_object().unwrap().clone());
    }
    let run = run(&f).await;
    let locate = locates(&run);
    let placed: BTreeSet<String> = run
        .atoms()
        .iter()
        .filter(|a| a["atom_type"] == "Claim" && a["data"]["subject"].is_string())
        .filter_map(|a| a["data"]["attributes"]["document_id"].as_str())
        .map(str::to_string)
        .collect();
    for doc in [
        "event-101-closed",
        "event-102-closed",
        "event-101-closed-again",
    ] {
        assert!(
            locate
                .iter()
                .any(|p| in_doc(p, doc) && asked_line(p) == closed),
            "{doc}: the act never reached Locate"
        );
        assert!(
            placed.contains(doc),
            "{doc}: no placed statement: {placed:?}"
        );
    }
}

/// Every pass the reader's plan has is asked on the fixtures (mail: Point
/// on an open claim and subject field and a quantity and a time, Mention and
/// Pick on a reference to a header-sourced type; issues and news: Point), and
/// still asked when every type and attribute is renamed (C6's fixture).
#[tokio::test]
async fn every_pass_is_asked_and_still_asked_under_rename() {
    let phases = |run: &Run| -> BTreeSet<String> {
        run.prompts
            .iter()
            .filter_map(|p| p.phase_id.clone())
            .collect()
    };
    let mail = Fixture::load("mail");
    let (renamed, _) = super::contracts::renamed(&mail);
    for (name, f) in [("mail", mail), ("mail renamed", renamed)] {
        let asked = phases(&run(&f).await);
        for phase in [
            "document_passes_act",
            "document_passes_cite",
            "document_passes_mention",
            "document_passes_point",
            "document_passes_pick",
        ] {
            assert!(
                asked.contains(phase),
                "{name}: `{phase}` never asked: {asked:?}"
            );
        }
    }
    for shape in ["issues", "news"] {
        let asked = phases(&run(&Fixture::load(shape)).await);
        for phase in [
            "document_passes_locate",
            "document_passes_choose",
            "document_passes_point",
        ] {
            assert!(
                asked.contains(phase),
                "{shape}: `{phase}` never asked: {asked:?}"
            );
        }
    }
}

/// Review 2026-10-10 (note 2347f4c6): a subject field the reader reads (mail's
/// `order.piece`, pointed at in every statement) stays on the statement that
/// read it, as the claim's own fields do, through RESOLVE into the atlas: cited
/// in that statement's own document, with its source and precision. RESOLVE
/// writes no value of it on the record. Statements of one order that read it
/// differently each keep their own reading; a record holds a value of a read
/// field only by a declared fold.
#[tokio::test]
async fn a_read_subject_field_stays_on_its_statement_cited() {
    // An act gives one statement per order per document, and the oracle never
    // joins documents (on five messages its choice's weight is estimated at
    // 0, so `weigh` sees nothing raised): a kind no rule names, about orders,
    // is located line by line and puts more statements on the order an act's
    // document is about.
    let mut f = Fixture::load("mail");
    let act = "[[enrichment.ontology.types]]\nname = \"order_update\"";
    assert!(f.recipe.contains(act));
    f.recipe = f.recipe.replace(
        act,
        &format!(
            "[[enrichment.ontology.types]]\nname = \"order_note\"\nkind = \"claim\"\n\
             description = \"anything said about an order\"\nforce = \"assertive\"\n\
             subject = \"order\"\n\n{act}"
        ),
    );
    let run = run(&f).await;
    let atoms = run.atoms();
    let folded = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let body_of: BTreeMap<&str, String> = f
        .documents
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap(),
                folded(d["body"].as_str().unwrap()),
            )
        })
        .collect();
    let mut statements_of: BTreeMap<&str, usize> = BTreeMap::new();
    for c in atoms.iter().filter(|a| {
        a["atom_type"] == "Claim"
            && matches!(
                a["data"]["claim_kind"].as_str(),
                Some("order_update" | "order_note")
            )
    }) {
        let reading = &c["data"]["attributes"]["__document_read_subject_fields"]["piece"];
        assert!(
            reading.is_object(),
            "a statement lost its reading of its order's piece: {c}"
        );
        if reading["status"] != "supported" {
            assert!(
                reading["reason"].is_string(),
                "an unknown without why: {reading}"
            );
            continue;
        }
        assert!(
            super::contracts::says_its_precision(&reading["by"]),
            "{reading}"
        );
        let document = c["data"]["evidence"][0]["source_doc_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a statement with no document: {c}"));
        let evidence = folded(
            reading["evidence"]
                .as_str()
                .unwrap_or_else(|| panic!("an uncited reading: {reading}")),
        );
        assert!(
            !evidence.is_empty() && body_of[document].contains(&evidence),
            "a reading not cited in its own document {document}: {reading}"
        );
        let order = c["data"]["subject"]
            .as_str()
            .unwrap_or_else(|| panic!("a statement placed on no order: {c}"));
        *statements_of.entry(order).or_default() += 1;
    }
    assert!(
        statements_of.values().any(|n| *n >= 2),
        "mail: no order has two statements' readings: {statements_of:?}"
    );
    for order in statements_of.keys() {
        let record = atoms
            .iter()
            .find(|a| a["atom_type"] == "Entity" && a["data"]["id"] == *order)
            .unwrap_or_else(|| panic!("no record {order}"));
        assert!(
            record["data"]["attributes"].get("piece").is_none(),
            "RESOLVE wrote a value on record {order}: {record}"
        );
    }
}

/// A read field the recipe folds (news's `happening.what`, `by = "agree"`):
/// a record holds its statements' one reading, through the derivation writer,
/// citing the documents it came through, and none when they differ.
#[tokio::test]
async fn a_folded_read_field_is_served_on_its_record() {
    let run = run(&Fixture::load("news")).await;
    let atoms = run.atoms();
    let mut read: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in atoms.iter().filter(|a| a["atom_type"] == "Claim") {
        let reading = &c["data"]["attributes"]["__document_read_subject_fields"]["what"];
        if let (Some(subject), Some(value)) =
            (c["data"]["subject"].as_str(), reading["value"].as_str())
        {
            read.entry(subject).or_default().insert(value);
        }
    }
    let lines: Vec<Value> = run
        .lines(DERIVED_FILE)
        .into_iter()
        .filter(|l| l["type"] == "happening" && l["attribute"] == "what")
        .collect();
    let mut served = 0;
    for (record, values) in &read {
        let atom = atoms
            .iter()
            .find(|a| a["data"]["id"] == *record)
            .unwrap_or_else(|| panic!("no record {record}"));
        let line = lines
            .iter()
            .find(|l| l["atom"] == *record)
            .unwrap_or_else(|| panic!("record {record} has no derivation of `what`"));
        let held = &atom["data"]["attributes"]["what"];
        if let [one] = values.iter().collect::<Vec<_>>()[..] {
            served += 1;
            assert_eq!(held, *one, "record {record}: {line}");
            assert_eq!(line["outcome"], "decided", "{line}");
            assert!(
                line["documents"].as_array().is_some_and(|d| !d.is_empty()),
                "{line}"
            );
        } else {
            assert!(
                held.is_null(),
                "record {record} holds one of differing readings: {held}"
            );
            assert_eq!(line["outcome"], "conflict", "{line}");
        }
    }
    assert!(served > 0, "news: no record holds its statements' `what`");
}

/// A read reference's value carries its source and precision (C3) and is one
/// of its own document's candidates: a record its header fields name, or
/// words of its own text; never a record of another document.
#[tokio::test]
async fn a_picked_reference_is_cited_in_its_own_document() {
    let f = Fixture::load("mail");
    let run = run(&f).await;
    let mut picked = 0;
    for a in run.atoms() {
        let fields = &a["data"]["attributes"]["__document_read_fields"];
        let Some(field) = fields.get("named_customer") else {
            continue;
        };
        if field["status"] != "supported" {
            continue;
        }
        picked += 1;
        assert_eq!(field["by"]["source"], "reader_pick", "{field}");
        assert_eq!(field["by"]["precision"], "unmeasured", "{field}");
        // Our own side is in the declared exclusion set `ours`.
        assert!(
            !field["value"].as_str().unwrap().contains("alderpine"),
            "an excluded record was offered: {field}"
        );
    }
    assert!(picked > 0, "no reference was picked on mail");
}

/// The mail fixture with an order-level read reference its records fold by
/// `most` (`client`), and a seventh message, from our own side to our own
/// side, so its header offers no outside record and its picks are mentions.
fn with_client_and_internal_note() -> Fixture {
    let mut f = Fixture::load("mail");
    let piece = "  { name = \"piece\", type = \"text\", description = \"what is to be made\" },\n";
    assert!(f.recipe.contains(piece), "the mail recipe declares `piece`");
    f.recipe = f.recipe.replace(
        piece,
        &format!(
            "{piece}  {{ name = \"client\", type = \"ref\", of = \"company\", by = \"most\", \
             description = \"the customer the order is for\" }},\n"
        ),
    );
    let note = json!({"id": "n1@alderpine.example", "thread_id": "n1@alderpine.example",
        "from": "Tom Avery <tom.avery@alderpine.example>",
        "to": "Mia Holt <mia.holt@alderpine.example>",
        "date": "Sat, 07 Mar 2026 09:00:00 -0800", "subject": "Walnut timber",
        "body": "Mia,\n\nAlder and Pine Workshop will not build the walnut bookcase for the shop front.\n\
                 Please set the cut walnut timber aside until we find another use for it.\n\nTom"});
    f.documents.push(note.as_object().unwrap().clone());
    f
}

/// A served reference holds a record of its declared type that a document
/// its decision came through names, never our own side; a reading that names
/// no record is never served and stays on the derivation line, cited by its
/// statement, document and words.
#[tokio::test]
async fn a_served_reference_is_a_record_of_its_type_and_a_mention_stays_unresolved() {
    let f = with_client_and_internal_note();
    let run = run(&f).await;
    let atoms = run.atoms();
    let by_id: BTreeMap<&str, &Value> = atoms
        .iter()
        .filter_map(|a| Some((a["data"]["id"].as_str()?, a)))
        .collect();
    let lines: Vec<Value> = run
        .lines(DERIVED_FILE)
        .into_iter()
        .filter(|l| l["type"] == "order" && l["attribute"] == "client")
        .collect();
    // The header fields a document names its parties in, by document id.
    let headers = |doc: &str| -> String {
        let d = f.documents.iter().find(|d| d["id"] == doc).unwrap();
        ["from", "to", "cc"]
            .iter()
            .filter_map(|k| d.get(*k).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    };
    assert!(!lines.is_empty(), "no order folds `client`");
    let (mut served, mut unresolved) = (0, 0);
    for line in &lines {
        let held = &by_id[line["atom"].as_str().unwrap()]["data"]["attributes"]["client"];
        for u in line["unresolved"].as_array().into_iter().flatten() {
            unresolved += 1;
            assert!(
                u["claim"].as_str().is_some_and(|c| by_id.contains_key(c)),
                "an unresolved reading cites no statement: {u}"
            );
            assert!(u["document"].is_string(), "{u}");
            assert!(u["evidence"].is_string(), "{u}");
            assert!(u["words"].as_str().is_some_and(|w| !w.is_empty()), "{u}");
            assert_ne!(held, &u["words"], "an unresolved reading is served: {line}");
        }
        if held.is_null() {
            continue;
        }
        served += 1;
        let target = held
            .as_str()
            .and_then(|id| by_id.get(id))
            .unwrap_or_else(|| panic!("order holds {held}, which is no record: {line}"));
        assert_eq!(target["data"]["entity_type"], "company", "{line}");
        let domain = target["data"]["attributes"]["web_domain"].as_str().unwrap();
        assert!(
            !domain.ends_with("alderpine.example"),
            "our own side is served: {line}"
        );
        assert_eq!(line["outcome"], "decided", "{line}");
        let through = line["documents"].as_array().unwrap();
        assert!(
            through
                .iter()
                .any(|d| headers(d.as_str().unwrap()).contains(&format!("@{domain}>"))),
            "no document the decision came through names {domain}: {line}"
        );
    }
    assert!(served > 0, "no order holds its `client`");
    assert!(unresolved > 0, "the internal note's mention left no trace");
}

/// About on the default path (ONTOLOGY_METHOD §Reading): a document is a
/// member of the particular it is about although no line of it states a claim
/// kind. Tom's thanks quotes a line of his own offer; a quoted line is never
/// located, so the reply states nothing, but About names the order on that
/// line, and the order's record holds the reply, cited by the whole line, with
/// no claim kind of its own and no handoff marker left on it.
#[tokio::test]
async fn a_document_no_line_of_which_states_a_claim_kind_is_a_member_of_what_it_is_about() {
    let mut f = Fixture::load("mail");
    let (content, _) = f.fields();
    let quoted = "dollars, including finishing, and we can deliver them by the end of April.";
    let thanks = f
        .documents
        .iter_mut()
        .find(|d| d["id"] == "q5@alderpine.example")
        .unwrap();
    let body = format!("{}\n\n> {quoted}", thanks[&content].as_str().unwrap());
    thanks.insert(content.clone(), Value::String(body));
    let run = run(&f).await;
    assert!(
        !line_questions(&run)
            .iter()
            .any(|p| in_doc(p, "q5@alderpine.example") && asked_line(p).ends_with(quoted)),
        "the quoted line is never asked"
    );
    let atoms = run.atoms();
    let orders: BTreeSet<&str> = atoms
        .iter()
        .filter(|a| a["atom_type"] == "Entity" && a["data"]["entity_type"] == "order")
        .filter_map(|a| a["data"]["id"].as_str())
        .collect();
    let held: Vec<&Value> = atoms
        .iter()
        .filter(|a| {
            a["atom_type"] == "Claim"
                && a["data"]["evidence"][0]["source_doc_id"] == "q5@alderpine.example"
        })
        .collect();
    assert_eq!(held.len(), 1, "one claim, the order it is about: {held:?}");
    let claim = &held[0]["data"];
    assert!(claim["claim_kind"].is_null(), "{claim}");
    assert!(
        claim["subject"]
            .as_str()
            .is_some_and(|s| orders.contains(s)),
        "the thanks sits on an order record: {claim}"
    );
    assert_eq!(claim["anchor"], format!("> {quoted}"), "{claim}");
    assert!(
        claim["attributes"].get("__document_read_about").is_none(),
        "the handoff marker is RESOLVE's alone: {claim}"
    );
    let failures = &run.files["resolution_failures.json"];
    assert!(
        !failures.contains("order l3:"),
        "its subject is left to RESOLVE, never bound by name: {failures}"
    );
}

/// `with_client_and_internal_note` with a company identity criterion, so
/// RESOLVE decides the company mentions (`decides_mentions`).
fn with_company_criterion() -> Fixture {
    let mut f = with_client_and_internal_note();
    let company = "name = \"company\"\nkind = \"entity\"\n";
    assert!(
        f.recipe.contains(company),
        "the mail recipe declares `company`"
    );
    f.recipe = f.recipe.replace(
        company,
        &format!("{company}identity_criterion = \"the same organisation, however it is named\"\n"),
    );
    f
}

/// A company named in prose binds to the record its message's header names
/// (cast instance 1; the delivery journey's first acceptance case). With a
/// company criterion and a model that answers RESOLVE right, "Birch Hall" in
/// Rosa's messages is the birchhall.example record, "Quay Market" in Ivo's is
/// quaymarket.example, and "Alder and Pine Workshop" is our own
/// alderpine.example: no company record is opened for them, the header
/// records keep their ids, each order's client holds its customer's id, and
/// our own company is never served and never unjudged.
#[tokio::test]
async fn a_company_named_in_prose_binds_to_its_header_record_and_is_the_orders_client() {
    const PARTIES: &[(&str, &str, &str)] = &[
        ("company", "Birch Hall", "web_domain: birchhall example"),
        ("company", "Quay Market", "web_domain: quaymarket example"),
        (
            "company",
            "Alder and Pine Workshop",
            "web_domain: alderpine example",
        ),
        ("order", "oak chairs", "oak chairs"),
        ("order", "walnut", "walnut"),
    ];
    let domain_of = |name: &str| match name {
        "Birch Hall" => "birchhall.example",
        "Quay Market" => "quaymarket.example",
        _ => "alderpine.example",
    };
    let companies = |run: &Run| -> BTreeSet<String> {
        run.atoms()
            .iter()
            .filter(|a| a["data"]["entity_type"] == "company")
            .map(|a| a["data"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    let base = run(&with_client_and_internal_note()).await;
    let run = run_knowing(&with_company_criterion(), PARTIES).await;
    let atoms = run.atoms();
    let headers = companies(&base);
    assert!(
        headers.is_subset(&companies(&run)),
        "a header record lost its id"
    );
    // Each company record a mention opened, by the words put on it.
    let opened: BTreeMap<String, Vec<&str>> = companies(&run)
        .difference(&headers)
        .map(|id| {
            let words = atoms
                .iter()
                .filter(|a| a["data"]["subject"] == id.as_str())
                .filter_map(|a| a["data"]["attributes"]["__document_read_words"].as_str())
                .collect();
            (id.clone(), words)
        })
        .collect();
    assert!(
        opened.is_empty(),
        "every company here has a header record, yet mentions opened {}: {opened:?}",
        opened.len()
    );
    let company = |domain: &str| -> String {
        atoms
            .iter()
            .find(|a| {
                a["data"]["entity_type"] == "company"
                    && a["data"]["attributes"]["web_domain"] == domain
            })
            .and_then(|a| a["data"]["id"].as_str())
            .unwrap_or_else(|| panic!("no company record for {domain}"))
            .to_string()
    };
    let mut bound = BTreeSet::new();
    for a in &atoms {
        let Some(words) = a["data"]["attributes"]["__document_read_words"].as_str() else {
            continue;
        };
        let Some((_, name, _)) = PARTIES
            .iter()
            .find(|(of, name, _)| *of == "company" && words.contains(name))
        else {
            continue;
        };
        let domain = domain_of(name);
        assert_eq!(
            a["data"]["subject"].as_str(),
            Some(company(domain).as_str()),
            "the mention {words:?} is not on {domain}'s record: {a}"
        );
        bound.insert(*name);
    }
    assert_eq!(
        bound.len(),
        3,
        "a party's mention was never decided: {bound:?}"
    );
    let lines: Vec<Value> = run
        .lines(DERIVED_FILE)
        .into_iter()
        .filter(|l| l["type"] == "order" && l["attribute"] == "client")
        .collect();
    let client = |doc: &str| -> &Value {
        lines
            .iter()
            .find(|l| {
                l["documents"]
                    .as_array()
                    .is_some_and(|d| d.contains(&json!(doc)))
            })
            .unwrap_or_else(|| panic!("no order's client is read from {doc}: {lines:?}"))
    };
    let orders: Vec<Value> = run
        .lines(DECISIONS_FILE)
        .into_iter()
        .filter(|d| d["type"] == "order")
        .map(|d| {
            json!([
                d["document"],
                d["candidates"],
                d["outcomes"]
                    .as_array()
                    .map(|o| o.iter().map(|o| o["outcome"].clone()).collect::<Vec<_>>())
            ])
        })
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "the chairs and the bookcase, each one order: {lines:?}; RESOLVE's order decisions: {orders:?}"
    );
    assert_eq!(
        client("q1@birchhall.example")["values"],
        json!([company("birchhall.example")])
    );
    assert_eq!(
        client("b1@quaymarket.example")["values"],
        json!([company("quaymarket.example")])
    );
    let ours = company("alderpine.example");
    for line in &lines {
        assert!(
            line["unjudged"].is_null(),
            "every client here is judged by its domain: {line}"
        );
        assert!(
            !line["values"]
                .as_array()
                .is_some_and(|v| v.contains(&json!(ours))),
            "our own side is served: {line}"
        );
    }
}
