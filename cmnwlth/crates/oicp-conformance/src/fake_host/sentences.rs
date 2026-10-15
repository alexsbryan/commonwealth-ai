// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake's sentences (v0.6 §2.11): its splitter, its scorer (cosine over
//! word counts), and sentence search. Verify's retrieval (§2.15) reads the
//! same scored sentences, one retrieval path for search and verify, as the
//! spec has it. A child of `fake_host`, so it reads the fake's private state.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oicp_types::knowledge::reasons::SCORER_UNAVAILABLE;
use oicp_types::KnowledgeResult;
use serde_json::json;

use super::{refuse, Fake, Stored};

/// The sentences of `text` as code-point ranges: a stop (`.`, `!`, `?`)
/// before whitespace or the end ends one; each is trimmed of whitespace.
pub(super) fn sentence_ranges(text: &str) -> Vec<(u64, u64)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..chars.len() {
        let stop = matches!(chars[i], '.' | '!' | '?')
            && chars.get(i + 1).is_none_or(|c| c.is_whitespace());
        if !stop && i + 1 < chars.len() {
            continue;
        }
        let (mut a, mut b) = (start, i + 1);
        start = i + 1;
        while a < b && chars[a].is_whitespace() {
            a += 1;
        }
        while b > a && chars[b - 1].is_whitespace() {
            b -= 1;
        }
        if a < b {
            out.push((a as u64, b as u64));
        }
    }
    out
}

/// Every sentence of the scope sharing a word with `query`, scored by
/// cosine over word counts, best first.
pub(super) fn scored<'a>(
    f: &'a Fake,
    query: &str,
    corpora: &'a [String],
    documents: &'a [String],
) -> Vec<(f32, &'a Stored, u64, u64)> {
    let q = word_counts(query);
    let mut found = Vec::new();
    for s in f.of(corpora).filter(|s| f.in_scope(documents, s)) {
        for (a, b) in sentence_ranges(&s.text) {
            let sentence = crate::fixture::code_points(&s.text, a, b).unwrap_or_default();
            let score = cosine(&q, &word_counts(&sentence));
            if score > 0.0 {
                found.push((score, s, a, b));
            }
        }
    }
    found.sort_by(|x, y| y.0.total_cmp(&x.0));
    found
}

/// The lawful sentence search (§2.11), every sentence indexed: the scope's
/// [`scored`] sentences, at most `limit`. `rerank_without_slot` refuses by
/// name; `rerank_falls_back` answers with cosine anyway.
pub(super) fn search(
    f: &Fake,
    query: &str,
    corpora: &[String],
    documents: &[String],
    limit: usize,
) -> Response {
    if f.breaks.rerank_without_slot && !f.breaks.rerank_falls_back {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": SCORER_UNAVAILABLE, "scorer": "rerank"}),
        );
    }
    let mut found = scored(f, query, corpora, documents);
    found.truncate(limit);
    let hits: Vec<KnowledgeResult> = found
        .into_iter()
        .map(|(score, s, a, b)| {
            let mut hit = f.hit(s, a, b, score);
            if let (true, Some(span)) = (f.breaks.sentence_range_off_by_one, hit.span.as_mut()) {
                (span.start, span.end) = (a + 1, b + 1);
            }
            hit
        })
        .collect();
    Json(json!({
        "results": hits, "corpora_searched": f.searched(corpora),
        "scorer": {"name": "cosine", "model": "fake-word-counts"}, "route": "index",
    }))
    .into_response()
}

pub(super) fn word_counts(s: &str) -> BTreeMap<String, f32> {
    let mut m = BTreeMap::new();
    for w in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        *m.entry(w.to_lowercase()).or_insert(0.0) += 1.0;
    }
    m
}

pub(super) fn cosine(a: &BTreeMap<String, f32>, b: &BTreeMap<String, f32>) -> f32 {
    let dot: f32 = a.iter().map(|(w, n)| n * b.get(w).unwrap_or(&0.0)).sum();
    let norm = |m: &BTreeMap<String, f32>| m.values().map(|n| n * n).sum::<f32>().sqrt();
    if dot == 0.0 {
        0.0
    } else {
        dot / (norm(a) * norm(b))
    }
}
