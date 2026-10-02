// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic tabular → typed-atom extractor.
//!
//! Reads tabular JSON rows — e.g. the bare-array response a Socrata /
//! `http_api` acquirer persists — and produces, per row, BOTH:
//!   * one [`ExtractedDoc`] (a rendered, FTS-indexable line for
//!     retrieval / citation, with the full typed row in `metadata`), and
//!   * (via [`build_atoms`], invoked from the ingest flow where the
//!     atlas dir is known) one atlas [`Entity`] atom whose declared
//!     numeric and string columns are recorded in [`Entity::attributes`]
//!     under a [`SignalProvenance`] of `extractor_id = "tabular_atoms"`.
//!
//! No inference runs here — parsing and typing are pure and
//! deterministic. The figures downstream analytics sum are read from
//! these atoms; the model never originates a number (ARCH glassbox +
//! the SF-LVT "no confabulated numbers" invariant). The `Extractor`
//! trait yields documents only (it has no atlas dir), so atom emission
//! is a sibling pure function the ingest orchestrator calls — the two
//! share [`parse_rows`] so the chunk and atom views never diverge.

use std::path::Path;

use jsonpath_rust::JsonPath;
use serde_json::{Map, Value};

use super::json_api::collect_json_files;
use super::{ExtractedDoc, Extractor};
use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, SignalKind, SignalProvenance};
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use crate::error::{Error, Result};

/// Resolved config for the `tabular_atoms` extractor — mirrors
/// [`crate::recipe::ExtractorConfig::TabularAtoms`] with its `Option`
/// defaults already applied by `make_extractor`.
#[derive(Debug, Clone)]
pub struct TabularAtomsConfig {
    /// JSONPath selecting the row array (`$[*]` for a bare top-level
    /// array; `$.results[*]` for an enveloped response).
    pub document_path: String,
    /// Column whose value is each row's stable identity.
    pub id_column: String,
    /// Atom entity-type label (becomes `EntityType::Other(..)`).
    pub entity_type: String,
    /// Columns parsed as numbers and stored as JSON numbers.
    pub numeric_attributes: Vec<String>,
    /// Columns kept verbatim as strings.
    pub string_attributes: Vec<String>,
}

/// `Extractor` impl: one rendered [`ExtractedDoc`] per row (atoms are
/// produced separately by [`build_atoms`]).
pub struct TabularAtomsExtractor {
    pub config: TabularAtomsConfig,
}

impl Extractor for TabularAtomsExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let rows = parse_rows(source_path, &self.config.document_path)?;
        let cfg = self.config.clone();
        Ok(Box::new(
            rows.into_iter().map(move |row| Ok(row_to_doc(&row, &cfg))),
        ))
    }
}

/// Parse every row object out of the acquired JSON file(s) at
/// `source_path` (a single file or a directory of page files — the
/// `http_api` acquirer writes one `.json` per page), selecting elements
/// with `document_path`. Pure and deterministic.
pub fn parse_rows(source_path: &Path, document_path: &str) -> Result<Vec<Map<String, Value>>> {
    let jpath: JsonPath<Value> = JsonPath::try_from(document_path).map_err(|e| {
        Error::Extraction(format!(
            "extract.document_path `{document_path}` is not a valid JSONPath: {e}"
        ))
    })?;
    let mut rows = Vec::new();
    for path in collect_json_files(source_path)? {
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Extraction(format!("read {}: {e}", path.display())))?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Extraction(format!("{} is not valid JSON: {e}", path.display())))?;
        let matches = match jpath.find(&body) {
            Value::Array(arr) => arr,
            other => vec![other],
        };
        for m in matches {
            if let Value::Object(obj) = m {
                rows.push(obj);
            }
        }
    }
    Ok(rows)
}

/// Render one row into an `ExtractedDoc`. `source_id` is the id-column
/// value so the chunk's `source_doc_id` matches the atom's provenance
/// join key.
fn row_to_doc(row: &Map<String, Value>, cfg: &TabularAtomsConfig) -> ExtractedDoc {
    let id = row_id(row, cfg);
    let content = render_content(row, cfg, &id);
    ExtractedDoc {
        title: Some(format!("{} {id}", cfg.entity_type)),
        content,
        url: None,
        source_id: id,
        metadata: Some(Value::Object(row.clone())),
        source_file: None,
        embed_text: None,
    }
}

fn row_id(row: &Map<String, Value>, cfg: &TabularAtomsConfig) -> String {
    value_to_string(row.get(&cfg.id_column)).unwrap_or_default()
}

/// Deterministic, human-readable rendering: `"<entity_type> <id> — col:
/// value; …"` over the declared attributes (strings first, then
/// numerics), in declared order. This is the chunk body FTS indexes and
/// citations surface.
fn render_content(row: &Map<String, Value>, cfg: &TabularAtomsConfig, id: &str) -> String {
    let mut parts = Vec::new();
    for col in cfg
        .string_attributes
        .iter()
        .chain(cfg.numeric_attributes.iter())
    {
        if let Some(s) = value_to_string(row.get(col)) {
            if !s.is_empty() {
                parts.push(format!("{col}: {s}"));
            }
        }
    }
    format!("{} {id} — {}", cfg.entity_type, parts.join("; "))
}

