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

use arrow::array::{Array, RecordBatch, StringArray};
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
                // Person-bearing roles common in org charts, budgets,
                // and rosters — the cell value is a human's name. The
                // classifier substring-matches, so "Engineering
                // Manager" / "Reports To" / "Project Sponsor" all hit.
                "manager".into(),
                "reports to".into(),
                "reports_to".into(),
                "sponsor".into(),
                "supervisor".into(),
                "approver".into(),
                "requestor".into(),
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

// ── Embedding-centroid header classifier (the generalizing path) ──
//
// Keyword/substring header matching ([`ColumnHeaderMap::classify`])
// works in-sample but cannot cover the unbounded space of real headers
// ("Cpty", "Resp. Party", "Beneficial Owner", "DRI"). Mirroring the
// repo's gold-standard ambiguous-routing pattern
// (`sovereign-core::scope_classifier`), this classifies a column by the
// cosine of its embedding against per-class centroids built from a
// small, diverse, corpus-disjoint exemplar set — with an absolute
// (`min_sim`) gate so signal-less columns abstain and a `margin` gate so
// borderline columns don't flip on embedding noise.
//
// **Classify on `header + sample values`, not the header alone.** Terse
// headers embed weakly; the column's *values* carry the robust signal —
// "Cpty: Dynegy, El Paso, Williams" lands squarely near the org
// centroid even though "Cpty" alone is cryptic. The caller composes the
// string (see `column_signal`).

/// Exemplars are deliberately disjoint from any benchmarked corpus
/// (fictional names / companies / places) so the centroids encode the
/// *shape* of the class, not memorised gold.
const PERSON_EXEMPLARS: &[&str] = &[
    "employee: Maria Alvarez, Wei Tanaka, Tomas Becker",
    "full name of the responsible person",
    "point of contact: Jane Okafor",
    "manager",
    "owner: Priyanka Rao",
    "assigned individual",
    "staff member",
    "approver: Liam O'Sullivan",
];
const ORG_EXEMPLARS: &[&str] = &[
    "vendor: Brightway Corporation, Umbra Systems, Initech LLC",
    "company name",
    "counterparty: Northgate Trading, Brightwater Partners",
    "legal entity",
    "supplier firm",
    "client organization",
    "issuer: Vandelay Industries",
    "institution",
];
const PLACE_EXEMPLARS: &[&str] = &[
    "city: London, Tokyo, São Paulo",
    "country",
    "office location: Berlin, Singapore",
    "region",
    "site address",
];
/// Negative / abstain class. Columns whose `header + values` land
/// nearest this centroid are NOT entity columns and abstain — the
/// embed-router way to say "this isn't a person/org/place column"
/// without enumerating noise headers by keyword. Catches the residual
/// status / category / level / title / amount / date / line-item
/// columns that otherwise leak as low-margin entity hits.
const NON_ENTITY_EXEMPLARS: &[&str] = &[
    "status: active, archived, draft",
    "category: bronze, silver, platinum",
    "type",
    "level: senior, junior",
    "priority",
    "amount in dollars: 1200, 4500",
    "total",
    "date: 2024-01-15, 2023-09-02",
    "quarter",
    "fiscal year",
    "line item: utilities, postage, insurance",
    "department: legal, marketing, facilities",
    "job title: analyst, director",
    "ticker symbol: AAPL, MSFT",
    "percentage",
    "notes",
    "description",
    "rating",
];

const HEADER_MIN_SIM: f32 = 0.34;
// Margin is the real discriminator: genuine entity columns commit to one
// class (margin ≥ 0.08), while non-entity columns (Status, dates,
// financial-row labels) sit ambiguously between classes. 0.05 tuned on
// the corp-sheets harness — cuts the egregious noise (dates, Status,
// EV/EBITDA, "Line ($mm)") while holding gold coverage at 44/45,
// including email columns. Residual Category/Location/footnote noise is
// value-shape, handled separately. Env-overridable via HEADER_MIN_MARGIN.
const HEADER_MIN_MARGIN: f32 = 0.05;

/// Number of distinct sample values to fold into a column's signal.
pub const COLUMN_SIGNAL_SAMPLES: usize = 5;

