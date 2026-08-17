// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parity with the deleted Python decider, plus the decision rules it
//! carried, each named by an input that would break it.
//!
//! # The oracle pattern, and why the oracle is a committed OUTPUT
//!
//! `scripts/sec_facts.py` was THE decider until this module replaced it.
//! Running it was the reference; its output on
//! `tests/fixtures/sec_facts/aapl-companyfacts.json` is committed under
//! `oracle/`, and `renderer_matches_the_python_oracle_exactly` asserts
//! the Rust reproduces it. Then the Python was DELETED in the same
//! commit — two implementations of alias precedence, period typing,
//! annual-filing selection or restatement supersession is the §15 smell
//! in the subsystem whose whole purpose is not to substitute silently
//! (ARCH §10.6, principle 8). The committed oracle is what keeps the
//! behaviour once the oracle program is gone.
//!
//! # Parity is CATEGORICAL, not a rate
//!
//! Every rendered fact, every rendered line and every coverage count
//! must match. "Most of them match" is a failure with the diverging
//! cases named, because the divergence is exactly where a wrong number
//! reaches a user.
//!
//! # The fixture is a REDUCTION, and the reduction was validated first
//!
//! Apple's real companyfacts is 3.8 MB. The committed fixture keeps
//! every one of its 503 `us-gaap` tag KEYS (coverage counts are key-set
//! arithmetic) and the VERBATIM unit history of the 24 tags any concept
//! chain names — the only tags the decider ever resolves — and drops the
//! unit bodies of the other 479 plus the unread `dei` namespace. That
//! reduction was proven decision-neutral before it was used as a
//! measurement (ARCH §18.4): the Python oracle run against the reduced
//! fixture and against the full 3.8 MB document produced BYTE-IDENTICAL
//! `sec_facts.json`, 20 `facts-*.txt` and `_unmapped_concepts.json`
//! (2026-08-16; the only diff was `_render_manifest.json`'s absolute
//! output path, which names the run's output directory, not a decision).
//!
//! # Fields allowed to differ: ONE, and it is not a decision
//!
//! `sec_facts.json` BYTES differ for integral values — Python's
//! `json.dump` writes `416161000000`, `serde_json` writes
//! `416161000000.0` — because [`SecFact::value`] is `f64` in the type
//! BOTH sides serialize through. The parity assertion therefore compares
//! the two documents PARSED INTO `SecFactStore`, which is exactly how
//! the shipped `sec_facts` tool reads the sidecar, so no consumer can
//! observe the difference. Every rendered `facts-*.txt` line — the
//! surface retrieval actually indexes — is byte-identical, integral
//! values included.

use super::*;

fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sec_facts")
}

fn load_fixture() -> Value {
    let raw = std::fs::read_to_string(fixture_dir().join("aapl-companyfacts.json"))
        .expect("companyfacts fixture is committed");
    serde_json::from_str(&raw).expect("companyfacts fixture parses")
}

fn load_map() -> ConceptMap {
    // The SHIPPED registry, not a test copy: a concept map that drifted
    // from the product's would make this test agree with nothing.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../sovereign-recipes/sec-filings-company/concept-map.toml");
    ConceptMap::from_toml(&std::fs::read_to_string(&path).expect("concept map is committed"))
        .expect("concept map parses")
}

fn render_fixture() -> RenderOutput {
    let facts = load_fixture();
    let cmap = load_map();
    render(RenderRequest {
        companyfacts: &facts,
        concept_map: &cmap,
        ticker: None,
        fiscal_years: None,
    })
    .expect("the fixture renders")
}

// ── the parity test: the deliverable of order sec-facts-decider-port ────────

