// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the SEC typed-fact store (`sec_facts/mod.rs`).

use super::fixtures::store;
use super::*;

/// The failing input by name, reproduced 2/2 on 2026-08-18: asked for
/// Apple's "Mac segment revenue", the planner sent `concept="revenue"`
/// — legal, resolvable, and the wrong SCOPE.
#[test]
fn scope_qualifier_catches_the_segment_ask() {
    for q in [
        "What was Apple Inc.'s Mac segment revenue in FY2025?",
        "What was Apple Inc.'s iPhone segment revenue in FY2025?",
        "Revenue by region for FY2025?",
        "What did the Services division earn in FY2025?",
        "Break out revenue by geography.",
        "What was the product line revenue?",
    ] {
        assert!(
            scope_qualifier_in_question(q).is_some(),
            "should have caught a below-entity ask: {q}"
        );
    }
}

/// Inert on ordinary company-wide questions — the same scoping the
/// calendar check uses. A guard that fires on everything is a guard
/// nobody can ship.
#[test]
fn scope_qualifier_is_inert_on_consolidated_asks() {
    for q in [
            "What was Apple Inc.'s revenue in FY2025?",
            "What was Apple Inc.'s Payments to acquire property, plant and equipment (capex) in FY2025?",
            "What was net income in FY2024?",
            "How much cash did Apple hold at the end of FY2025?",
        ] {
            assert_eq!(
                scope_qualifier_in_question(q),
                None,
                "must not fire on a consolidated ask: {q}"
            );
        }
}

/// The refusal must name the source limit AND what IS available —
/// "we cannot" without "here is what we can" is the abstention §7.7
/// forbids. It must also refuse to offer the consolidated figure.
#[test]
fn scope_refusal_names_the_limit_and_the_alternatives() {
    let r = SecRefusal::ScopeNotInSource {
        concept: "revenue".to_string(),
        qualifier: "segment".to_string(),
        mapped: vec!["revenue".to_string(), "net_income".to_string()],
    };
    let reason = r.reason();
    assert!(reason.contains("consolidated-only"), "{reason}");
    assert!(reason.contains("segment"), "{reason}");
    assert!(
        reason.contains("net_income"),
        "names alternatives: {reason}"
    );
    assert!(
        reason.contains("not offered as a substitute"),
        "must refuse the substitution outright: {reason}"
    );
}

#[test]
fn fiscal_year_lookup_returns_the_typed_fact() {
    let s = store();
    let f = lookup(&s, "revenue", "FY2025").expect("hit");
    assert_eq!(f.value, 416_161_000_000.0);
    assert_eq!(f.accession, "0000320193-25-000079");
    assert_eq!(f.start.as_deref(), Some("2024-09-29"));
}

#[test]
fn instant_lookup_by_date_and_by_fy() {
    let s = store();
    assert_eq!(
        lookup(&s, "total_assets", "2025-09-27").expect("hit").value,
        359_241_000_000.0
    );
    assert_eq!(
        lookup(&s, "total_assets", "FY2025").expect("hit").value,
        359_241_000_000.0
    );
}

#[test]
fn unmapped_concept_refuses_by_name_and_names_the_source_limit() {
    // The failing input, by name (§6.4): services_revenue is a
    // dimensional concept companyfacts cannot carry.
    let s = store();
    let r = lookup(&s, "services_revenue", "FY2025").expect_err("must refuse");
    let reason = r.reason();
    assert!(
        reason.contains("services_revenue"),
        "names the concept: {reason}"
    );
    assert!(
        reason.contains("consolidated-only"),
        "names the source limit: {reason}"
    );
    assert!(
        reason.contains("revenue"),
        "names what IS available: {reason}"
    );
}