/// Centroid-of-embeddings classifier for column headers. One centroid
/// per [`EntityType`] (Person / Institution / Place), built at runtime
/// from [`PERSON_EXEMPLARS`] etc. via the caller's [`EmbedFn`].
pub struct HeaderClassifier {
    /// `None` tags the non-entity / abstain centroid; `Some(t)` an
    /// entity class. A column whose nearest centroid is the abstain one
    /// is classified as "not an entity column".
    centroids: Vec<(Option<EntityType>, Vec<f32>)>,
    min_sim: f32,
    min_margin: f32,
}

impl HeaderClassifier {
    /// Build the per-class centroids by embedding the exemplar sets.
    /// Sequential — the exemplar counts are tiny and the embed slot
    /// serialises anyway.
    pub async fn build(embed: &crate::types::EmbedFn) -> Result<Self> {
        let centroids = vec![
            (Some(EntityType::Person), centroid(PERSON_EXEMPLARS, embed).await?),
            (Some(EntityType::Institution), centroid(ORG_EXEMPLARS, embed).await?),
            (Some(EntityType::Place), centroid(PLACE_EXEMPLARS, embed).await?),
            // Abstain class: a column nearest this is not an entity column.
            (None, centroid(NON_ENTITY_EXEMPLARS, embed).await?),
        ];
        // Gates are env-tunable (HEADER_MIN_SIM / HEADER_MIN_MARGIN) so
        // the abstain thresholds can be swept against a harness without
        // recompiling — the empirical step that decides which non-entity
        // columns (Status, Category, dates) correctly abstain.
        let min_sim = env_f32("HEADER_MIN_SIM", HEADER_MIN_SIM);
        let min_margin = env_f32("HEADER_MIN_MARGIN", HEADER_MIN_MARGIN);
        Ok(Self {
            centroids,
            min_sim,
            min_margin,
        })
    }