#[test]
fn renderer_matches_the_python_oracle_exactly() {
    let out = render_fixture();
    let oracle = fixture_dir().join("oracle");

    // 1. The typed sidecar, compared through the type the tool reads.
    let store = out.sidecar.expect("the fixture resolves facts");
    let oracle_store: SecFactStore = serde_json::from_str(
        &std::fs::read_to_string(oracle.join("sec_facts.json")).expect("oracle sidecar committed"),
    )
    .expect("the Python's sidecar parses into SecFactStore");
    let ours = serde_json::to_value(&store).expect("serializes");
    let theirs = serde_json::to_value(&oracle_store).expect("serializes");
    assert_eq!(
        ours, theirs,
        "typed fact sidecar diverges from the Python decider"
    );

    // Parity is categorical: assert the store is not trivially small, or
    // an empty render would "match" a broken oracle.
    assert_eq!(store.concepts.len(), 20, "oracle renders 20 concepts");
    let fact_count: usize = store.concepts.values().map(|c| c.facts.len()).sum();
    assert_eq!(fact_count, 60, "oracle renders 60 typed facts");

    // 2. The ingested fact lines, byte for byte.
    let mut oracle_files: Vec<String> = std::fs::read_dir(&oracle)
        .expect("oracle dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("facts-") && n.ends_with(".txt"))
        .collect();
    oracle_files.sort();
    let ours_files: Vec<String> = out.fact_files.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(ours_files, oracle_files, "rendered fact files differ");
    for (name, body) in &out.fact_files {
        let want = std::fs::read_to_string(oracle.join(name)).expect("oracle fact file");
        assert_eq!(body, &want, "rendered lines diverge in {name}");
    }

    // 3. The coverage deliverable (F5's growth chart).
    let oracle_unmapped: UnmappedReport = serde_json::from_str(
        &std::fs::read_to_string(oracle.join("_unmapped_concepts.json"))
            .expect("oracle unmapped report committed"),
    )
    .expect("parses");
    assert_eq!(
        out.unmapped, oracle_unmapped,
        "unmapped-tag report diverges from the Python decider"
    );
    assert_eq!(out.unmapped.filer_tags_total, 503);
    assert_eq!(out.unmapped.unmapped.len(), 479);
}

/// The REFUSAL half of parity, and it needs its own oracle: the Apple
/// fixture resolves every concept for every default year, so
/// `renderer_matches_the_python_oracle_exactly` never exercises a
/// refusal. Refusals are what a user reads when the corpus cannot
/// answer — the honesty half of the whole subsystem — so
/// `oracle/refusals.json` carries the Python's verbatim text for all ten
/// refusal branches, captured while the Python still existed, each case
/// self-contained (its own map and companyfacts where they are not the
/// shipped ones).
///
/// The set carries a CONTROL that must ANSWER. A refusal oracle whose
/// every case refuses is also satisfied by a decider that refuses
/// everything — the exact failure mode F2's competence half was amended
/// to catch (quality/initiative-bars.toml, 2026-08-14).
#[test]
fn refusal_texts_match_the_python_oracle_exactly() {
    #[derive(Deserialize)]
    struct Case {
        name: String,
        concept_map: Option<String>,
        companyfacts: Option<Value>,
        concept: String,
        period: String,
        refused: bool,
        reason: Option<String>,
        tag: Option<String>,
        value: Option<f64>,
        unit: Option<String>,
        basis: Option<String>,
        accession: Option<String>,
    }

    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("oracle/refusals.json"))
            .expect("refusal oracle committed"),
    )
    .expect("refusal oracle parses");
    assert_eq!(cases.len(), 11, "the captured oracle has 11 cases");

    let shipped_map = load_map();
    let shipped_facts = load_fixture();
    let mut refused = 0;
    let mut answered = 0;
    for c in &cases {
        let cmap = match &c.concept_map {
            Some(src) => ConceptMap::from_toml(src).expect("case map parses"),
            None => shipped_map.clone(),
        };
        let facts = c
            .companyfacts
            .clone()
            .unwrap_or_else(|| shipped_facts.clone());
        let got = resolve(&cmap, &facts, &c.concept, &c.period).expect("resolves");
        match (c.refused, got) {
            (true, Resolution::Refused(r)) => {
                assert_eq!(
                    Some(&r.reason),
                    c.reason.as_ref(),
                    "refusal text diverges in case {}",
                    c.name
                );
                refused += 1;
            }
            (false, Resolution::Fact(f)) => {
                assert_eq!(Some(&f.tag), c.tag.as_ref(), "case {}", c.name);
                assert_eq!(f.value.as_f64(), c.value, "case {}", c.name);
                assert_eq!(Some(&f.unit), c.unit.as_ref(), "case {}", c.name);
                assert_eq!(Some(&f.basis), c.basis.as_ref(), "case {}", c.name);
                assert_eq!(
                    f.accession.as_ref(),
                    c.accession.as_ref(),
                    "case {}",
                    c.name
                );
                answered += 1;
            }
            (want, _) => panic!(
                "case {}: expected refused={want}, got the other verdict",
                c.name
            ),
        }
    }
    assert_eq!(refused, 10, "ten refusal branches");
    assert_eq!(answered, 1, "one control that must answer");
}

