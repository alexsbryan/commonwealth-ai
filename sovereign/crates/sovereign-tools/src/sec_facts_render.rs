// SPDX-License-Identifier: AGPL-3.0-or-later
//! SEC fact rendering — **THE one decider** (ARCH §10.6) for SEC XBRL
//! companyfacts: `(company, concept, period)` -> a figure with its basis,
//! or a refusal that states why.
//!
//! Ported here from `scripts/sec_facts.py::resolve()` (order
//! `sec-facts-decider-port`, G3). The Python was DELETED in the same
//! commit as the parity test below — a second implementation of alias
//! precedence, period typing, annual-filing selection or restatement
//! supersession is the §15 smell in the one subsystem whose entire
//! purpose is not to substitute silently. There is one decider; this is
//! it.
//!
//! Consumes the concept-normalization registry
//! (`sovereign-recipes/sec-filings-company/concept-map.toml`) and a raw
//! companyfacts document
//! (`data.sec.gov/api/xbrl/companyfacts/CIK##########.json`). Nothing else
//! in the repo may interpret either file.
//!
//! # The seam (fixed by the seat, 2026-08-16)
//!
//! [`render`] is **pure**: no I/O, no network, no filesystem. It takes
//! parsed data and returns owned data; ALL file placement belongs to the
//! caller (M2's `sec_edgar` acquirer, and `scripts/setup-sec-corpus.sh`
//! via `examples/sec_facts_render.rs`). Both outputs come back from one
//! call because they have different deadlines: `facts/*.txt` are INGESTED
//! DOCUMENTS and must exist before extraction runs, while
//! [`SecFactStore`] is written to the corpus index dir and can be placed
//! later.
//!
//! # Refusal posture (ARCH §18.3): absence is REPORTED, never defaulted
//!
//! - concept not in the map -> refuse, name it as unmapped;
//! - no tag of the chain in the facts -> refuse, name the chain tried;
//! - tag present, period absent -> refuse, NAME the nearest available
//!   period (naming is reporting; its value is never substituted).
//!
//! Period matching is on start/end DATES only. The XBRL `frame` label
//! (e.g. `CY2025` on Apple's fiscal-2025 fact) is SEC's nearest-calendar
//! bucketing, NOT calendar alignment, and is never consulted. Neither is
//! `fy`: companyfacts stamps it with the fiscal year of the FILING, so a
//! 10-K's prior-year comparative column carries the CURRENT filing's `fy`
//! (measured: Apple's `2023-10-01..2024-09-28` revenue appears under both
//! `fy=2024` and `fy=2025`). Identity comes from the fact's essence —
//! `(start, end, unit)` — per ARCH §7.5.
//!
//! # Glassbox
//!
//! Every resolution step is a `sec_facts_render`-target `debug!` event:
//! the requested concept, the tag chain, WHICH ALIAS FIRED (or the filer
//! override), the candidate count, the selection, the restatement
//! supersession, and every refusal reason. This replaces the Python's
//! `--debug` stderr trace (`render_debug.log`) — a renderer that cannot
//! say which alias produced a fact has the same defect as an answer that
//! cannot say where its number came from.
//!
//! The Python additionally wrote `_render_manifest.json` (per-concept
//! rendered/miss counts). Nothing in the repo ever read it — verified by
//! grep at the port — so the seat's `RenderOutput` contract drops it and
//! the miss reasons ride the debug trace instead. `_unmapped_concepts.json`
//! is a different thing entirely: a DELIVERABLE (F5's coverage growth
//! chart), and it is [`RenderOutput::unmapped`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use corpus_engine::enrichment::atlas::analysis::sec_facts::{
    AsOf, ConceptFacts, ConceptKind, Coverage, Period, SecFact, SecFactStore,
};
use sovereign_core::error::{Error, Result};

/// Tracing target for the whole decider; one name, greppable.
const TRACE: &str = "sec_facts_render";

// ── the concept-normalization registry (DATA, not code — ARCH §2/§4) ────────

