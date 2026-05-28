//! Column-aware extractor (Phase 4 of the architecture-over-Enron push).
//!
//! Reads an [`Atom::Asset`](crate::enrichment::atlas::atoms::Asset)
//! with `asset_kind = "xlsx"` via its **`parsed_form` parquet cache**
//! — no re-parsing of the raw bytes via calamine — and emits
//! [`Entity`](crate::enrichment::atlas::atoms::Entity) atoms with
//! `Provenance { signal_kind: ColumnHeader, ... }` so the multi-origin
//! merger can fold them with their email-body cousins.
//!
//! Column-header semantics — "Employee", "Counterparty", "Customer" —
//! become entity-type hints structurally (no LLM). The pattern
//! generalises per asset kind in future verticals (calendar ATTENDEE →
//! Person, transactions counterparty → Organization). The
//! described-asset substrate (Phase 1, AD-3) is what those future
//! verticals plug into; this module is the tabular instance.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use arrow::array::{Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::{
    AtomId, ChunkRef, Entity, Provenance, SignalKind,
};
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use crate::error::{Error, Result};

/// Per-header → entity-type hint. The default map covers the headers
/// most likely to appear in Enron-style finance/operations
/// spreadsheets; recipe-authors extend it via the
/// `[enrichment.reconciliation.column_aware]` TOML block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnHeaderMap {
    pub person_headers: Vec<String>,
    pub organization_headers: Vec<String>,
    pub place_headers: Vec<String>,
}

impl Default for ColumnHeaderMap {
    fn default() -> Self {
        Self {
            person_headers: vec![
                "employee".into(),
                "person".into(),
                "name".into(),
                "trader".into(),
                "owner".into(),
                "contact".into(),
                "attendee".into(),
            ],
            organization_headers: vec![
                "counterparty".into(),
                "customer".into(),
                "vendor".into(),
                "supplier".into(),
                "company".into(),
                "organization".into(),
                "client".into(),
                "broker".into(),
            ],
            place_headers: vec![
                "city".into(),
                "state".into(),
                "country".into(),
                "location".into(),
            ],
        }
    }
}

impl ColumnHeaderMap {
    /// Classify a column header to an [`EntityType`]. Returns `None`
    /// when no rule fires — the caller should skip the column rather
    /// than guess.
    pub fn classify(&self, header: &str) -> Option<EntityType> {
        let h = header.trim().to_ascii_lowercase();
        if self.person_headers.iter().any(|p| h == *p || h.contains(p)) {
            return Some(EntityType::Person);
        }
        if self
            .organization_headers
            .iter()
            .any(|p| h == *p || h.contains(p))
        {
            return Some(EntityType::Institution);
        }
        if self.place_headers.iter().any(|p| h == *p || h.contains(p)) {
            return Some(EntityType::Place);
        }
        None
    }
}

/// Configuration the recipe's `[enrichment.reconciliation]` block
/// passes through.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnAwareConfig {
    #[serde(default)]
    pub column_headers: ColumnHeaderMap,
    /// Maximum entities to emit per sheet × header pair. Caps the
    /// noise on long lists (e.g. a 30k-row counterparty ledger).
    /// Zero = no cap.
    #[serde(default)]
    pub max_entities_per_column: usize,
}

