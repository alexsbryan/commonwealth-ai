// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidence harvesting for the typed-extension passes.
//!
//! Everything here reads the ALREADY-LOADED tiered store and shapes what the
//! passes get to see: Person seeds out of GLiNER chunk-entity rows, verbatim
//! excerpts and figure-bearing sentences out of a small leaf's member chunks,
//! and quote spans out of a theme's member leaves. No pass logic, no writes,
//! no inference — every function is a pure or best-effort reader, and a fetch
//! or parse failure yields empty rather than an error (the caller's prompt is
//! simply built without the source-recovery handles).
//!
//! Carved out of `mod.rs` 2026-09-01: the four readers and the caps that bound
//! them are one concern, and they were the half of that file with no
//! dependency on the extraction flow itself.

use std::collections::HashMap;

use corpus_engine::enrichment::atlas::atoms::{AtomId, Entity};
use corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth;
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, VaultThemeRow};

use super::pass;

/// Pass A source recovery: only leaves with at most this many member
/// chunks get verbatim excerpts (big leaves already aggregate too
/// much text for excerpts to stay representative, and their summaries
/// compress less per chunk).
const PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS: usize = 6;

/// Per-excerpt character budget. 6 excerpts × 700 chars ≈ 4.2KB
/// prefill on top of the summary — bounded, and the fast slot's
/// prefill is cheap relative to the decode.
const PASS_A_EXCERPT_CHARS: usize = 700;

/// Figure-sentence recovery (v4) bounds: per-chunk and per-leaf caps
/// plus a per-sentence char budget. Generic digit-bearing-sentence
/// detection — not tuned to any golden's values.
const PASS_A_FIGURE_SENTENCES_PER_CHUNK: usize = 3;
const PASS_A_MAX_FIGURE_SENTENCES: usize = 8;
const PASS_A_FIGURE_SENTENCE_CHARS: usize = 240;

/// Hard cap on member-leaf quotes forwarded into one Pass B body.
/// Pass B's job is cross-leaf oppositions + concessions; the
/// excerpts are source recovery, not a replacement for the theme
/// summary itself. 6 quotes keeps the prompt under ~2KB above the
/// theme summary even when each is the full 320-char cap.
const PASS_B_QUOTE_CAP_PER_THEME: usize = 6;

/// Build Person Entity seeds from GLiNER chunk-entity mentions.
///
/// Noise gates (GLiNER emits ~5 mentions per chunk, most of them
/// generic role words):
/// - `label == "Person"` and extractor score ≥ 0.5 only.
/// - No digits in the name (the wikilink/date trap — `[[2024-01-15]]`
///   must NEVER surface as a Person; see the vault-port invariant).
/// - The canonical form must be MULTI-TOKEN ("Elinor Ostrom") —
///   single-token mentions ("user", "Margaret", "CEO") only survive
///   by SUBSUMPTION: a single-token name that appears as a whole
///   word inside exactly one multi-token name folds into it as an
///   alias ("Ostrom" → "Elinor Ostrom"), merging counts. Ambiguous
///   or host-less single tokens are dropped.
///
/// Canonical = the most frequent multi-token surface form. Returns
/// entities with sequential ids starting at 1 (caller offsets the
/// resolver's `next_entity_idx` accordingly).
pub(super) fn build_person_seed_entities(
    rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
) -> Vec<Entity> {
    use corpus_engine::enrichment::atlas::atoms::ChunkRef;
    use corpus_engine::enrichment::pipeline::atlas::EntityType;

    fn fold(s: &str) -> String {
        s.trim().to_lowercase()
    }

    // folded form → (count, best surface form, first chunk_id)
    let mut by_form: HashMap<String, (usize, String, u64)> = HashMap::new();
    for r in rows {
        if r.label != "Person" || r.score < 0.5 {
            continue;
        }
        let text = r.text.trim();
        if text.len() < 3 || text.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let key = fold(text);
        let entry = by_form
            .entry(key)
            .or_insert_with(|| (0, text.to_string(), r.chunk_id));
        entry.0 += 1;
    }

    let multi: Vec<(String, usize, String, u64)> = by_form
        .iter()
        .filter(|(k, _)| k.split_whitespace().count() >= 2)
        .map(|(k, (n, surface, chunk))| (k.clone(), *n, surface.clone(), *chunk))
        .collect();

    // Subsume single-token forms into a UNIQUE multi-token host.
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new(); // host key → alias surfaces
    let mut extra_counts: HashMap<String, usize> = HashMap::new();
    for (k, (n, surface, _)) in &by_form {
        if k.split_whitespace().count() >= 2 {
            continue;
        }
        let mut hosts = multi
            .iter()
            .filter(|(mk, ..)| mk.split_whitespace().any(|w| w == k));
        match (hosts.next(), hosts.next()) {
            (Some((host_key, ..)), None) => {
                aliases
                    .entry(host_key.clone())
                    .or_default()
                    .push(surface.clone());
                *extra_counts.entry(host_key.clone()).or_default() += n;
            }
            _ => {} // host-less or ambiguous single token → dropped
        }
    }

    let mut out: Vec<Entity> = Vec::new();
    let mut ordered = multi;
    // Deterministic output order: by descending mention count, then name.
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (i, (key, count, surface, chunk_id)) in ordered.into_iter().enumerate() {
        let total = count + extra_counts.get(&key).copied().unwrap_or(0);
        out.push(Entity {
            id: AtomId::entity(i + 1),
            canonical_name: surface,
            aliases: aliases.get(&key).cloned().unwrap_or_default(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("chunk:{chunk_id}"), None),
            description: String::new(),
            defining_quote: None,
            // Mention-count-scaled, capped — a seed is corroborated
            // NER signal, not an LLM-judged extraction.
            salience: (0.3 + 0.05 * total as f64).min(0.8) as f32,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        });
    }
    out
}

