// SPDX-License-Identifier: AGPL-3.0-or-later
//! SEC typed-fact store — the pure lookup/derivation half of the
//! `sec_facts` tool (spec `svrn/docs/specs/FINANCIAL_CORPORA.md` §6.2).
//!
//! The store is a sidecar (`sec_facts.json`) written at corpus setup time
//! by `sovereign_tools::sec_facts_render::render` — THE one decider for
//! interpreting companyfacts + the concept map (ARCH §10.6; it was
//! `scripts/sec_facts.py` until order `sec-facts-decider-port` moved it
//! into Rust and deleted the Python). This module never reads
//! companyfacts and never re-selects facts: every fact here was already
//! selected, restated-superseded, and period-typed by the renderer. Rust's
//! job is lookup, refusal, and deterministic arithmetic over named facts.
//!
//! Invariants inherited from slice 1 (FINANCIAL_CORPORA §5):
//! - identity is `(concept, start, end, unit, accession)`; `fiscal_year`
//!   comes from the fact's own end date, never the filing's `fy` field;
//! - absence is REPORTED, never defaulted: an unmapped concept refuses by
//!   name, a missing period refuses naming what IS available (ARCH §18.3);
//! - derived quantities are computed here, in Rust, with formula + inputs
//!   + result emitted — a model doing arithmetic is a model originating a
//!   number (§6.2(3)).
//!
//! Glassbox: every lookup emits `sec_facts`-target debug events — the
//! concept requested, the period parsed, the match or the refusal reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use derive::contains_phrase;

/// Sidecar filename under the corpus index dir
/// (`~/.svrnmesh/indexes/<corpus_id>/sec_facts.json`).
pub const SEC_FACTS_SIDECAR: &str = "sec_facts.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecFactStore {
    pub schema: u32,
    pub entity: String,
    #[serde(default)]
    pub ticker: String,
    pub cik: String,
    pub as_of: AsOf,
    /// concept id -> its facts. BTreeMap: deterministic iteration order.
    pub concepts: BTreeMap<String, ConceptFacts>,
    pub coverage: Coverage,
}

pub use sovereign_contracts::daemon_wire::sec_coverage::{AsOf, ConceptKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptFacts {
    pub label: String,
    pub kind: ConceptKind,
    /// Question vocabulary for the authority claim (§7.3): phrases a
    /// user asking about THIS concept plausibly uses ("revenue", "net
    /// sales", "earnings per share"). Registry data from
    /// concept-map.toml, rendered into the sidecar — never code.
    #[serde(default)]
    pub ask_terms: Vec<String>,
    pub facts: Vec<SecFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecFact {
    pub value: f64,
    pub unit: String,
    /// `None` for instant (balance-sheet) facts.
    pub start: Option<String>,
    pub end: String,
    /// From the fact's OWN end date (never companyfacts' `fy`, which names
    /// the filing).
    pub fiscal_year: i32,
    pub tag: String,
    pub accession: String,
    pub form: String,
    pub filed: String,
}

/// Coverage surface (F5): what this corpus can and cannot answer, stated
/// rather than implied. `consolidated_only` names the structural source
/// limit — companyfacts carries no dimension axis, so segment figures
/// (e.g. Apple's Services revenue) cannot be typed from it even when the
/// number appears in the ingested 10-K prose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub filer_tags_total: usize,
    pub covered_tags: usize,
    pub unmapped_tags: usize,
    pub consolidated_only: bool,
}

/// A requested reporting period. Closed set of spellings, mirroring the
/// renderer's grammar: `FY2025` | `YYYY-MM-DD` | `YYYY-MM-DD..YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Period {
    FiscalYear(i32),
    Instant(String),
    Duration(String, String),
}

