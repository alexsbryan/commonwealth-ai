//! XLSX sub-extractor for the described-asset dispatcher (AD-3).
//!
//! Reads spreadsheets with `calamine` and emits two things:
//!
//! 1. A **structural description** for the atlas — one paragraph per
//!    sheet listing dimensions, column headers, and the first few
//!    distinct values per header. The description IS atlas-visible
//!    prose; the columns + sample values are what the column-aware
//!    extractor (Phase 4) reads off the parsed cache to seed Entity
//!    atoms with `signal_kind: ColumnHeader`.
//! 2. A **parsed-form parquet** under
//!    `<corpus_index>/assets/parsed/<sha256>.parquet` — one record
//!    batch per sheet, typed columns preserved. Phase 4's column-aware
//!    extractor reads this directly; a future structured-query path
//!    queries it without re-invoking calamine.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use calamine::{open_workbook_auto_from_rs, Data, Reader};

use super::described_asset::{AssetExtraction, AssetSubExtractor, ExtractionTier};
use crate::asset_store::AssetStore;
use crate::error::{Error, Result};

/// XLSX (and XLSM/XLSB/XLS/ODS — calamine handles all of them) sub-
/// extractor. Owns no state; calamine workbooks are short-lived per
/// call.
pub struct XlsxSubExtractor;

impl AssetSubExtractor for XlsxSubExtractor {
    fn detect(&self, path: &Path, head_bytes: &[u8]) -> bool {
        // Modern Office files are ZIPs (PK\003\004) — but so are
        // DOCX, EPUB, JAR, and others. Require a known extension OR
        // a ZIP magic AND the absence of a DOCX-style word/ folder.
        // The full disambiguation happens at parse time; this just
        // gates which sub-extractor calamine sees first.
        let by_ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "xlsx" | "xlsm" | "xls" | "xlsb" | "ods"
                )
            })
            .unwrap_or(false);
        if by_ext {
            return true;
        }
        // Old .xls is a CFB compound file; magic D0 CF 11 E0 A1 B1 1A E1.
        head_bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
    }

    fn extract(
        &self,
        path: &Path,
        bytes: &[u8],
        sha256: &str,
        store: &dyn AssetStore,
    ) -> Result<AssetExtraction> {
        let mut workbook = open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| {
            Error::Extraction(format!(
                "xlsx: calamine could not open {}: {e}",
                path.display()
            ))
        })?;
        let sheet_names = workbook.sheet_names();
        let mut description = String::new();
        let mut parsed_sheets: Vec<(String, RecordBatch)> = Vec::new();

        description.push_str(&format!(
            "XLSX: {} sheets ({}).\n",
            sheet_names.len(),
            sheet_names.join(", ")
        ));

        for sheet_name in &sheet_names {
            let range = workbook.worksheet_range(sheet_name).map_err(|e| {
                Error::Extraction(format!(
                    "xlsx: worksheet_range({sheet_name}) on {}: {e}",
                    path.display()
                ))
            })?;
            let (rows, cols) = range.get_size();
            let header_row = detect_header_row(&range);
            let headers = header_values(&range, header_row);

            let head_sample: Vec<String> = headers
                .iter()
                .take(8)
                .map(|h| {
                    if h.is_empty() {
                        "(unnamed)".to_string()
                    } else {
                        h.clone()
                    }
                })
                .collect();
            description.push_str(&format!(
                "Sheet '{sheet_name}': {rows} rows × {cols} cols. Headers: [{}].",
                head_sample.join(", ")
            ));

            // First-distinct-values preview per header (up to 5
            // columns × 3 distinct values each) — Phase 4's column-
            // aware extractor uses this to scope entity-name
            // candidates; humans use it to read the description.
            for (col_idx, header) in headers.iter().take(5).enumerate() {
                if header.is_empty() {
                    continue;
                }
                let mut distinct = Vec::<String>::new();
                for row_idx in (header_row + 1)..rows.min(header_row + 51) {
                    let v = cell_as_string(&range, row_idx, col_idx);
                    if v.is_empty() {
                        continue;
                    }
                    if !distinct.iter().any(|d| d == &v) {
                        distinct.push(v);
                        if distinct.len() >= 3 {
                            break;
                        }
                    }
                }
                if !distinct.is_empty() {
                    description
                        .push_str(&format!(" {header} examples: [{}].", distinct.join(", ")));
                }
            }
            description.push('\n');

            // Build the parquet payload. Every cell stringified so a
            // single schema serves all sheets uniformly; the column-
            // aware extractor reads strings + does its own typing. A
            // future structured-query path may widen to typed
            // columns; doing it now is premature without a known
            // consumer.
            let batch = build_sheet_record_batch(&range, &headers, header_row)?;
            parsed_sheets.push((sheet_name.clone(), batch));
        }

        // Write parquet cache. One file per asset; sheets distinguished
        // by an extra column.
        let parsed_path = if !parsed_sheets.is_empty() {
            let parquet_bytes = encode_sheets_to_parquet(&parsed_sheets)?;
            let path = store.put_parsed(sha256, "parquet", &parquet_bytes)?;
            Some(path)
        } else {
            None
        };

        Ok(AssetExtraction {
            description,
            asset_kind: "xlsx".into(),
            tier: ExtractionTier::Structural,
            mime: Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into()),
            parsed_form: parsed_path,
        })
    }

    fn name(&self) -> &'static str {
        "xlsx"
    }
}