/// Build one atlas [`Entity`] per row, typing the declared columns into
/// `Entity::attributes` (numbers as JSON numbers — Socrata serialises
/// cells as strings, which we parse; strings kept verbatim). `corpus_id`
/// makes the content-hash id stable across re-ingest. Pure: no I/O, no
/// inference.
pub fn build_atoms(
    rows: &[Map<String, Value>],
    cfg: &TabularAtomsConfig,
    corpus_id: &str,
) -> Vec<Entity> {
    let entity_type = EntityType::Other(cfg.entity_type.clone());
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id_str = row_id(row, cfg);
        if id_str.is_empty() {
            continue; // a row without the id column can't anchor a stable atom
        }
        let mut attributes = Map::new();
        for col in &cfg.numeric_attributes {
            if let Some(n) = coerce_number(row.get(col)) {
                attributes.insert(col.clone(), Value::from(n));
            }
        }
        for col in &cfg.string_attributes {
            if let Some(s) = value_to_string(row.get(col)) {
                if !s.is_empty() {
                    attributes.insert(col.clone(), Value::String(s));
                }
            }
        }
        let rendered = render_content(row, cfg, &id_str);
        out.push(Entity {
            id: AtomId::entity_content_hash(&id_str, &entity_type, corpus_id),
            canonical_name: id_str.clone(),
            aliases: Vec::new(),
            entity_type: entity_type.clone(),
            first_appearance: ChunkRef::new(id_str.clone(), Some(rendered.clone()))
                .with_source_doc(Some(id_str.clone())),
            description: rendered,
            defining_quote: None,
            salience: 0.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: SignalProvenance::new(
                "tabular_atoms",
                id_str.clone(),
                SignalKind::ColumnHeader,
            )
            .with_chunk(id_str),
            attributes,
            concept_kind: None,
        });
    }
    out
}

/// Coerce a JSON value to f64. Socrata serialises every cell as a string
/// (`"172620140416.0"`), so we parse numeric strings as well as honour
/// genuine JSON numbers. Non-numeric or absent → `None`.
fn coerce_number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn value_to_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TabularAtomsConfig {
        TabularAtomsConfig {
            document_path: "$[*]".to_string(),
            id_column: "parcel_number".to_string(),
            entity_type: "parcel".to_string(),
            numeric_attributes: vec![
                "assessed_land_value".to_string(),
                "assessed_improvement_value".to_string(),
            ],
            string_attributes: vec!["use_code".to_string(), "analysis_neighborhood".to_string()],
        }
    }

    fn rows() -> Vec<Map<String, Value>> {
        // Socrata-shaped: every cell is a string.
        let json = r#"[
          {"parcel_number":"0001001","assessed_land_value":"1000.0","assessed_improvement_value":"500.0","use_code":"COMM","analysis_neighborhood":"Russian Hill"},
          {"parcel_number":"0002001","assessed_land_value":"0.0","assessed_improvement_value":"0.0","use_code":"COMM","analysis_neighborhood":"Russian Hill"}
        ]"#;
        match serde_json::from_str(json).unwrap() {
            Value::Array(a) => a
                .into_iter()
                .filter_map(|v| match v {
                    Value::Object(o) => Some(o),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    #[test]
    fn build_atoms_types_numeric_strings_and_records_provenance() {
        let atoms = build_atoms(&rows(), &cfg(), "sf-assessor-roll");
        assert_eq!(atoms.len(), 2);
        let a = &atoms[0];
        assert_eq!(a.canonical_name, "0001001");
        assert_eq!(a.entity_type, EntityType::Other("parcel".to_string()));
        // Numeric columns parsed from strings into JSON numbers.
        assert_eq!(
            a.attributes
                .get("assessed_land_value")
                .and_then(|v| v.as_f64()),
            Some(1000.0)
        );
        assert_eq!(
            a.attributes
                .get("assessed_improvement_value")
                .and_then(|v| v.as_f64()),
            Some(500.0)
        );
        // String columns kept verbatim.
        assert_eq!(
            a.attributes.get("use_code").and_then(|v| v.as_str()),
            Some("COMM")
        );
        // Deterministic provenance — no inference signal.
        assert_eq!(a.provenance.extractor_id, "tabular_atoms");
        assert_eq!(a.provenance.signal_kind, SignalKind::ColumnHeader);
        assert_eq!(a.provenance.source_chunk_id.as_deref(), Some("0001001"));
        assert_eq!(a.first_appearance.source_doc_id.as_deref(), Some("0001001"));
    }

    #[test]
    fn build_atoms_id_is_stable_across_calls() {
        let a1 = build_atoms(&rows(), &cfg(), "sf-assessor-roll");
        let a2 = build_atoms(&rows(), &cfg(), "sf-assessor-roll");
        assert_eq!(
            a1[0].id, a2[0].id,
            "content-hash id must reproduce across re-ingest"
        );
        assert_ne!(a1[0].id, a1[1].id, "distinct parcels get distinct ids");
    }
}
