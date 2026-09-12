// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parcel reads for the SF-LVT mesh app — the three atom folds the desktop
//! ran in-process until 2026-09-11 (`commands/meshapp.rs`'s
//! `meshapp_read_corpus`, `meshapp_search_parcels`,
//! `meshapp_parcel_analytics`), moved here so the daemon serves them over
//! `/internal/meshapp/{corpus}/parcels…` and a thin client only gates and
//! calls (thin-desktop order).
//!
//! Deterministic, path-in DTO-out, Tauri-free like the rest of this crate.
//! The "no confabulated numbers" guarantee is corpus-engine's
//! `compute_aggregates` / `flags`; this module only filters atoms and
//! renders the derivation trace, in the same words the desktop rendered it.

use std::collections::HashSet;
use std::path::Path;

use corpus_engine::enrichment::atlas::analysis::{compute_aggregates, flags, FlagKind};
use corpus_engine::enrichment::atlas::atoms::Entity;
use corpus_engine::enrichment::atlas::AtomEnvelope;
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use sovereign_contracts::daemon_wire::{ParcelAnalyticsDto, ParcelDto};

use crate::MeshAppError;

/// Default SF business-tax take (~$1.4B) the flat land levy must replace.
pub const DEFAULT_BUSINESS_TAX_TARGET: f64 = 1_400_000_000.0;
/// The declared entity type parcel atoms carry (`EntityType::Other`).
pub const PARCEL_ENTITY_TYPE: &str = "parcel";
/// SF's effective secured property-tax rate (the 1% Prop-13 base + voter-
/// approved add-ons). A labeled estimate — used to derive the revenue-neutral
/// land-only ("swap") rate, which is the only coherent per-parcel comparison:
/// today's tax falls on land + improvements; a land-value tax shifts the same
/// revenue onto land alone, producing real winners (improvement-heavy parcels)
/// and losers (land-rich / underused parcels).
pub const DEFAULT_PROPERTY_TAX_RATE: f64 = 0.0118;
/// `meshapp_search_parcels`' page cap.
pub const SEARCH_LIMIT_DEFAULT: usize = 25;
pub const SEARCH_LIMIT_MAX: usize = 100;

/// Every atom in the corpus's atlas. An absent `atlas/` is an ABSENCE —
/// the caller asked for a corpus that has no map — not an empty answer.
fn read_atoms(index_path: &Path, corpus_id: &str) -> Result<Vec<AtomEnvelope>, MeshAppError> {
    let atlas_dir = index_path.join("atlas");
    if !atlas_dir.is_dir() {
        return Err(MeshAppError::not_found(format!(
            "corpus `{corpus_id}` has no atlas"
        )));
    }
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| MeshAppError::io("read atoms", e))?;
    Ok(file.atoms)
}

fn parcel_dto(e: &Entity) -> ParcelDto {
    ParcelDto {
        atom_id: e.id.as_str().to_string(),
        parcel_number: e.canonical_name.clone(),
        source_chunk: e.provenance.source_chunk_id.clone(),
        attributes: e.attributes.clone(),
    }
}

/// `meshapp_read_corpus`: the requested parcel atoms with provenance. Each
/// id matches by EITHER the atom id (content-hash) OR the parcel number
/// (canonical name) — so a UI that knows only a human parcel number (a
/// blklot) can look it up without deriving the host-side hash.
pub fn parcels_by_id(
    index_path: &Path,
    corpus_id: &str,
    ids: &[String],
) -> Result<Vec<ParcelDto>, MeshAppError> {
    let want: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let out = read_atoms(index_path, corpus_id)?
        .iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e)
                if want.contains(e.id.as_str()) || want.contains(e.canonical_name.as_str()) =>
            {
                Some(parcel_dto(e))
            }
            _ => None,
        })
        .collect();
    Ok(out)
}