/// The reduction that made the fixture committable is only decision-
/// neutral because every tag a concept chain names has exactly ONE unit.
/// If that stops holding, `nearest_period`'s cross-unit tie-break becomes
/// observable — and `serde_json`'s map order is alphabetical where
/// Python's was the document's, so the refusal text could diverge.
/// Structural, not remembered (principle 10).
#[test]
fn every_mapped_tag_has_exactly_one_unit() {
    let facts = load_fixture();
    let cmap = load_map();
    let gaap = us_gaap(&facts);
    for (concept, entry) in &cmap.concepts {
        for tag in &entry.tags {
            if let Some(units) = gaap.get(tag) {
                assert!(
                    units.len() <= 1,
                    "{concept}/{tag} reports {} units; cross-unit ordering is now observable",
                    units.len()
                );
            }
        }
    }
}

// ── the decisions the port had to preserve, each with a failing input ───────

/// A filer override REPLACES the global chain; it never merges, and the
/// order inside the chain decides. Failing input: a document carrying
/// BOTH the global chain's first tag and the override's. Merge or append
/// and the override case still answers
/// `RevenueFromContractWithCustomerExcludingAssessedTax`, because that
/// is the global chain's head.
#[test]
fn filer_override_replaces_the_global_chain_whole() {
    let year = |val: f64| {
        serde_json::json!({"units": {"USD": [
            {"start": "2024-09-29", "end": "2025-09-27", "val": val, "form": "10-K",
             "fp": "FY", "accn": "a", "filed": "2025-10-31"}
        ]}})
    };
    let doc = serde_json::json!({
        "cik": 320193, "entityName": "Apple Inc.",
        "facts": {"us-gaap": {
            "RevenueFromContractWithCustomerExcludingAssessedTax": year(1.0),
            "Revenues": year(2.0),
        }}
    });

    let cmap = load_map();
    let Resolution::Fact(global) = resolve(&cmap, &doc, "revenue", "FY2025").expect("resolves")
    else {
        panic!("the global chain resolves");
    };
    assert_eq!(
        global.tag,
        "us-gaap:RevenueFromContractWithCustomerExcludingAssessedTax"
    );

    let mut overridden = cmap;
    overridden
        .filers
        .get_mut("cik0000320193")
        .expect("Apple is a registry row")
        .overrides
        .insert(
            "revenue".to_string(),
            ConceptOverride {
                tags: vec!["Revenues".to_string()],
                kind: None,
            },
        );
    let Resolution::Fact(f) = resolve(&overridden, &doc, "revenue", "FY2025").expect("resolves")
    else {
        panic!("the override chain resolves");
    };
    assert_eq!(f.tag, "us-gaap:Revenues");
    assert_eq!(f.value.as_f64(), Some(2.0));
}

/// The `fy` field names the FILING, not the period. Apple's
/// `2023-10-01..2024-09-28` revenue appears under both `fy=2024` and
/// `fy=2025`; keying on `fy` returns the wrong year's figure
/// confidently. Failing input: FY2024 must be the period ENDING in 2024.
#[test]
fn fiscal_year_comes_from_the_facts_own_end_date_never_the_fy_field() {
    let facts = load_fixture();
    let cmap = load_map();
    let Resolution::Fact(f) = resolve(&cmap, &facts, "revenue", "FY2024").expect("resolves") else {
        panic!("FY2024 revenue is present");
    };
    assert_eq!(f.end, "2024-09-28");
    assert_eq!(f.start.as_deref(), Some("2023-10-01"));
    assert!(f.basis.starts_with("fiscal year FY2024 ("), "{}", f.basis);
}

