// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sec_facts` — typed SEC-filing figures with basis and citation, or a
//! refusal that names what IS available.
//!
//! The product half of FINANCIAL_CORPORA.md §6.2: reads the typed fact
//! sidecar (`sec_facts.json`, written at corpus setup by
//! `scripts/sec_facts.py render` — THE one decider for companyfacts) and
//! emits the existing no-confabulated-numbers contract: `cited_figures` +
//! `derivation` + `reproduce` + raw numeric leaves, exactly as
//! `parcel_analytics` does, so `handlers/complex_task.rs` harvests it and
//! the Layer-3 numeric audit applies.
//!
//! Additionally declares the OPT-IN bare-numeral audit (§6.3(b)): SEC
//! answers carry bare figures (`416,161` millions, EPS `7.46`), so this
//! tool sets `numeric_audit.audit_bare_numerals` and supplies the
//! traceable-token set — built with the auditor's own lexer
//! (`numeric_audit::numeric_tokens`) over the tool's own emitted text, so
//! "allowed" is by construction "the tool said it". General turns and
//! other tools keep the default `$`/`%`-only scope.
//!
//! Derived quantities (ratios, year-over-year changes) are computed in
//! Rust by `corpus_engine`'s pure `sec_facts` lib with formula + inputs +
//! result in the derivation trace — a model doing arithmetic is a model
//! originating a number (§6.2(3)). Refusals stay first-class: they emit
//! the opt-in too, so a model reciting a figure from pretraining over a
//! refusal is flagged.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::runtime::numeric_audit::numeric_tokens;
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

use corpus_engine::enrichment::atlas::analysis::sec_facts::{
    change, coverage_summary, fmt_compact, fmt_full, fmt_pct, lookup, ratio, resolve_concept,
    store_claims, SecFact, SecFactStore, SEC_FACTS_SIDECAR,
};
use corpus_engine::CorpusEngine;
use sovereign_core::types::AuthorityClaim;

/// Typed SEC-filing figures over an installed `sec-cik…` corpus.
pub struct SecFactsTool {
    engine: Arc<CorpusEngine>,
    /// Lazily built claim index for the §7.3 authority pre-check:
    /// every installed `sec-cik…` corpus whose MATERIALIZED RECIPE
    /// declares `[authority] tool = "sec_facts"`, with its typed
    /// store. Built once per process (a corpus installed mid-session
    /// is picked up on the next boot — the claim test must stay ~µs
    /// per turn, so it never re-scans the disk).
    claim_stores: std::sync::OnceLock<Vec<(String, SecFactStore)>>,
}

