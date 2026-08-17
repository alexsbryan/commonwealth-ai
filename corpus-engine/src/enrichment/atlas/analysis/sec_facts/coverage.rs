// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a typed SEC fact store can and cannot answer, stated rather than
//! implied — FINANCIAL_CORPORA.md §7.4/§7.7, bars F5 (coverage visible)
//! and F6 (freshness).
//!
//! Two renderings, ONE derivation (ARCH §10.6): [`coverage_summary`] for
//! tool and CLI consumers, [`coverage_card`] for the desktop. Both read
//! the same store through the same helpers, so the text answer and the
//! card can never disagree about what is answerable.

use serde::{Deserialize, Serialize};

use super::{AsOf, ConceptFacts, ConceptKind, SecFactStore};

/// The fiscal years a concept carries facts for, ascending and deduped.
/// THE one decider for "what periods does this concept answer" — both the
/// text summary and the structured card read it (§10.6), so the card can
/// never claim a period the tool would refuse.
pub fn concept_fiscal_years(cf: &ConceptFacts) -> Vec<i32> {
    let mut fys: Vec<i32> = cf.facts.iter().map(|f| f.fiscal_year).collect();
    fys.sort_unstable();
    fys.dedup();
    fys
}

/// The coverage statement (F5): what this corpus answers, what it cannot,
/// and why — including the consolidated-only source limit by name.
///
/// The text rendering for tool/CLI consumers. The structured rendering for
/// the desktop is [`coverage_card`]; both derive from the same store and
/// the same helpers, so they cannot disagree about what is answerable.
pub fn coverage_summary(store: &SecFactStore) -> String {
    let mut lines = vec![format!(
        "{} ({}) — typed-fact coverage as of {} accession {} filed {}:",
        store.entity, store.ticker, store.as_of.form, store.as_of.accession, store.as_of.filed
    )];
    for (id, cf) in &store.concepts {
        let fys: Vec<String> = concept_fiscal_years(cf)
            .into_iter()
            .map(|y| format!("FY{y}"))
            .collect();
        lines.push(format!("- {id} ({}): {}", cf.label, fys.join(", ")));
    }
    lines.push(format!(
        "Coverage: {} of {} filer XBRL tags typed ({} unmapped, reported by name in \
         the corpus's _unmapped_concepts.json).",
        store.coverage.covered_tags, store.coverage.filer_tags_total, store.coverage.unmapped_tags
    ));
    for limit in coverage_limits(store) {
        lines.push(limit.statement);
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------
// The coverage card (F5/F6) — FINANCIAL_CORPORA §7.7
//
// §7.7 settles this surface from the honest-abstention ethos, not from
// taste. Three of its five rules are held by the TYPES below rather than
// by anyone remembering them (ARCH principle 10 — structural, not
// remembered):
//
//   - "content is DERIVED, never authored" (§7.7(3)): every field is a
//     function of the store. Nothing here names a company, so the next
//     installed filer gets a truthful card with no new copy written.
//   - "do not render a percentage" (order, §7.7(2)): the card carries NO
//     ratio and NOT the two tag counts a ratio needs. A renderer cannot
//     show a percentage it was never given.
//   - "a refusal is a correct answer, never styled as a failure"
//     (§7.7(1)): limits are `statement` strings — facts — with no
//     severity, level, or warning field for a renderer to key styling
//     off. There is nothing here that says "problem".
// ---------------------------------------------------------------------

/// What this corpus answers, over what period, as of which filing, and
/// what it structurally cannot answer — read from the store (§7.4).
///
/// Capability leads and boundaries sit beside it at equal weight
/// (§7.7(2)); the field order is the reading order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageCard {
    pub entity: String,
    pub ticker: String,
    pub cik: String,
    /// Capability, first (§7.7(2)). Concept id order is the store's
    /// `BTreeMap` order, so the card is deterministic.
    pub answers: Vec<AnsweredConcept>,
    /// The span across every answerable concept, e.g. `FY2015-FY2025`.
    /// Empty when the store carries no facts at all.
    pub period_label: String,
    /// Boundaries, as facts at equal weight (§7.7(2)) — never warnings.
    pub limits: Vec<CoverageLimit>,
    /// Always present (§7.7(5), F6): a corpus that cannot say how current
    /// it is cannot be trusted about periods.
    pub as_of: AsOf,
}

/// One concept the typed store answers authoritatively, with the periods
/// it actually carries facts for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnsweredConcept {
    pub id: String,
    pub label: String,
    pub kind: ConceptKind,
    /// `FY2025` for a single year, `FY2013-FY2025` for a span.
    pub period_label: String,
    pub fiscal_years: Vec<i32>,
}

