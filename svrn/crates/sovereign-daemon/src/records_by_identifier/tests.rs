// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records by identifier against replies recorded from Crossref and Open
//! Library (`RecordedFetch::recorded_2026_10_15`), offline. The library is
//! the conformance suite's records shelf (`fixture/records.json`) and two
//! books declaring an ISBN.

use serde_json::json;

use super::*;

fn table() -> ResolverTable {
    ResolverTable::from_data_file().expect("the data file loads")
}

/// Each document's declared metadata, by name; a document declaring none
/// is absent. The first three are the conformance suite's records shelf
/// (`cmnwlth/crates/oicp-conformance/fixture/records.json`), restated here
/// so the package needs no tree outside it; `oicp_records_e2e` installs
/// that file itself.
fn library() -> BTreeMap<String, Map<String, Value>> {
    let mut out = BTreeMap::new();
    let books = [
        json!({"name": "fair-principles", "metadata": {"id": "fair-principles",
               "type": "article-journal", "title": "A Title the Recipe Declares",
               "DOI": "10.1038/sdata.2016.18"}}),
        json!({"name": "no-such-doi", "metadata": {"id": "no-such-doi",
               "type": "article-journal", "DOI": "10.0000/oicp-conformance.unresolvable"}}),
        json!({"name": "plain-note"}),
        json!({"name": "sicp", "metadata": {"id": "sicp", "ISBN": "978-0-262-51087-5"}}),
        json!({"name": "no-such-isbn", "metadata": {"id": "no-such-isbn", "type": "book",
               "title": "A Book Nobody Catalogued", "ISBN": "9780999999990"}}),
    ];
    for d in &books {
        if let Some(Value::Object(m)) = d.get("metadata") {
            out.insert(d["name"].as_str().unwrap().to_string(), m.clone());
        }
    }
    out
}

/// Each declared document's record, its install allowing lookups or not.
async fn records(allow: bool, fetch: &RecordedFetch) -> BTreeMap<String, Option<MergedRecord>> {
    let (t, lib) = (table(), library());
    let answers = resolve(&t, lib.values(), &RecordLookups::for_install(allow, fetch)).await;
    lib.iter()
        .map(|(n, d)| (n.clone(), merge(&t, d, answers.as_ref())))
        .collect()
}

fn golden(text: &str) -> MergedRecord {
    serde_json::from_str(text).expect("a golden record parses")
}

#[test]
fn the_data_file_maps_doi_to_crossref_and_isbn_to_open_library() {
    let t = table();
    let rows: Vec<(&str, &str, ReplyShape)> = t
        .resolvers
        .iter()
        .map(|r| (r.variable.as_str(), r.name.as_str(), r.reply))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("DOI", "crossref", ReplyShape::Csl),
            ("ISBN", "open_library", ReplyShape::OpenLibraryBooks)
        ]
    );
    assert!(ResolverTable::parse(&DATA_FILE.replace("concurrency", "concurency")).is_err());
    // A type read as one CSL lacks, and a name variable `keep` lacks.
    let not_csl = DATA_FILE.replace(
        r#"journal-article = "article-journal""#,
        r#"journal-article = "journal-article""#,
    );
    assert!(ResolverTable::parse(&not_csl)
        .unwrap_err()
        .contains("not a CSL type"));
    let not_kept = DATA_FILE.replace(r#"names = ["author""#, r#"names = ["composer""#);
    assert!(ResolverTable::parse(&not_kept)
        .unwrap_err()
        .contains("`composer`"));
}

#[test]
fn a_declared_identifier_is_asked_for_normalised() {
    let t = table();
    let (doi, isbn) = (&t.resolvers[0], &t.resolvers[1]);
    assert_eq!(
        doi.url_for("https://doi.org/10.1038/sdata.2016.18"),
        doi.url_for("10.1038/sdata.2016.18")
    );
    assert_eq!(
        doi.url_for("doi:10.1002/(SICI)1097"),
        "https://api.crossref.org/works/10.1002/%28SICI%291097/transform/application/vnd.citationstyles.csl+json"
    );
    assert_eq!(
        isbn.url_for("978-0-262 51087-5"),
        isbn.url_for("9780262510875")
    );
}