/// `meshapp_search_parcels`: substring/number search over parcel atoms so
/// a UI (a homeowner) can find their parcel by street name or number
/// without knowing the atom id. Matches the parcel number (exact,
/// case-folded) OR `property_location` (substring, case-folded); capped at
/// `limit`. A blank query is `[]` without reading the atlas.
pub fn search_parcels(
    index_path: &Path,
    corpus_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<ParcelDto>, MeshAppError> {
    let q = query.trim().to_uppercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<ParcelDto> = read_atoms(index_path, corpus_id)?
        .iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => {
                let num_match = e.canonical_name.to_uppercase() == q;
                let addr_match = e
                    .attributes
                    .get("property_location")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_uppercase().contains(&q))
                    .unwrap_or(false);
                (num_match || addr_match).then(|| parcel_dto(e))
            }
            _ => None,
        })
        .collect();
    out.truncate(limit);
    Ok(out)
}

/// `meshapp_parcel_analytics`: the parcel atoms folded into the
/// revenue-neutral land-levy aggregate via corpus-engine's pure lib. No
/// inference; the macro model's headline figures are computed here, never
/// originated by a model. A corpus with no `parcel` atoms is an absence.
pub fn parcel_analytics(
    index_path: &Path,
    corpus_id: &str,
    business_tax_target: Option<f64>,
) -> Result<ParcelAnalyticsDto, MeshAppError> {
    let target = business_tax_target.unwrap_or(DEFAULT_BUSINESS_TAX_TARGET);
    let parcels: Vec<Entity> = read_atoms(index_path, corpus_id)?
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => match &e.entity_type {
                EntityType::Other(t) if t.as_str() == PARCEL_ENTITY_TYPE => Some(e),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if parcels.is_empty() {
        return Err(MeshAppError::not_found(format!(
            "corpus `{corpus_id}` has no `{PARCEL_ENTITY_TYPE}` atoms"
        )));
    }

    let agg = compute_aggregates(&parcels, corpus_id, target, DEFAULT_PROPERTY_TAX_RATE);
    let fs = flags(&parcels);
    let high = fs
        .iter()
        .filter(|f| f.kind == FlagKind::HighLandShare)
        .count();
    let under = fs.iter().filter(|f| f.kind == FlagKind::Underused).count();

    // The revenue-neutral property-tax → land-only swap is computed by the lib
    // (single source — the chat `parcel_analytics` tool reads the same fields).
    // Bind locals so the derivation/DTO below render the lib's values.
    let roll = agg.land_value_total + agg.improvement_value_total;
    let property_tax_rate = agg.property_tax_rate;
    let property_tax_revenue_est = agg.property_tax_revenue_est;
    let property_tax_swap_rate = agg.property_tax_swap_rate;

    let n = fmt_int(agg.parcel_count as f64);
    let derivation = vec![
        format!(
            "land_value_total = Σ assessed_land_value over {n} parcel atoms ({corpus_id}) = {}",
            fmt_usd(agg.land_value_total)
        ),
        format!(
            "neutral_rate = business_tax_target ÷ land_value_total = {} ÷ {} = {}",
            fmt_usd(agg.business_tax_target),
            fmt_usd(agg.land_value_total),
            fmt_pct(agg.neutral_rate)
        ),
        format!(
            "property_tax_revenue_est = (Σland + Σimprovement) × property_tax_rate = {} × {} = {}",
            fmt_usd(roll),
            fmt_pct(property_tax_rate),
            fmt_usd(property_tax_revenue_est)
        ),
        format!(
            "property_tax_swap_rate = property_tax_revenue_est ÷ land_value_total = {} ÷ {} = {}",
            fmt_usd(property_tax_revenue_est),
            fmt_usd(agg.land_value_total),
            fmt_pct(property_tax_swap_rate)
        ),
    ];

    Ok(ParcelAnalyticsDto {
        corpus_id: agg.corpus_id,
        parcel_count: agg.parcel_count,
        land_value_total: agg.land_value_total,
        improvement_value_total: agg.improvement_value_total,
        business_tax_target: agg.business_tax_target,
        neutral_rate: agg.neutral_rate,
        property_tax_rate,
        property_tax_revenue_est,
        property_tax_swap_rate,
        high_land_share_count: high,
        underused_count: under,
        derivation,
    })
}

/// `$174,097,946,887.00` — full-precision, comma-grouped USD for the
/// derivation trace (matches the chat/tool surface's `fmt_usd_full` so the
/// two agree).
fn fmt_usd(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let dollars = (cents / 100) as f64;
    format!("${}.{:02}", fmt_int(dollars), (cents % 100).abs())
}

fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

fn fmt_int(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
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