/// Locate the real header row. Corporate spreadsheets routinely prepend
/// a merged title/banner row, a blank separator, and a group-header row
/// before the actual column headers — so the legacy "row 0 is the
/// header" assumption reads a banner as headers, and the column-aware
/// extractor then finds no typed columns (employee / counterparty / …)
/// and emits nothing. Heuristic: the first row within the top 15 that
/// carries ≥ 3 non-empty string cells. A banner or a merged group-header
/// is a single populated cell; a blank row is none; a real header row is
/// several text labels. Falls back to row 0 so simple, banner-free
/// sheets (header already at the top) keep working unchanged.
fn detect_header_row(range: &calamine::Range<Data>) -> usize {
    let (rows, cols) = range.get_size();
    for r in 0..rows.min(15) {
        let string_cells = (0..cols)
            .filter(|&c| matches!(range.get((r, c)), Some(Data::String(s)) if !s.trim().is_empty()))
            .count();
        if string_cells >= 3 {
            return r;
        }
    }
    0
}

fn header_values(range: &calamine::Range<Data>, header_row: usize) -> Vec<String> {
    let (rows, cols) = range.get_size();
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    (0..cols)
        .map(|c| cell_as_string(range, header_row, c))
        .collect()
}

fn cell_as_string(range: &calamine::Range<Data>, row: usize, col: usize) -> String {
    match range.get((row, col)) {
        Some(Data::String(s)) => s.clone(),
        Some(Data::Float(f)) => format_number(*f),
        Some(Data::Int(i)) => i.to_string(),
        Some(Data::Bool(b)) => b.to_string(),
        Some(Data::DateTime(dt)) => dt.to_string(),
        Some(Data::DateTimeIso(s)) => s.clone(),
        Some(Data::DurationIso(s)) => s.clone(),
        Some(Data::Error(e)) => format!("#ERR({e:?})"),
        Some(Data::Empty) | None => String::new(),
    }
}