/// Quarterly comparatives must never masquerade as annual figures: a
/// duration fact only counts as annual when its own span is 330-380
/// days. Failing input: drop the window and a 10-K's Q4 column would
/// match FY.
#[test]
fn annual_selection_rejects_spans_outside_the_330_380_day_window() {
    let quarter = serde_json::json!({
        "start": "2024-06-30", "end": "2024-09-28", "val": 1.0,
        "form": "10-K", "fp": "FY", "accn": "x", "filed": "2024-11-01"
    });
    let year = serde_json::json!({
        "start": "2023-10-01", "end": "2024-09-28", "val": 2.0,
        "form": "10-K", "fp": "FY", "accn": "x", "filed": "2024-11-01"
    });
    assert!(!is_annual_10k_fact(&quarter, Some(ConceptKind::Duration)).expect("dates parse"));
    assert!(is_annual_10k_fact(&year, Some(ConceptKind::Duration)).expect("dates parse"));
}

/// An unmapped concept is REPORTED BY NAME, never defaulted to a near
/// neighbour. `services_revenue` is deliberately absent from the map
/// (companyfacts is consolidated-only), and `revenue` is the tempting
/// neighbour.
#[test]
fn unmapped_concept_refuses_by_name_rather_than_reaching_for_a_neighbour() {
    let facts = load_fixture();
    let cmap = load_map();
    let Resolution::Refused(r) =
        resolve(&cmap, &facts, "services_revenue", "FY2025").expect("resolves")
    else {
        panic!("an unmapped concept must refuse");
    };
    assert!(
        r.reason.contains("services_revenue")
            && r.reason.contains("not in the normalization map")
            && r.reason.contains("never defaulted to a near neighbour"),
        "{}",
        r.reason
    );
}

/// A period the filing range cannot carry refuses AND NAMES the nearest
/// period that does exist — naming is reporting; the value is never
/// substituted (ARCH §18.3).
#[test]
fn absent_period_refuses_naming_the_nearest_available_never_substituting_it() {
    let facts = load_fixture();
    let cmap = load_map();
    let Resolution::Refused(r) = resolve(&cmap, &facts, "revenue", "FY2099").expect("resolves")
    else {
        panic!("FY2099 cannot exist");
    };
    let near = r.nearest_available.expect("the nearest period is named");
    assert!(
        r.reason
            .contains("Nearest available period (named, not substituted)"),
        "{}",
        r.reason
    );
    assert!(near.end.starts_with("20"), "{near:?}");
    // The refusal names the period; it must not carry the figure.
    assert!(!r.reason.contains("million USD"), "{}", r.reason);
}

/// An instant concept asked with a date range, and a duration concept
/// asked with a bare date, both refuse rather than coercing.
#[test]
fn concept_kind_mismatch_refuses_in_both_directions() {
    let facts = load_fixture();
    let cmap = load_map();
    let Resolution::Refused(a) =
        resolve(&cmap, &facts, "total_assets", "2024-09-28..2025-09-27").expect("resolves")
    else {
        panic!("an instant concept cannot take a range");
    };
    assert!(
        a.reason.contains("is an instant (balance-sheet) concept"),
        "{}",
        a.reason
    );

    let Resolution::Refused(b) = resolve(&cmap, &facts, "revenue", "2025-09-27").expect("resolves")
    else {
        panic!("a duration concept cannot take a bare date");
    };
    assert!(b.reason.contains("is a duration concept"), "{}", b.reason);
}

/// Long CamelCase tags are word-split so tantivy's `RemoveLongFilter`
/// (~40 chars) cannot drop them from the FTS index; concatenating the
/// words recovers the exact tag. Short tags stay verbatim.
#[test]
fn long_tags_are_word_split_for_the_fts_index_and_short_ones_are_not() {
    assert_eq!(fts_tag("GrossProfit"), "us-gaap:GrossProfit");
    let long = "IncomeLossFromContinuingOperationsBeforeIncomeTaxesExtraordinaryItemsNoncontrollingInterest";
    let split = fts_tag(long);
    assert!(
        split.starts_with("us-gaap: Income Loss From Continuing"),
        "{split}"
    );
    assert_eq!(
        split.trim_start_matches("us-gaap: ").replace(' ', ""),
        long,
        "concatenating the words must recover the exact tag"
    );
}