/// A named boundary on what the store can answer. Closed set (ARCH §2):
/// each variant corresponds to a refusal the tool actually emits, which
/// is what makes the card "the refusal's voice at rest" (§7.7(4)) rather
/// than a separately-maintained disclaimer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// Pairs with [`SecRefusal::UnmappedConcept`] `consolidated_only`.
    Consolidated,
    /// Pairs with [`SecRefusal::UnmappedConcept`]: tags the filer reports
    /// that the concept map does not yet type.
    UntypedTags,
    /// Pairs with [`SecRefusal::BeyondAsOf`] (F6).
    BeyondAsOf,
}

/// A boundary stated as a fact. Deliberately carries no severity or
/// level field — see the module note above (§7.7(1)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageLimit {
    pub kind: LimitKind,
    pub statement: String,
}

/// The boundaries this store has, derived. THE one decider for limit
/// wording (§10.6) — [`coverage_summary`] and [`coverage_card`] both
/// render these, so the text answer and the desktop card state the same
/// limits in the same words.
///
/// No statement names a company: the consolidated-only wording says
/// "a single business segment", not "Services revenue", so it is true for
/// every filer rather than copywritten for one (§7.7(3)).
pub fn coverage_limits(store: &SecFactStore) -> Vec<CoverageLimit> {
    let mut limits = Vec::new();
    if store.coverage.consolidated_only {
        limits.push(CoverageLimit {
            kind: LimitKind::Consolidated,
            statement: "SEC companyfacts is consolidated-only: figures broken out by \
                        segment or other dimension — revenue for a single business \
                        segment, for example — are not typed here, and are refused \
                        rather than approximated even where the number appears in the \
                        filing's prose."
                .to_string(),
        });
    }
    if store.coverage.unmapped_tags > 0 {
        limits.push(CoverageLimit {
            kind: LimitKind::UntypedTags,
            statement: format!(
                "{} further XBRL tags this filer reports are not typed yet. They are \
                 listed by name in the corpus's _unmapped_concepts.json, and questions \
                 about them are refused rather than answered from prose.",
                store.coverage.unmapped_tags
            ),
        });
    }
    limits.push(CoverageLimit {
        kind: LimitKind::BeyondAsOf,
        statement: format!(
            "No figure exists for any period ending after {}. This corpus is exactly as \
             current as the {} filed {}, and says so rather than estimating.",
            store.as_of.latest_period_end, store.as_of.form, store.as_of.filed
        ),
    });
    limits
}