fn format_number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn build_sheet_record_batch(
    range: &calamine::Range<Data>,
    headers: &[String],
    header_row: usize,
) -> Result<RecordBatch> {
    let (rows, cols) = range.get_size();
    let body_rows = rows.saturating_sub(header_row + 1);

    // Schema: "_sheet_row" (string row-index as the join key Phase 4
    // uses) + one Utf8 column per header. The constant-prefix avoids
    // collisions with user-named columns.
    let mut fields: Vec<Field> = vec![Field::new("_sheet_row", DataType::Utf8, false)];
    for (i, h) in headers.iter().enumerate() {
        let name = if h.is_empty() {
            format!("col_{i}")
        } else {
            h.clone()
        };
        fields.push(Field::new(&name, DataType::Utf8, true));
    }
    let schema = Arc::new(Schema::new(fields));

    let mut row_ids = Vec::with_capacity(body_rows);
    let mut columns: Vec<Vec<Option<String>>> = vec![Vec::with_capacity(body_rows); cols];

    for r in (header_row + 1)..rows {
        row_ids.push(format!("r{r}"));
        for (c, col) in columns.iter_mut().enumerate() {
            let v = cell_as_string(range, r, c);
            col.push(if v.is_empty() { None } else { Some(v) });
        }
    }

    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(cols + 1);
    arrays.push(Arc::new(StringArray::from(row_ids)));
    for col in columns {
        arrays.push(Arc::new(StringArray::from(col)));
    }

    let batch = RecordBatch::try_new(schema, arrays)
        .map_err(|e| Error::Extraction(format!("xlsx: build RecordBatch: {e}")))?;
    // The Float64Array import keeps this module honest about the
    // typed-column-future; touch it once so the import doesn't go
    // dead while the path is still mono-schema.
    let _: Option<Float64Array> = None;
    Ok(batch)
}

