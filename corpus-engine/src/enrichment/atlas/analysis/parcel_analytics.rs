// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic land-value-tax analytics over Parcel atoms.
//!
//! Pure functions — no inference, no I/O. Every figure is a sum or
//! quotient over `Entity::attributes` typed by the `tabular_atoms`
//! extractor, so the SF-LVT "no confabulated numbers" guarantee holds
//! *structurally*: a number is, by construction, a fold over a named
//! atom set, and each headline result carries the `atom_ids` it
//! summarises so the synthesis layer can cite them. The agent narrates
//! these results; it never recomputes them.
//!
//! Modelling note — an LVT taxes LAND ONLY. The revenue-neutral rate is
//! `target_revenue / Σ assessed_land_value`, NOT `target / total_roll`:
//! dividing by the full roll (land + improvements) would understate the
//! land-only rate roughly two-fold and is an economic error.

use serde::Serialize;

use crate::enrichment::atlas::atoms::Entity;

/// land / (land + improvement) at or above which a parcel is "land-rich".
const HIGH_LAND_SHARE: f64 = 0.6;
/// improvement / land at or below which a parcel is "underused"
/// (near-vacant high-value lot — a speculation signal).
const UNDERUSED_RATIO: f64 = 0.1;

/// City-wide aggregates for the revenue-neutral land levy.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelAggregates {
    pub corpus_id: String,
    /// Parcels with a positive assessed land value that fed the sum.
    pub parcel_count: usize,
    /// Σ assessed_land_value — the land-value-tax BASE.
    pub land_value_total: f64,
    /// Σ assessed_improvement_value (context; NOT part of the LVT base).
    pub improvement_value_total: f64,
    /// Revenue the flat land levy must raise (e.g. the volatile
    /// business-tax take being retired). An input constant — itself
    /// cited to its source corpus, never invented.
    pub business_tax_target: f64,
    /// `business_tax_target / land_value_total` — the revenue-neutral
    /// rate on the LAND base.
    pub neutral_rate: f64,
    /// Effective property-tax rate used to derive the swap scenario — a
    /// labelled estimate (the roll carries assessed values, not tax paid).
    pub property_tax_rate: f64,
    /// `(land + improvement) × property_tax_rate` — estimated revenue today's
    /// property tax raises (it falls on land + improvements).
    pub property_tax_revenue_est: f64,
    /// `property_tax_revenue_est / land_value_total` — the revenue-neutral
    /// rate for a land-ONLY tax replacing the property tax. The coherent
    /// per-parcel comparison: shift the SAME revenue off improvements onto
    /// land alone (winners = improvement-heavy, losers = land-rich).
    pub property_tax_swap_rate: f64,
    /// Every parcel atom that fed the sum — the citation handle for the
    /// headline figures.
    pub atom_ids: Vec<String>,
}

/// Per-parcel winner/loser line under the revenue-neutral levy.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelDelta {
    pub atom_id: String,
    pub parcel_number: String,
    pub land_value: f64,
    pub improvement_value: f64,
    /// `(land + improvement) × current_property_tax_rate`. Labelled an
    /// estimate — the roll carries assessed values, not tax paid.
    pub estimated_current_property_tax: f64,
    /// `land_value × neutral_rate`.
    pub lvt_levy: f64,
    /// `lvt_levy − estimated_current_property_tax`. Negative = winner.
    pub delta: f64,
}

/// A parcel flagged by a deterministic threshold.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelFlag {
    pub atom_id: String,
    pub parcel_number: String,
    pub kind: FlagKind,
    /// The ratio that tripped the flag (land share, or improvement/land).
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    /// land / (land + improvement) ≥ 0.6 — the land-rich parcels that
    /// are the LVT base.
    HighLandShare,
    /// improvement / land ≤ 0.1 — near-vacant high-value lots.
    Underused,
}

/// Read a numeric attribute off a Parcel atom (`tabular_atoms` stores
/// these as JSON numbers).
fn attr_f64(e: &Entity, key: &str) -> Option<f64> {
    e.attributes.get(key).and_then(|v| v.as_f64())
}

