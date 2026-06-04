//! `parcel_analytics` — deterministic land-value-tax aggregates over a
//! corpus of Parcel atoms (default `sf-assessor-roll`).
//!
//! Reads the corpus's `atoms.json`, filters to Parcel `Entity` atoms,
//! and folds their typed `attributes` into the revenue-neutral land-levy
//! figures via `corpus_engine`'s pure `parcel_analytics` lib. Returns the
//! figures PRE-FORMATTED WITH CITATIONS so the synthesis layer quotes
//! numbers it cannot have invented — Layer 1 of the LVT "no confabulated
//! numbers" guarantee. `Effect::Read`, no permissions, no inference: the
//! model never originates a figure here.
//!
//! Contrast with `compute` (the Python executor): that puts arithmetic
//! back in LLM-authored code. This tool keeps every dollar a fold over a
//! named atom set, deterministic and citeable by construction.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
};

use corpus_engine::enrichment::atlas::analysis::{compute_aggregates, flags, FlagKind};
use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::CorpusEngine;

const DEFAULT_CORPUS_ID: &str = "sf-assessor-roll";
const DEFAULT_ENTITY_TYPE: &str = "parcel";
/// SF business-tax take being retired (~$1.4B; SPUR / Controller). The
/// revenue the flat land levy must replace.
const DEFAULT_BUSINESS_TAX_TARGET: f64 = 1_400_000_000.0;
/// Effective SF secured property-tax rate, for the (labelled) current-tax
/// estimate in per-parcel deltas.
const DEFAULT_PROPERTY_TAX_RATE: f64 = 0.0118;

/// Deterministic LVT analytics over a parcel corpus.
pub struct ParcelAnalyticsTool {
    engine: Arc<CorpusEngine>,
}