/// `sovereign-recipes/sec-filings-company/concept-map.toml`, parsed.
///
/// Open set with a registry shape (ARCH §9): concepts are rows, not
/// match arms, and a concept with NO row is UNMAPPED — reported by name,
/// never defaulted to a near neighbour.
#[derive(Debug, Clone, Deserialize)]
pub struct ConceptMap {
    pub schema: u32,
    #[serde(default)]
    pub concepts: BTreeMap<String, ConceptEntry>,
    /// Keyed `cik0000320193` (10-digit CIK — the SEC identity; a ticker
    /// is an alias).
    #[serde(default)]
    pub filers: BTreeMap<String, FilerEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConceptEntry {
    pub label: String,
    pub kind: ConceptKind,
    /// Question vocabulary for the deterministic authority claim
    /// (FINANCIAL_CORPORA §7.3). Registry data, rendered into the
    /// sidecar — never code.
    #[serde(default)]
    pub ask_terms: Vec<String>,
    /// Alias chain, walked IN ORDER; the first tag present in the
    /// filer's companyfacts wins.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilerEntry {
    #[serde(default)]
    pub ticker: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// A per-filer override REPLACES the global chain for that concept —
    /// it never merges.
    #[serde(default)]
    pub overrides: BTreeMap<String, ConceptOverride>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConceptOverride {
    pub tags: Vec<String>,
    #[serde(default)]
    pub kind: Option<ConceptKind>,
}

impl ConceptMap {
    pub fn from_toml(src: &str) -> Result<Self> {
        toml::from_str(src)
            .map_err(|e| Error::InvalidInput(format!("concept map is not valid TOML: {e}")))
    }
}

/// Which half of the registry produced a tag chain. Named in the debug
/// trace so a reader can tell a global alias from a filer override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainSource {
    Global,
    FilerOverride,
}

impl ChainSource {
    fn as_str(self) -> &'static str {
        match self {
            ChainSource::Global => "global",
            ChainSource::FilerOverride => "filer-override",
        }
    }
}

// ── the glassbox deliverable ────────────────────────────────────────────────

/// `_unmapped_concepts.json`: which of the filer's own XBRL tags the map
/// covers and which it does not. A DELIVERABLE, not a log — F5 (coverage
/// visible) renders it, and it is the coverage card's growth chart.
///
/// Field order matches the Python's `json.dump` key order so the two
/// documents are byte-comparable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmappedReport {
    pub cik: String,
    pub entity: String,
    pub filer_tags_total: usize,
    pub covered_by_map: Vec<String>,
    pub unmapped: Vec<String>,
}

// ── the seam ────────────────────────────────────────────────────────────────

/// Inputs to [`render`]. Parsed data only — the caller owns acquisition.
pub struct RenderRequest<'a> {
    /// The raw companyfacts document, already parsed.
    pub companyfacts: &'a Value,
    pub concept_map: &'a ConceptMap,
    /// Fallback ticker. A `[filers.cik…] ticker` row WINS over this.
    pub ticker: Option<&'a str>,
    /// `None` = the latest 3 fiscal years available for each concept.
    pub fiscal_years: Option<&'a [i32]>,
}

/// Outputs of [`render`]. The caller places every one of them.
pub struct RenderOutput {
    /// `(filename, contents)` for `docs/facts/`. INGESTED DOCUMENTS —
    /// they must be on disk before extraction runs.
    pub fact_files: Vec<(String, String)>,
    /// The typed fact sidecar (`sec_facts.json`), read from the corpus
    /// INDEX dir by the `sec_facts` tool. `None` when no concept
    /// resolved — absence is reported, never a store full of nothing.
    pub sidecar: Option<SecFactStore>,
    pub unmapped: UnmappedReport,
}