/// PASS BAR: recorded Crossref and Open Library replies give the committed
/// golden records.
#[tokio::test]
async fn recorded_replies_give_the_golden_records() {
    let fetch = RecordedFetch::recorded_2026_10_15();
    let r = records(true, &fetch).await;
    for (name, want) in [
        (
            "fair-principles",
            include_str!("../../tests/fixtures/records/golden_fair-principles.json"),
        ),
        (
            "sicp",
            include_str!("../../tests/fixtures/records/golden_sicp.json"),
        ),
    ] {
        let got = r[name].as_ref().expect("a record");
        assert_eq!(
            got,
            &golden(want),
            "{name}: {}",
            serde_json::to_string_pretty(got).unwrap()
        );
    }
    assert!(!r.contains_key("plain-note"), "it declares nothing");
    assert_eq!(fetch.asked().len(), 4, "each identifier asked once");
}

/// Platform row PB1b's PASS BAR: Crossref's reply for a paper, recorded
/// 2026-10-18, gives zoracite's golden record for it
/// (`fixtures/v1-bib/references.json` `einstein1905`, restated here) field for
/// field, and every field is CSL 1.0.2's: `type` `article-journal`, not
/// Crossref's `journal-article`; `ISSN` one string, not a list; each name
/// `family` and `given` alone. FAILING INPUT: the record as Crossref sends
/// it, which pandoc 3.7.0.2 refuses ("expected String or Number, but
/// encountered Array").
#[tokio::test]
async fn a_recorded_crossref_reply_is_the_papers_csl_record() {
    let t = table();
    let doi = "10.1002/andp.19053221004";
    let fetch = RecordedFetch::default().with(
        t.resolvers[0].url_for(doi),
        200,
        include_str!("../../tests/fixtures/records/crossref_10.1002_andp.19053221004.json"),
    );
    let declared = json!({"id": "einstein1905", "DOI": doi});
    let declared = declared.as_object().unwrap();
    let answers = resolve(&t, [declared], &RecordLookups::for_install(true, &fetch)).await;
    let record = merge(&t, declared, answers.as_ref()).expect("a record").csl;
    let golden = json!({
        "DOI": "10.1002/andp.19053221004",
        "URL": "http://dx.doi.org/10.1002/andp.19053221004",
        "author": [{"family": "Einstein", "given": "A."}],
        "container-title": "Annalen der Physik",
        "id": "einstein1905",
        "issue": "10",
        "issued": {"date-parts": [[1905, 1]]},
        "page": "891-921",
        "publisher": "Wiley",
        "title": "Zur Elektrodynamik bewegter K\u{f6}rper",
        "type": "article-journal",
        "volume": "322"
    });
    for (field, want) in golden.as_object().unwrap() {
        assert_eq!(record.get(field), Some(want), "{field}: {record:?}");
    }
    assert_eq!(record.get("ISSN"), Some(&json!("0003-3804")), "{record:?}");
}

/// FAILING INPUT: a field invented for an identifier that did not resolve.
#[tokio::test]
async fn an_identifier_that_does_not_resolve_is_named_and_adds_nothing() {
    let lib = library();
    let r = records(true, &RecordedFetch::recorded_2026_10_15()).await;
    for name in ["no-such-doi", "no-such-isbn"] {
        let rec = r[name].as_ref().expect("an identifier, so a record");
        let outcomes: Vec<LookupOutcome> = rec.lookups.iter().map(|l| l.outcome).collect();
        assert_eq!(outcomes, vec![LookupOutcome::Unresolved], "{name}");
        assert_eq!(
            rec.csl, lib[name],
            "{name}: nothing added to what it declared"
        );
        assert_eq!(rec.looked_up().count(), 0, "{name}: {:?}", rec.origins);
    }
}

/// FAILING INPUT: a declared title overwritten by the looked-up one.
#[tokio::test]
async fn a_declared_title_is_kept_over_the_looked_up_one() {
    let r = records(true, &RecordedFetch::recorded_2026_10_15()).await;
    let rec = r["fair-principles"].as_ref().expect("a record");
    assert_eq!(rec.csl["title"], json!("A Title the Recipe Declares"));
    assert_eq!(rec.origins["title"], DECLARED);
    assert_eq!(
        rec.origins["container-title"], "crossref",
        "{:?}",
        rec.origins
    );
    assert_eq!(rec.csl["container-title"], json!("Scientific Data"));
}