impl ParcelAnalyticsTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ParcelAnalyticsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "parcel_analytics".to_string(),
            name: "Parcel Analytics (Land-Value Tax)".to_string(),
            description: "Compute the revenue-neutral land-value-tax rate and \
                land-base aggregates DETERMINISTICALLY from a corpus of parcel \
                atoms (default `sf-assessor-roll`). Returns pre-cited figures — \
                land value total, the neutral rate, parcel count, and \
                land-share / underuse flag counts — that MUST be quoted verbatim: \
                each number is summed from source parcels, never estimated. Use \
                this for any land-value-tax dollar figure, rate, or count instead \
                of doing the arithmetic yourself."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "corpus_id": {
                        "type": "string",
                        "description": "Parcel corpus id (default `sf-assessor-roll`)."
                    },
                    "business_tax_target": {
                        "type": "number",
                        "description": "Revenue the flat land levy must replace, in dollars (default 1.4e9 — the SF business-tax take)."
                    },
                    "current_property_tax_rate": {
                        "type": "number",
                        "description": "Effective property-tax rate for the labelled current-tax estimate (default 0.0118)."
                    }
                },
                "required": []
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "corpus_id": {"type": "string"},
                    "parcel_count": {"type": "number"},
                    "land_value_total": {"type": "number"},
                    "improvement_value_total": {"type": "number"},
                    "business_tax_target": {"type": "number"},
                    "neutral_rate": {"type": "number", "description": "= business_tax_target / land_value_total, on the LAND base"},
                    "high_land_share_count": {"type": "number"},
                    "underused_count": {"type": "number"},
                    "cited_figures": {"type": "array", "description": "Pre-formatted figures with [corpus: …] citations — quote these verbatim."},
                    "summary": {"type": "string", "description": "All cited figures as one quotable block."}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus_id = params
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_CORPUS_ID)
            .to_string();
        let business_tax_target = params
            .get("business_tax_target")
            .and_then(|v| v.as_f64())
            .unwrap_or(DEFAULT_BUSINESS_TAX_TARGET);
        let _property_tax_rate = params
            .get("current_property_tax_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(DEFAULT_PROPERTY_TAX_RATE);

        let atlas_dir = self.engine.index_dir().join(&corpus_id).join(ATLAS_DIRNAME);
        if !atlas_dir.join("atoms.json").exists() {
            return Err(Error::Execution(format!(
                "no atoms.json for corpus `{corpus_id}` at {} — is it ingested? \
                 parcel_analytics reads the deterministic Parcel atoms the \
                 tabular_atoms extractor writes during ingest.",
                atlas_dir.display()
            )));
        }
        let atoms_file = read_atlas_atoms(&atlas_dir)
            .map_err(|e| Error::Execution(format!("read atoms.json for `{corpus_id}`: {e}")))?;

        // Move Parcel entity atoms out of their envelopes (no clone).
        let parcels: Vec<_> = atoms_file
            .atoms
            .into_iter()
            .filter_map(|env| match env {
                AtomEnvelope::Entity(e) => match &e.entity_type {
                    EntityType::Other(t) if t.as_str() == DEFAULT_ENTITY_TYPE => Some(e),
                    _ => None,
                },
                _ => None,
            })
            .collect();

        if parcels.is_empty() {
            return Err(Error::Execution(format!(
                "corpus `{corpus_id}` has no `{DEFAULT_ENTITY_TYPE}` atoms — nothing to aggregate."
            )));
        }

        let agg = compute_aggregates(&parcels, &corpus_id, business_tax_target);
        let fs = flags(&parcels);
        let high_land = fs.iter().filter(|f| f.kind == FlagKind::HighLandShare).count();
        let underused = fs.iter().filter(|f| f.kind == FlagKind::Underused).count();

        // Representative atom id for the citation handle.
        let rep = agg.atom_ids.first().cloned().unwrap_or_default();
        let cite = format!("[{corpus_id}: {} parcels; e.g. atom {rep}]", fmt_int(agg.parcel_count as f64));

        let cited_figures = vec![
            format!("land_value_total = {} {cite}", fmt_usd(agg.land_value_total)),
            format!(
                "improvement_value_total = {} {cite}",
                fmt_usd(agg.improvement_value_total)
            ),
            format!("parcel_count = {} {cite}", fmt_int(agg.parcel_count as f64)),
            format!(
                "neutral_rate = {} [= business_tax_target {} ÷ land_value_total {}]",
                fmt_pct(agg.neutral_rate),
                fmt_usd(agg.business_tax_target),
                fmt_usd(agg.land_value_total)
            ),
            format!("high_land_share parcels = {} {cite}", fmt_int(high_land as f64)),
            format!("underused parcels = {} {cite}", fmt_int(underused as f64)),
        ];
        let summary = cited_figures.join("\n");

        Ok(StepOutput::Json(json!({
            "corpus_id": agg.corpus_id,
            "parcel_count": agg.parcel_count,
            "land_value_total": agg.land_value_total,
            "improvement_value_total": agg.improvement_value_total,
            "business_tax_target": agg.business_tax_target,
            "neutral_rate": agg.neutral_rate,
            "high_land_share_count": high_land,
            "underused_count": underused,
            "cited_figures": cited_figures,
            "summary": summary,
        })))
    }
}

/// `$172.62B` / `$169.11M` / `$5.0K` / `$42` — compact USD.
fn fmt_usd(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("${:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("${:.2}M", v / 1e6)
    } else if a >= 1e3 {
        format!("${:.1}K", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

/// `0.81%` — rate rendered as a percentage.
fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

/// `207,792` — thousands-grouped integer.
fn fmt_int(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_helpers() {
        assert_eq!(fmt_usd(172_620_140_416.0), "$172.62B");
        assert_eq!(fmt_usd(1_400_000_000.0), "$1.40B");
        assert_eq!(fmt_pct(0.008109), "0.81%");
        assert_eq!(fmt_int(207_792.0), "207,792");
        assert_eq!(fmt_int(874.0), "874");
    }
}