/// Build the coverage card for an installed corpus (§7.7). Pure over the
/// store: no I/O, no clock, no per-corpus text.
pub fn coverage_card(store: &SecFactStore) -> CoverageCard {
    let answers: Vec<AnsweredConcept> = store
        .concepts
        .iter()
        .filter(|(_, cf)| !cf.facts.is_empty())
        .map(|(id, cf)| {
            let fiscal_years = concept_fiscal_years(cf);
            AnsweredConcept {
                id: id.clone(),
                label: cf.label.clone(),
                kind: cf.kind,
                period_label: fy_span(&fiscal_years),
                fiscal_years,
            }
        })
        .collect();
    let mut all_years: Vec<i32> = answers
        .iter()
        .flat_map(|a| a.fiscal_years.iter().copied())
        .collect();
    all_years.sort_unstable();
    all_years.dedup();
    let card = CoverageCard {
        entity: store.entity.clone(),
        ticker: store.ticker.clone(),
        cik: store.cik.clone(),
        period_label: fy_span(&all_years),
        answers,
        limits: coverage_limits(store),
        as_of: store.as_of.clone(),
    };
    // Glassbox: the card is a USER-FACING claim about what a corpus can
    // answer, so every row it displays names the store field it came
    // from — a card that cannot say where a value came from has the same
    // defect as an answer that cannot.
    tracing::debug!(target: "sec_facts",
        entity = %card.entity, ticker = %card.ticker, cik = %card.cik,
        answers = card.answers.len(), period = %card.period_label,
        limits = card.limits.len(),
        as_of_form = %card.as_of.form, as_of_accession = %card.as_of.accession,
        as_of_filed = %card.as_of.filed,
        latest_period_end = %card.as_of.latest_period_end,
        "sec_facts: coverage card derived from the typed store");
    for a in &card.answers {
        tracing::debug!(target: "sec_facts",
            concept = %a.id, label = %a.label, kind = ?a.kind,
            period = %a.period_label, facts = a.fiscal_years.len(),
            "sec_facts: card row — concepts.{} of the typed store", a.id);
    }
    for l in &card.limits {
        tracing::debug!(target: "sec_facts",
            limit = ?l.kind, statement = %l.statement,
            "sec_facts: card limit — derived from store.coverage/as_of");
    }
    card
}