/// Run the column-aware extraction over a parsed XLSX parquet cache.
/// `source_doc_id` is what the asset store's ledger recorded as the
/// first-seen document — typically the original filename + the
/// content-hash short id; threaded through onto each emitted
/// Entity's [`Provenance`].
pub fn extract_entities_from_parquet(
    parsed_form_path: &Path,
    source_doc_id: &str,
    config: &ColumnAwareConfig,
) -> Result<Vec<Entity>> {
    let file = File::open(parsed_form_path).map_err(|e| {
        Error::Extraction(format!(
            "column_aware: open parsed-form {}: {e}",
            parsed_form_path.display()
        ))
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
        Error::Extraction(format!(
            "column_aware: parquet open {}: {e}",
            parsed_form_path.display()
        ))
    })?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(|e| {
        Error::Extraction(format!("column_aware: parquet build: {e}"))
    })?;

    // Map each column index to its (name, EntityType-hint).
    let mut header_hints: BTreeMap<usize, (String, EntityType)> = BTreeMap::new();
    for (idx, field) in schema.fields().iter().enumerate() {
        let name = field.name();
        if name.starts_with('_') {
            continue;
        }
        if let Some(ty) = config.column_headers.classify(name) {
            header_hints.insert(idx, (name.to_string(), ty));
        }
    }
    if header_hints.is_empty() {
        return Ok(Vec::new());
    }

    // For each row, for each interesting column, dedup surface forms
    // by lowercase fold within the file.
    let mut emitted: BTreeMap<String, Entity> = BTreeMap::new();
    let mut entity_counter: u32 = 0;
    let mut per_column_count: BTreeMap<usize, usize> = BTreeMap::new();

    for batch_result in reader {
        let batch = batch_result.map_err(|e| {
            Error::Extraction(format!("column_aware: parquet batch: {e}"))
        })?;
        for (col_idx, (header_name, et)) in &header_hints {
            let Some(col) = batch.column(*col_idx).as_any().downcast_ref::<StringArray>()
            else {
                // Schema is uniform across sheets — Utf8 only. If a
                // future schema introduces typed columns, this code
                // path simply skips them. Phase 5 may revisit.
                continue;
            };
            let cap = config.max_entities_per_column;
            for row in 0..batch.num_rows() {
                if cap > 0 {
                    let cur = per_column_count.get(col_idx).copied().unwrap_or(0);
                    if cur >= cap {
                        break;
                    }
                }
                if col.is_null(row) {
                    continue;
                }
                let value = col.value(row).trim();
                if value.is_empty() {
                    continue;
                }
                let key = format!(
                    "{}|{}",
                    et.as_str_repr(),
                    value.to_ascii_lowercase().trim()
                );
                if emitted.contains_key(&key) {
                    continue;
                }
                entity_counter += 1;
                let id = AtomId::from_raw(format!("entity-col-{entity_counter:06}"));
                let entity = Entity {
                    id,
                    canonical_name: value.to_string(),
                    aliases: Vec::new(),
                    entity_type: et.clone(),
                    first_appearance: ChunkRef::new(header_name.clone(), None),
                    description: format!(
                        "Column-aware extraction from `{header_name}`."
                    ),
                    defining_quote: None,
                    salience: 0.4,
                    enrichment_depth: EnrichmentDepth::Extracted,
                    affiliation: None,
                    role: None,
                    participants: Vec::new(),
                    provenance: Provenance::new(
                        "column_aware",
                        source_doc_id,
                        SignalKind::ColumnHeader,
                    ),
                    concept_kind: None,
                };
                emitted.insert(key, entity);
                *per_column_count.entry(*col_idx).or_insert(0) += 1;
            }
        }
    }

    Ok(emitted.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_xlsx(headers: &[&str], rows: &[Vec<&str>]) -> Vec<u8> {
        // Build a minimal in-memory xlsx via the calamine
        // round-trip-ability is non-trivial. Instead, use
        // `rust_xlsxwriter` if it's in deps. The simpler path: write a
        // CSV and let the dispatcher's plaintext path handle it. But
        // column_aware specifically requires parquet, so we ship a
        // tiny fake parquet directly.
        use arrow::array::StringArray;
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::io::Cursor;
        use std::sync::Arc as StdArc;

        let mut fields: Vec<Field> = vec![Field::new("_sheet_name", DataType::Utf8, false)];
        fields.push(Field::new("_sheet_row", DataType::Utf8, false));
        for h in headers {
            fields.push(Field::new(*h, DataType::Utf8, true));
        }
        let schema = StdArc::new(Schema::new(fields));
        let n = rows.len();
        let sheet_names = StringArray::from(vec!["Sheet1"; n]);
        let row_ids: Vec<String> = (0..n).map(|i| format!("r{i}")).collect();
        let row_id_arr = StringArray::from(row_ids);
        let mut columns: Vec<arrow::array::ArrayRef> =
            vec![StdArc::new(sheet_names), StdArc::new(row_id_arr)];
        for (col_idx, _) in headers.iter().enumerate() {
            let col_values: Vec<Option<String>> = rows
                .iter()
                .map(|r| r.get(col_idx).map(|s| s.to_string()))
                .collect();
            columns.push(StdArc::new(StringArray::from(col_values)));
        }
        let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
        let buf: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(buf);
        {
            let mut writer = ArrowWriter::try_new(&mut cursor, schema, None).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn classifies_known_headers() {
        let map = ColumnHeaderMap::default();
        assert!(matches!(
            map.classify("Counterparty"),
            Some(EntityType::Institution)
        ));
        assert!(matches!(map.classify("Employee"), Some(EntityType::Person)));
        assert!(matches!(map.classify("State"), Some(EntityType::Place)));
        assert!(map.classify("Notes").is_none());
    }

    #[test]
    fn extracts_unique_entities_per_typed_column() {
        let dir = tempfile::tempdir().unwrap();
        let parquet_bytes = build_test_xlsx(
            &["Counterparty", "Trader", "Notes"],
            &[
                vec!["Dynegy", "Ken Lay", "first row"],
                vec!["El Paso", "Ken Lay", "second row"],
                vec!["Dynegy", "Jeff Skilling", "third row"],
                vec!["", "", ""],
            ],
        );
        let path = dir.path().join("test.parquet");
        std::fs::write(&path, &parquet_bytes).unwrap();
        let entities = extract_entities_from_parquet(
            &path,
            "spread:fixture",
            &ColumnAwareConfig::default(),
        )
        .unwrap();
        let names: std::collections::BTreeSet<_> =
            entities.iter().map(|e| e.canonical_name.clone()).collect();
        assert!(names.contains("Dynegy"));
        assert!(names.contains("El Paso"));
        assert!(names.contains("Ken Lay"));
        assert!(names.contains("Jeff Skilling"));
        // "Notes" is not a classified header — content there must not
        // produce entities.
        assert!(!names.contains("first row"));
        // Provenance carries column_aware + ColumnHeader.
        for e in &entities {
            assert_eq!(e.provenance.extractor_id, "column_aware");
            assert_eq!(e.provenance.source_doc_id, "spread:fixture");
            assert!(matches!(e.provenance.signal_kind, SignalKind::ColumnHeader));
        }
    }

    #[test]
    fn typed_columns_route_to_appropriate_entity_type() {
        let dir = tempfile::tempdir().unwrap();
        let parquet_bytes = build_test_xlsx(
            &["Counterparty", "Employee"],
            &[vec!["Dynegy", "Ken Lay"]],
        );
        let path = dir.path().join("test.parquet");
        std::fs::write(&path, &parquet_bytes).unwrap();
        let entities = extract_entities_from_parquet(
            &path,
            "spread:fixture",
            &ColumnAwareConfig::default(),
        )
        .unwrap();
        let dyn_entity = entities
            .iter()
            .find(|e| e.canonical_name == "Dynegy")
            .unwrap();
        assert!(matches!(dyn_entity.entity_type, EntityType::Institution));
        let ken_entity = entities
            .iter()
            .find(|e| e.canonical_name == "Ken Lay")
            .unwrap();
        assert!(matches!(ken_entity.entity_type, EntityType::Person));
    }

    #[test]
    fn empty_column_returns_no_entities() {
        let dir = tempfile::tempdir().unwrap();
        let parquet_bytes = build_test_xlsx(&["Notes"], &[vec!["row1"], vec!["row2"]]);
        let path = dir.path().join("test.parquet");
        std::fs::write(&path, &parquet_bytes).unwrap();
        let entities = extract_entities_from_parquet(
            &path,
            "spread:fixture",
            &ColumnAwareConfig::default(),
        )
        .unwrap();
        assert!(entities.is_empty());
    }
}