/// Render every mapped concept for a filer.
///
/// Pure. Errors only on input the decider cannot read at all (a
/// companyfacts document with no `cik`, a malformed date inside a fact —
/// the analogue of the Python raising rather than guessing). A concept
/// that cannot be resolved is a REFUSAL on the debug trace, never an
/// error and never a substituted figure.
pub fn render(req: RenderRequest<'_>) -> Result<RenderOutput> {
    let facts = req.companyfacts;
    let cmap = req.concept_map;
    let cik = cik10(facts)?;
    let entity = entity_name(facts);
    let filer = cmap.filers.get(&format!("cik{cik}"));
    let ticker = filer
        .and_then(|f| f.ticker.as_deref())
        .or(req.ticker)
        .unwrap_or("?")
        .to_string();
    let gaap = us_gaap(facts);

    debug!(
        target: TRACE,
        %cik, %entity, %ticker,
        concepts = cmap.concepts.len(),
        filer_tags = gaap.len(),
        fiscal_years = ?req.fiscal_years,
        "render start"
    );

    let mut fact_files: Vec<(String, String)> = Vec::new();
    let mut sidecar_concepts: BTreeMap<String, ConceptFacts> = BTreeMap::new();
    // Insertion-ordered mirror of `sidecar_concepts`' facts, for the
    // `as_of` first-maximum scan (the Python scans a list, and `max`
    // returns the FIRST maximum).
    let mut all_typed: Vec<SecFact> = Vec::new();

    // BTreeMap iteration == Python's `sorted(cmap["concepts"])` for the
    // ASCII snake_case concept ids the registry uses.
    for (concept, entry) in &cmap.concepts {
        let years: Vec<i32> = match req.fiscal_years {
            Some(fys) => {
                let mut v = fys.to_vec();
                v.sort_unstable();
                v
            }
            None => default_years(cmap, &gaap, &cik, concept, entry)?,
        };

        let mut lines: Vec<String> = Vec::new();
        let mut typed: Vec<SecFact> = Vec::new();
        for fy in &years {
            let spec = format!("FY{fy}");
            match resolve(cmap, facts, concept, &spec)? {
                Resolution::Refused(r) => {
                    debug!(target: TRACE, %concept, fy, reason = %r.reason, "render miss");
                }
                Resolution::Fact(f) => {
                    lines.push(f.fact_line(&ticker, &cik));
                    typed.push(f.to_sec_fact()?);
                }
            }
        }

        if !lines.is_empty() {
            let head = format!(
                "{entity} ({ticker}) — {label} — XBRL facts from SEC companyfacts, CIK {cik}.\n",
                label = entry.label
            );
            fact_files.push((
                format!("facts-{concept}.txt"),
                format!("{head}{}\n", lines.join("\n")),
            ));
        }
        if !typed.is_empty() {
            all_typed.extend(typed.iter().cloned());
            sidecar_concepts.insert(
                concept.clone(),
                ConceptFacts {
                    label: entry.label.clone(),
                    kind: entry.kind,
                    ask_terms: entry.ask_terms.clone(),
                    facts: typed,
                },
            );
        }
    }

    // Coverage: every tag named by any chain (global or filer-override),
    // intersected with the tags the filer actually reports.
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for concept in cmap.concepts.keys() {
        if let Some((chain, _)) = tag_chain(cmap, &cik, concept) {
            covered.extend(chain.iter().cloned());
        }
    }
    let filer_tags: BTreeSet<String> = gaap.keys().cloned().collect();
    let covered_by_map: Vec<String> = covered.intersection(&filer_tags).cloned().collect();
    let unmapped_tags: Vec<String> = filer_tags.difference(&covered).cloned().collect();

    let unmapped = UnmappedReport {
        cik: cik.clone(),
        entity: entity.clone(),
        filer_tags_total: gaap.len(),
        covered_by_map,
        unmapped: unmapped_tags,
    };

    let sidecar = if all_typed.is_empty() {
        debug!(target: TRACE, "no concept resolved: NO sidecar written (absence reported, not an empty store)");
        None
    } else {
        // First maximum by `(filed, end)` — the reporting filing this
        // corpus is anchored to (F6).
        let latest = all_typed
            .iter()
            .reduce(|acc, f| {
                if (&f.filed, &f.end) > (&acc.filed, &acc.end) {
                    f
                } else {
                    acc
                }
            })
            .expect("all_typed is non-empty");
        let latest_period_end = all_typed
            .iter()
            .map(|f| f.end.as_str())
            .max()
            .expect("all_typed is non-empty")
            .to_string();
        Some(SecFactStore {
            schema: 1,
            entity: entity.clone(),
            ticker,
            cik: cik.clone(),
            as_of: AsOf {
                form: latest.form.clone(),
                accession: latest.accession.clone(),
                filed: latest.filed.clone(),
                latest_period_end,
            },
            concepts: sidecar_concepts,
            coverage: Coverage {
                filer_tags_total: unmapped.filer_tags_total,
                covered_tags: unmapped.covered_by_map.len(),
                unmapped_tags: unmapped.unmapped.len(),
                consolidated_only: true,
            },
        })
    };

    debug!(
        target: TRACE,
        fact_files = fact_files.len(),
        facts = all_typed.len(),
        unmapped = unmapped.unmapped.len(),
        of = unmapped.filer_tags_total,
        "render done"
    );

    Ok(RenderOutput {
        fact_files,
        sidecar,
        unmapped,
    })
}

// ── THE decider ─────────────────────────────────────────────────────────────

/// One resolution: a figure with its basis, or a refusal that says why.
#[derive(Debug, Clone)]
pub enum Resolution {
    Fact(Box<ResolvedFact>),
    Refused(Refusal),
}

/// A selected fact, with everything needed to cite it.
#[derive(Debug, Clone)]
pub struct ResolvedFact {
    pub entity: String,
    pub cik: String,
    pub concept: String,
    pub label: String,
    /// Prefixed `us-gaap:`.
    pub tag: String,
    /// Kept as the JSON number so integral values render as the filer
    /// wrote them (`7` vs `7.0`) in the fact line.
    pub value: serde_json::Number,
    pub unit: String,
    pub start: Option<String>,
    pub end: String,
    pub basis: String,
    pub accession: Option<String>,
    pub form: Option<String>,
    pub filed: Option<String>,
}