impl SecFactsTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self {
            engine,
            claim_stores: std::sync::OnceLock::new(),
        }
    }

    /// The claim index. Authority comes from the corpus recipe's
    /// `[authority]` block — a sidecar alone never grants it (the
    /// recipe author declares; data placement does not).
    fn claim_stores(&self) -> &[(String, SecFactStore)] {
        self.claim_stores.get_or_init(|| {
            let mut out: Vec<(String, SecFactStore)> = Vec::new();
            let Ok(entries) = std::fs::read_dir(self.engine.index_dir()) else {
                return out;
            };
            for e in entries.flatten() {
                let corpus_id = e.file_name().to_string_lossy().to_string();
                let sidecar = e.path().join(SEC_FACTS_SIDECAR);
                if !sidecar.exists() {
                    continue;
                }
                let recipe_path = self
                    .engine
                    .recipes_dir()
                    .join(&corpus_id)
                    .join("recipe.toml");
                let declared = std::fs::read_to_string(&recipe_path)
                    .ok()
                    .and_then(|s| corpus_engine::Recipe::from_toml(&s).ok())
                    .and_then(|r| r.authority)
                    .is_some_and(|a| a.tool == "sec_facts");
                if !declared {
                    tracing::debug!(target: "sec_facts",
                        corpus_id = %corpus_id, recipe = %recipe_path.display(),
                        "sec_facts: sidecar present but recipe declares no \
                         [authority] tool = \"sec_facts\" — not claiming for it");
                    continue;
                }
                match std::fs::read_to_string(&sidecar)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<SecFactStore>(&s).map_err(|e| e.to_string())
                    }) {
                    Ok(store) => {
                        tracing::debug!(target: "sec_facts",
                            corpus_id = %corpus_id, entity = %store.entity,
                            concepts = store.concepts.len(),
                            "sec_facts: authority claim index loaded store");
                        out.push((corpus_id, store));
                    }
                    Err(err) => tracing::warn!(target: "sec_facts",
                        corpus_id = %corpus_id, error = %err,
                        "sec_facts: unreadable sidecar — excluded from claim index"),
                }
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        })
    }

    /// Resolve the corpus: explicit id, or the single installed
    /// `sec-cik…` corpus with a fact sidecar. Zero or several installed
    /// is an error that NAMES them — never a silent pick (ARCH §18.3).
    fn resolve_corpus(&self, explicit: Option<&str>) -> Result<(String, SecFactStore)> {
        let index_dir = self.engine.index_dir();
        if let Some(id) = explicit {
            let path = index_dir.join(id).join(SEC_FACTS_SIDECAR);
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                Error::Execution(format!(
                    "no typed fact store for corpus `{id}` at {} ({e}) — is it an \
                     installed SEC filings corpus? scripts/setup-sec-corpus.sh writes \
                     the sidecar at install.",
                    path.display()
                ))
            })?;
            let store: SecFactStore = serde_json::from_str(&raw)
                .map_err(|e| Error::Execution(format!("malformed {}: {e}", path.display())))?;
            return Ok((id.to_string(), store));
        }
        let mut found: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(index_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("sec-cik") && e.path().join(SEC_FACTS_SIDECAR).exists() {
                    found.push(name);
                }
            }
        }
        found.sort();
        match found.as_slice() {
            [] => Err(Error::Execution(
                "no installed SEC filings corpus (no `sec-cik…` index with a \
                 sec_facts.json sidecar). Install one with \
                 scripts/setup-sec-corpus.sh <TICKER>."
                    .to_string(),
            )),
            [one] => self.resolve_corpus(Some(one)),
            many => Err(Error::Execution(format!(
                "several SEC filings corpora are installed ({}) — pass corpus_id to \
                 name the company.",
                many.join(", ")
            ))),
        }
    }
}