/// Σ over the land base + the revenue-neutral rate. Parcels missing or
/// with non-positive land value are skipped (they aren't part of the
/// base). Deterministic; logs the headline figures for glassbox tracing.
pub fn compute_aggregates(
    atoms: &[Entity],
    corpus_id: &str,
    business_tax_target: f64,
    property_tax_rate: f64,
) -> ParcelAggregates {
    let mut land_value_total = 0.0;
    let mut improvement_value_total = 0.0;
    let mut atom_ids = Vec::new();
    for e in atoms {
        let land = attr_f64(e, "assessed_land_value").unwrap_or(0.0);
        if land <= 0.0 {
            continue;
        }
        land_value_total += land;
        improvement_value_total += attr_f64(e, "assessed_improvement_value").unwrap_or(0.0);
        atom_ids.push(e.id.as_str().to_string());
    }
    let neutral_rate = if land_value_total > 0.0 {
        business_tax_target / land_value_total
    } else {
        0.0
    };
    // Revenue-neutral property-tax → land-only swap: the rate at which a
    // land-only tax raises the SAME as today's property tax (which falls on
    // land + improvements). The coherent per-parcel comparison.
    let property_tax_revenue_est = (land_value_total + improvement_value_total) * property_tax_rate;
    let property_tax_swap_rate = if land_value_total > 0.0 {
        property_tax_revenue_est / land_value_total
    } else {
        0.0
    };
    tracing::info!(
        corpus = %corpus_id,
        parcels = atom_ids.len(),
        land_value_total,
        improvement_value_total,
        business_tax_target,
        neutral_rate,
        property_tax_rate,
        property_tax_revenue_est,
        property_tax_swap_rate,
        "parcel_analytics: computed revenue-neutral land levy aggregates"
    );
    ParcelAggregates {
        corpus_id: corpus_id.to_string(),
        parcel_count: atom_ids.len(),
        land_value_total,
        improvement_value_total,
        business_tax_target,
        neutral_rate,
        property_tax_rate,
        property_tax_revenue_est,
        property_tax_swap_rate,
        atom_ids,
    }
}

/// Per-parcel delta under the revenue-neutral levy, binnable downstream
/// by use / neighborhood. `current_property_tax_rate` is the effective
/// rate used for the (labelled) current-tax estimate.
pub fn per_parcel_deltas(
    atoms: &[Entity],
    neutral_rate: f64,
    current_property_tax_rate: f64,
) -> Vec<ParcelDelta> {
    let mut out = Vec::new();
    for e in atoms {
        let land = attr_f64(e, "assessed_land_value").unwrap_or(0.0);
        if land <= 0.0 {
            continue;
        }
        let improvement = attr_f64(e, "assessed_improvement_value").unwrap_or(0.0);
        let estimated_current_property_tax = (land + improvement) * current_property_tax_rate;
        let lvt_levy = land * neutral_rate;
        out.push(ParcelDelta {
            atom_id: e.id.as_str().to_string(),
            parcel_number: e.canonical_name.clone(),
            land_value: land,
            improvement_value: improvement,
            estimated_current_property_tax,
            lvt_levy,
            delta: lvt_levy - estimated_current_property_tax,
        });
    }
    out
}