/// A refusal, first-class: it names the chain it tried, or the nearest
/// period that DOES exist. Naming is reporting; the value is never
/// substituted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub concept: String,
    pub requested_period: String,
    pub reason: String,
    pub tags_tried: Option<Vec<String>>,
    pub nearest_available: Option<NearestPeriod>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearestPeriod {
    pub start: Option<String>,
    pub end: String,
    pub fy: Option<i64>,
    pub fp: Option<String>,
    pub form: Option<String>,
}

/// Resolve one `(concept, period)` against a filer's companyfacts.
///
/// The period grammar is NOT re-implemented here: it is
/// [`Period::parse`], the single spelling shared with the read side
/// (ARCH §10.6). Its refusal text for an unparseable spec differs from
/// the Python's (which surfaced a `datetime` `ValueError`); that branch
/// is unreachable from [`render`], which only ever asks `FY<year>`.
pub fn resolve(
    cmap: &ConceptMap,
    facts: &Value,
    concept: &str,
    period_spec: &str,
) -> Result<Resolution> {
    let cik = cik10(facts)?;
    let entity = entity_name(facts);

    let Some((chain, chain_src)) = tag_chain(cmap, &cik, concept) else {
        return Ok(refuse(
            concept,
            period_spec,
            format!(
                "concept '{concept}' is not in the normalization map — unmapped concepts are \
                 reported, never defaulted to a near neighbour"
            ),
        ));
    };
    if chain_src == ChainSource::FilerOverride {
        debug!(target: TRACE, %concept, chain = ?chain, "filer override chain");
    }

    // Kind: the override's own `kind` if it declares one, else the global
    // row's. (An override with no `kind` inherits — the Python's
    // `.get("kind", global_kind)`.)
    let global_kind = cmap.concepts.get(concept).map(|c| c.kind);
    let kind = match chain_src {
        ChainSource::Global => global_kind,
        ChainSource::FilerOverride => cmap
            .filers
            .get(&format!("cik{cik}"))
            .and_then(|f| f.overrides.get(concept))
            .and_then(|o| o.kind)
            .or(global_kind),
    };

    let gaap = us_gaap(facts);

    let mut matched: Option<&String> = None;
    for (i, t) in chain.iter().enumerate() {
        if gaap.contains_key(t) {
            debug!(
                target: TRACE, %concept, chain = ?chain, tag = %t,
                alias = format!("{}[{i}]", chain_src.as_str()),
                "alias fired"
            );
            matched = Some(t);
            break;
        }
    }
    let Some(tag) = matched else {
        return Ok(refuse_with(
            concept,
            period_spec,
            format!(
                "none of the tags {} is present in {entity}'s companyfacts",
                py_list(&chain)
            ),
            Some(chain.clone()),
            None,
        ));
    };

    let units = units_of(&gaap, tag);
    let period = match Period::parse(period_spec) {
        Ok(p) => p,
        Err(_) => {
            return Ok(refuse(
                concept,
                period_spec,
                format!("unparseable period spec: {period_spec}"),
            ))
        }
    };

    let mut candidates: Vec<(&str, &Value)> = Vec::new();
    for (unit, entries) in &units {
        let sel: Vec<&Value> = match &period {
            Period::FiscalYear(fy) => annual_10k_facts(entries, *fy, kind)?,
            Period::Duration(start, end) => {
                if kind != Some(ConceptKind::Duration) {
                    return Ok(refuse(
                        concept,
                        period_spec,
                        format!(
                            "'{concept}' is an instant (balance-sheet) concept; a date-range \
                             period does not apply"
                        ),
                    ));
                }
                entries
                    .iter()
                    .filter(|e| {
                        field(e, "start") == Some(start.as_str())
                            && field(e, "end") == Some(end.as_str())
                    })
                    .copied()
                    .collect()
            }
            Period::Instant(at) => {
                if kind != Some(ConceptKind::Instant) {
                    return Ok(refuse(
                        concept,
                        period_spec,
                        format!(
                            "'{concept}' is a duration concept; a single date names an instant — \
                             pass a start..end range or FY<year>"
                        ),
                    ));
                }
                entries
                    .iter()
                    .filter(|e| field(e, "start").is_none() && field(e, "end") == Some(at.as_str()))
                    .copied()
                    .collect()
            }
        };
        candidates.extend(sel.into_iter().map(|e| (unit.as_str(), e)));
    }

    debug!(
        target: TRACE, %concept, tag = %tag, period = %period_spec,
        candidates = candidates.len(), "candidate scan"
    );

    if candidates.is_empty() {
        // NAME the nearest available period. Naming is reporting; its
        // value is never substituted.
        let mut near: Option<NearestPeriod> = None;
        for (_unit, entries) in &units {
            if let Some(n) = nearest_period(entries) {
                if near.as_ref().is_none_or(|cur| n.end > cur.end) {
                    near = Some(n);
                }
            }
        }
        let mut reason = format!("{entity} has no {tag} fact for period '{period_spec}'");
        if matches!(period, Period::Duration(_, _)) {
            reason.push_str(
                " — no fact with exactly that start..end exists; the filer's fiscal basis differs \
                 (the XBRL 'frame' label is nearest-calendar bucketing and is never treated as \
                 calendar alignment)",
            );
        }
        if let Some(n) = &near {
            reason.push_str(&format!(
                ". Nearest available period (named, not substituted): {}..{} (fy={} {}, {})",
                n.start.as_deref().unwrap_or("instant"),
                n.end,
                n.fy.map(|v| v.to_string()).unwrap_or_else(none),
                n.fp.clone().unwrap_or_else(none),
                n.form.clone().unwrap_or_else(none),
            ));
        }
        return Ok(refuse_with(concept, period_spec, reason, None, near));
    }

    // One unit per answer: a figure that exists in two units is ambiguous,
    // and refusing beats picking.
    let mut by_unit: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for (unit, e) in &candidates {
        by_unit.entry(unit).or_default().push(e);
    }
    if by_unit.len() > 1 {
        let units_listed: Vec<String> = by_unit.keys().map(|u| (*u).to_string()).collect();
        return Ok(refuse(
            concept,
            period_spec,
            format!(
                "ambiguous: facts exist in multiple units {}",
                py_list(&units_listed)
            ),
        ));
    }
    let (unit, mut sel) = by_unit
        .into_iter()
        .next()
        .map(|(u, v)| (u.to_string(), v))
        .expect("candidates is non-empty");

    let distinct: BTreeSet<(Option<&str>, &str)> = sel
        .iter()
        .map(|e| (field(e, "start"), field(e, "end").unwrap_or("")))
        .collect();
    if distinct.len() > 1 {
        // 53-week transition edge: two annual periods ending in one
        // calendar year. Refusing beats guessing which one is meant.
        let listed: Vec<String> = distinct
            .iter()
            .map(|(s, e)| {
                format!(
                    "({}, '{e}')",
                    s.map(|v| format!("'{v}'")).unwrap_or_else(none)
                )
            })
            .collect();
        return Ok(refuse(
            concept,
            period_spec,
            format!(
                "ambiguous: multiple distinct periods match '{period_spec}': [{}]",
                listed.join(", ")
            ),
        ));
    }

    // Provenance rule: the same fact recurs across filings (8-K earnings,
    // 10-Q comparatives, next year's 10-K).
    //   1) prefer annual-report forms when any exist — this corpus is
    //      10-K based;
    //   2) all values equal -> cite the EARLIEST filing (the original
    //      disclosure);
    //   3) values differ -> a restatement: the LATEST filed supersedes,
    //      and the supersession is logged, never silent.
    let annual: Vec<&Value> = sel
        .iter()
        .filter(|e| is_annual_form(field(e, "form")))
        .copied()
        .collect();
    if !annual.is_empty() {
        sel = annual;
    } else {
        let forms: BTreeSet<&str> = sel.iter().filter_map(|e| field(e, "form")).collect();
        debug!(
            target: TRACE, %concept,
            forms = ?forms,
            "no 10-K fact for the period; using non-annual forms"
        );
    }
    // Stable sort: ties keep companyfacts' own array order, as Python's
    // `list.sort` does.
    sel.sort_by(|a, b| {
        field(a, "filed")
            .unwrap_or("")
            .cmp(field(b, "filed").unwrap_or(""))
    });

    let distinct_values: BTreeSet<String> = sel
        .iter()
        .filter_map(|e| e.get("val"))
        .map(number_key)
        .collect();
    let chosen: &Value = if distinct_values.len() > 1 {
        debug!(
            target: TRACE, %concept,
            filings = ?sel.iter().map(|e| (field(e, "filed"), field(e, "accn"), e.get("val"))).collect::<Vec<_>>(),
            "RESTATED across filings; latest filed supersedes"
        );
        sel[sel.len() - 1]
    } else {
        sel[0]
    };
    if sel.len() > 1 {
        debug!(
            target: TRACE, %concept, n = sel.len(),
            filed = ?field(chosen, "filed"), accn = ?field(chosen, "accn"),
            "multiple facts for the period; citing"
        );
    }

    let Some(end) = field(chosen, "end") else {
        return Err(Error::InvalidInput(format!(
            "companyfacts fact for {tag} has no `end` date (concept {concept}, period {period_spec})"
        )));
    };
    let Some(value) = chosen.get("val").and_then(Value::as_number).cloned() else {
        return Err(Error::InvalidInput(format!(
            "companyfacts fact for {tag} has no numeric `val` (concept {concept}, period {period_spec})"
        )));
    };
    let start = field(chosen, "start").map(str::to_string);
    // Fiscal-year label from the fact's OWN end date (never the `fy`
    // field, which names the filing, not the period).
    let basis = match &start {
        Some(s) => format!("fiscal year FY{} ({s} to {end})", &end[..4]),
        None => format!("as of {end} (fiscal FY{} balance date)", &end[..4]),
    };

    Ok(Resolution::Fact(Box::new(ResolvedFact {
        entity,
        cik,
        concept: concept.to_string(),
        label: cmap
            .concepts
            .get(concept)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| concept.to_string()),
        tag: format!("us-gaap:{tag}"),
        value,
        unit,
        start,
        end: end.to_string(),
        basis,
        accession: field(chosen, "accn").map(str::to_string),
        form: field(chosen, "form").map(str::to_string),
        filed: field(chosen, "filed").map(str::to_string),
    })))
}