/// FAILING INPUT: a lookup sent for a client that did not allow it.
#[tokio::test]
async fn no_lookup_is_sent_for_an_install_that_did_not_allow_it() {
    let lib = library();
    let fetch = RecordedFetch::recorded_2026_10_15();
    let r = records(false, &fetch).await;
    assert_eq!(fetch.asked(), Vec::<String>::new(), "nothing left the host");
    for (name, rec) in &r {
        let Some(rec) = rec else { continue };
        assert!(
            rec.lookups
                .iter()
                .all(|l| l.outcome == LookupOutcome::NotAllowed),
            "{name}: {:?}",
            rec.lookups
        );
        assert_eq!(rec.csl, lib[name], "{name}");
    }
}

/// A resolver that does not answer, or answers other than its record or
/// its not-found, is unreachable and adds nothing.
#[tokio::test]
async fn an_unreachable_resolver_is_named_and_adds_nothing() {
    let t = table();
    let d = library().remove("fair-principles").unwrap();
    for fetch in [
        RecordedFetch::default(),
        RecordedFetch::default().with(t.resolvers[0].url_for("10.1038/sdata.2016.18"), 503, "busy"),
        RecordedFetch::default().with(
            t.resolvers[0].url_for("10.1038/sdata.2016.18"),
            200,
            "<html>",
        ),
    ] {
        let answers = resolve(&t, [&d], &RecordLookups::Allowed(&fetch)).await;
        let rec = merge(&t, &d, answers.as_ref()).unwrap();
        assert_eq!(rec.lookups[0].outcome, LookupOutcome::Unreachable);
        assert_eq!(rec.csl, d);
    }
}

fn hit_over(corpus: &str, sha: &str) -> KnowledgeResult {
    use oicp_types::evidence::{Document, SourceRef};
    let document = Document {
        text_sha256: sha.into(),
        extractor: "plaintext@1".into(),
        source: SourceRef {
            id: "notes".into(),
            sha256: None,
        },
        metadata: None,
        record: None,
    };
    serde_json::from_value(json!({
        "content": "c", "corpus_id": corpus, "score": 0.5,
        "document": document,
    }))
    .expect("a hit parses")
}

/// A `_records.json` that does not parse is unreadable, not "kept none" (ARCH 6).
/// On a hit that is `document unreadable` (v0.5 §3), never a document that
/// merely has no record; an absent file leaves hits as they were.
#[test]
fn a_corrupt_records_file_makes_the_hit_document_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let path = Corpus::records_in(dir.path());
    let sha = "a".repeat(64);

    let mut hits = [hit_over("c", &sha)];
    attach_to_hits(dir.path(), "c", &mut hits);
    assert!(
        hits[0].document.is_some(),
        "no file: the hit is left as it is"
    );

    std::fs::write(&path, "{ not json").unwrap();
    assert!(matches!(
        InstallRecords::read(&path),
        RecordsRead::Unreadable(_)
    ));
    let mut hits = [hit_over("c", &sha)];
    attach_to_hits(dir.path(), "c", &mut hits);
    assert!(hits[0].document.is_none());
    assert_eq!(
        hits[0].document_absent.as_deref(),
        Some(oicp_types::evidence::reasons::DOCUMENT_UNREADABLE)
    );
}

/// Absent, kept-empty, and failed-to-make are three different reads, and
/// only the first two allow an answer of "no record".
#[test]
fn not_written_kept_and_unreadable_are_told_apart() {
    let dir = tempfile::tempdir().unwrap();
    let path = Corpus::records_in(dir.path());
    assert_eq!(InstallRecords::read(&path), RecordsRead::NotWritten);

    let kept = InstallRecords {
        allow_lookups: true,
        ..Default::default()
    };
    std::fs::write(&path, serde_json::to_vec(&kept).unwrap()).unwrap();
    assert_eq!(InstallRecords::read(&path), RecordsRead::Kept(kept));

    let failed = InstallRecords {
        allow_lookups: true,
        unread: Some("the installed corpus does not open: gone".into()),
        ..Default::default()
    };
    std::fs::write(&path, serde_json::to_vec(&failed).unwrap()).unwrap();
    assert!(matches!(
        InstallRecords::read(&path),
        RecordsRead::Unreadable(w) if w.contains("does not open")
    ));
}

/// A corrupt file is an unknown allowance, so the install reads again; an
/// absent one is a pre-records install that allowed none.
#[test]
fn a_corrupt_records_file_leaves_the_allowance_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let corpus = Corpus::named(dir.path(), "c").unwrap();
    std::fs::create_dir_all(corpus.root()).unwrap();
    assert_eq!(allowed_by_last_install(&corpus), Some(false));
    std::fs::write(corpus.records_path(), "{ not json").unwrap();
    assert_eq!(allowed_by_last_install(&corpus), None);
}