/// Deterministic threshold flags (high land share, underused) over the
/// parcel atoms.
pub fn flags(atoms: &[Entity]) -> Vec<ParcelFlag> {
    let mut out = Vec::new();
    for e in atoms {
        let land = attr_f64(e, "assessed_land_value").unwrap_or(0.0);
        if land <= 0.0 {
            continue;
        }
        let improvement = attr_f64(e, "assessed_improvement_value").unwrap_or(0.0);
        let total = land + improvement;
        if total > 0.0 && land / total >= HIGH_LAND_SHARE {
            out.push(ParcelFlag {
                atom_id: e.id.as_str().to_string(),
                parcel_number: e.canonical_name.clone(),
                kind: FlagKind::HighLandShare,
                value: land / total,
            });
        }
        if improvement / land <= UNDERUSED_RATIO {
            out.push(ParcelFlag {
                atom_id: e.id.as_str().to_string(),
                parcel_number: e.canonical_name.clone(),
                kind: FlagKind::Underused,
                value: improvement / land,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::tabular_atoms::{build_atoms, TabularAtomsConfig};
    use serde_json::{Map, Value};

    fn cfg() -> TabularAtomsConfig {
        TabularAtomsConfig {
            document_path: "$[*]".to_string(),
            id_column: "parcel_number".to_string(),
            entity_type: "parcel".to_string(),
            numeric_attributes: vec![
                "assessed_land_value".to_string(),
                "assessed_improvement_value".to_string(),
            ],
            string_attributes: vec![],
        }
    }

    /// Build Parcel atoms via the real `tabular_atoms` builder, from
    /// Socrata-shaped string cells — exercises extractor → analytics.
    fn parcels(specs: &[(&str, f64, f64)]) -> Vec<Entity> {
        let rows: Vec<Map<String, Value>> = specs
            .iter()
            .map(|(id, land, impr)| {
                let mut m = Map::new();
                m.insert("parcel_number".into(), Value::String(id.to_string()));
                m.insert("assessed_land_value".into(), Value::String(land.to_string()));
                m.insert(
                    "assessed_improvement_value".into(),
                    Value::String(impr.to_string()),
                );
                m
            })
            .collect();
        build_atoms(&rows, &cfg(), "sf-assessor-roll")
    }

    #[test]
    fn aggregates_sum_land_base_and_derive_neutral_rate() {
        // p3 has zero land → excluded from the base.
        let atoms = parcels(&[("p1", 1000.0, 500.0), ("p2", 2000.0, 100.0), ("p3", 0.0, 0.0)]);
        let agg = compute_aggregates(&atoms, "sf-assessor-roll", 300.0, 0.0118);
        assert_eq!(agg.parcel_count, 2);
        assert_eq!(agg.land_value_total, 3000.0);
        assert_eq!(agg.improvement_value_total, 600.0);
        // 300 / 3000 = 0.10 — neutral rate is on the LAND base, not the
        // total roll (which would be 300 / 3600 ≈ 0.083).
        assert!((agg.neutral_rate - 0.10).abs() < 1e-9, "rate = {}", agg.neutral_rate);
        // Swap scenario: revenue = (3000 + 600) × 0.0118 = 42.48; swap rate =
        // 42.48 / 3000 = 0.01416 (on the LAND base).
        assert_eq!(agg.property_tax_rate, 0.0118);
        assert!((agg.property_tax_revenue_est - 42.48).abs() < 1e-9);
        assert!((agg.property_tax_swap_rate - 0.01416).abs() < 1e-9);
        assert_eq!(agg.atom_ids.len(), 2, "atom_ids is the citation set");
    }

    #[test]
    fn per_parcel_delta_is_levy_minus_estimated_tax() {
        let atoms = parcels(&[("p1", 1000.0, 500.0)]);
        let deltas = per_parcel_deltas(&atoms, 0.10, 0.0118);
        assert_eq!(deltas.len(), 1);
        let d = &deltas[0];
        assert_eq!(d.lvt_levy, 100.0); // 1000 × 0.10
        assert!((d.estimated_current_property_tax - 17.7).abs() < 1e-9); // 1500 × 0.0118
        assert!((d.delta - 82.3).abs() < 1e-9); // loser under this rate
    }

    #[test]
    fn flags_high_land_share_and_underused() {
        // p2: share 2000/2100 ≈ 0.95 (high), impr/land 0.05 (underused).
        // p1: share 1000/1500 ≈ 0.67 (high), impr/land 0.5 (not underused).
        let atoms = parcels(&[("p1", 1000.0, 500.0), ("p2", 2000.0, 100.0)]);
        let fs = flags(&atoms);
        let kinds: Vec<(&str, FlagKind)> =
            fs.iter().map(|f| (f.parcel_number.as_str(), f.kind)).collect();
        assert!(kinds.contains(&("p1", FlagKind::HighLandShare)));
        assert!(kinds.contains(&("p2", FlagKind::HighLandShare)));
        assert!(kinds.contains(&("p2", FlagKind::Underused)));
        assert!(!kinds.contains(&("p1", FlagKind::Underused)));
    }
}
