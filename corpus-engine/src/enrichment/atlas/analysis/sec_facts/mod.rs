// SPDX-License-Identifier: AGPL-3.0-or-later
//! SEC typed-fact store — the pure lookup/derivation half of the
//! `sec_facts` tool (spec `sovereign/docs/specs/FINANCIAL_CORPORA.md` §6.2).
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

/// The corpus's freshness anchor (F6): the reporting filing it was built
/// from. Periods ending after `latest_period_end` refuse by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsOf {
    pub form: String,
    pub accession: String,
    pub filed: String,
    /// Latest period end date across every stored fact (ISO date).
    pub latest_period_end: String,
}

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

/// Closed set (ARCH §2): a concept is a flow over a period or a stock at
/// an instant. The renderer's concept map declares which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConceptKind {
    Duration,
    Instant,
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
    let id_form = normalize(requested).replace(' ', "_");
    if store.concepts.contains_key(&id_form) {
        return Ok(id_form);
    }
    let phrase = normalize(requested);
    let hits: Vec<&String> = store
        .concepts
        .iter()
        .filter(|(_, cf)| cf.ask_terms.iter().any(|t| normalize(t) == phrase))
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
    let q = normalize(question);
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

/// A quantity computed in Rust over named facts — formula, inputs and
/// result all emitted (§6.2(3)).
#[derive(Debug, Clone)]
pub struct Derived {
    pub value: f64,
    /// `a ÷ b = x` with full-precision inputs — rendered verbatim into
    /// the derivation appendix.
    pub formula: String,
}

/// `numerator ÷ denominator`, as a percentage.
pub fn ratio(num_name: &str, num: &SecFact, den_name: &str, den: &SecFact) -> Option<Derived> {
    if den.value == 0.0 {
        return None;
    }
    let value = num.value / den.value;
    Some(Derived {
        value,
        formula: format!(
            "{num_name} ÷ {den_name} = {} ÷ {} = {}",
            fmt_full(num.value, &num.unit),
            fmt_full(den.value, &den.unit),
            fmt_pct(value)
        ),
    })
}

/// Absolute and percent change from `prior` to `cur`.
pub fn change(name: &str, cur: &SecFact, prior: &SecFact) -> (Derived, Option<Derived>) {
    let delta = cur.value - prior.value;
    let abs = Derived {
        value: delta,
        formula: format!(
            "Δ {name} = {} − {} = {}",
            fmt_full(cur.value, &cur.unit),
            fmt_full(prior.value, &prior.unit),
            fmt_full(delta, &cur.unit)
        ),
    };
    let pct = (prior.value != 0.0).then(|| {
        let p = delta / prior.value;
        Derived {
            value: p,
            formula: format!(
                "Δ% {name} = ({} − {}) ÷ {} = {}",
                fmt_full(cur.value, &cur.unit),
                fmt_full(prior.value, &prior.unit),
                fmt_full(prior.value, &prior.unit),
                fmt_pct(p)
            ),
        }
    });
    (abs, pct)
}

/// Compact figure for cited strings. USD values at millions grain render
/// with an explicit magnitude word so even the DEFAULT numeric-audit
/// scope ($-token + magnitude) can parse them: `$416,161 million`.
pub fn fmt_compact(value: f64, unit: &str) -> String {
    match unit {
        "USD" if value.abs() >= 1_000_000.0 => {
            format!("${} million", group(value / 1_000_000.0, 0))
        }
        "USD" => format!("${}", group(value, 2)),
        u if u.starts_with("USD/") => format!("${}", group(value, 2)),
        "shares" => format!("{} shares", group(value, 0)),
        u => format!("{} {u}", group(value, 4)),
    }
}

/// Full-precision figure for derivation lines.
pub fn fmt_full(value: f64, unit: &str) -> String {
    match unit {
        "USD" => format!("${}", group(value, 2)),
        u if u.starts_with("USD/") => format!("${}", group(value, 2)),
        u => format!("{} {u}", group(value, 4)),
    }
}

/// `0.0830` → `8.30%`.
pub fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

