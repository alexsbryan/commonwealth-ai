// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate <recipe> --corpus <path>`: the metadata fields a recipe's
//! ontology declares, read against the documents its own extractor reads from
//! `<path>`.
//!
//! A field name in `change.document` or a type's `source.metadata` is the
//! corpus's own, so nothing static can say it is right: round 1 of the
//! blind-author test (research/ontology-apps/blind-author/) guessed
//! `issue_number`, `date_published` and `url` against fields the documents do
//! not carry, and `validate` passed all three. This reads every document the
//! way ingest stores it (the extractor, then [`DeclaredMetadata::input`]),
//! names each declared field none of them carries, counts the ones some lack,
//! reads stamps through the one stamp reader ([`read_stamp`]), and lists every
//! field the documents do carry so the author can pick the right one.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};
use tracing::{debug, info};

use crate::enrichment::atlas::read_stamp;
use crate::enrichment::ontology::{DocumentStamp, OntologyPolicies, SourceDecl};
use crate::recipe::{ExtractorConfig, Recipe};
use crate::recipe_documents::DeclaredMetadata;
use crate::CorpusEngine;

/// What the corpus said about the recipe's declared fields. Same three
/// channels as static validation: an error fails `validate`.
#[derive(Debug, Default, PartialEq)]
pub struct CorpusFieldReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

/// Where the recipe uses a metadata field: a `change.document` stamp, or a
/// type's metadata source.
#[derive(Debug, Clone, PartialEq)]
enum FieldUse {
    Stamp(DocumentStamp),
    Source(String),
}

impl FieldUse {
    fn describe(&self) -> String {
        match self {
            Self::Stamp(s) => format!("change.document.{}", s.key()),
            Self::Source(ty) => format!("type `{ty}` source.metadata"),
        }
    }
}

/// Every metadata field the ontology names, with where it names it.
fn declared_fields(p: &OntologyPolicies) -> Vec<(String, FieldUse)> {
    let stamps = p
        .change
        .document
        .iter()
        .flat_map(|d| d.declared())
        .map(|(s, f)| (f.to_string(), FieldUse::Stamp(s)));
    let sources = p.shape.types.iter().flat_map(|t| match &t.source {
        Some(SourceDecl::Metadata(s)) => s
            .metadata
            .iter()
            .map(|f| (f.clone(), FieldUse::Source(t.name.clone())))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    });
    stamps.chain(sources).collect()
}

/// Read `source` with the recipe's extractor and check its declared fields.
/// An extractor that cannot open `source`, or reads no document from it, is
/// an error: a check over no documents judged nothing.
pub fn check(engine: &CorpusEngine, recipe: &Recipe, source: &Path) -> CorpusFieldReport {
    let mut report = CorpusFieldReport::default();
    let iter = match engine.recipe_extractor(recipe).extract(source) {
        Ok(it) => it,
        Err(e) => {
            report.errors.push(format!(
                "corpus {}: the recipe's extractor could not open it: {e}",
                source.display()
            ));
            return report;
        }
    };
    let declared = DeclaredMetadata::of(recipe, Some(source));
    let mut docs: Vec<Map<String, Value>> = Vec::new();
    let mut failed = 0usize;
    for (ordinal, doc) in iter.enumerate() {
        let doc = match doc {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                debug!(ordinal, error = %e, "recipe corpus fields: document failed to extract");
                continue;
            }
        };
        let input = declared.input(
            &doc.content,
            &doc.source_id,
            ordinal as u32,
            &doc.source,
            doc.metadata.as_ref(),
        );
        let fields = match input.metadata.as_deref().map(serde_json::from_str::<Value>) {
            Some(Ok(Value::Object(m))) => m,
            // Not an object: it carries no field, so every declared field
            // counts as lacking in it, as the stamper reads it at resolve.
            other => {
                debug!(source_id = %doc.source_id, parsed = ?other.map(|r| r.is_ok()), "recipe corpus fields: metadata is not a JSON object");
                Map::new()
            }
        };
        docs.push(fields);
    }
    let policies = match recipe.ontology_block().map(|b| b.policies()) {
        Some(Ok(p)) => p,
        Some(Err(e)) => {
            report.errors.push(e.to_string());
            return report;
        }
        None => OntologyPolicies::default(),
    };
    info!(
        corpus = %source.display(),
        documents = docs.len(),
        failed,
        "recipe corpus fields: documents read"
    );
    if failed > 0 {
        report.warnings.push(format!(
            "corpus {}: {failed} document(s) failed to extract and are not counted",
            source.display()
        ));
    }
    judge(&policies, &docs, &source.display().to_string(), &mut report);
    if docs.is_empty() {
        if let Some((key, value)) = content_key(&recipe.extract) {
            report.errors.push(format!(
                "the extractor skips a record with no text under its content field, and this \
                 recipe names `extract.{key} = \"{value}\"`: check the records carry it"
            ));
        }
    }
    report
}