#[async_trait]
impl Tool for SecFactsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "sec_facts".to_string(),
            name: "SEC Filing Facts (typed financial figures)".to_string(),
            description: "Look up a company's EXACT reported financial figures from its \
                SEC filings corpus (Form 10-K, XBRL) — revenue, net sales, cost of \
                revenue, gross profit, operating income, net income, earnings per share \
                (EPS), total assets, liabilities, shareholders' equity, cash flow, \
                capital expenditures — for a fiscal year or balance date, with unit, \
                fiscal period basis, and SEC accession citation. Computes derived \
                quantities DETERMINISTICALLY: margins and ratios (ratio_to), \
                year-over-year change (compare_period). When the corpus cannot support \
                a figure it refuses and names what IS available — never approximate. \
                Use this for ANY financial figure, ratio, growth rate, or fiscal-period \
                amount from a public company's SEC filings instead of recalling or \
                computing numbers yourself."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "concept": {
                        "type": "string",
                        "description": "Canonical concept id, e.g. revenue, gross_profit, net_income, eps_diluted, total_assets, operating_cash_flow. Unknown concepts are refused by name (mode=coverage lists them)."
                    },
                    "period": {
                        "type": "string",
                        "description": "FY<year> (e.g. FY2025), a balance date YYYY-MM-DD, or a duration YYYY-MM-DD..YYYY-MM-DD. Pass the period AS THE USER STATED IT — a calendar year is the range YYYY-01-01..YYYY-12-31, NEVER converted to FY<year>: fiscal and calendar years are different periods, and when no fact matches the stated period the tool's refusal naming what IS available is the correct answer."
                    },
                    "corpus_id": {
                        "type": "string",
                        "description": "SEC corpus id (sec-cik<10 digits>). Optional when exactly one SEC filings corpus is installed."
                    },
                    "ratio_to": {
                        "type": "string",
                        "description": "Optional denominator concept: returns concept ÷ ratio_to for the same period (e.g. gross margin percent = gross_profit ratio_to revenue)."
                    },
                    "compare_period": {
                        "type": "string",
                        "description": "Optional prior period: returns the change and percent change from it (e.g. FY2024)."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["figure", "coverage"],
                        "description": "figure (default) answers one concept+period; coverage states what this corpus can and cannot answer."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "What was Apple's revenue in fiscal 2025?".to_string(),
                    call: json!({"concept": "revenue", "period": "FY2025"}),
                },
                ToolExample {
                    situation: "Gross margin percentage for fiscal 2025.".to_string(),
                    call: json!({"concept": "gross_profit", "period": "FY2025",
                                 "ratio_to": "revenue"}),
                },
                ToolExample {
                    situation: "Revenue for the calendar year 2025, January through \
                                December — NOT the fiscal year."
                        .to_string(),
                    call: json!({"concept": "revenue",
                                 "period": "2025-01-01..2025-12-31"}),
                },
                ToolExample {
                    situation: "How much did R&D spend grow year over year?".to_string(),
                    call: json!({"concept": "research_and_development_expense",
                                 "period": "FY2025", "compare_period": "FY2024"}),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "corpus_id": {"type": "string"},
                    "refused": {"type": "boolean"},
                    "reason": {"type": "string", "description": "Refusal reason naming what IS available (present when refused)."},
                    "value": {"type": "number"},
                    "unit": {"type": "string"},
                    "fiscal_year": {"type": "number"},
                    "accession": {"type": "string"},
                    "cited_figures": {"type": "array", "description": "Pre-formatted figures with period basis and accession — quote these verbatim."},
                    "derivation": {"type": "array", "description": "Formula, inputs and result for every figure — rendered verbatim downstream."},
                    "reproduce": {"type": "string"},
                    "summary": {"type": "string", "description": "All cited figures (or the refusal) as one quotable block."},
                    "numeric_audit": {"type": "object", "description": "Opt-in bare-numeral audit declaration for this turn."}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// §7.3: deterministic authority claim. Claims a question iff it
    /// names an entity AND a concept ask-term of a store whose recipe
    /// declared this tool authoritative. Pure phrase matching over the
    /// cached claim index (`store_claims`) — no embeddings, no
    /// threshold, ~µs. Over-claiming fails safe: the tool refuses,
    /// naming what IS available, and the refusal is audited.
    fn claims(&self, question: &str) -> Vec<AuthorityClaim> {
        self.claim_stores()
            .iter()
            .filter_map(|(corpus_id, store)| {
                store_claims(store, question).map(|matched| AuthorityClaim {
                    tool_id: "sec_facts".to_string(),
                    corpus_id: corpus_id.clone(),
                    matched,
                })
            })
            .collect()
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let explicit = params.get("corpus_id").and_then(|v| v.as_str());
        let (corpus_id, store) = self.resolve_corpus(explicit)?;
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("figure");

        if mode == "coverage" {
            let summary = coverage_summary(&store);
            tracing::debug!(target: "sec_facts", corpus_id = %corpus_id,
                "sec_facts: coverage requested");
            return Ok(StepOutput::Json(with_audit_optin(json!({
                "corpus_id": corpus_id,
                "entity": store.entity,
                "ticker": store.ticker,
                "refused": false,
                "coverage": store.coverage,
                "as_of": store.as_of,
                "cited_figures": [],
                "summary": summary,
                "figure_tool": "sec_facts",
            }))));
        }

        let Some(requested_concept) = params.get("concept").and_then(|v| v.as_str()) else {
            return Err(Error::Execution(
                "sec_facts figure mode needs `concept` (mode=coverage lists them).".to_string(),
            ));
        };
        let Some(period) = params.get("period").and_then(|v| v.as_str()) else {
            return Err(Error::Execution(
                "sec_facts figure mode needs `period` (FY<year>, YYYY-MM-DD, or a \
                 start..end range)."
                    .to_string(),
            ));
        };

        // Planner-spelled concept names resolve through the declared
        // alias registry (separator normalization + ask_terms); the
        // canonical id is NAMED in the output, never a silent
        // substitution (ARCH §18.3).
        let concept = match resolve_concept(&store, requested_concept) {
            Ok(c) => c,
            Err(r) => {
                return Ok(refusal_output(
                    &corpus_id,
                    &store,
                    requested_concept,
                    period,
                    &r.reason(),
                ))
            }
        };
        let concept = concept.as_str();

        // Any refusal — primary, denominator, or comparand — is the
        // answer: first-class, names what IS available, and still arms
        // the bare-numeral audit for the turn.
        let fact = match lookup(&store, concept, period) {
            Ok(f) => f,
            Err(r) => {
                return Ok(refusal_output(
                    &corpus_id,
                    &store,
                    concept,
                    period,
                    &r.reason(),
                ))
            }
        };

        let mut cited: Vec<String> = vec![cite_line(concept, fact)];
        let mut derivation: Vec<String> = vec![derivation_line(concept, fact)];
        let mut extra_values = serde_json::Map::new();

        if let Some(requested_den) = params.get("ratio_to").and_then(|v| v.as_str()) {
            let den_id = match resolve_concept(&store, requested_den) {
                Ok(c) => c,
                Err(r) => {
                    return Ok(refusal_output(
                        &corpus_id,
                        &store,
                        requested_den,
                        period,
                        &r.reason(),
                    ))
                }
            };
            let den_id = den_id.as_str();
            let den = match lookup(&store, den_id, period) {
                Ok(f) => f,
                Err(r) => {
                    return Ok(refusal_output(
                        &corpus_id,
                        &store,
                        den_id,
                        period,
                        &r.reason(),
                    ))
                }
            };
            let Some(d) = ratio(concept, fact, den_id, den) else {
                return Ok(refusal_output(
                    &corpus_id,
                    &store,
                    den_id,
                    period,
                    &format!("cannot compute {concept} ÷ {den_id}: the denominator is zero"),
                ));
            };
            cited.push(cite_line(den_id, den));
            cited.push(format!(
                "{concept} ÷ {den_id} ({period}) = {} [computed deterministically; see derivation]",
                fmt_pct(d.value)
            ));
            derivation.push(derivation_line(den_id, den));
            derivation.push(d.formula.clone());
            extra_values.insert("ratio".into(), json!(d.value));
        }

        if let Some(prior_period) = params.get("compare_period").and_then(|v| v.as_str()) {
            let prior = match lookup(&store, concept, prior_period) {
                Ok(f) => f,
                Err(r) => {
                    return Ok(refusal_output(
                        &corpus_id,
                        &store,
                        concept,
                        prior_period,
                        &r.reason(),
                    ))
                }
            };
            let (abs, pct) = change(concept, fact, prior);
            cited.push(cite_line(concept, prior));
            cited.push(format!(
                "Δ {concept} ({prior_period} → {period}) = {} [computed deterministically; see derivation]",
                fmt_compact(abs.value, &fact.unit)
            ));
            derivation.push(derivation_line(concept, prior));
            derivation.push(abs.formula.clone());
            extra_values.insert("change".into(), json!(abs.value));
            if let Some(p) = pct {
                cited.push(format!(
                    "Δ% {concept} ({prior_period} → {period}) = {} [computed deterministically; see derivation]",
                    fmt_pct(p.value)
                ));
                derivation.push(p.formula.clone());
                extra_values.insert("change_pct".into(), json!(p.value));
            }
        }

        let summary = cited.join("\n");
        let reproduce = format!(
            "Reproduce: https://data.sec.gov/api/xbrl/companyfacts/CIK{}.json → \
             facts[\"us-gaap\"][\"{}\"] for period ending {}; or open accession {} at \
             sec.gov/cgi-bin/browse-edgar.",
            store.cik,
            fact.tag.trim_start_matches("us-gaap:"),
            fact.end,
            fact.accession
        );

        let mut out = json!({
            "corpus_id": corpus_id,
            "entity": store.entity,
            "ticker": store.ticker,
            "refused": false,
            "concept": concept,
            // The alias resolution NAMED, when one fired (ARCH §18.3).
            "requested_concept": if requested_concept == concept {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(requested_concept.to_string())
            },
            "label": store.concepts.get(concept).map(|c| c.label.clone()),
            "tag": fact.tag,
            "value": fact.value,
            "unit": fact.unit,
            "period": {"start": fact.start, "end": fact.end},
            "fiscal_year": fact.fiscal_year,
            "accession": fact.accession,
            "form": fact.form,
            "filed": fact.filed,
            "cited_figures": cited,
            "derivation": derivation,
            "reproduce": reproduce,
            "summary": summary,
            "figure_tool": "sec_facts",
        });
        // USD facts are conventionally relayed at millions grain
        // ("416,161 million") — emit that scaling as a raw leaf so the
        // bare form traces by value.
        if fact.unit == "USD" {
            out["value_millions"] = json!(fact.value / 1_000_000.0);
        }
        for (k, v) in extra_values {
            out[k.as_str()] = v;
        }
        Ok(StepOutput::Json(with_audit_optin(out)))
    }
}