impl ResolvedFact {
    /// The rendered corpus line. ONE grammar, here — the retrieval-side
    /// judge (`scripts/check-sec-corpus.py`) parses it back.
    pub fn fact_line(&self, ticker: &str, cik: &str) -> String {
        format!(
            "{entity} ({ticker}, CIK {cik}) — {label} [{tag}]: {value} — {basis}. Reported in \
             Form {form}, accession {accn}, filed {filed}.",
            entity = self.entity,
            label = self.label,
            tag = fts_tag(self.tag.strip_prefix("us-gaap:").unwrap_or(&self.tag)),
            value = fmt_value(&self.value, &self.unit),
            basis = self.basis,
            form = self.form.clone().unwrap_or_else(none),
            accn = self.accession.clone().unwrap_or_else(none),
            filed = self.filed.clone().unwrap_or_else(none),
        )
    }

    /// Identity from essence (ARCH §7.5): concept + period + unit +
    /// accession; `fiscal_year` from the fact's OWN end date.
    ///
    /// The sidecar's type carries no `Option` for accession/form/filed:
    /// a fact that cannot be cited is REFUSED here rather than stored
    /// with a blank provenance (ARCH §18.3).
    pub fn to_sec_fact(&self) -> Result<SecFact> {
        let cite = |field: &Option<String>, name: &str| -> Result<String> {
            field.clone().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "companyfacts fact {} for period ending {} has no `{name}`; a fact that \
                     cannot be cited is not stored",
                    self.tag, self.end
                ))
            })
        };
        Ok(SecFact {
            value: self.value.as_f64().ok_or_else(|| {
                Error::InvalidInput(format!("non-representable `val` for {}", self.tag))
            })?,
            unit: self.unit.clone(),
            start: self.start.clone(),
            end: self.end.clone(),
            fiscal_year: self.end[..4].parse::<i32>().map_err(|_| {
                Error::InvalidInput(format!("fact end date is not ISO: {}", self.end))
            })?,
            tag: self.tag.clone(),
            accession: cite(&self.accession, "accn")?,
            form: cite(&self.form, "form")?,
            filed: cite(&self.filed, "filed")?,
        })
    }
}