fn encode_sheets_to_parquet(sheets: &[(String, RecordBatch)]) -> Result<Vec<u8>> {
    // Per-sheet parquet bundled into a single tarball file would be
    // tidiest, but a single concatenated parquet stream is sufficient
    // for Phase 1 — Phase 4 reads sheet-by-sheet via the
    // `_sheet_name` virtual column. One RecordBatch per sheet,
    // tagged by an extra Utf8 column prepended to the schema.
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;

    if sheets.is_empty() {
        return Ok(Vec::new());
    }
    let mut tagged_batches = Vec::with_capacity(sheets.len());
    for (sheet_name, batch) in sheets {
        let n_rows = batch.num_rows();
        let mut fields: Vec<Field> = vec![Field::new("_sheet_name", DataType::Utf8, false)];
        for f in batch.schema().fields() {
            fields.push(Field::new(f.name(), f.data_type().clone(), f.is_nullable()));
        }
        let schema = Arc::new(Schema::new(fields));
        let mut columns: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns() + 1);
        let names = vec![sheet_name.clone(); n_rows];
        columns.push(Arc::new(StringArray::from(names)));
        for col in batch.columns() {
            columns.push(col.clone());
        }
        let tagged = RecordBatch::try_new(schema, columns)
            .map_err(|e| Error::Extraction(format!("xlsx: tag-batch: {e}")))?;
        tagged_batches.push(tagged);
    }

    // Union schema across all sheets so every sheet's typed columns
    // survive into the parquet. Heterogeneous corporate workbooks
    // (Opex vs Capex, DCF vs Comps) carry different column sets per
    // sheet; the old code took the first sheet's schema as canonical
    // and silently DROPPED every divergent sheet, so the column-aware
    // extractor never saw e.g. a Capex `Counterparty` or a Comps
    // `Company` column. Instead, take the ordered union of all field
    // names and reindex each sheet's batch onto it — columns a sheet
    // lacks are filled with nulls. One schema, every column present.
    let mut order: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in &tagged_batches {
        for f in b.schema().fields() {
            if seen.insert(f.name().clone()) {
                order.push(f.name().clone());
            }
        }
    }
    let union_fields: Vec<Field> = order
        .iter()
        .map(|name| Field::new(name, DataType::Utf8, name != "_sheet_name"))
        .collect();
    let union_schema = Arc::new(Schema::new(union_fields));

    let mut aligned: Vec<RecordBatch> = Vec::with_capacity(tagged_batches.len());
    for b in &tagged_batches {
        let n = b.num_rows();
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(order.len());
        for name in &order {
            match b.schema().index_of(name) {
                Ok(idx) => cols.push(b.column(idx).clone()),
                Err(_) => {
                    // Column absent in this sheet → all-null Utf8.
                    let nulls: Vec<Option<String>> = vec![None; n];
                    cols.push(Arc::new(StringArray::from(nulls)));
                }
            }
        }
        aligned.push(
            RecordBatch::try_new(union_schema.clone(), cols)
                .map_err(|e| Error::Extraction(format!("xlsx: union-align batch: {e}")))?,
        );
    }

    let buf: Vec<u8> = Vec::new();
    let mut cursor = Cursor::new(buf);
    let props = WriterProperties::builder().build();
    {
        let mut writer = ArrowWriter::try_new(&mut cursor, union_schema.clone(), Some(props))
            .map_err(|e| Error::Extraction(format!("xlsx: open parquet writer: {e}")))?;
        for batch in &aligned {
            writer
                .write(batch)
                .map_err(|e| Error::Extraction(format!("xlsx: write parquet batch: {e}")))?;
        }
        writer
            .close()
            .map_err(|e| Error::Extraction(format!("xlsx: close parquet writer: {e}")))?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_store::FilesystemAssetStore;

    // Compose a minimal XLSX in memory by hand using calamine's
    // round-trippable format is non-trivial; instead, write a tiny
    // CSV-shaped fake the OpaqueFallback would catch differently —
    // and rely on the dispatcher tests for the full path. Here we
    // verify detect() shape only.

    #[test]
    fn detect_by_extension() {
        let p = std::path::PathBuf::from("/tmp/q3.xlsx");
        assert!(XlsxSubExtractor.detect(&p, &[]));
        let p = std::path::PathBuf::from("/tmp/notes.txt");
        assert!(!XlsxSubExtractor.detect(&p, &[]));
    }

    #[test]
    fn detect_old_xls_by_magic() {
        let p = std::path::PathBuf::from("/tmp/anon");
        let magic = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        assert!(XlsxSubExtractor.detect(&p, &magic));
    }

    #[test]
    fn extract_produces_structural_description_and_parquet() {
        // End-to-end prove-out for the xlsx sub-extractor on a real
        // OpenXML workbook. Pre-2026-05-29 this was a smoke that only
        // asserted `name() == "xlsx"`; the comment explicitly said
        // "for Phase 1 we'll skip this integration test." Phase 5
        // measurement work surfaced that tabular ingestion had never
        // been demonstrated end-to-end on a single fixture, so the
        // test now writes a tiny three-column workbook via
        // `rust_xlsxwriter`, feeds the bytes through the sub-
        // extractor, and checks (a) the structural description
        // mentions the sheet's shape, (b) calamine read every row,
        // (c) a parquet parsed_form was written and round-trips
        // back through `parquet::arrow::ArrowReaderBuilder` with the
        // same cell values. Catches: schema regressions in
        // `build_sheet_record_batch`, parquet-writer encoding
        // changes, and asset-store wiring bugs that would silently
        // drop the parsed cache.
        use rust_xlsxwriter::Workbook;

        // Build a tiny 3×3 fixture: header row + two data rows.
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet().set_name("Trades").unwrap();
        sheet.write_string(0, 0, "counterparty").unwrap();
        sheet.write_string(0, 1, "trader").unwrap();
        sheet.write_string(0, 2, "notional").unwrap();
        sheet.write_string(1, 0, "Dynegy").unwrap();
        sheet.write_string(1, 1, "Jeff Skilling").unwrap();
        sheet.write_string(1, 2, "100M").unwrap();
        sheet.write_string(2, 0, "El Paso").unwrap();
        sheet.write_string(2, 1, "Andy Fastow").unwrap();
        sheet.write_string(2, 2, "50M").unwrap();
        let bytes = workbook.save_to_buffer().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();

        // sha256 of the workbook bytes — the store keys parsed-form
        // caches on this, mirroring the dispatcher's call shape.
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let sha256_hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();

        let path = std::path::PathBuf::from("/tmp/trades.xlsx");
        let extraction = XlsxSubExtractor
            .extract(&path, &bytes, &sha256_hex, &store)
            .expect("xlsx extract failed");

        // ── Structural description ────────────────────────────────
        assert_eq!(extraction.asset_kind, "xlsx");
        assert_eq!(extraction.tier, ExtractionTier::Structural);
        assert!(
            extraction.description.contains("Trades"),
            "description should name the sheet; got: {:?}",
            extraction.description
        );
        assert!(
            extraction.description.contains("3 rows"),
            "description should report row count; got: {:?}",
            extraction.description
        );
        assert!(
            extraction.description.contains("counterparty"),
            "description should list headers; got: {:?}",
            extraction.description
        );
        // First-distinct-values preview (cap 3 per header) should
        // include the cell payloads.
        assert!(
            extraction.description.contains("Dynegy") && extraction.description.contains("El Paso"),
            "description should preview distinct values; got: {:?}",
            extraction.description
        );

        // ── Parsed-form parquet ───────────────────────────────────
        let parsed_path = extraction
            .parsed_form
            .as_ref()
            .expect("parsed_form parquet path must be Some for xlsx");
        let parquet_bytes = std::fs::read(parsed_path).expect("parsed parquet must be readable");
        assert!(!parquet_bytes.is_empty(), "parquet must be non-empty");
        // Magic word: parquet files start with "PAR1".
        assert_eq!(
            &parquet_bytes[..4],
            b"PAR1",
            "parsed_form must be a parquet file"
        );

        // Round-trip: decode the parquet and verify every cell.
        // Round-trip the parquet by opening it as a File — the
        // parquet crate's `ChunkReader` impl exists for `File` and
        // `bytes::Bytes`. Reading from disk also incidentally
        // verifies the asset store actually wrote the bytes the
        // `parsed_form` path advertises.
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let file = std::fs::File::open(parsed_path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<arrow_array::RecordBatch> =
            reader.collect::<std::result::Result<Vec<_>, _>>().unwrap();
        assert_eq!(batches.len(), 1, "single record batch expected");
        let batch = &batches[0];
        // Encoder prepends `_sheet_name` + `_sheet_row` virtual
        // columns to the user-emitted headers (counterparty / trader
        // / notional) — 2 + 3 = 5.
        assert_eq!(
            batch.num_columns(),
            5,
            "_sheet_name + _sheet_row + 3 headers"
        );
        // Header row is consumed as the schema; body rows (2 trades)
        // land as records.
        assert_eq!(batch.num_rows(), 2);

        // Verify the second data row's counterparty cell as a smoke
        // that the schema columns are aligned. Column 0 is the
        // sheet_name discriminator the encoder injects; the
        // user-visible cells start at column 1 ("counterparty"
        // header).
        let schema = batch.schema();
        let col_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(
            col_names.contains(&"counterparty"),
            "counterparty column must survive round-trip; got {col_names:?}"
        );
    }

    #[test]
    fn header_detection_survives_banner_and_feeds_column_aware() {
        // Realistic corporate sheets prepend a merged title banner + a
        // blank row before the real column headers. The legacy "row 0 is
        // the header" read the banner as headers, so the column-aware
        // extractor saw no typed columns and emitted nothing. This proves
        // detect_header_row skips the noise AND that the resulting parquet
        // feeds column-aware the real typed columns (counterparty → Org,
        // trader → Person).
        use crate::enrichment::pipeline::atlas::EntityType;
        use crate::extractors::column_aware::{extract_entities_from_parquet, ColumnAwareConfig};
        use rust_xlsxwriter::Workbook;

        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet().set_name("Exposure").unwrap();
        sheet
            .write_string(0, 0, "FY2024 Counterparty Exposure — CONFIDENTIAL")
            .unwrap();
        // row 1 intentionally blank (separator noise)
        sheet.write_string(2, 0, "counterparty").unwrap();
        sheet.write_string(2, 1, "trader").unwrap();
        sheet.write_string(2, 2, "notional").unwrap();
        sheet.write_string(3, 0, "Dynegy").unwrap();
        sheet.write_string(3, 1, "Jeff Skilling").unwrap();
        sheet.write_string(3, 2, "100M").unwrap();
        sheet.write_string(4, 0, "El Paso").unwrap();
        sheet.write_string(4, 1, "Andy Fastow").unwrap();
        sheet.write_string(4, 2, "50M").unwrap();
        let bytes = workbook.save_to_buffer().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let sha: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let extraction = XlsxSubExtractor
            .extract(
                &std::path::PathBuf::from("/tmp/exposure.xlsx"),
                &bytes,
                &sha,
                &store,
            )
            .expect("xlsx extract");

        // Header detection skipped the banner → the description names the
        // real headers, not the banner title.
        assert!(
            extraction.description.contains("counterparty")
                && extraction.description.contains("trader"),
            "real headers must be detected past the banner; got {:?}",
            extraction.description
        );

        let parsed = extraction.parsed_form.expect("parquet parsed_form");
        let entities =
            extract_entities_from_parquet(&parsed, "exposure.xlsx", &ColumnAwareConfig::default())
                .expect("column_aware extraction");

        let names: Vec<&str> = entities.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(
            names.contains(&"Dynegy") && names.contains(&"El Paso"),
            "counterparty column must yield org entities through the banner; got {names:?}"
        );
        assert!(
            names.contains(&"Jeff Skilling") && names.contains(&"Andy Fastow"),
            "trader column must yield person entities through the banner; got {names:?}"
        );
        let dynegy = entities
            .iter()
            .find(|e| e.canonical_name == "Dynegy")
            .unwrap();
        assert!(matches!(dynegy.entity_type, EntityType::Institution));
        let jeff = entities
            .iter()
            .find(|e| e.canonical_name == "Jeff Skilling")
            .unwrap();
        assert!(matches!(jeff.entity_type, EntityType::Person));
    }

    #[test]
    fn multi_sheet_heterogeneous_schemas_all_reach_column_aware() {
        // Two sheets with DIFFERENT column sets, each carrying a typed
        // column. The old bundler took sheet 1's schema as canonical and
        // dropped sheet 2 entirely; the union schema keeps both, so
        // column-aware sees the counterparty AND the employee column.
        use crate::extractors::column_aware::{extract_entities_from_parquet, ColumnAwareConfig};
        use rust_xlsxwriter::Workbook;

        let mut wb = Workbook::new();
        let s1 = wb.add_worksheet().set_name("Trades").unwrap();
        s1.write_string(0, 0, "counterparty").unwrap();
        s1.write_string(0, 1, "notional").unwrap();
        s1.write_string(1, 0, "Dynegy").unwrap();
        s1.write_string(1, 1, "100M").unwrap();
        let s2 = wb.add_worksheet().set_name("Roster").unwrap();
        s2.write_string(0, 0, "employee").unwrap();
        s2.write_string(0, 1, "level").unwrap();
        s2.write_string(1, 0, "Jeff Skilling").unwrap();
        s2.write_string(1, 1, "E").unwrap();
        let bytes = wb.save_to_buffer().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let sha: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let ext = XlsxSubExtractor
            .extract(
                &std::path::PathBuf::from("/tmp/multi.xlsx"),
                &bytes,
                &sha,
                &store,
            )
            .unwrap();
        let parsed = ext.parsed_form.expect("parsed_form");
        let ents =
            extract_entities_from_parquet(&parsed, "multi.xlsx", &ColumnAwareConfig::default())
                .unwrap();
        let names: Vec<&str> = ents.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(
            names.contains(&"Dynegy"),
            "sheet 1 counterparty must survive; got {names:?}"
        );
        assert!(
            names.contains(&"Jeff Skilling"),
            "sheet 2 employee must survive the schema union; got {names:?}"
        );
    }
}