/// One cited figure, compact form: parseable by the numeric audit and
/// carrying period basis + accession so the reader can check it.
fn cite_line(concept: &str, f: &SecFact) -> String {
    let basis = match &f.start {
        Some(s) => format!("fiscal year FY{} ({} to {})", f.fiscal_year, s, f.end),
        None => format!("as of {} (fiscal FY{} balance date)", f.end, f.fiscal_year),
    };
    format!(
        "{concept} = {} — {basis} [{}; {} accession {} filed {}]",
        fmt_compact(f.value, &f.unit),
        f.tag,
        f.form,
        f.accession,
        f.filed
    )
}

/// One derivation line, full precision.
fn derivation_line(concept: &str, f: &SecFact) -> String {
    format!(
        "{concept} = {} — SEC XBRL {}, period {}..{}, Form {} accession {} filed {}",
        fmt_full(f.value, &f.unit),
        f.tag,
        f.start.as_deref().unwrap_or("instant"),
        f.end,
        f.form,
        f.accession,
        f.filed
    )
}

/// A refusal is a first-class answer: reason names what IS available,
/// and the bare-numeral audit is STILL armed — with the refusal's own
/// numerals allowed — so a model layering a recalled figure on top of
/// the refusal is flagged.
fn refusal_output(
    corpus_id: &str,
    store: &SecFactStore,
    concept: &str,
    period: &str,
    reason: &str,
) -> StepOutput {
    tracing::debug!(target: "sec_facts", corpus_id = %corpus_id, concept, period,
        reason = %reason, "sec_facts: refusal emitted");
    StepOutput::Json(with_audit_optin(json!({
        "corpus_id": corpus_id,
        "entity": store.entity,
        "ticker": store.ticker,
        "refused": true,
        "concept": concept,
        "requested_period": period,
        "reason": reason,
        "cited_figures": [],
        "summary": format!("REFUSED: {reason}"),
        "figure_tool": "sec_facts",
    })))
}