// ── selection helpers ───────────────────────────────────────────────────────

/// Resolve the tag chain for a concept: a filer override wins WHOLE (it
/// replaces the global chain, never merges).
fn tag_chain(cmap: &ConceptMap, cik: &str, concept: &str) -> Option<(Vec<String>, ChainSource)> {
    if let Some(o) = cmap
        .filers
        .get(&format!("cik{cik}"))
        .and_then(|f| f.overrides.get(concept))
    {
        return Some((o.tags.clone(), ChainSource::FilerOverride));
    }
    cmap.concepts
        .get(concept)
        .map(|c| (c.tags.clone(), ChainSource::Global))
}

/// A fact reported as a fiscal-year figure in a 10-K: `fp=FY`, and for
/// durations a ~1-year span (330-380 days) so quarterly comparatives
/// never masquerade as annual figures.
fn is_annual_10k_fact(e: &Value, kind: Option<ConceptKind>) -> Result<bool> {
    if !is_annual_form(field(e, "form")) || field(e, "fp") != Some("FY") {
        return Ok(false);
    }
    if kind == Some(ConceptKind::Duration) {
        let (Some(start), Some(end)) = (field(e, "start"), field(e, "end")) else {
            return Ok(false);
        };
        let days = day_span(start, end)?;
        return Ok((330..=380).contains(&days));
    }
    Ok(field(e, "start").is_none() && field(e, "end").is_some())
}