impl Period {
    pub fn parse(spec: &str) -> Result<Period, SecRefusal> {
        let s = spec.trim();
        let upper = s.to_ascii_uppercase();
        if let Some(y) = upper.strip_prefix("FY") {
            if let Ok(year) = y.parse::<i32>() {
                return Ok(Period::FiscalYear(year));
            }
        }
        if let Some((a, b)) = s.split_once("..") {
            if is_iso_date(a) && is_iso_date(b) {
                return Ok(Period::Duration(a.to_string(), b.to_string()));
            }
        } else if is_iso_date(s) {
            return Ok(Period::Instant(s.to_string()));
        }
        Err(SecRefusal::BadPeriod {
            spec: s.to_string(),
        })
    }

    /// The period's end date proxy, for the freshness comparison. For a
    /// fiscal year this is the year alone (compared against the as-of
    /// fiscal year); ISO strings compare lexically = chronologically.
    fn end_hint(&self) -> String {
        match self {
            Period::FiscalYear(y) => format!("{y}"),
            Period::Instant(d) => d.clone(),
            Period::Duration(_, e) => e.clone(),
        }
    }
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// Refusal, first-class (§6.2(6)): each variant names what went wrong AND
/// what IS available. Never a silent substitution (ARCH §18.3).
#[derive(Debug, Clone, PartialEq)]
pub enum SecRefusal {
    /// Concept not in the typed store — includes every dimensional/segment
    /// concept (consolidated-only source limit) and everything unmapped.
    UnmappedConcept {
        concept: String,
        mapped: Vec<String>,
        consolidated_only: bool,
    },
    /// Concept exists but no fact matches the period; the nearest facts
    /// are NAMED, never substituted.
    NoFactForPeriod {
        concept: String,
        period: String,
        available_period_ends: Vec<String>,
    },
    /// Freshness (F6): the requested period ends after the corpus's as-of
    /// filing — the corpus cannot know it yet. Still names the period
    /// ends that DO carry a fact: "we cannot know that yet" without
    /// "here is what we do know" is the abstention §7.7 forbids.
    BeyondAsOf {
        concept: String,
        period: String,
        as_of_form: String,
        as_of_accession: String,
        as_of_filed: String,
        latest_period_end: String,
        available_period_ends: Vec<String>,
    },
    /// The QUESTION stated one period and the tool was called with
    /// another — a calendar range answered with a fiscal year. Code
    /// enforces this because the parameter comes from a model: the
    /// descriptor asks the planner to preserve the user's period, and
    /// asking is not a guarantee (§7.6). A correct label on the wrong
    /// period is still the wrong period.
    PeriodNotAsAsked {
        concept: String,
        asked: String,
        called_with: String,
        available_period_ends: Vec<String>,
    },
    /// Instant concept asked with a range, or duration concept with a
    /// bare date.
    KindMismatch {
        concept: String,
        kind: ConceptKind,
        period: String,
    },
    /// Unparseable period spec.
    BadPeriod { spec: String },
    /// Two distinct stored facts match one request (53-week transition
    /// edge). Refusing beats guessing which one the asker means.
    Ambiguous {
        concept: String,
        period: String,
        periods: Vec<String>,
    },
    /// A requested concept name matches several concepts' declared
    /// ask-terms. Refusing and naming them beats picking one.
    AmbiguousConcept {
        requested: String,
        candidates: Vec<String>,
    },
    /// The QUESTION scopes the figure BELOW the consolidated entity — a
    /// segment, division, product line, or geography — and the source
    /// cannot carry that. Code enforces this because the failure is
    /// invisible to every other guard: the planner substitutes the nearest
    /// LEGAL concept (`revenue` for "Mac segment revenue"), the store
    /// resolves it correctly, and a consolidated figure is narrated as a
    /// segment one. Reproduced 2/2 on 2026-08-18; the provenance guard
    /// caught both only incidentally (a rounding restatement, then an
    /// accession fragment), never the granularity itself.
    ///
    /// This is the §18.3 twin of `PeriodNotAsAsked`: a correct label on
    /// the wrong SCOPE is still the wrong answer.
    ScopeNotInSource {
        concept: String,
        qualifier: String,
        mapped: Vec<String>,
    },
}

impl SecRefusal {
    /// The user-facing reason. Every variant names what IS available.
    pub fn reason(&self) -> String {
        match self {
            SecRefusal::UnmappedConcept {
                concept,
                mapped,
                consolidated_only,
            } => {
                let limit = if *consolidated_only {
                    " The source (SEC companyfacts) is consolidated-only: segment and \
                     dimensional figures cannot be typed from it, even when the number \
                     appears in the filing's prose."
                } else {
                    ""
                };
                format!(
                    "concept '{concept}' has no typed fact in this corpus — unmapped \
                     concepts are reported, never defaulted to a near neighbour.{limit} \
                     Typed concepts available: {}.",
                    mapped.join(", ")
                )
            }
            SecRefusal::ScopeNotInSource {
                concept,
                qualifier,
                mapped,
            } => format!(
                "this question asks for a '{qualifier}'-level figure, and the source \
                 (SEC companyfacts) is consolidated-only: it carries no segment, \
                 division, product-line or geographic breakdown, even when the number \
                 appears in the filing's prose. The consolidated '{concept}' figure is \
                 NOT that number and is not offered as a substitute. Typed \
                 company-wide concepts available: {}.",
                mapped.join(", ")
            ),
            SecRefusal::NoFactForPeriod {
                concept,
                period,
                available_period_ends,
            } => format!(
                "no typed {concept} fact for period '{period}'. Available period end \
                 date(s), named not substituted: {}.",
                available_period_ends.join(", ")
            ),
            SecRefusal::BeyondAsOf {
                concept,
                period,
                as_of_form,
                as_of_accession,
                as_of_filed,
                latest_period_end,
                available_period_ends,
            } => format!(
                "period '{period}' ends after this corpus's as-of filing ({as_of_form} \
                 accession {as_of_accession}, filed {as_of_filed}; latest period end \
                 {latest_period_end}) — no fact for {concept} can exist here yet. \
                 Available period end date(s), named not substituted: {}.",
                available_period_ends.join(", ")
            ),
            SecRefusal::PeriodNotAsAsked {
                concept,
                asked,
                called_with,
                available_period_ends,
            } => format!(
                "the question asked for period '{asked}', but this lookup was called \
                 with '{called_with}' — a different period. A fiscal-year figure is not \
                 a calendar-year figure, and a correct period label does not rescue a \
                 wrong period, so this is refused rather than answered. No typed \
                 {concept} fact represents '{asked}'. Available period end date(s), \
                 named not substituted: {}.",
                available_period_ends.join(", ")
            ),
            SecRefusal::KindMismatch {
                concept,
                kind,
                period,
            } => match kind {
                ConceptKind::Instant => format!(
                    "'{concept}' is an instant (balance-sheet) concept; the date-range \
                     period '{period}' does not apply — pass a single date or FY<year>."
                ),
                ConceptKind::Duration => format!(
                    "'{concept}' is a duration concept; the single date '{period}' names \
                     an instant — pass a start..end range or FY<year>."
                ),
            },
            SecRefusal::BadPeriod { spec } => format!(
                "unparseable period spec '{spec}' — expected FY<year>, YYYY-MM-DD, or \
                 YYYY-MM-DD..YYYY-MM-DD."
            ),
            SecRefusal::Ambiguous {
                concept,
                period,
                periods,
            } => format!(
                "ambiguous: multiple distinct stored periods match {concept} \
                 '{period}': {} — refusing rather than guessing.",
                periods.join("; ")
            ),
            SecRefusal::AmbiguousConcept {
                requested,
                candidates,
            } => format!(
                "ambiguous: '{requested}' matches several typed concepts: {} — \
                 name one explicitly rather than have one guessed.",
                candidates.join(", ")
            ),
        }
    }
}

/// Resolve a requested concept name to a canonical concept id.
///
/// Planners spell concepts freely ("gross profit",
/// "diluted_earnings_per_share"); the store's ids are canonical. Two
/// DECLARED resolution steps, never a similarity guess (ARCH §18.3):
/// 1. separator normalization — lowercase, spaces/hyphens to
///    underscores — matched against concept ids;
/// 2. the normalized phrase matched EXACTLY against each concept's
///    declared `ask_terms` (the concept-map author's own synonyms —
///    an alias, not a near neighbour). One hit resolves (logged);
///    several hits refuse naming the candidates.
///
/// EVERY call emits exactly one `f5_demand` event — see [`F5_DEMAND_ANCHOR`].
pub fn resolve_concept(store: &SecFactStore, requested: &str) -> Result<String, SecRefusal> {
    let outcome = resolve_concept_inner(store, requested);
    // ── F5 demand-side telemetry, the whole of it ────────────────────
    // F5's second clause asks how many of the concepts people ACTUALLY
    // ASK FOR miss for a reason we could fix. This is the one place that
    // knows a concept was asked for at all, so the event is emitted HERE
    // and nowhere else: one per ask, covering both the numerator (the
    // misses) and the denominator (the asks). Emitting it in the arms
    // instead would make "how many asks were there" unanswerable, and
    // would put the telemetry one forgotten `return` away from a hole.
    //
    // There is deliberately NO STORE, no counter and no cadence.
    // Operator constraint, standing: telemetry must offer "as much
    // signal with as little burden as possible otherwise it will just
    // be another speedbump we route around". A debug event on a path
    // that already runs costs nothing to keep current and nothing to
    // remember to run; `scripts/sec-miss-demand.py` reads it back out of
    // logs a run already produced.
    let (label, resolved) = match &outcome {
        Ok(id) => ("resolved", Some(id.as_str())),
        Err(SecRefusal::UnmappedConcept { .. }) => ("unmapped", None),
        Err(SecRefusal::AmbiguousConcept { .. }) => ("ambiguous", None),
        // resolve_concept_inner returns only the three above; a fourth
        // arm would be a new outcome the reader must learn about, so it
        // is named rather than folded into one of the three.
        Err(_) => ("other", None),
    };
    tracing::debug!(
        target: "sec_facts",
        f5_demand = true,
        requested = ?requested,
        outcome = label,
        resolved = ?resolved,
        // The STORE's structural source limit, not a verdict on this ask:
        // companyfacts has no dimension axis. It cannot tell you that THIS
        // ask was a segment ask, and the reader must not treat it as if it
        // could — a consolidated-only miss is a source limit to disclose,
        // never a gap to close.
        consolidated_only = store.coverage.consolidated_only,
        store_concepts = store.concepts.len(),
        "sec_facts: concept ask"
    );
    outcome
}

/// The anchor field on the F5 demand event, exported so the contract is
/// citable from both sides of the language boundary.
///
/// `scripts/sec-miss-demand.py` greps THIS declaration in `--self-test`
/// and the unit test below asserts the event really emits a field of this
/// name, so renaming it fails two checks instead of silently zeroing the
/// instrument (the reader would simply match nothing and report a clean
/// score, which is the §18.3 silent-substitution failure).
pub const F5_DEMAND_ANCHOR: &str = "f5_demand";

fn resolve_concept_inner(store: &SecFactStore, requested: &str) -> Result<String, SecRefusal> {
    let id_form = normalize_concept_phrase(requested).replace(' ', "_");
    if store.concepts.contains_key(&id_form) {
        return Ok(id_form);
    }
    let phrase = normalize_concept_phrase(requested);
    let hits: Vec<&String> = store
        .concepts
        .iter()
        .filter(|(_, cf)| {
            cf.ask_terms
                .iter()
                .any(|t| normalize_concept_phrase(t) == phrase)
        })
        .map(|(id, _)| id)
        .collect();
    match hits.as_slice() {
        [one] => {
            tracing::debug!(target: "sec_facts", requested, resolved = %one,
                "sec_facts: concept resolved via declared ask_terms alias");
            Ok((*one).clone())
        }
        [] => Err(SecRefusal::UnmappedConcept {
            concept: requested.to_string(),
            mapped: store.concepts.keys().cloned().collect(),
            consolidated_only: store.coverage.consolidated_only,
        }),
        many => Err(SecRefusal::AmbiguousConcept {
            requested: requested.to_string(),
            candidates: many.iter().map(|s| s.to_string()).collect(),
        }),
    }
}

/// The period end dates this concept DOES carry, sorted and deduped.
/// One decider (§10.6): every refusal that must name alternatives calls
/// this, so the naming can never drift between refusal variants.
pub fn available_period_ends(cf: &ConceptFacts) -> Vec<String> {
    let mut ends: Vec<String> = cf.facts.iter().map(|f| f.end.clone()).collect();
    ends.sort();
    ends.dedup();
    ends
}

/// The period a QUESTION states in calendar terms, as `(start, end)`, or
/// `None`.
///
/// Deliberately NOT a period algebra (operator direction 2026-08-16:
/// fiscal-vs-calendar is a thorny subproblem and must not absorb the
/// program's energy). It recognises the CLEAR case only — the question
/// says "calendar year 2025", or names January through December — and
/// returns that calendar range. Everything else returns `None` and the
/// tool behaves exactly as before.
///
/// This is the tool-side half of the §7.6 guarantee: `Tool::claims` sees
/// the question at routing time and `Tool::execute` now sees the same
/// question, so ONE function decides what period a question states, at
/// both ends (§10.6).
pub fn calendar_period_in_question(question: &str) -> Option<(String, String)> {
    let q = normalize_concept_phrase(question);
    let stated_calendar = contains_phrase(&q, "calendar")
        || (contains_phrase(&q, "january") && contains_phrase(&q, "december"))
        || (contains_phrase(&q, "jan") && contains_phrase(&q, "dec"));
    if !stated_calendar {
        return None;
    }
    // The year the calendar phrase refers to. A question naming several
    // years is ambiguous, and ambiguity refuses rather than guesses
    // (§7.7) — handled by the caller, which sees `None` here only when
    // no year is stated at all.
    let year = q
        .split_whitespace()
        .filter_map(|w| w.parse::<i32>().ok())
        .find(|y| (1994..=2031).contains(y))?;
    Some((format!("{year}-01-01"), format!("{year}-12-31")))
}

/// Structural markers that scope a figure BELOW the consolidated entity.
///
/// Deliberately NOT a list of one filer's product names ("Mac", "iPhone"):
/// those are home-context and would not transfer to the next filer. These
/// are the words the SEC's own segment-reporting vocabulary uses, so they
/// hold for any company this recipe installs.
const SCOPE_QUALIFIERS: &[&str] = &[
    "segment",
    "segments",
    "reportable segment",
    "operating segment",
    "division",
    "divisions",
    "business unit",
    "product line",
    "product category",
    "by region",
    "by geography",
    "geographic",
    "geographical",
    "regional",
];

/// The scope qualifier a question states, if any — the SCOPE twin of
/// [`calendar_period_in_question`].
///
/// Scoped to the CLEAR case on purpose, exactly as the calendar check is
/// (operator direction 2026-08-16): a question that names no qualifier
/// leaves this inert, so ordinary company-wide questions are untouched.
///
/// KNOWN RESIDUE, disclosed rather than hidden: a question naming a
/// product WITHOUT a structural word ("What was Mac revenue in FY2025?")
/// is not caught here, because catching it needs the filer's own segment
/// names and companyfacts does not carry them (§18.3 — the gap is
/// reported, not papered over with similarity guessing).
pub fn scope_qualifier_in_question(question: &str) -> Option<String> {
    let q = normalize_concept_phrase(question);
    SCOPE_QUALIFIERS
        .iter()
        .find(|marker| contains_phrase(&q, marker))
        .map(|marker| (*marker).to_string())
}

/// THE lookup. One fact or a refusal that names what is available.
pub fn lookup<'a>(
    store: &'a SecFactStore,
    concept: &str,
    period_spec: &str,
) -> Result<&'a SecFact, SecRefusal> {
    let period = Period::parse(period_spec)?;
    let Some(cf) = store.concepts.get(concept) else {
        tracing::debug!(target: "sec_facts", concept, period = period_spec,
            "sec_facts: REFUSE unmapped concept");
        return Err(SecRefusal::UnmappedConcept {
            concept: concept.to_string(),
            mapped: store.concepts.keys().cloned().collect(),
            consolidated_only: store.coverage.consolidated_only,
        });
    };
    match (&period, cf.kind) {
        (Period::Duration(..), ConceptKind::Instant)
        | (Period::Instant(_), ConceptKind::Duration) => {
            tracing::debug!(target: "sec_facts", concept, period = period_spec,
                kind = ?cf.kind, "sec_facts: REFUSE kind mismatch");
            return Err(SecRefusal::KindMismatch {
                concept: concept.to_string(),
                kind: cf.kind,
                period: period_spec.to_string(),
            });
        }
        _ => {}
    }
    let matches: Vec<&SecFact> = cf
        .facts
        .iter()
        .filter(|f| match &period {
            Period::FiscalYear(y) => f.fiscal_year == *y,
            Period::Instant(d) => f.start.is_none() && f.end == *d,
            Period::Duration(s, e) => f.start.as_deref() == Some(s.as_str()) && f.end == *e,
        })
        .collect();
    match matches.as_slice() {
        [] => {
            // Named for EVERY period refusal, not just the in-range one:
            // telling a reader what is missing without telling them what
            // to ask for instead is the "technically honest, bad"
            // abstention §7.7 forbids.
            let available = available_period_ends(cf);
            let beyond = match &period {
                Period::FiscalYear(y) => {
                    *y > store.as_of.latest_period_end[..4]
                        .parse::<i32>()
                        .unwrap_or(i32::MAX)
                }
                p => p.end_hint() > store.as_of.latest_period_end,
            };
            if beyond {
                tracing::debug!(target: "sec_facts", concept, period = period_spec,
                    latest = %store.as_of.latest_period_end, available = ?available,
                    "sec_facts: REFUSE beyond as-of (freshness)");
                return Err(SecRefusal::BeyondAsOf {
                    concept: concept.to_string(),
                    period: period_spec.to_string(),
                    as_of_form: store.as_of.form.clone(),
                    as_of_accession: store.as_of.accession.clone(),
                    as_of_filed: store.as_of.filed.clone(),
                    latest_period_end: store.as_of.latest_period_end.clone(),
                    available_period_ends: available.clone(),
                });
            }
            tracing::debug!(target: "sec_facts", concept, period = period_spec,
                available = ?available, "sec_facts: REFUSE no fact for period");
            Err(SecRefusal::NoFactForPeriod {
                concept: concept.to_string(),
                period: period_spec.to_string(),
                available_period_ends: available,
            })
        }
        [f] => {
            tracing::debug!(target: "sec_facts", concept, period = period_spec,
                tag = %f.tag, value = f.value, unit = %f.unit, end = %f.end,
                accession = %f.accession, "sec_facts: matched fact");
            Ok(f)
        }
        many => {
            let periods: Vec<String> = many
                .iter()
                .map(|f| format!("{}..{}", f.start.as_deref().unwrap_or("instant"), f.end))
                .collect();
            tracing::debug!(target: "sec_facts", concept, period = period_spec,
                periods = ?periods, "sec_facts: REFUSE ambiguous");
            Err(SecRefusal::Ambiguous {
                concept: concept.to_string(),
                period: period_spec.to_string(),
                periods,
            })
        }
    }
}

