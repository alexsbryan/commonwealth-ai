// SPDX-License-Identifier: AGPL-3.0-or-later
//! Windows, the chapters the window reader reads (ONTOLOGY_METHOD §Reading):
//! every document of the corpus in clock order, cut into consecutive runs of
//! at most [`WINDOW_CHARS`] where a document ends; a document longer than a
//! window is cut where the recipe's chunker cut it, into consecutive parts of
//! whole chunks, each read in its own window (a novel, a long report). The clock is the date
//! the recipe declares (`change.document.date`), read as RESOLVE reads it
//! (`read_stamp`), with undated documents after every dated one, as RESOLVE
//! orders them. A window is one reading call, so a reader sees a thread's
//! earlier messages beside its later ones whatever folders or titles the
//! corpus came in, and no call's input grows with the corpus.

use std::collections::BTreeMap;

use corpus_engine::enrichment::atlas::{read_stamp, DocumentTexts};
use corpus_engine::enrichment::ontology::{DocumentStamp, OntologyPolicies};
use corpus_engine::enrichment::pipeline::{ChapterEntry, ChapterManifest};
use corpus_index::index::EnrichmentChunkRow;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

/// The most characters of stored text one window holds, about 5k tokens at the
/// 4.2 characters a token measured on mail. A reader writes about as many
/// statements per call whatever it is shown (21-38 per saltgrass window at 20k
/// and at 40k), so the size sets how much of a document becomes statements, not
/// only how many calls a corpus takes: at 20k saltgrass-asked read 37 and 37 of
/// 42 agreeing on 36, at 40k 26-32 agreeing on 33-34, and the held-out recipes
/// agree with each other on 32.6 against 29.2 (`determinism-prereg.md`,
/// 2026-10-17). Mail kept its deals (world-s3 + ward-story 33 of 41, 31 at 40k)
/// for about 1.7x the answer tokens. Prompt and the reasoned answer
/// (`window.rs`, 64k) fit the 128k context the system is held to
/// (REVIEW_BRIEF, "no one-way doors").
pub const WINDOW_CHARS: usize = 20_000;

/// What a window holds of one document: all of it, or a run of its chunks.
struct Piece<'a> {
    doc: &'a Doc,
    chunks: &'a [(u64, String)],
    chars: usize,
}

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

/// The window size `policies` read at: the recipe's `window_chars`, or the
/// shipped [`WINDOW_CHARS`].
pub fn window_chars(policies: &OntologyPolicies) -> usize {
    policies.shape.window_chars.unwrap_or(WINDOW_CHARS)
}

/// [`build_window_manifest`] for `policies`: clocked by its declared date, cut
/// at its [`window_chars`], and sized by `index`'s stored texts, as the reader
/// will read them; when the store gives none, by the chunks, and it says so.
pub async fn build_window_manifest_for(
    corpus_id: &str,
    index: &corpus_index::index::CorpusIndex,
    rows: Vec<EnrichmentChunkRow>,
    policies: &OntologyPolicies,
) -> ChapterManifest {
    let date_field = policies
        .change
        .document
        .as_ref()
        .and_then(|d| d.field(DocumentStamp::Date));
    let texts = match crate::corpus_io::document_texts(index).await {
        Ok(texts) => texts,
        Err(e) => {
            warn!(error = %e, "window manifest: no stored texts; windows are sized by their chunks");
            DocumentTexts::new()
        }
    };
    let max_chars = window_chars(policies);
    info!(corpus_id, max_chars, "window manifest: window size");
    build_window_manifest(corpus_id, rows, &texts, date_field, max_chars)
}

/// The window manifest of `rows`: documents in clock order by `date_field`
/// (none declared: every document undated, in chunk order), cut where a
/// document ends once a window would pass `max_chars` of the text `texts`
/// holds for it (its chunks', where it holds none). A document longer than
/// `max_chars` is cut at its chunk boundaries into parts of at most
/// `max_chars` of chunk text (a chunk longer than that is a part alone), and
/// each part is placed as a document is.
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

    // Each document whole, or, longer than a window, its consecutive parts.
    let mut pieces: Vec<Piece<'_>> = Vec::new();
    let mut cut = 0usize;
    for doc in &docs {
        let chars = doc.chars(texts);
        if chars <= max_chars {
            pieces.push(Piece {
                doc,
                chunks: &doc.chunks,
                chars,
            });
            continue;
        }
        let mut parts: Vec<Piece<'_>> = Vec::new();
        let (mut from, mut size) = (0usize, 0usize);
        for (i, (_, c)) in doc.chunks.iter().enumerate() {
            if i > from && size + c.len() > max_chars {
                parts.push(Piece {
                    doc,
                    chunks: &doc.chunks[from..i],
                    chars: size,
                });
                (from, size) = (i, 0);
            }
            size += c.len();
        }
        parts.push(Piece {
            doc,
            chunks: &doc.chunks[from..],
            chars: size,
        });
        if parts.len() == 1 {
            // Its chunks fit one window though its stored text does not: it is
            // read whole, as its stored text, so it is sized by that and stands alone.
            pieces.push(Piece {
                doc,
                chunks: &doc.chunks,
                chars,
            });
            continue;
        }
        cut += 1;
        debug!(document = %doc.key, chars, parts = parts.len(), "window manifest: a document longer than a window cut at its chunks");
        pieces.extend(parts);
    }

    let mut windows: Vec<Vec<Piece<'_>>> = Vec::new();
    let mut size = 0usize;
    for piece in pieces {
        let chars = piece.chars;
        match windows.last_mut() {
            Some(window) if !window.is_empty() && size + chars <= max_chars => window.push(piece),
            _ => {
                windows.push(vec![piece]);
                size = 0;
            }
        }
        size += chars;
    }

    let n = windows.len();
    let mut manifest = ChapterManifest::new(corpus_id);
    for (i, window) in windows.iter().enumerate() {
        let ordinal = i as u32 + 1;
        let chunks: Vec<&(u64, String)> = window.iter().flat_map(|p| p.chunks.iter()).collect();
        let span = match (
            window[0].doc.date.as_deref(),
            window[window.len() - 1].doc.date.as_deref(),
        ) {
            (Some(first), Some(last)) => format!("{first} to {last}"),
            (Some(first), None) => format!("{first} on, with undated documents"),
            _ => "undated documents".to_string(),
        };
        debug!(window = ordinal, documents = window.len(), first = %window[0].doc.key, %span, "window manifest: window cut");
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
        cut_at_chunks = cut,
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
