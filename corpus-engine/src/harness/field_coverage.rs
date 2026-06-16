// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Extract field-coverage rung's mechanism: what each structured extractor
//! DECLARES it will populate, and a boolean presence check per document.
//!
//! Pure boolean presence — no fuzzy matching. Coverage thresholds and the
//! section-vs-per-document aggregation are policy and live in
//! `sovereign-eval::authoring_harness`.

use crate::extractors::ExtractedDoc;
use crate::recipe::ExtractorConfig;

use super::miss::FieldMiss;

/// Where a declared field lands on an `ExtractedDoc`, and how to test presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldProbe {
    /// A key in `doc.metadata` — `tabular_atoms` stashes the full typed row
    /// there, so each declared column is a metadata key.
    Metadata(String),
    /// `doc.metadata.section_name == name` — `html_sections` emits one doc per
    /// matched section, so coverage is per source file (via the miss sidecar).
    Section(String),
    /// `doc.content` is non-empty (the extractor's content_field/column landed).
    Content,
    /// `doc.title` is present and non-empty (title_field/title_attr landed).
    Title,
    /// `doc.url` is present and non-empty (url_field landed).
    Url,
}

/// One field the recipe's `[extract]` config declares it will populate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// Display label, e.g. `"column: parcel_number"` / `"section: md_and_a"`.
    pub label: String,
    pub probe: FieldProbe,
    /// Optional declared fields (e.g. json `title_field`) warn rather than fail
    /// when absent; required fields fail.
    pub required: bool,
}

impl FieldDecl {
    fn required(label: String, probe: FieldProbe) -> Self {
        Self { label, probe, required: true }
    }
    fn optional(label: String, probe: FieldProbe) -> Self {
        Self { label, probe, required: false }
    }

    /// True when this declared field is present on `doc`. Boolean presence
    /// only — a metadata key exists and is non-null, a section name matches,
    /// or a mapped doc field is non-empty.
    pub fn is_present(&self, doc: &ExtractedDoc) -> bool {
        match &self.probe {
            FieldProbe::Metadata(key) => doc
                .metadata
                .as_ref()
                .and_then(|m| m.get(key))
                .is_some_and(|v| !v.is_null()),
            FieldProbe::Section(name) => {
                doc.metadata
                    .as_ref()
                    .and_then(|m| m.get("section_name"))
                    .and_then(|v| v.as_str())
                    == Some(name.as_str())
            }
            FieldProbe::Content => !doc.content.is_empty(),
            FieldProbe::Title => doc.title.as_deref().is_some_and(|t| !t.is_empty()),
            FieldProbe::Url => doc.url.as_deref().is_some_and(|u| !u.is_empty()),
        }
    }
}

/// The fields the recipe's `[extract]` config promises to populate. The match
/// is confined to extractors that DECLARE structured fields; everything else
/// falls through to the implicit `content` field only.
pub fn declared_fields(config: &ExtractorConfig) -> Vec<FieldDecl> {
    use ExtractorConfig::*;
    match config {
        // Section extractors: one doc per matched section; coverage is
        // per-file via the `_section_misses.json` sidecar.
        HtmlSections { sections, .. } => sections
            .iter()
            .map(|r| {
                FieldDecl::required(
                    format!("section: {}", r.name),
                    FieldProbe::Section(r.name.clone()),
                )
            })
            .collect(),
        // Element extractor: one doc per matched element body, optional title.
        XmlSections { title_attr, .. } => {
            let mut v = vec![FieldDecl::required(
                "content (element body)".into(),
                FieldProbe::Content,
            )];
            if title_attr.is_some() {
                v.push(FieldDecl::optional("title (title_attr)".into(), FieldProbe::Title));
            }
            v
        }
        // The full typed row is stashed in metadata — the richest coverage:
        // every declared column is a metadata key.
        TabularAtoms {
            id_column,
            numeric_attributes,
            string_attributes,
            ..
        } => {
            let mut v = vec![FieldDecl::required(
                format!("column: {id_column}"),
                FieldProbe::Metadata(id_column.clone()),
            )];
            for c in numeric_attributes.iter().chain(string_attributes) {
                v.push(FieldDecl::required(
                    format!("column: {c}"),
                    FieldProbe::Metadata(c.clone()),
                ));
            }
            v
        }
        // Field-mapping extractors: declared fields land on doc.content/title/url.
        Json {
            title_field,
            url_field,
            ..
        } => {
            let mut v = vec![FieldDecl::required("field: content".into(), FieldProbe::Content)];
            if title_field.is_some() {
                v.push(FieldDecl::optional("field: title".into(), FieldProbe::Title));
            }
            if url_field.is_some() {
                v.push(FieldDecl::optional("field: url".into(), FieldProbe::Url));
            }
            v
        }
        Jsonl { title_field, .. } => {
            let mut v = vec![FieldDecl::required("field: content".into(), FieldProbe::Content)];
            if title_field.is_some() {
                v.push(FieldDecl::optional("field: title".into(), FieldProbe::Title));
            }
            v
        }
        Csv { title_column, .. } => {
            let mut v = vec![FieldDecl::required("column: content".into(), FieldProbe::Content)];
            if title_column.is_some() {
                v.push(FieldDecl::optional("column: title".into(), FieldProbe::Title));
            }
            v
        }
        Parquet { url_column, .. } => {
            let mut v = vec![FieldDecl::required("column: content".into(), FieldProbe::Content)];
            if url_column.is_some() {
                v.push(FieldDecl::optional("column: url".into(), FieldProbe::Url));
            }
            v
        }
        // Free-text / catalog / chat-export extractors declare no structured
        // fields beyond the body — only the implicit content field is checked.
        _ => vec![FieldDecl::required("content".into(), FieldProbe::Content)],
    }
}