// ---------------------------------------------------------------------
// Split out of this file when it crossed ARCH §3.1's 1200-line hard
// trigger. The public path is unchanged — every `sec_facts::X` import
// still resolves, because the submodules are re-exported here (§3.2(3),
// keep the façade intact).
//
//   coverage.rs  — what the corpus can answer, as text (`coverage_summary`,
//                  for tool/CLI consumers) and as the structured card the
//                  desktop renders (`coverage_card`, FINANCIAL_CORPORA §7.7).
//   derive.rs    — derivations over named facts and their formatters.
// Which corpora DECLARE this store authoritative is read from their
// recipes; that lookup is ingest's (corpus-engine's `recipe_authority_tool`),
// so discovery below takes it as an argument.
// ---------------------------------------------------------------------
mod coverage;
mod derive;

pub use coverage::{
    concept_fiscal_years, coverage_card, coverage_limits, coverage_summary, AnsweredConcept,
    CoverageCard, CoverageLimit, LimitKind,
};
pub use derive::{
    change, fmt_compact, fmt_full, fmt_pct, normalize_concept_phrase, ratio, store_claims, Derived,
};

/// The tool id a recipe names in `[authority]` to declare this typed
/// store authoritative for its corpus. One name (§10.6).
pub const SEC_FACTS_AUTHORITY_TOOL: &str = "sec_facts";