/// Verbatim member-chunk source recovery for a SMALL Pass A leaf:
/// `(excerpts, figure_sentences)`.
///
/// The leaf summary paraphrases; for an essay leaf of a few chunks
/// the paraphrase compresses out the named binaries / concession
/// phrasings the typed atoms must reproduce verbatim to resolve
/// downstream (measured 2026-06-11). Leaves with more than
/// [`PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS`] members get none —
/// excerpts of a 20-chunk leaf are no longer representative, and the
/// summary compresses less per chunk there.
///
/// `figure_sentences` (v4) are digit-bearing sentences drawn from the
/// FULL chunk text BEYOND each excerpt window — quantitative evidence
/// (figures, dollar amounts, percentages) tends to sit mid-chunk,
/// past the positional excerpt cut, and an evidence atom whose label
/// paraphrases away the figure loses its identity. The detector is
/// generic (any digit-bearing sentence), deliberately NOT tuned to
/// any bench golden's particular values (overfitting audit,
/// 2026-06-11). Best-effort: any parse or fetch failure returns
/// empty vecs (the v1 input shape).
pub(super) async fn member_source_for_leaf(
    index: &corpus_engine::index::CorpusIndex,
    leaf: &ConvRaptorNodeRow,
) -> (Vec<String>, Vec<String>) {
    let member_ids: Vec<u64> = leaf
        .direct_member_chunk_ids_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    if member_ids.is_empty() || member_ids.len() > PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS {
        return (Vec::new(), Vec::new());
    }
    let mut chunks = match index.get_chunks(&member_ids).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                node = %leaf.node_id,
                error = %e,
                "typed_extension: member chunk fetch failed; no excerpts"
            );
            return (Vec::new(), Vec::new());
        }
    };
    // get_chunks returns rows in storage order; keep document order.
    chunks.sort_by_key(|c| c.id);

    let mut excerpts = Vec::new();
    let mut figures = Vec::new();
    for c in &chunks {
        let excerpt: String = c.content.chars().take(PASS_A_EXCERPT_CHARS).collect();
        let tail: String = c.content.chars().skip(PASS_A_EXCERPT_CHARS).collect();
        figures.extend(figure_sentences_from(
            &tail,
            PASS_A_FIGURE_SENTENCES_PER_CHUNK,
        ));
        let mut text = excerpt;
        if !tail.is_empty() {
            text.push('…');
        }
        if !text.trim().is_empty() {
            excerpts.push(text);
        }
    }
    figures.truncate(PASS_A_MAX_FIGURE_SENTENCES);
    (excerpts, figures)
}

/// Digit-bearing sentences from `text`, up to `cap`, each truncated
/// to [`PASS_A_FIGURE_SENTENCE_CHARS`]. Sentence boundary = `.`/`!`/`?`
/// followed by whitespace (or end of text) — a bare `.` split would
/// sever decimal figures ("$224.8") mid-number, mangling exactly the
/// values this recovery exists to carry. Naive beyond that — good
/// enough for recall; the LLM re-reads the sentence anyway.
pub(super) fn figure_sentences_from(text: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && out.len() < cap {
        let b = bytes[i];
        let at_boundary = matches!(b, b'.' | b'!' | b'?')
            && (i + 1 >= bytes.len() || bytes[i + 1].is_ascii_whitespace());
        if at_boundary || i + 1 == bytes.len() {
            let end = (i + 1).min(bytes.len());
            if let Some(raw) = text.get(start..end) {
                let s = raw.trim();
                if s.len() >= 20 && s.chars().any(|c| c.is_ascii_digit()) {
                    let mut sentence: String =
                        s.chars().take(PASS_A_FIGURE_SENTENCE_CHARS).collect();
                    if s.chars().count() > PASS_A_FIGURE_SENTENCE_CHARS {
                        sentence.push('…');
                    }
                    out.push(sentence);
                }
            }
            start = end;
        }
        i += 1;
    }
    out
}

/// Pull verbatim quote spans (text + chunk_id) from the leaves that
/// contributed to a `theme`. The vault_themes row carries
/// `member_source_doc_ids_json` (the notes whose RAPTOR leaves
/// clustered into this theme). Looks them up in the already-loaded
/// `leaves_by_doc` index, flattens each leaf's `quote_spans_json`
/// via `pass::parse_quote_spans`, and returns the first
/// [`PASS_B_QUOTE_CAP_PER_THEME`] spans.
///
/// Returns an empty vec when the theme has no resolvable members —
/// Pass B still runs on the theme summary alone in that case, just
/// without the source-recovery handles AND without a `chunk:<id>`
/// citation handle (atoms fall back to `theme:<theme_id>`).
pub(super) fn collect_member_quotes_for_theme(
    theme: &VaultThemeRow,
    leaves_by_doc: &HashMap<String, Vec<&ConvRaptorNodeRow>>,
) -> Vec<pass::ParsedQuoteSpan> {
    let member_doc_ids: Vec<String> =
        serde_json::from_str(&theme.member_source_doc_ids_json).unwrap_or_default();
    if member_doc_ids.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<pass::ParsedQuoteSpan> = Vec::new();
    'outer: for doc_id in &member_doc_ids {
        let Some(doc_leaves) = leaves_by_doc.get(doc_id) else {
            continue;
        };
        for leaf in doc_leaves {
            for span in pass::parse_quote_spans(&leaf.quote_spans_json) {
                out.push(span);
                if out.len() >= PASS_B_QUOTE_CAP_PER_THEME {
                    break 'outer;
                }
            }
        }
    }
    out
}