/// `FY2025`, `FY2013-FY2025`, or empty for no years.
fn fy_span(years: &[i32]) -> String {
    match (years.first(), years.last()) {
        (Some(a), Some(b)) if a == b => format!("FY{a}"),
        (Some(a), Some(b)) => format!("FY{a}-FY{b}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::analysis::sec_facts::fixtures::{other_store, store};
    use crate::enrichment::atlas::analysis::sec_facts::lookup;

    #[test]
    fn coverage_summary_names_limits_and_freshness() {
        let s = store();
        let c = coverage_summary(&s);
        assert!(c.contains("24 of 503"));
        assert!(c.contains("consolidated-only"));
        assert!(c.contains("2025-09-27"), "freshness anchor named: {c}");
        assert!(c.contains("advertising_expense (Advertising expense): FY2015"));
    }

    // -----------------------------------------------------------------
    // The coverage card (§7.7) — F5 visible coverage, F6 freshness
    // -----------------------------------------------------------------

    /// §7.7(3) + the order's F5/F6 bar: a second corpus gets a truthful
    /// card with NO new copy written. Every value below comes from
    /// `other_store()`, and no Apple string survives into it.
    #[test]
    fn a_second_corpus_gets_a_truthful_card_with_no_new_copy() {
        let card = coverage_card(&other_store());
        assert_eq!(card.entity, "Contoso Pharmaceuticals PLC");
        assert_eq!(card.ticker, "CTSO");
        assert_eq!(card.cik, "0000999999");
        assert_eq!(card.as_of.form, "20-F");
        assert_eq!(card.as_of.filed, "2024-03-15");
        assert_eq!(card.period_label, "FY2022-FY2023");
        assert_eq!(card.answers.len(), 1);
        assert_eq!(card.answers[0].label, "Research and development expense");
        assert_eq!(card.answers[0].period_label, "FY2022-FY2023");

        // The limits are DERIVED, so this store's card carries only the
        // limits this store actually has: no consolidated-only (it is
        // false here), no untyped tags (zero), and freshness always.
        let kinds: Vec<LimitKind> = card.limits.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![LimitKind::BeyondAsOf],
            "a store with no consolidated or untyped limit must not be told it has one"
        );
        assert!(card.limits[0].statement.contains("2023-12-31"));
        assert!(card.limits[0].statement.contains("20-F"));

        let rendered = serde_json::to_string(&card).expect("card serializes");
        for apple in ["Apple", "AAPL", "0000320193", "Services"] {
            assert!(
                !rendered.contains(apple),
                "card leaked per-corpus copy from another filer: {apple}"
            );
        }
    }

    /// The order forbids rendering "5% coverage". Held structurally: the
    /// card carries neither a ratio nor the two tag counts one needs, so
    /// a renderer cannot compute one from what it is given.
    #[test]
    fn card_carries_no_percentage_and_no_tag_ratio() {
        let card = coverage_card(&store());
        let rendered = serde_json::to_string(&card).expect("card serializes");
        assert!(!rendered.contains('%'), "no percentage on the card");
        // 24 covered of 503 filer tags: neither number reaches the card,
        // so "5%" is not derivable by any renderer.
        assert!(
            !rendered.contains("503"),
            "filer tag total must not reach the card"
        );
        assert!(
            !rendered.contains("\"covered_tags\""),
            "covered/total pair must not reach the card"
        );
        // The unmapped count DOES appear — as a named growth list, which
        // is a fact, not a ratio.
        assert!(rendered.contains("479"));
        assert!(rendered.contains("_unmapped_concepts.json"));
    }

    /// §7.7(1): a refusal is a correct answer, never styled as a failure.
    /// The limit type carries no severity/level/status field for a
    /// renderer to key a warning colour off, and the wording states facts
    /// rather than apologising.
    #[test]
    fn limits_are_facts_with_no_severity_and_no_apology() {
        let card = coverage_card(&store());
        let rendered = serde_json::to_string(&card).expect("card serializes");
        for styling_hook in ["severity", "level", "warning", "error", "status"] {
            assert!(
                !rendered.contains(styling_hook),
                "card exposes `{styling_hook}` — a renderer would style a correct \
                 answer as a fault (§7.7(1))"
            );
        }
        for limit in &card.limits {
            let s = limit.statement.to_lowercase();
            for apology in [
                "sorry",
                "unfortunately",
                "failed",
                "problem",
                "missing data",
            ] {
                assert!(
                    !s.contains(apology),
                    "limit reads as an apology or a fault: {}",
                    limit.statement
                );
            }
        }
    }

    /// §7.7(5)/F6: as-of is always shown, and it is the real filing.
    #[test]
    fn as_of_is_always_present_on_the_card() {
        for s in [store(), other_store()] {
            let card = coverage_card(&s);
            assert_eq!(card.as_of.accession, s.as_of.accession);
            assert!(!card.as_of.form.is_empty());
            assert!(!card.as_of.filed.is_empty());
            assert!(!card.as_of.latest_period_end.is_empty());
        }
    }

    /// The honesty bar that ties the card to the tool: every period the
    /// card advertises must actually resolve through `lookup`. A card
    /// that promises what the tool refuses is the fabrication this
    /// initiative exists to prevent, one layer up.
    #[test]
    fn every_period_the_card_advertises_is_answerable_by_the_tool() {
        for s in [store(), other_store()] {
            let card = coverage_card(&s);
            assert!(!card.answers.is_empty(), "fixture answers something");
            for a in &card.answers {
                for fy in &a.fiscal_years {
                    let spec = format!("FY{fy}");
                    assert!(
                        lookup(&s, &a.id, &spec).is_ok(),
                        "card advertises {} {spec} but the tool refuses it",
                        a.id
                    );
                }
            }
        }
    }

    /// A concept present in the store but carrying no facts is not
    /// advertised — the card states what is answerable, not what is
    /// configured.
    #[test]
    fn a_concept_with_no_facts_is_not_advertised() {
        let mut s = store();
        s.concepts
            .get_mut("advertising_expense")
            .expect("fixture concept")
            .facts
            .clear();
        let card = coverage_card(&s);
        assert!(
            !card.answers.iter().any(|a| a.id == "advertising_expense"),
            "an empty concept must not be advertised as answerable"
        );
        assert!(card.answers.iter().any(|a| a.id == "revenue"));
    }
}
