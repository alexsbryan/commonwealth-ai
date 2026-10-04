// SPDX-License-Identifier: AGPL-3.0-or-later
//! Land-value-tax analytics over Parcel atoms — moved to the
//! `corpus-engine-atlas-reader` leaf by fp-67 so read paths reach it without
//! linking the engine. The tests stay here: their fixture is the engine's
//! `tabular_atoms` extractor.

pub use corpus_engine_atlas_reader::parcel_analytics::*; // shim: moved by fp-67

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::Entity;
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
                m.insert(
                    "assessed_land_value".into(),
                    Value::String(land.to_string()),
                );
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
        let atoms = parcels(&[
            ("p1", 1000.0, 500.0),
            ("p2", 2000.0, 100.0),
            ("p3", 0.0, 0.0),
        ]);
        let agg = compute_aggregates(&atoms, "sf-assessor-roll", 300.0, 0.0118);
        assert_eq!(agg.parcel_count, 2);
        assert_eq!(agg.land_value_total, 3000.0);
        assert_eq!(agg.improvement_value_total, 600.0);
        // 300 / 3000 = 0.10 — neutral rate is on the LAND base, not the
        // total roll (which would be 300 / 3600 ≈ 0.083).
        assert!(
            (agg.neutral_rate - 0.10).abs() < 1e-9,
            "rate = {}",
            agg.neutral_rate
        );
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
        let kinds: Vec<(&str, FlagKind)> = fs
            .iter()
            .map(|f| (f.parcel_number.as_str(), f.kind))
            .collect();
        assert!(kinds.contains(&("p1", FlagKind::HighLandShare)));
        assert!(kinds.contains(&("p2", FlagKind::HighLandShare)));
        assert!(kinds.contains(&("p2", FlagKind::Underused)));
        assert!(!kinds.contains(&("p1", FlagKind::Underused)));
    }
}