/// Whether a field's coverage is measured per document or per source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageUnit {
    /// Per-document (tabular columns, json/csv fields, content/title/url).
    Docs,
    /// Per source file — section extractors emit one doc per matched section,
    /// so a declared section's presence is measured against the sample's files.
    Files,
}

/// Raw coverage counts for one declared field over the sample — the mechanism.
/// The Pass/Fail threshold is policy and lives in the eval layer.
#[derive(Debug, Clone)]
pub struct FieldCoverage {
    pub label: String,
    pub required: bool,
    pub found: usize,
    pub total: usize,
    pub unit: CoverageUnit,
    /// The concrete misses (never just a count) — shown to the author.
    pub misses: Vec<FieldMiss>,
}

/// The document's stable id: prefer the URL, fall back to the source id.
/// Mirrors the production ingest's skip-key derivation so harness `doc_id`s
/// line up with what a real ingest would record.
pub fn doc_id(d: &ExtractedDoc) -> String {
    d.url
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| d.source_id.clone())
}

fn content_preview(d: &ExtractedDoc, n: usize) -> String {
    d.content.chars().take(n).collect()
}

/// Count, for every field the extract config declares, how many docs (or source
/// files, for section extractors) populated it — plus the concrete misses. No
/// thresholds here; the Pass/Fail call is the eval layer's.
pub fn coverage(
    config: &ExtractorConfig,
    docs: &[ExtractedDoc],
    section_misses: &[FieldMiss],
    source_files: usize,
) -> Vec<FieldCoverage> {
    declared_fields(config)
        .into_iter()
        .map(|field| match &field.probe {
            FieldProbe::Section(name) => {
                let misses: Vec<FieldMiss> = section_misses
                    .iter()
                    .filter(|m| &m.field == name)
                    .cloned()
                    .collect();
                let missed_files: std::collections::HashSet<&str> =
                    misses.iter().map(|m| m.doc_id.as_str()).collect();
                let total = source_files.max(1);
                let found = total.saturating_sub(missed_files.len());
                FieldCoverage {
                    label: field.label,
                    required: field.required,
                    found,
                    total,
                    unit: CoverageUnit::Files,
                    misses,
                }
            }
            _ => {
                let total = docs.len();
                let mut found = 0;
                let mut misses = Vec::new();
                for d in docs {
                    if field.is_present(d) {
                        found += 1;
                    } else if misses.len() < 25 {
                        misses.push(FieldMiss {
                            field: field.label.clone(),
                            doc_id: doc_id(d),
                            nearby_text: Some(content_preview(d, 200)),
                        });
                    }
                }
                FieldCoverage {
                    label: field.label,
                    required: field.required,
                    found,
                    total,
                    unit: CoverageUnit::Docs,
                    misses,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::ExtractedDoc;
    use crate::recipe::Recipe;

    fn parse(extract_block: &str) -> Recipe {
        let toml = format!(
            r#"
[corpus]
id = "t"
name = "t"
[acquire]
type = "local_file"
path = "/tmp/x"
{extract_block}
[chunk]
type = "paragraph"
[index]
"#
        );
        Recipe::from_toml(&toml).expect("test recipe parses")
    }

    #[test]
    fn jsonl_declares_content_and_optional_title() {
        let r = parse("[extract]\ntype = \"jsonl\"\ncontent_field = \"text\"\ntitle_field = \"title\"");
        let fields = declared_fields(&r.extract);
        assert!(fields
            .iter()
            .any(|f| matches!(f.probe, FieldProbe::Content) && f.required));
        assert!(fields
            .iter()
            .any(|f| matches!(f.probe, FieldProbe::Title) && !f.required));
    }

    #[test]
    fn tabular_declares_every_column_required() {
        let r = parse(
            "[extract]\ntype = \"tabular_atoms\"\ndocument_path = \"$\"\nid_column = \"parcel\"\nentity_type = \"parcel\"\nnumeric_attributes = [\"assessed_value\"]\nstring_attributes = [\"use_code\"]",
        );
        let labels: Vec<String> = declared_fields(&r.extract)
            .iter()
            .map(|f| f.label.clone())
            .collect();
        assert!(labels.iter().any(|l| l.contains("parcel")));
        assert!(labels.iter().any(|l| l.contains("assessed_value")));
        assert!(labels.iter().any(|l| l.contains("use_code")));
    }

    #[test]
    fn is_present_reads_metadata_section_and_doc_fields() {
        let doc = ExtractedDoc {
            title: Some("T".into()),
            content: "body".into(),
            url: None,
            source_id: "s".into(),
            metadata: Some(serde_json::json!({"section_name": "md_and_a", "parcel": 5})),
            source_file: None,
            embed_text: None,
        };
        let present = |p: FieldProbe| FieldDecl::required(String::new(), p).is_present(&doc);
        assert!(present(FieldProbe::Content));
        assert!(present(FieldProbe::Title));
        assert!(!present(FieldProbe::Url));
        assert!(present(FieldProbe::Metadata("parcel".into())));
        assert!(!present(FieldProbe::Metadata("missing".into())));
        assert!(present(FieldProbe::Section("md_and_a".into())));
        assert!(!present(FieldProbe::Section("dissent".into())));
    }
}
