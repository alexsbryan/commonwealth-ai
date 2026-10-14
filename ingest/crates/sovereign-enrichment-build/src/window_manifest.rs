// SPDX-License-Identifier: AGPL-3.0-or-later
//! Windows, the chapters the window reader reads (ONTOLOGY_METHOD §Reading):
//! every document of the corpus in clock order, cut into consecutive runs of
//! at most [`WINDOW_CHARS`] only where a document ends. The clock is the date
//! the recipe declares (`change.document.date`), read as RESOLVE reads it
//! (`read_stamp`), with undated documents after every dated one, as RESOLVE
//! orders them. A window is one reading call, so a reader sees a thread's
//! earlier messages beside its later ones whatever folders or titles the
//! corpus came in, and no call's input grows with the corpus.

use std::collections::BTreeMap;

use corpus_engine::enrichment::atlas::{read_stamp, DocumentTexts};
use corpus_engine::enrichment::ontology::DocumentStamp;
use corpus_engine::enrichment::pipeline::{ChapterEntry, ChapterManifest};
use corpus_index::index::EnrichmentChunkRow;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

/// The most characters of stored text one window holds, about 10k tokens at
/// the 4.2 characters a token measured on mail (49k prompt tokens for world-s4's
/// 190k characters). The answer grows with the window: at about 120k characters
/// world-s4's answer passed 8k tokens and was cut off, and Ward's whole 37
/// messages drew a 30k-token prompt and a 6.8k-token answer (2026-10-13). Here
/// prompt and a 16k answer (`window.rs`) fit the 32k context of the smallest model the system is
/// held to (REVIEW_BRIEF, "no one-way doors"). A cost bound: it sets how many
/// calls a corpus takes, never what a record says.
pub const WINDOW_CHARS: usize = 40_000;

/// One document as its chunk rows give it.
struct Doc {
    key: String,
    /// Its date in the stamp's form (UTC ISO 8601, so it sorts), when read.
    date: Option<String>,
    chunks: Vec<(u64, String)>,
}

impl Doc {
    /// The size the reader reads: its stored text, or its chunks when the
    /// store holds none (`SourceDocument::raw_body`).
    fn chars(&self, texts: &DocumentTexts) -> usize {
        texts
            .get(&self.key)
            .map(String::len)
            .unwrap_or_else(|| self.chunks.iter().map(|(_, c)| c.len()).sum())
    }
}

/// [`build_window_manifest`] sized by `index`'s stored texts, as the reader
/// will read them; when the store gives none, by the chunks, and it says so.
pub async fn build_window_manifest_for(
    corpus_id: &str,
    index: &corpus_index::index::CorpusIndex,
    rows: Vec<EnrichmentChunkRow>,
    date_field: Option<&str>,
) -> ChapterManifest {
    let texts = match crate::corpus_io::document_texts(index).await {
        Ok(texts) => texts,
        Err(e) => {
            warn!(error = %e, "window manifest: no stored texts; windows are sized by their chunks");
            DocumentTexts::new()
        }
    };
    build_window_manifest(corpus_id, rows, &texts, date_field, WINDOW_CHARS)
}

/// The window manifest of `rows`: documents in clock order by `date_field`
/// (none declared: every document undated, in chunk order), cut where a
/// document ends once a window would pass `max_chars` of the text `texts`
/// holds for it (its chunks', where it holds none). A document longer than
/// `max_chars` is a window of its own.
pub fn build_window_manifest(
    corpus_id: &str,
    rows: Vec<EnrichmentChunkRow>,
    texts: &DocumentTexts,
    date_field: Option<&str>,
    max_chars: usize,
) -> ChapterManifest {
    let mut docs: Vec<Doc> = Vec::new();
    let mut at: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows = rows;
    rows.sort_by_key(|r| r.id);
    for row in rows {
        let key = row
            .source_doc_id
            .clone()
            .unwrap_or_else(|| format!("chunk:{}", row.id));
        let k = *at.entry(key.clone()).or_insert_with(|| {
            docs.push(Doc {
                key: key.clone(),
                date: None,
                chunks: Vec::new(),
            });
            docs.len() - 1
        });
        let doc = &mut docs[k];
        if doc.date.is_none() {
            doc.date = date_field.and_then(|field| date_of(&row, field));
        }
        doc.chunks.push((row.id, row.content));
    }
    // Dated first, by date; undated after, each by its first chunk.
    docs.sort_by(|a, b| {
        (a.date.is_none(), &a.date, a.chunks[0].0).cmp(&(b.date.is_none(), &b.date, b.chunks[0].0))
    });
    let undated = docs.iter().filter(|d| d.date.is_none()).count();

    let mut windows: Vec<Vec<&Doc>> = Vec::new();
    let mut size = 0usize;
    for doc in &docs {
        let chars = doc.chars(texts);
        match windows.last_mut() {
            Some(window) if !window.is_empty() && size + chars <= max_chars => window.push(doc),
            _ => {
                windows.push(vec![doc]);
                size = 0;
            }
        }
        size += chars;
    }

    let n = windows.len();
    let mut manifest = ChapterManifest::new(corpus_id);
    for (i, window) in windows.iter().enumerate() {
        let ordinal = i as u32 + 1;
        let chunks: Vec<&(u64, String)> = window.iter().flat_map(|d| d.chunks.iter()).collect();
        let span = match (window[0].date.as_deref(), window[window.len() - 1].date.as_deref()) {
            (Some(first), Some(last)) => format!("{first} to {last}"),
            (Some(first), None) => format!("{first} on, with undated documents"),
            _ => "undated documents".to_string(),
        };
        debug!(window = ordinal, documents = window.len(), first = %window[0].key, %span, "window manifest: window cut");
        let mut metadata = BTreeMap::new();
        metadata.insert("ordinal".to_string(), ordinal.to_string());
        metadata.insert("window_documents".to_string(), window.len().to_string());
        manifest.chapters.push(ChapterEntry {
            id: format!("sec_{ordinal:05}"),
            title: format!("window {ordinal} of {n}: {span}"),
            part: None,
            chapter: Some(ordinal),
            first_line: chunks[0]
                .1
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(160)
                .collect(),
            word_count: chunks
                .iter()
                .map(|(_, c)| c.split_whitespace().count() as u64)
                .sum(),
            chunk_ids: chunks.iter().map(|(id, _)| *id).collect(),
            characters_present: Vec::new(),
            metadata,
        });
    }
    info!(
        documents = docs.len(),
        sized_by_stored_text = docs.iter().filter(|d| texts.contains_key(&d.key)).count(),
        undated,
        windows = n,
        max_chars,
        date_field = ?date_field,
        "window manifest: documents cut into windows in clock order"
    );
    manifest
}

/// A chunk row's document date in the stamp's form, or `None`, traced.
fn date_of(row: &EnrichmentChunkRow, field: &str) -> Option<String> {
    let fields: Map<String, Value> = row
        .metadata_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    match read_stamp(&fields, DocumentStamp::Date, field) {
        Ok(date) => Some(date),
        Err(why) => {
            debug!(chunk = row.id, %why, "window manifest: no date on this chunk");
            None
        }
    }
}

#[cfg(test)]
#[path = "window_manifest_tests.rs"]
mod tests;