fn is_annual_form(form: Option<&str>) -> bool {
    matches!(form, Some("10-K") | Some("10-K/A"))
}

/// Facts for fiscal year N = annual-shaped 10-K facts whose OWN period
/// ends in calendar year N. NEVER keyed on the `fy` field (see the module
/// doc). This also excludes prior-year comparative balance dates for
/// instants (their end year is N-1) with no separate guard.
fn annual_10k_facts<'a>(
    entries: &[&'a Value],
    fy: i32,
    kind: Option<ConceptKind>,
) -> Result<Vec<&'a Value>> {
    let mut out = Vec::new();
    for e in entries {
        if is_annual_10k_fact(e, kind)?
            && field(e, "end")
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse::<i32>().ok())
                == Some(fy)
        {
            out.push(*e);
        }
    }
    Ok(out)
}

/// Latest available fact, for NAMING in a refusal (never substitution).
fn nearest_period(entries: &[&Value]) -> Option<NearestPeriod> {
    entries
        .iter()
        .filter(|e| field(e, "end").is_some())
        .reduce(|acc, e| {
            let k = |v: &Value| {
                (
                    field(v, "end").unwrap_or("").to_string(),
                    field(v, "filed").unwrap_or("").to_string(),
                )
            };
            if k(e) > k(acc) {
                e
            } else {
                acc
            }
        })
        .map(|e| NearestPeriod {
            start: field(e, "start").map(str::to_string),
            end: field(e, "end").unwrap_or_default().to_string(),
            fy: e.get("fy").and_then(Value::as_i64),
            fp: field(e, "fp").map(str::to_string),
            form: field(e, "form").map(str::to_string),
        })
}

/// The default fiscal years for a concept: the latest 3 the filer
/// reports annually.
///
/// Deliberately keyed on the filing's `fy` field HERE and nowhere else:
/// this is a candidate-year shortlist, not a period decision. `resolve`
/// then re-keys each candidate on the fact's own end date, which is what
/// makes a mis-stamped `fy` harmless. The whole chain is scanned, not
/// just the tag that fires, and the GLOBAL `kind` is used (an override's
/// `kind` narrows resolution, not the shortlist).
fn default_years(
    cmap: &ConceptMap,
    gaap: &BTreeMap<String, Vec<(String, Vec<&Value>)>>,
    cik: &str,
    concept: &str,
    entry: &ConceptEntry,
) -> Result<Vec<i32>> {
    let Some((chain, _)) = tag_chain(cmap, cik, concept) else {
        return Ok(Vec::new());
    };
    let mut avail: BTreeSet<i64> = BTreeSet::new();
    for t in &chain {
        let Some(units) = gaap.get(t) else { continue };
        for (_unit, entries) in units {
            for e in entries {
                if is_annual_10k_fact(e, Some(entry.kind))? {
                    if let Some(fy) = e.get("fy").and_then(Value::as_i64) {
                        avail.insert(fy);
                    }
                }
            }
        }
    }
    let years: Vec<i32> = avail
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|y| *y as i32)
        .collect();
    debug!(target: TRACE, %concept, chain = ?chain, years = ?years, "default fiscal years");
    Ok(years)
}

// ── companyfacts accessors ──────────────────────────────────────────────────

/// `facts.us-gaap` as `tag -> [(unit, entries)]`.
///
/// The unit list is a `Vec`, not a map, so the iteration order is the one
/// thing it can be: the document's. (`serde_json`'s `Map` is sorted, so
/// reading through it would silently re-order.)
fn us_gaap(facts: &Value) -> BTreeMap<String, Vec<(String, Vec<&Value>)>> {
    let mut out = BTreeMap::new();
    let Some(tags) = facts
        .get("facts")
        .and_then(|f| f.get("us-gaap"))
        .and_then(Value::as_object)
    else {
        return out;
    };
    for (tag, body) in tags {
        let mut units = Vec::new();
        if let Some(u) = body.get("units").and_then(Value::as_object) {
            for (unit, entries) in u {
                let list: Vec<&Value> = entries
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                units.push((unit.clone(), list));
            }
        }
        out.insert(tag.clone(), units);
    }
    out
}