/// The `[extract]` key naming where a record-per-row extractor finds a
/// document's text, and its value: a wrong one reads as an empty corpus.
fn content_key(extract: &ExtractorConfig) -> Option<(&'static str, &str)> {
    match extract {
        ExtractorConfig::Jsonl {
            content_field: Some(f),
            ..
        } => Some(("content_field", f)),
        ExtractorConfig::Json { content_field, .. } => Some(("content_field", content_field)),
        ExtractorConfig::Csv { content_column, .. } => Some(("content_column", content_column)),
        _ => None,
    }
}

/// The verdicts over documents already read: pure, so it is tested without an
/// extractor.
fn judge(
    p: &OntologyPolicies,
    docs: &[Map<String, Value>],
    corpus: &str,
    out: &mut CorpusFieldReport,
) {
    let n = docs.len();
    if n == 0 {
        out.errors.push(format!(
            "corpus {corpus}: the extractor read no documents, so no declared field could be checked"
        ));
        return;
    }
    let mut carried: BTreeMap<&str, usize> = BTreeMap::new();
    for d in docs {
        for (k, v) in d {
            if !v.is_null() {
                *carried.entry(k.as_str()).or_default() += 1;
            }
        }
    }
    let have = if carried.is_empty() {
        "none".to_string()
    } else {
        carried
            .iter()
            .map(|(k, c)| format!("`{k}` ({c})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    out.notes.push(format!(
        "corpus {corpus}: {n} documents; the metadata fields they carry (documents carrying each): {have}"
    ));
    for (field, using) in declared_fields(p) {
        let present = carried.get(field.as_str()).copied().unwrap_or(0);
        debug!(field = %field, using = %using.describe(), present, n, "recipe corpus fields: declared field");
        if present == 0 {
            out.errors.push(format!(
                "`{field}` ({}) is in none of the {n} documents; they carry: {have}",
                using.describe()
            ));
            continue;
        }
        // A source reads several fields and a document lacking one of them
        // (no `cc`) is ordinary; the counts are in the note above. A stamp is
        // one field per claim, so a document lacking it leaves claims unstamped.
        if present < n && matches!(using, FieldUse::Stamp(_)) {
            out.warnings.push(format!(
                "`{field}` ({}) is missing from {} of {n} documents",
                using.describe(),
                n - present
            ));
        }
        if let FieldUse::Stamp(stamp) = using {
            let mut unreadable = docs
                .iter()
                .filter(|d| d.get(field.as_str()).is_some_and(|v| !v.is_null()))
                .filter_map(|d| read_stamp(d, stamp, &field).err());
            if let Some(first) = unreadable.next() {
                let count = 1 + unreadable.count();
                out.errors.push(format!(
                    "`{field}` (change.document.{}) cannot be read in {count} of the {present} documents carrying it: {first}",
                    stamp.key()
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "recipe_corpus_fields_tests.rs"]
mod tests;