/// The write-side figure grammar: USD at millions grain carries the
/// magnitude word AND the raw value, so the retrieval judge recovers the
/// exact figure without a rounding round-trip. Non-USD units render as
/// the filer wrote them.
#[test]
fn figure_rendering_carries_unit_magnitude_and_raw_value() {
    let n = |s: &str| serde_json::from_str::<serde_json::Number>(s).expect("number");
    assert_eq!(
        fmt_value(&n("416161000000"), "USD"),
        "$416,161 million USD (raw: 416,161,000,000)"
    );
    assert_eq!(fmt_value(&n("7.46"), "USD/shares"), "7.46 USD/shares");
    assert_eq!(fmt_value(&n("500000"), "USD"), "500000 USD");
    // Half-to-even, matching the Python's `f"{v:,.0f}"` exactly:
    // -2.5 rounds to -2, not -3. Verified against the oracle.
    assert_eq!(
        fmt_value(&n("-2500000"), "USD"),
        "$-2 million USD (raw: -2,500,000)"
    );
    assert_eq!(
        fmt_value(&n("2500000"), "USD"),
        "$2 million USD (raw: 2,500,000)"
    );
}

/// Values differing across filings are a RESTATEMENT: the latest filed
/// supersedes and the supersession is traced, never silent. Values that
/// agree cite the EARLIEST filing — the original disclosure.
#[test]
fn restatement_supersedes_and_agreement_cites_the_original_disclosure() {
    let cmap = ConceptMap::from_toml(
        r#"
schema = 1
[concepts.total_assets]
label = "Total assets"
kind = "instant"
tags = ["Assets"]
"#,
    )
    .expect("map parses");
    let doc = |first: f64, second: f64| {
        serde_json::json!({
            "cik": 320193, "entityName": "T Inc.",
            "facts": {"us-gaap": {"Assets": {"units": {"USD": [
                {"end": "2024-09-28", "val": first, "form": "10-K", "fp": "FY",
                 "accn": "orig", "filed": "2024-11-01"},
                {"end": "2024-09-28", "val": second, "form": "10-K", "fp": "FY",
                 "accn": "restated", "filed": "2025-11-01"}
            ]}}}}
        })
    };

    let agreed = doc(10.0, 10.0);
    let Resolution::Fact(f) = resolve(&cmap, &agreed, "total_assets", "FY2024").expect("resolves")
    else {
        panic!("resolves");
    };
    assert_eq!(
        f.accession.as_deref(),
        Some("orig"),
        "agreement cites the original"
    );

    let restated = doc(10.0, 11.0);
    let Resolution::Fact(f) =
        resolve(&cmap, &restated, "total_assets", "FY2024").expect("resolves")
    else {
        panic!("resolves");
    };
    assert_eq!(
        f.accession.as_deref(),
        Some("restated"),
        "a differing value is a restatement; the latest filed supersedes"
    );
}

/// Two distinct annual periods ending in one calendar year (the 53-week
/// transition edge) refuse rather than guessing which one is meant.
#[test]
fn two_annual_periods_in_one_calendar_year_refuse_rather_than_guess() {
    let cmap = ConceptMap::from_toml(
        r#"
schema = 1
[concepts.revenue]
label = "Total revenue (net sales)"
kind = "duration"
tags = ["Revenues"]
"#,
    )
    .expect("map parses");
    let doc = serde_json::json!({
        "cik": 320193, "entityName": "T Inc.",
        "facts": {"us-gaap": {"Revenues": {"units": {"USD": [
            {"start": "2023-09-01", "end": "2024-08-25", "val": 1.0, "form": "10-K",
             "fp": "FY", "accn": "a", "filed": "2024-10-01"},
            {"start": "2023-12-31", "end": "2024-12-28", "val": 2.0, "form": "10-K",
             "fp": "FY", "accn": "b", "filed": "2025-02-01"}
        ]}}}}
    });
    let Resolution::Refused(r) = resolve(&cmap, &doc, "revenue", "FY2024").expect("resolves")
    else {
        panic!("two distinct annual periods must refuse");
    };
    assert!(
        r.reason
            .starts_with("ambiguous: multiple distinct periods match 'FY2024'"),
        "{}",
        r.reason
    );
}