fn units_of<'a>(
    gaap: &'a BTreeMap<String, Vec<(String, Vec<&'a Value>)>>,
    tag: &str,
) -> Vec<(String, Vec<&'a Value>)> {
    gaap.get(tag).cloned().unwrap_or_default()
}

/// A string field, with the empty string read as absent (the Python's
/// truthiness test, which is what the selection rules were written
/// against).
fn field<'a>(e: &'a Value, key: &str) -> Option<&'a str> {
    e.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn entity_name(facts: &Value) -> String {
    facts
        .get("entityName")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// The SEC identity, zero-padded to 10 digits. Accepts the number or the
/// string spelling; a document with neither cannot be rendered at all.
fn cik10(facts: &Value) -> Result<String> {
    let raw = facts
        .get("cik")
        .ok_or_else(|| Error::InvalidInput("companyfacts document has no `cik`".into()))?;
    let n = match raw {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| Error::InvalidInput(format!("companyfacts `cik` is not an integer: {raw}")))?;
    Ok(format!("{n:010}"))
}

fn day_span(start: &str, end: &str) -> Result<i64> {
    let parse = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| Error::InvalidInput(format!("companyfacts date is not ISO: {s}")))
    };
    Ok((parse(end)? - parse(start)?).num_days())
}

/// A stable key for value equality. Integral and float spellings of the
/// same number are ONE value (the Python compares them in a `set`, where
/// `1 == 1.0`), so a re-filed identical figure is not read as a
/// restatement because the filer changed `1` to `1.0`.
fn number_key(v: &Value) -> String {
    v.as_f64()
        .map(|f| format!("{f:?}"))
        .unwrap_or_else(|| v.to_string())
}

// ── rendering ───────────────────────────────────────────────────────────────

fn none() -> String {
    "None".to_string()
}

/// Python's `repr` of a list of strings — the refusal texts quote the tag
/// chain in that shape and the retrieval judge reads them.
fn py_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Render an XBRL tag for corpus text. The FTS index drops tokens over
/// ~40 chars (tantivy `RemoveLongFilter`), so long CamelCase tags are
/// word-split at camel boundaries — concatenating the words recovers the
/// exact tag. Short tags stay verbatim.
fn fts_tag(tag: &str) -> String {
    if tag.chars().count() < 40 {
        return format!("us-gaap:{tag}");
    }
    format!("us-gaap: {}", camel_words(tag).join(" "))
}

fn camel_words(tag: &str) -> Vec<String> {
    let chars: Vec<char> = tag.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let tail = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_uppercase() {
            let mut w = String::from(c);
            i += 1;
            while i < chars.len() && tail(chars[i]) {
                w.push(chars[i]);
                i += 1;
            }
            out.push(w);
        } else if tail(c) {
            let mut w = String::new();
            while i < chars.len() && tail(chars[i]) {
                w.push(chars[i]);
                i += 1;
            }
            out.push(w);
        } else {
            i += 1;
        }
    }
    out
}

/// The corpus-line figure. Deliberately NOT
/// `corpus_engine::…::sec_facts::fmt_compact`: that is the READ side's
/// citation grammar (`$416,161 million`), while this is the WRITE side's
/// ingested-line grammar, which also carries the raw value so the
/// retrieval judge can recover the exact figure without a rounding
/// round-trip.
fn fmt_value(value: &serde_json::Number, unit: &str) -> String {
    if unit == "USD" {
        if let Some(f) = value.as_f64() {
            if f.abs() >= 1_000_000.0 {
                return format!(
                    "${} million USD (raw: {})",
                    group0(f / 1_000_000.0),
                    group0(f)
                );
            }
        }
    }
    format!("{value} {unit}")
}

/// Thousands-grouped integer rendering, sign preserved — Python's
/// `f"{v:,.0f}"`, including its round-half-to-even.
fn group0(v: f64) -> String {
    let s = format!("{v:.0}");
    let (sign, digits) = match s.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", s.as_str()),
    };
    let n = digits.len();
    let mut grouped = String::with_capacity(n + n / 3 + 1);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{grouped}")
}

fn refuse(concept: &str, period: &str, reason: String) -> Resolution {
    refuse_with(concept, period, reason, None, None)
}

fn refuse_with(
    concept: &str,
    period: &str,
    reason: String,
    tags_tried: Option<Vec<String>>,
    nearest_available: Option<NearestPeriod>,
) -> Resolution {
    debug!(target: TRACE, %concept, %period, %reason, "REFUSE");
    Resolution::Refused(Refusal {
        concept: concept.to_string(),
        requested_period: period.to_string(),
        reason,
        tags_tried,
        nearest_available,
    })
}

#[cfg(test)]
mod tests;