#[test]
fn stale_concept_refuses_naming_the_nearest_available_period() {
    // advertising_expense exists — latest FY2015. FY2025 refuses and
    // NAMES 2015-09-26; the FY2015 value is never substituted.
    let s = store();
    let r = lookup(&s, "advertising_expense", "FY2025").expect_err("must refuse");
    match &r {
        SecRefusal::NoFactForPeriod {
            available_period_ends,
            ..
        } => {
            assert_eq!(available_period_ends, &vec!["2015-09-26".to_string()]);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
}

#[test]
fn calendar_year_duration_refuses_not_approximates() {
    // The frame-label trap: Apple's FY2025 fact is bucketed CY2025 by
    // SEC, but a calendar-2025 request has no matching fact.
    let s = store();
    let r = lookup(&s, "revenue", "2025-01-01..2025-12-31").expect_err("must refuse");
    assert!(
        matches!(r, SecRefusal::BeyondAsOf { .. }),
        "calendar 2025 ends after the as-of period end: {r:?}"
    );
}

#[test]
fn every_period_refusal_names_the_periods_that_do_exist() {
    // WAS WRONG: a period running past the as-of filing refused with
    // BeyondAsOf, whose reason named the as-of filing and the latest
    // period end but NEVER the period ends that DO carry a fact —
    // the "technically honest, bad" abstention §7.7 forbids. The
    // test above (`calendar_year_duration_refuses_not_approximates`)
    // asserted only the variant, so it pinned the defect in place.
    let s = store();
    // The failing inputs, by name (ARCH §18.1).
    for spec in ["2025-01-01..2025-12-31", "FY2030"] {
        let reason = lookup(&s, "revenue", spec)
            .expect_err("must refuse")
            .reason();
        assert!(reason.contains(spec), "names what was asked: {reason}");
        // Case-folded: the phrase is slice 1's, but it sits at a
        // sentence boundary in some variants and mid-sentence in
        // others — the naming is the contract, not the capital A.
        assert!(
            reason
                .to_lowercase()
                .contains("available period end date(s), named not substituted"),
            "slice 1's naming form: {reason}"
        );
        assert!(
            reason.contains("2024-09-28") && reason.contains("2025-09-27"),
            "names the period ends that DO exist: {reason}"
        );
    }
    // ...and the freshness fact is not lost in the process.
    let r = lookup(&s, "revenue", "FY2030")
        .expect_err("must refuse")
        .reason();
    assert!(
        r.contains("0000320193-25-000079"),
        "as-of filing still named: {r}"
    );
}

#[test]
fn calendar_question_is_read_only_in_the_clear_case() {
    // The honesty half: a stated calendar period is recognised, so
    // the tool can refuse when it is handed a fiscal period instead.
    assert_eq!(
        calendar_period_in_question(
            "What was Apple's revenue for the calendar year 2025, January through December?"
        ),
        Some(("2025-01-01".to_string(), "2025-12-31".to_string()))
    );
    assert_eq!(
        calendar_period_in_question("Apple revenue for calendar 2024"),
        Some(("2024-01-01".to_string(), "2024-12-31".to_string()))
    );

    // The COMPETENCE half, and the reason this is scoped to the
    // clear case (§7.6 is PAIRED — the honesty fix must not cost a
    // fiscal answer). Every one of these must read as "no calendar
    // period stated", or a legitimate question starts refusing.
    for q in [
        "What was Apple's revenue in fiscal 2025?",
        "How much did Apple's revenue grow year over year from fiscal 2024 to fiscal 2025?",
        "What was Apple's gross margin percentage in fiscal 2025?",
        "What were Apple's total assets as of September 27, 2025?",
        "What was Apple's revenue for FY2025?",
        // A calendar phrase with no year states no period.
        "Does Apple report on a calendar year?",
    ] {
        assert_eq!(
            calendar_period_in_question(q),
            None,
            "must not fire on: {q}"
        );
    }
}

#[test]
fn period_beyond_as_of_refuses_with_freshness_reason() {
    let s = store();
    let r = lookup(&s, "revenue", "FY2030").expect_err("must refuse");
    match &r {
        SecRefusal::BeyondAsOf {
            latest_period_end,
            as_of_accession,
            ..
        } => {
            assert_eq!(latest_period_end, "2025-09-27");
            assert_eq!(as_of_accession, "0000320193-25-000079");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert!(r.reason().contains("2025-09-27"));
}

#[test]
fn kind_mismatch_refuses_with_guidance() {
    let s = store();
    assert!(matches!(
        lookup(&s, "total_assets", "2024-09-29..2025-09-27"),
        Err(SecRefusal::KindMismatch {
            kind: ConceptKind::Instant,
            ..
        })
    ));
    assert!(matches!(
        lookup(&s, "revenue", "2025-09-27"),
        Err(SecRefusal::KindMismatch {
            kind: ConceptKind::Duration,
            ..
        })
    ));
}

#[test]
fn bad_period_spec_refuses() {
    let s = store();
    assert!(matches!(
        lookup(&s, "revenue", "Q3-2025"),
        Err(SecRefusal::BadPeriod { .. })
    ));
}

#[test]
fn ratio_emits_formula_with_full_precision_inputs() {
    let s = store();
    let gp = lookup(&s, "gross_profit", "FY2025").unwrap();
    let rev = lookup(&s, "revenue", "FY2025").unwrap();
    let d = ratio("gross_profit", gp, "revenue", rev).expect("nonzero denominator");
    assert!((d.value - 0.469_05).abs() < 1e-4);
    assert!(d.formula.contains("$195,201,000,000.00"), "{}", d.formula);
    assert!(d.formula.contains("$416,161,000,000.00"), "{}", d.formula);
    assert!(d.formula.contains("46.91%"), "{}", d.formula);
}

#[test]
fn change_emits_delta_and_percent() {
    let s = store();
    let cur = lookup(&s, "revenue", "FY2025").unwrap();
    let prior = lookup(&s, "revenue", "FY2024").unwrap();
    let (abs, pct) = change("revenue", cur, prior);
    assert_eq!(abs.value, 25_126_000_000.0);
    let pct = pct.expect("nonzero prior");
    assert!((pct.value - 0.064_25).abs() < 1e-4);
    assert!(pct.formula.contains("6.43%"), "{}", pct.formula);
}

#[test]
fn compact_formats_are_default_audit_parseable() {
    // `$416,161 million` is a $-token + magnitude word — parseable
    // even by the default numeric-audit scope.
    assert_eq!(fmt_compact(416_161_000_000.0, "USD"), "$416,161 million");
    assert_eq!(fmt_compact(7.46, "USD/shares"), "$7.46");
    assert_eq!(fmt_full(416_161_000_000.0, "USD"), "$416,161,000,000.00");
    assert_eq!(fmt_pct(0.469_05), "46.91%");
}

// ── the §7.3 authority claim, both directions ────────────────────────

#[test]
fn claims_an_entity_plus_concept_question() {
    let s = store();
    let m = store_claims(&s, "What was Apple's total revenue in fiscal 2025?")
        .expect("entity + concept term must claim");
    assert!(m.contains("apple") && m.contains("revenue"), "{m}");
    // A segment question still claims — the refusal downstream is
    // the honest answer, and it only exists if the store claims.
    assert!(store_claims(&s, "What was Apple's Services revenue in fiscal 2025?").is_some());
    // Ticker works as the entity term.
    assert!(store_claims(&s, "AAPL gross margin percentage for fiscal 2025?").is_some());
}

#[test]
fn never_claims_without_an_entity_match() {
    // The failing inputs, by name (ARCH §18.1): generic finance
    // wording — literally exemplar router/exemplars.toml:345 — and
    // another company's question must NOT claim.
    let s = store();
    assert_eq!(
        store_claims(&s, "What's the difference between gross and net margin?"),
        None
    );
    assert_eq!(
        store_claims(&s, "What was Microsoft's revenue in fiscal 2025?"),
        None
    );
    // Entity without any concept term: no claim either.
    assert_eq!(store_claims(&s, "Who founded Apple?"), None);
}

#[test]
fn concept_resolution_normalizes_and_follows_declared_aliases() {
    let s = store();
    // Separator normalization: planner spellings of the id itself.
    assert_eq!(resolve_concept(&s, "Gross-Profit").unwrap(), "gross_profit");
    assert_eq!(resolve_concept(&s, "gross profit").unwrap(), "gross_profit");
    // Declared ask_terms alias ("gross margin" is the concept-map
    // author's own synonym, from the label's parenthetical).
    assert_eq!(resolve_concept(&s, "gross margin").unwrap(), "gross_profit");
    // The failing input, by name: an invented id refuses unmapped —
    // never a near-neighbour guess.
    assert!(matches!(
        resolve_concept(&s, "selling_and_marketing_expense"),
        Err(SecRefusal::UnmappedConcept { .. })
    ));
}

#[test]
fn ambiguous_alias_refuses_naming_both_candidates() {
    // Two concepts DECLARING the same ask_term is a map bug the
    // resolver must surface, not adjudicate.
    let mut s = store();
    if let Some(cf) = s.concepts.get_mut("gross_profit") {
        cf.ask_terms.push("sales".to_string()); // collides with revenue's
    }
    match resolve_concept(&s, "sales") {
        Err(SecRefusal::AmbiguousConcept { candidates, .. }) => {
            assert!(candidates.contains(&"gross_profit".to_string()));
            assert!(candidates.contains(&"revenue".to_string()));
        }
        other => panic!("expected AmbiguousConcept, got {other:?}"),
    }
}

#[test]
fn explanation_shaped_questions_are_not_claimed() {
    // The store is authoritative for FIGURES; "why" answers live in
    // prose and stay on the retrieval path (measured F4 regression,
    // 2026-08-15). Both directions:
    let s = store();
    assert_eq!(
        store_claims(
            &s,
            "According to Apple's 10-K, why did Mac net sales increase in fiscal 2025?"
        ),
        None
    );
    assert!(
        store_claims(&s, "How much were Apple's net sales in fiscal 2025?").is_some(),
        "figure-shaped questions still claim"
    );
}

/// The F5 demand instrument's cross-language contract, pinned from the
/// Rust side. `scripts/sec-miss-demand.py` greps this module for the
/// `F5_DEMAND_ANCHOR` declaration; this asserts the event the module
/// actually emits carries a field of that name, and that it sits on
/// the single covering path rather than in the arms.
///
/// Why source inspection rather than capturing the log line: capturing
/// needs a `tracing-subscriber` dev-dependency this crate does not
/// have, and adding one to pin a field name is a dep for a string
/// (ARCH §8.2). The failing input is named in each message.
#[test]
fn f5_demand_event_is_emitted_once_per_ask_under_the_declared_anchor() {
    let src = include_str!("mod.rs");
    assert_eq!(
        F5_DEMAND_ANCHOR, "f5_demand",
        "the reader (scripts/sec-miss-demand.py, ANCHOR) greps for this \
             exact spelling"
    );
    assert!(
        src.contains(&format!("{F5_DEMAND_ANCHOR} = true")),
        "resolve_concept no longer emits a `{F5_DEMAND_ANCHOR} = true` \
             field. The const and the event have drifted apart, and \
             sec-miss-demand.py would match nothing and report a clean \
             coverage score for a store nobody instrumented — absence \
             reported as success is exactly the §18.3 failure."
    );
    // ONE emission site. A second would double-count every ask and
    // silently halve the reported miss rate.
    assert_eq!(
        src.matches(&format!("{F5_DEMAND_ANCHOR} = true")).count(),
        1,
        "the {F5_DEMAND_ANCHOR} event must be emitted from exactly one \
             place (§10.6). More than one site makes the denominator — the \
             number of asks — depend on which path ran."
    );
    // ...and it must cover every arm, i.e. sit in `resolve_concept`
    // itself rather than inside the match on the outcome.
    let body = src
        .split_once("pub fn resolve_concept(")
        .expect("resolve_concept is the covering entry point")
        .1
        .split_once("fn resolve_concept_inner(")
        .expect("the inner resolver is separate so the event covers all arms")
        .0;
    assert!(
        body.contains(&format!("{F5_DEMAND_ANCHOR} = true")),
        "the {F5_DEMAND_ANCHOR} event moved out of the covering \
             `resolve_concept` wrapper. Emitted from an arm, it stops \
             counting the asks that did NOT take that arm."
    );
}

/// Every outcome the reader classifies is one this resolver can
/// actually produce, and they are distinguishable. The reader keys the
/// numerator on `outcome=unmapped` exactly; if a miss started
/// reporting as `other`, the measured miss rate would silently fall.
#[test]
fn f5_demand_outcomes_cover_the_arms_the_reader_distinguishes() {
    let s = store();
    assert!(resolve_concept(&s, "gross profit").is_ok(), "resolved arm");
    assert!(
        matches!(
            resolve_concept(&s, "deferred revenue"),
            Err(SecRefusal::UnmappedConcept { .. })
        ),
        "unmapped arm — the one the reader counts as a miss"
    );
    let mut amb = store();
    if let Some(cf) = amb.concepts.get_mut("gross_profit") {
        cf.ask_terms.push("sales".to_string());
    }
    assert!(
        matches!(
            resolve_concept(&amb, "sales"),
            Err(SecRefusal::AmbiguousConcept { .. })
        ),
        "ambiguous arm — a map bug, NOT a coverage gap, so the reader \
             must be able to tell it apart from `unmapped`"
    );
}

/// Capture what a `tracing` event really renders to, so the log-line
/// grammar the Python reader parses is pinned by a rendered event and
/// not by a string this test composed.
#[derive(Clone, Default)]
struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for CaptureWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// VALIDATE THE INSTRUMENT BEFORE THE RESULT (ARCH §18.4). The F5
/// demand number is only as good as the agreement between this
/// module (writer) and `scripts/sec-miss-demand.py` (reader), and
/// that agreement is a LOG LINE — the least type-checked interface
/// in the system. So render real events and assert every field the
/// reader's grammar depends on:
///
///   - the `f5_demand` anchor it greps for;
///   - `requested="..."` QUOTED, so a concept spelled with a space
///     survives. This is why the field is emitted with `?` (Debug):
///     rendered with Display, `gross profit` would arrive unquoted
///     and the reader would silently truncate it at the space;
///   - `outcome=` naming the arm, so a miss is distinguishable from
///     an ambiguity and from a resolution;
///   - `consolidated_only=` as a bare `true`/`false`.
///
/// The failing input is named in every message.
#[test]
fn f5_demand_event_renders_the_grammar_the_reader_parses() {
    let buf = CaptureWriter::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let s = store();
    tracing::subscriber::with_default(sub, || {
        // A concept spelled with a SPACE, and a miss.
        let _ = resolve_concept(&s, "gross profit");
        let _ = resolve_concept(&s, "deferred revenue");
    });
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| l.contains(F5_DEMAND_ANCHOR))
        .collect();

    assert_eq!(
        lines.len(),
        2,
        "expected exactly one {F5_DEMAND_ANCHOR} event per ask (2 asks). \
             Got {}:\n{out}",
        lines.len()
    );
    assert!(
        lines[0].contains(r#"requested="gross profit""#),
        "a concept spelled with a SPACE must render QUOTED — the reader's \
             field grammar is `requested=\"...\"` and an unquoted value would \
             be truncated at the space, silently mis-attributing the ask. \
             Got:\n{}",
        lines[0]
    );
    // QUOTED. `outcome` is a &str field, and the fmt layer writes
    // &str values through `record_str`, which quotes. The reader's
    // first draft grepped `outcome=(\w+)` and matched nothing — it
    // would have reported a clean zero for every store forever.
    // That is why this test renders rather than composes.
    assert!(
        lines[0].contains(r#"outcome="resolved""#),
        "the resolved arm must name itself, QUOTED — the reader's \
             grammar is `outcome=\"...\"`:\n{}",
        lines[0]
    );
    assert!(
        lines[1].contains(r#"outcome="unmapped""#),
        "the MISS arm must render `outcome=unmapped` — this is the exact \
             token the reader counts as the F5 numerator, so any other \
             spelling reports a miss rate of zero for a store that missed:\n{}",
        lines[1]
    );
    assert!(
        lines[1].contains("consolidated_only=true") || lines[1].contains("consolidated_only=false"),
        "the store's source-limit flag must render as a bare boolean:\n{}",
        lines[1]
    );
}