/// Attach the §6.3(b) opt-in: audit bare numerals this turn, allowing
/// exactly the numeric tokens of the tool's own emitted text (built with
/// the auditor's lexer — one decider, ARCH §10.6).
fn with_audit_optin(mut out: serde_json::Value) -> serde_json::Value {
    let mut tokens: Vec<String> = Vec::new();
    collect_text(&out, &mut |s| tokens.extend(numeric_tokens(s)));
    tokens.sort();
    tokens.dedup();
    out["numeric_audit"] = json!({
        "audit_bare_numerals": true,
        "allowed_tokens": tokens,
    });
    out
}

fn collect_text(v: &serde_json::Value, f: &mut impl FnMut(&str)) {
    match v {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_text(x, f)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_text(x, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact() -> SecFact {
        serde_json::from_value(serde_json::json!({
            "value": 416161000000.0, "unit": "USD",
            "start": "2024-09-29", "end": "2025-09-27", "fiscal_year": 2025,
            "tag": "us-gaap:RevenueFromContractWithCustomerExcludingAssessedTax",
            "accession": "0000320193-25-000079", "form": "10-K", "filed": "2025-10-31"
        }))
        .unwrap()
    }

    #[test]
    fn cite_line_carries_basis_and_accession() {
        let line = cite_line("revenue", &fact());
        assert!(line.contains("$416,161 million"), "{line}");
        assert!(
            line.contains("fiscal year FY2025 (2024-09-29 to 2025-09-27)"),
            "{line}"
        );
        assert!(line.contains("accession 0000320193-25-000079"), "{line}");
    }

    #[test]
    fn audit_optin_allows_exactly_the_tools_own_numerals() {
        let out = with_audit_optin(serde_json::json!({
            "summary": "revenue = $416,161 million — fiscal year FY2025 \
                        (2024-09-29 to 2025-09-27) [accession 0000320193-25-000079]",
        }));
        let na = &out["numeric_audit"];
        assert_eq!(na["audit_bare_numerals"], serde_json::json!(true));
        let toks: Vec<String> = na["allowed_tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        assert!(toks.contains(&"$416,161 million".to_string()), "{toks:?}");
        assert!(toks.contains(&"2024-09-29".to_string()), "{toks:?}");
        assert!(
            toks.contains(&"0000320193-25-000079".to_string()),
            "{toks:?}"
        );
        // And the model-side guarantee: a figure the tool never emitted is
        // NOT in the allowed set.
        assert!(!toks.iter().any(|t| t.contains("109,158")), "{toks:?}");
    }
}