/// A tag chain no filer tag satisfies refuses NAMING THE CHAIN TRIED —
/// the reader can see which aliases were considered.
#[test]
fn absent_tag_chain_refuses_naming_every_alias_it_tried() {
    let cmap = ConceptMap::from_toml(
        r#"
schema = 1
[concepts.revenue]
label = "Total revenue (net sales)"
kind = "duration"
tags = ["NotATag", "AlsoNotATag"]
"#,
    )
    .expect("map parses");
    let doc = serde_json::json!({
        "cik": 320193, "entityName": "T Inc.", "facts": {"us-gaap": {}}
    });
    let Resolution::Refused(r) = resolve(&cmap, &doc, "revenue", "FY2024").expect("resolves")
    else {
        panic!("no chain tag present must refuse");
    };
    assert_eq!(
        r.reason,
        "none of the tags ['NotATag', 'AlsoNotATag'] is present in T Inc.'s companyfacts"
    );
    assert_eq!(
        r.tags_tried.as_deref(),
        Some(["NotATag".to_string(), "AlsoNotATag".to_string()].as_slice())
    );
}

/// A fact whose provenance is missing is REFUSED from the sidecar rather
/// than stored with a blank accession — a figure nobody can trace is the
/// thing this whole subsystem exists to prevent.
#[test]
fn a_fact_with_no_accession_is_not_stored() {
    let f = ResolvedFact {
        entity: "T Inc.".into(),
        cik: "0000320193".into(),
        concept: "revenue".into(),
        label: "Total revenue (net sales)".into(),
        tag: "us-gaap:Revenues".into(),
        value: serde_json::from_str("1.0").expect("number"),
        unit: "USD".into(),
        start: Some("2023-10-01".into()),
        end: "2024-09-28".into(),
        basis: "fiscal year FY2024 (2023-10-01 to 2024-09-28)".into(),
        accession: None,
        form: Some("10-K".into()),
        filed: Some("2024-11-01".into()),
    };
    let err = f
        .to_sec_fact()
        .expect_err("a fact with no accession is refused");
    assert!(err.to_string().contains("cannot be cited"), "{err}");
}

/// `fiscal_years` overrides the default shortlist, and the shortlist is
/// the latest 3 the filer reports annually.
#[test]
fn explicit_fiscal_years_override_the_latest_three_default() {
    let facts = load_fixture();
    let cmap = load_map();
    let out = render(RenderRequest {
        companyfacts: &facts,
        concept_map: &cmap,
        ticker: None,
        fiscal_years: Some(&[2024]),
    })
    .expect("renders");
    let store = out.sidecar.expect("resolves");
    for (concept, cf) in &store.concepts {
        assert_eq!(cf.facts.len(), 1, "{concept} should carry one year");
        assert_eq!(cf.facts[0].fiscal_year, 2024, "{concept}");
    }
    let default = render_fixture().sidecar.expect("resolves");
    assert_eq!(
        default.concepts["revenue"].facts.len(),
        3,
        "the default shortlist is the latest 3"
    );
}

/// A `[filers.…] ticker` row WINS over the caller's fallback — the
/// registry is the authority on a filer's ticker, not the install form.
#[test]
fn the_registry_ticker_wins_over_the_callers_fallback() {
    let facts = load_fixture();
    let cmap = load_map();
    let out = render(RenderRequest {
        companyfacts: &facts,
        concept_map: &cmap,
        ticker: Some("WRONG"),
        fiscal_years: None,
    })
    .expect("renders");
    assert_eq!(out.sidecar.expect("resolves").ticker, "AAPL");
}

/// The `cik` is zero-padded to 10 digits whichever spelling the document
/// uses; a document with neither refuses rather than rendering a corpus
/// keyed on nothing.
#[test]
fn cik_is_the_ten_digit_sec_identity_or_the_document_is_refused() {
    assert_eq!(
        cik10(&serde_json::json!({"cik": 320193})).expect("number spelling"),
        "0000320193"
    );
    assert_eq!(
        cik10(&serde_json::json!({"cik": "320193"})).expect("string spelling"),
        "0000320193"
    );
    assert!(cik10(&serde_json::json!({})).is_err());
}