/// Read the typed store for `corpus_id` IF `authority` says its recipe
/// declares `[authority] tool = "sec_facts"` and the sidecar is present.
///
/// Authority is DECLARED by the recipe author (FINANCIAL_CORPORA §7.3); a
/// sidecar alone never grants it, and the corpus id's SPELLING never enters
/// into it (ARCH §7.5). `authority` resolves a corpus's declared tool and is
/// asked only when the sidecar exists.
pub fn authoritative_store_by(
    index_dir: &std::path::Path,
    corpus_id: &str,
    authority: &dyn Fn(&str) -> Option<String>,
) -> Option<SecFactStore> {
    let sidecar = index_dir.join(corpus_id).join(SEC_FACTS_SIDECAR);
    if !sidecar.exists() {
        return None;
    }
    let declared = authority(corpus_id).is_some_and(|tool| tool == SEC_FACTS_AUTHORITY_TOOL);
    if !declared {
        tracing::debug!(target: "sec_facts",
            corpus_id = %corpus_id,
            "sec_facts: sidecar present but recipe declares no \
             [authority] tool = \"sec_facts\" — not authoritative for it");
        return None;
    }
    match std::fs::read_to_string(&sidecar)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<SecFactStore>(&s).map_err(|e| e.to_string()))
    {
        Ok(store) => {
            tracing::debug!(target: "sec_facts",
                corpus_id = %corpus_id, entity = %store.entity,
                concepts = store.concepts.len(),
                "sec_facts: loaded declared-authoritative typed store");
            Some(store)
        }
        Err(err) => {
            tracing::warn!(target: "sec_facts",
                corpus_id = %corpus_id, error = %err,
                "sec_facts: unreadable sidecar — excluded");
            None
        }
    }
}

/// Every installed corpus this typed store is DECLARED authoritative for,
/// id-sorted for determinism.
///
/// The single discovery rule: the `sec_facts` tool's claim index and the
/// desktop coverage card both reach it, so a corpus can never be
/// answerable by one and invisible to the other (§10.6).
pub fn discover_authoritative_stores_by(
    index_dir: &std::path::Path,
    authority: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, SecFactStore)> {
    let Ok(entries) = std::fs::read_dir(index_dir) else {
        tracing::debug!(target: "sec_facts", index_dir = %index_dir.display(),
            "sec_facts: index dir unreadable — no authoritative corpora");
        return Vec::new();
    };
    let mut out: Vec<(String, SecFactStore)> = Vec::new();
    for e in entries.flatten() {
        let corpus_id = e.file_name().to_string_lossy().to_string();
        if let Some(store) = authoritative_store_by(index_dir, &corpus_id, authority) {
            out.push((corpus_id, store));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    tracing::debug!(target: "sec_facts", declared = out.len(),
        "sec_facts: discovery complete");
    out
}

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
mod tests;