    /// Top class + its similarity + its margin over the runner-up,
    /// ignoring the gates. For logging / threshold tuning.
    pub fn best(&self, emb: &[f32]) -> Option<(Option<EntityType>, f32, f32)> {
        if self.centroids.is_empty() || emb.len() != self.centroids[0].1.len() {
            return None;
        }
        let mut scored: Vec<(Option<EntityType>, f32)> = self
            .centroids
            .iter()
            .map(|(t, c)| (t.clone(), dot(emb, c)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (best_t, best) = scored[0].clone();
        let second = scored.get(1).map(|x| x.1).unwrap_or(0.0);
        Some((best_t, best, best - second))
    }

    pub fn with_gates(mut self, min_sim: f32, min_margin: f32) -> Self {
        self.min_sim = min_sim;
        self.min_margin = min_margin;
        self
    }

    /// Compose a column's classification signal: `header + sample
    /// values`. The values do the heavy lifting for cryptic headers.
    pub fn column_signal(header: &str, sample_values: &[String]) -> String {
        if sample_values.is_empty() {
            header.to_string()
        } else {
            format!("{header}: {}", sample_values.join(", "))
        }
    }

    /// Classify a pre-normalised column-signal embedding. Returns the
    /// nearest class only when it clears both the absolute and margin
    /// gates; otherwise `None` (abstain — the column is skipped).
    pub fn classify(&self, signal_emb_normalized: &[f32]) -> Option<EntityType> {
        let (top, sim, margin) = self.best(signal_emb_normalized)?;
        // Nearest centroid is the abstain class → not an entity column.
        let top = top?;
        (sim >= self.min_sim && margin >= self.min_margin).then_some(top)
    }
}

/// True if a cell value plausibly *is* an entity name, by shape alone
/// (no keyword lists). Rejects footnote/banner prose (too long) and
/// numeric / date / currency / multiplier cells (digit-dominant or no
/// letters). Entity names are short and letter-dominant; "Marcus Webb",
/// "El Paso Corp.", "AWS" pass, while "* Annual spend in $000s…",
/// "2019-02-01", "11.2x", "100M", "1,850" are dropped.
fn is_name_shaped(v: &str) -> bool {
    if v.chars().count() > 64 || v.split_whitespace().count() > 6 {
        return false; // prose / footnote
    }
    let alpha = v.chars().filter(|c| c.is_alphabetic()).count();
    let digit = v.chars().filter(|c| c.is_ascii_digit()).count();
    alpha > 0 && digit <= alpha
}

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

async fn centroid(exemplars: &[&str], embed: &crate::types::EmbedFn) -> Result<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for ex in exemplars {
        let mut e = (embed)(ex).await?;
        l2_normalize(&mut e);
        match sum.as_mut() {
            Some(s) if s.len() == e.len() => {
                for (i, v) in e.into_iter().enumerate() {
                    s[i] += v;
                }
            }
            Some(s) => {
                return Err(Error::Extraction(format!(
                    "header centroid: embedding dim mismatch {} vs {}",
                    s.len(),
                    e.len()
                )))
            }
            None => sum = Some(e),
        }
    }
    let mut c =
        sum.ok_or_else(|| Error::Extraction("header centroid: empty exemplar set".into()))?;
    l2_normalize(&mut c);
    Ok(c)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

pub(crate) fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
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
    let batches = read_parquet_batches(parsed_form_path)?;
    // Keyword-map header hints — the no-embed fallback path.
    let header_hints = resolve_keyword_hints(&batches, &config.column_headers);
    Ok(emit_entities_from_batches(
        &batches,
        source_doc_id,
        &header_hints,
        config.max_entities_per_column,
    ))
}

/// Embedding-centroid variant. Classifies each column by the cosine of
/// its `header + sample values` signal against the [`HeaderClassifier`]
/// centroids (abstain + margin gated), so it generalizes to headers the
/// keyword map never enumerated ("Cpty", "Resp. Party", …). Extraction
/// is otherwise identical to the keyword path.
pub async fn extract_entities_from_parquet_embed(
    parsed_form_path: &Path,
    source_doc_id: &str,
    classifier: &HeaderClassifier,
    embed: &crate::types::EmbedFn,
    max_entities_per_column: usize,
) -> Result<Vec<Entity>> {
    let batches = read_parquet_batches(parsed_form_path)?;
    let mut header_hints: BTreeMap<usize, (String, EntityType)> = BTreeMap::new();
    if let Some(first) = batches.first() {
        for (idx, field) in first.schema().fields().iter().enumerate() {
            let name = field.name();
            if name.starts_with('_') {
                continue;
            }
            let samples = sample_column_values(&batches, idx, COLUMN_SIGNAL_SAMPLES);
            let signal = HeaderClassifier::column_signal(name, &samples);
            let mut emb = (embed)(&signal).await?;
            l2_normalize(&mut emb);
            let decided = classifier.classify(&emb);
            if let Some((cls, sim, margin)) = classifier.best(&emb) {
                // Glass-box for gate tuning: see where real vs non-entity
                // columns score. `RUST_LOG=column_aware.classify=debug`.
                tracing::debug!(
                    target: "column_aware.classify",
                    header = %name,
                    top_class = ?cls,
                    sim,
                    margin,
                    decided = ?decided,
                    "header column classification"
                );
            }
            if let Some(ty) = decided {
                header_hints.insert(idx, (name.to_string(), ty));
            }
        }
    }
    Ok(emit_entities_from_batches(
        &batches,
        source_doc_id,
        &header_hints,
        max_entities_per_column,
    ))
}

fn read_parquet_batches(path: &Path) -> Result<Vec<RecordBatch>> {
    let file = File::open(path).map_err(|e| {
        Error::Extraction(format!(
            "column_aware: open parsed-form {}: {e}",
            path.display()
        ))
    })?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| {
            Error::Extraction(format!("column_aware: parquet open {}: {e}", path.display()))
        })?
        .build()
        .map_err(|e| Error::Extraction(format!("column_aware: parquet build: {e}")))?;
    let mut batches = Vec::new();
    for b in reader {
        batches.push(
            b.map_err(|e| Error::Extraction(format!("column_aware: parquet batch: {e}")))?,
        );
    }
    Ok(batches)
}