/// Thousands-grouped decimal with `places` fraction digits (trailing
/// zeros trimmed for places > 2).
fn group(v: f64, places: usize) -> String {
    let neg = v < 0.0;
    let s = format!("{:.*}", places, v.abs());
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut out = String::new();
    let len = int_part.len();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let frac = frac.map(|f| {
        if places > 2 {
            f.trim_end_matches('0').to_string()
        } else {
            f
        }
    });
    let mut out = match frac {
        Some(f) if !f.is_empty() => format!("{out}.{f}"),
        _ => out,
    };
    if neg {
        out = format!("-{out}");
    }
    out
}

/// Does this store claim authority over `question` (§7.3)? Deterministic
/// and enumerable: the question must name the ENTITY (ticker or entity
/// name words) AND at least one concept's `ask_terms` phrase. Matching is
/// word-boundary phrase containment over a normalized form — no
/// embeddings, no threshold. Returns the matched evidence for glassbox
/// logs, or `None`.
///
/// Over-claiming fails safe (the tool refuses naming what IS available),
/// so the vocabulary may be generous; but an entity match is REQUIRED —
/// generic finance wording ("what is gross margin?") never claims.
pub fn store_claims(store: &SecFactStore, question: &str) -> Option<String> {
    let q = normalize(question);
    // Explanation-shaped questions are OUT of the store's domain: the
    // store is authoritative for FIGURES; "why" answers live in the
    // filing's prose and are best served by the retrieval path with
    // quote verification (measured 2026-08-15: claiming a "why did Mac
    // net sales increase" question pulled it off the DeepQuery path
    // that answered it verbatim, onto a plan whose search step failed).
    if contains_phrase(&q, "why") {
        return None;
    }
    let entity_hit = entity_terms(store)
        .into_iter()
        .find(|t| contains_phrase(&q, t))?;
    for (concept_id, cf) in &store.concepts {
        if let Some(term) = cf
            .ask_terms
            .iter()
            .find(|t| contains_phrase(&q, &normalize(t)))
        {
            return Some(format!(
                "entity '{entity_hit}' + concept '{concept_id}' term '{term}'"
            ));
        }
    }
    None
}

/// The entity vocabulary: the ticker plus each word of the entity name
/// that is not a corporate suffix ("Apple Inc." → ["aapl", "apple"]).
fn entity_terms(store: &SecFactStore) -> Vec<String> {
    let mut terms = Vec::new();
    if !store.ticker.is_empty() {
        terms.push(store.ticker.to_lowercase());
    }
    for w in normalize(&store.entity).split_whitespace() {
        if !matches!(
            w,
            "inc" | "corp" | "corporation" | "co" | "ltd" | "plc" | "the"
        ) {
            terms.push(w.to_string());
        }
    }
    terms
}

/// Lowercase, non-alphanumerics to spaces, collapsed. "Apple's" →
/// "apple s", so possessives match the bare entity term.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim_end().to_string()
}

/// Word-boundary phrase containment: `phrase` (already normalized)
/// appears in `normalized` as whole words.
fn contains_phrase(normalized: &str, phrase: &str) -> bool {
    if phrase.is_empty() {
        return false;
    }
    let padded = format!(" {normalized} ");
    padded.contains(&format!(" {phrase} "))
}

// ---------------------------------------------------------------------
// Split out of this file when it crossed ARCH §3.1's 1200-line hard
// trigger. The public path is unchanged — every `analysis::sec_facts::X`
// import still resolves, because both submodules are re-exported here
// (§3.2(3), keep the façade intact).
//
//   coverage.rs  — what the corpus can answer, as text (`coverage_summary`,
//                  for tool/CLI consumers) and as the structured card the
//                  desktop renders (`coverage_card`, FINANCIAL_CORPORA §7.7).
//   discovery.rs — which installed corpora DECLARE this store authoritative.
// ---------------------------------------------------------------------
mod coverage;
mod discovery;

pub use coverage::{
    concept_fiscal_years, coverage_card, coverage_limits, coverage_summary, AnsweredConcept,
    CoverageCard, CoverageLimit, LimitKind,
};
pub use discovery::{authoritative_store, discover_authoritative_stores, SEC_FACTS_AUTHORITY_TOOL};

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::analysis::sec_facts::fixtures::store;

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
            lines[1].contains("consolidated_only=true")
                || lines[1].contains("consolidated_only=false"),
            "the store's source-limit flag must render as a bare boolean:\n{}",
            lines[1]
        );
    }
}