fn resolve_keyword_hints(
    batches: &[RecordBatch],
    map: &ColumnHeaderMap,
) -> BTreeMap<usize, (String, EntityType)> {
    let mut hints = BTreeMap::new();
    if let Some(first) = batches.first() {
        for (idx, field) in first.schema().fields().iter().enumerate() {
            let name = field.name();
            if name.starts_with('_') {
                continue;
            }
            if let Some(ty) = map.classify(name) {
                hints.insert(idx, (name.to_string(), ty));
            }
        }
    }
    hints
}

/// First `n` distinct, non-empty string values of a column across all
/// batches — the value signal folded into header classification.
fn sample_column_values(batches: &[RecordBatch], col_idx: usize, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for batch in batches {
        let Some(col) = batch.column(col_idx).as_any().downcast_ref::<StringArray>() else {
            continue;
        };
        for row in 0..batch.num_rows() {
            if col.is_null(row) {
                continue;
            }
            let v = col.value(row).trim();
            if v.is_empty() || out.iter().any(|x| x == v) {
                continue;
            }
            out.push(v.to_string());
            if out.len() >= n {
                return out;
            }
        }
    }
    out
}

/// Shared emission: for each typed column in `header_hints`, emit one
/// Entity per distinct non-empty cell value (lowercase-folded dedup
/// within the file), capped at `cap` per column (0 = uncapped).
fn emit_entities_from_batches(
    batches: &[RecordBatch],
    source_doc_id: &str,
    header_hints: &BTreeMap<usize, (String, EntityType)>,
    cap: usize,
) -> Vec<Entity> {
    if header_hints.is_empty() {
        return Vec::new();
    }
    let mut emitted: BTreeMap<String, Entity> = BTreeMap::new();
    let mut entity_counter: u32 = 0;
    let mut per_column_count: BTreeMap<usize, usize> = BTreeMap::new();
    for batch in batches {
        for (col_idx, (header_name, et)) in header_hints {
            let Some(col) = batch.column(*col_idx).as_any().downcast_ref::<StringArray>()
            else {
                continue;
            };
            for row in 0..batch.num_rows() {
                if cap > 0 && per_column_count.get(col_idx).copied().unwrap_or(0) >= cap {
                    break;
                }
                if col.is_null(row) {
                    continue;
                }
                let value = col.value(row).trim();
                if value.is_empty() || !is_name_shaped(value) {
                    // Drop footnote/banner prose and numeric/date/currency
                    // cells that slip into a typed column (subtotal rows,
                    // "Source: …" footers, dates). Shape-only — no keyword
                    // lists — so it generalizes across corpora.
                    continue;
                }
                let key =
                    format!("{}|{}", et.as_str_repr(), value.to_ascii_lowercase().trim());
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
                    description: format!("Column-aware extraction from `{header_name}`."),
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
    emitted.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_classifier_gates_abstain_on_weak_or_ambiguous() {
        // Orthogonal unit centroids; exercise the gate logic without
        // any embedding model.
        // 4-dim: Person / Institution / Place / abstain (None).
        let c = HeaderClassifier {
            centroids: vec![
                (Some(EntityType::Person), vec![1.0, 0.0, 0.0, 0.0]),
                (Some(EntityType::Institution), vec![0.0, 1.0, 0.0, 0.0]),
                (Some(EntityType::Place), vec![0.0, 0.0, 1.0, 0.0]),
                (None, vec![0.0, 0.0, 0.0, 1.0]),
            ],
            min_sim: 0.5,
            min_margin: 0.1,
        };
        // Squarely a person → Person.
        assert!(matches!(c.classify(&[1.0, 0.0, 0.0, 0.0]), Some(EntityType::Person)));
        // No signal → absolute gate abstains.
        assert!(c.classify(&[0.0, 0.0, 0.0, 0.0]).is_none());
        // Equidistant person/org → margin gate abstains.
        let h = (0.5f32).sqrt();
        assert!(c.classify(&[h, h, 0.0, 0.0]).is_none());
        // Nearest the abstain centroid → not an entity column (the
        // Status / Category / Level failure mode handled semantically).
        assert!(c.classify(&[0.0, 0.0, 0.0, 1.0]).is_none());
    }

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
