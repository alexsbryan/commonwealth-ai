// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthesis-prompt chunk formatter and `ResponseProvenance` builders.
//!
//! `format_scored_chunks` / `format_scored_chunks_with_kinds` render the
//! retrieved-chunk block the synthesis prompt consumes — sub-bucketed by
//! Catalog / Web / Folder / conversation / meta-atlas-articulation so the
//! synthesis model sees structural cues, not a flat dump.
//!
//! `build_provenance_components` produces the `(sources, coverage)` tuple
//! that `ResponseProvenance` carries to the desktop chat surface, and
//! `build_coverage_gaps_note` synthesises the prompt-time "what I don't
//! have" reminder for thin folder corpora.

use std::collections::{BTreeMap, HashMap, HashSet};

use corpus_engine::{CorpusKind, ScoredChunk};

use crate::traits::FolderMetadata;
use crate::types::{CoverageNote, SourceSummary, ThinFolder};

use super::evidence::strip_leading_title_duplicate;
use super::text_utils::truncate_chunk_content;

/// Maximum characters of knowledge context to inject into prompts.
/// ~1000 tokens at ~4 chars/token, leaving room for history + system + response.
/// Default prompt budget for retrieved-chunk context. 8000 chars ≈
/// 2k prompt tokens, which fits 15 chunks at ~530 chars each — the
/// merged top-K used by both KnowledgeQuery and DeepQuery. The
/// budget was 4000 when per-corpus K was 5 and merged K was 8;
/// raising K without raising the budget meant the formatter dropped
/// half the chunks we'd just gone to the trouble of retrieving. The
/// expansion path's `EXPANDED_KNOWLEDGE_CHARS` is now coincident
/// with this default, since both serve roughly 12-15 chunks.
pub(crate) const MAX_KNOWLEDGE_CHARS: usize = 8000;

/// Threshold below which a folder corpus's per-turn chunk count is
/// flagged as "thin coverage" in `ResponseProvenance.coverage`.
/// Folder-ingest v1 §6.3: a folder corpus that contributed *some*
/// chunks but fewer than this many likely under-served the query —
/// the chat surface chip enumerates the folder so the user can
/// extend it or reformulate.
///
/// Tuned to 3 because typical retrieval pulls 8–16 chunks total per
/// query; a folder contributing 0–2 of them is a clearer "thin"
/// signal than the 4–5 threshold that would over-trigger on
/// well-served queries with mixed-source retrieval.
const FOLDER_THIN_COVERAGE_THRESHOLD: usize = 3;

/// A formatted knowledge context plus the identity of the chunks that
/// actually survived the `max_chars` budget.
///
/// `admitted` exists because retrieval telemetry used to report the
/// size of the chunk pool handed *to* the formatter, not what reached
/// the prompt. Those differ whenever the budget bites, and the gap was
/// silent: a soak journal row reading `retrieved: 28` could describe a
/// prompt carrying 8 chunks. Every evidence count derived from the pool
/// overstated what the model saw, which made two runs incomparable and
/// made "the evidence was present" an unsound premise for judging a
/// bad answer.
///
/// Glassbox rule for callers: report `admitted` when describing what
/// the model was given, and the pool length only when describing what
/// retrieval found. Never re-derive membership — the budget loop in
/// [`format_scored_chunks_counted`] is the single decider
/// (`ARCH_PRINCIPLES.md` §10.6), and a second copy of that arithmetic
/// would drift the moment the packing strategy changes.
pub(crate) struct FormattedChunks {
    pub text: String,
    /// One entry per chunk that reached `text`, in admission order:
    /// its index into the input slice, paired with the EXACT body the
    /// prompt carried for it. The remainder were dropped by the budget
    /// and never seen by the model.
    ///
    /// Deliberately indices rather than a count. Today's budget loop
    /// `break`s on the first overflow, so the admitted set happens to
    /// be a prefix and a bare `usize` would describe it — but that is
    /// a property of the packing strategy, not of the contract. If
    /// that loop ever becomes a `continue` (or packs by score), a
    /// count would keep type-checking while silently mislabelling
    /// which chunks the model saw. Indices cannot go quietly wrong.
    ///
    /// The body rides along because chunk admission turned out to be
    /// the SMALLER of the two losses and reporting it alone was
    /// actively misleading. Measured 2026-08-06 on the soak baseline's
    /// three most evidence-heavy turns: 0 of 20, 0 of 19 and 0 of 20
    /// chunks were evicted — while every single passage exceeded
    /// `MAX_CHUNK_CHARS` (600). One of those turns resolved 214,129
    /// chars of evidence into a prompt that could hold at most 12,000.
    /// An oracle asking "was the answer present in the evidence?"
    /// against full chunk text can therefore find it 5,000 chars into
    /// a passage the model saw the first 600 of, and call a synthesis
    /// failure on a system that was never shown the answer.
    ///
    /// Emitting the rendered body is what lets a consumer ask that
    /// question against what was actually sent — without re-deriving
    /// the truncation, which would put a second copy of
    /// `MAX_CHUNK_CHARS` in a judge harness and drift from this one
    /// (`ARCH_PRINCIPLES.md` §10.6, §15 "two implementations of one
    /// threshold").
    pub admitted: Vec<(usize, String)>,
}

/// Build a truncated knowledge context string from corpus-engine scored chunks,
/// grouped by provenance tier (corpus vs web) and staying within a character budget.
pub(crate) fn format_scored_chunks(chunks: &[ScoredChunk], max_chars: usize) -> String {
    format_scored_chunks_with_kinds(chunks, max_chars, None, None, None, None)
}

/// Like [`format_scored_chunks`], but if a `kinds` map is supplied,
/// chunks from `Catalog` corpora are routed into a separate
/// "CATALOG-AWARE SOURCES" section that the synthesis prompt
/// (`KNOWLEDGE_SYNTHESIS_SYSTEM`) knows how to handle (orient from
/// metadata, do not invent, end with ingest offer).
///
/// When `contested` is supplied, chunks whose title appears in the
/// set get a ` (contested)` suffix on their source label — a hint
/// the synthesis prompt knows how to handle (present multiple views,
/// don't synthesise false consensus). Populated by
/// `prepare_knowledge_query_plan` from the Wikipedia link graph.
///
/// When `folder_metadata` is supplied, chunks whose `corpus_id`
/// matches a watched-folder corpus are emitted under a separate
/// "From your folders" section with a `[Folder: <display-name> — <title>]`
/// label. Folder-ingest v1 §6.3: gives the synthesis model an
/// explicit signal that a chunk came from the user's own corpus
/// (rather than a public knowledge base) so it can attribute
/// faithfully ("From your `case-files` folder, three documents…").
pub(crate) fn format_scored_chunks_with_kinds(
    chunks: &[ScoredChunk],
    max_chars: usize,
    kinds: Option<&HashMap<String, CorpusKind>>,
    contested: Option<&HashSet<String>>,
    folder_metadata: Option<&HashMap<String, FolderMetadata>>,
    display_categories: Option<&HashMap<String, String>>,
) -> String {
    format_scored_chunks_counted(
        chunks,
        max_chars,
        kinds,
        contested,
        folder_metadata,
        display_categories,
    )
    .text
}

/// [`format_scored_chunks_with_kinds`], but also reporting how many
/// chunks fit the budget — see [`FormattedChunks`]. This is the
/// function that owns the budget decision; the string-only variants
/// delegate here so there is exactly one place where a chunk is
/// admitted or dropped.
pub(crate) fn format_scored_chunks_counted(
    chunks: &[ScoredChunk],
    max_chars: usize,
    kinds: Option<&HashMap<String, CorpusKind>>,
    contested: Option<&HashSet<String>>,
    folder_metadata: Option<&HashMap<String, FolderMetadata>>,
    // corpus_id → `display.category` lookup. When a chunk's corpus
    // declares `category = "conversation"`, that chunk peels off the
    // generic trace bucket and renders under a dedicated
    // "## From your conversations" section. Other categories are
    // ignored today — the section rename is the only category-aware
    // synthesis hook in v1 of the conversation-imports landing.
    display_categories: Option<&HashMap<String, String>>,
) -> FormattedChunks {
    // Move 5 — sub-bucket the corpus bucket by the meta-atlas
    // articulation axis when chunks carry the `articulation`
    // metadata tag. Three new prompt sections render before the
    // existing "From knowledge base" catch-all so the synthesis
    // model sees the structural-map / articulated-claim /
    // lived-practice distinction.
    //
    // Conversation-imports landing — chunks from corpora declaring
    // `[display] category = "conversation"` are split out of the
    // generic trace bucket into a dedicated `conversation_parts`
    // section. Renamed prompt heading ("From your conversations")
    // signals to the synthesis model that the two conversation
    // corpora (`conversations-anthropic`, `conversation-history`)
    // belong to one logical pool regardless of which one served
    // each chunk.
    let mut corpus_inventory = Vec::new();
    let mut corpus_argument = Vec::new();
    let mut corpus_trace = Vec::new();
    let mut conversation_parts = Vec::new();
    let mut corpus_parts = Vec::new();
    let mut folder_parts = Vec::new();
    let mut web_parts = Vec::new();
    let mut catalog_parts = Vec::new();
    let mut total = 0;
    let mut admitted: Vec<(usize, String)> = Vec::new();

    for (idx, c) in chunks.iter().enumerate() {
        let is_catalog = matches!(
            kinds.and_then(|m| m.get(&c.corpus_id)),
            Some(CorpusKind::Catalog)
        );
        let folder_meta = folder_metadata.and_then(|m| m.get(&c.corpus_id));
        let is_conversation = display_categories
            .and_then(|m| m.get(&c.corpus_id))
            .map(|cat| cat == "conversation")
            .unwrap_or(false);
        let body = strip_leading_title_duplicate(&c.content, c.title.as_deref());
        let content = truncate_chunk_content(body);
        let title = c.title.as_deref().unwrap_or(c.corpus_id.as_str());
        let contested_suffix = contested
            .and_then(|set| {
                if c.title.as_deref().map(|t| set.contains(t)).unwrap_or(false) {
                    Some(" (contested)")
                } else {
                    None
                }
            })
            .unwrap_or("");

        // Articulation + stability tags, when present.
        let articulation_tag = c.metadata.get("articulation").map(String::as_str);
        let stability_tag = c.metadata.get("stability").map(String::as_str);
        let stability_suffix = match stability_tag {
            Some(s) => format!(" · {s}"),
            None => String::new(),
        };

        let (label, bucket) = if is_conversation && !is_catalog && c.url.is_none() {
            // Both conversation corpora — imported Claude chats AND
            // the user's Sovereign-internal chats — surface under
            // one synthesis-prompt section so the model treats them
            // as one logical "your conversations" pool. The
            // corpus_id is dropped from the label since the user
            // perceives them as one source.
            (
                format!("[Your conversations — {title}{contested_suffix}]"),
                &mut conversation_parts,
            )
        } else if let Some(meta) = folder_meta {
            // Folder corpora win precedence over Catalog/Web/articulation
            // — a folder is by definition the user's own material and
            // the synthesis register changes accordingly.
            (
                format!(
                    "[Folder: {} — {title}{contested_suffix}{stability_suffix}]",
                    meta.display_name
                ),
                &mut folder_parts,
            )
        } else if is_catalog {
            (
                format!("[Catalog: {title}{contested_suffix}]"),
                &mut catalog_parts,
            )
        } else if c.url.is_some() && c.chunk_id.is_none() {
            // `[Web:]` ONLY for genuine live web-fetch results: a URL but
            // no corpus handle (`chunk_id`). An INSTALLED-CORPUS chunk
            // (sep, wikipedia, …) also carries its source-article URL, but
            // it is NOT a web result — it has a `chunk_id` and falls through
            // to the `[Source:]`/articulation headers below. Without this
            // guard, corpus passages were headed `[Web: title]`, so the
            // model faithfully cited them as `[Web:]`, misrepresenting a
            // local-corpus answer as web search (and the discriminator now
            // matches `projection::project_citation`'s corpus-grounded rule).
            (format!("[Web: {title}{contested_suffix}]"), &mut web_parts)
        } else if let Some(axis) = articulation_tag {
            // Meta-atlas-tagged corpus chunk — sub-bucket by axis.
            let bucket = match axis {
                "inventory" => &mut corpus_inventory,
                "argument" => &mut corpus_argument,
                "trace" => &mut corpus_trace,
                _ => &mut corpus_parts,
            };
            (
                format!(
                    "[{}: {title}{contested_suffix}{stability_suffix}]",
                    c.corpus_id
                ),
                bucket,
            )
        } else {
            (
                format!("[Source: {title}{contested_suffix}]"),
                &mut corpus_parts,
            )
        };

        let part = format!("{label}\n{content}");
        let part_len = part.len() + 5; // account for separator

        if total + part_len > max_chars {
            break;
        }

        total += part_len;
        // `content`, not `part`: the label is the formatter's framing,
        // the body is the evidence. A downstream presence judge should
        // be asked about the passage the model read, not about the
        // `[Source: …]` header wrapped around it.
        admitted.push((idx, content));
        bucket.push(part);
    }

    // The budget dropped chunks retrieval had already paid for. Trace
    // it at debug so an operator reading a soak journal can tell "we
    // found 28" from "the model saw 8" without attaching a debugger
    // (§9.1) — this branch was previously silent, which is how the
    // pre-eviction counts got mistaken for delivered evidence.
    if admitted.len() < chunks.len() {
        tracing::debug!(
            target: "sovereign::retrieval",
            retrieved = chunks.len(),
            included = admitted.len(),
            dropped = chunks.len() - admitted.len(),
            chars_used = total,
            max_chars,
            "knowledge budget evicted chunks before the prompt"
        );
    }

    let mut sections = Vec::new();
    if !conversation_parts.is_empty() {
        sections.push(format!(
            "## From your conversations\n\n{}",
            conversation_parts.join("\n\n---\n\n")
        ));
    }
    if !folder_parts.is_empty() {
        sections.push(format!(
            "## From your folders\n\n{}",
            folder_parts.join("\n\n---\n\n")
        ));
    }
    if !corpus_inventory.is_empty() {
        sections.push(format!(
            "## Broad map (inventory)\n\n{}",
            corpus_inventory.join("\n\n---\n\n")
        ));
    }
    if !corpus_argument.is_empty() {
        sections.push(format!(
            "## Articulated claims (arguments)\n\n{}",
            corpus_argument.join("\n\n---\n\n")
        ));
    }
    if !corpus_trace.is_empty() {
        sections.push(format!(
            "## Lived practice (traces)\n\n{}",
            corpus_trace.join("\n\n---\n\n")
        ));
    }
    if !corpus_parts.is_empty() {
        sections.push(format!(
            "## From knowledge base\n\n{}",
            corpus_parts.join("\n\n---\n\n")
        ));
    }
    if !catalog_parts.is_empty() {
        sections.push(format!(
            "## CATALOG-AWARE SOURCES (metadata only — full text NOT yet ingested)\n\n{}",
            catalog_parts.join("\n\n---\n\n")
        ));
    }
    if !web_parts.is_empty() {
        sections.push(format!(
            "## From web search\n\n{}",
            web_parts.join("\n\n---\n\n")
        ));
    }

    FormattedChunks {
        text: if sections.is_empty() {
            String::new()
        } else {
            sections.join("\n\n")
        },
        admitted,
    }
}

/// Build the `(sources, coverage)` pair attached to
/// `ResponseProvenance` from a per-turn `source_map` plus the
/// snapshots of peer-attribution and folder-metadata.
///
/// `peer_attribution` decorates `from_peer`; `folder_meta` decorates
/// both `display_name` (so the chat surface renders the user-typed
/// label) and the coverage chip's enumerated thin folders. When no
/// folder corpus contributed retrieval, `coverage` is `None` and the
/// chat surface omits the chip entirely.
pub(crate) fn build_provenance_components(
    source_map: &HashMap<String, usize>,
    peer_attribution: &HashMap<String, String>,
    folder_meta: &HashMap<String, FolderMetadata>,
    // corpus_id → `display.category` lookup so the source's chat-UI
    // chip can render "your conversations" instead of a per-corpus
    // slug when the underlying corpus is a conversation source. None
    // (or empty) preserves the prior behaviour.
    display_categories: Option<&HashMap<String, String>>,
) -> (Vec<SourceSummary>, Option<CoverageNote>) {
    let mut sources: Vec<SourceSummary> = source_map
        .iter()
        .map(|(origin, &count)| {
            let from_peer = peer_attribution.get(origin).cloned();
            // Display label precedence:
            //   1. Folder-corpus user-typed display name (folder-ingest v1 §6.3)
            //   2. "Your conversations" for any corpus declaring
            //      `[display] category = "conversation"` — collapses
            //      the two conversation corpora into one label.
            //   3. None — chat UI falls back to corpus_id slug.
            let display_name = folder_meta
                .get(origin)
                .map(|m| m.display_name.clone())
                .or_else(|| {
                    display_categories
                        .and_then(|m| m.get(origin))
                        .and_then(|cat| {
                            if cat == "conversation" {
                                Some("Your conversations".to_string())
                            } else {
                                None
                            }
                        })
                });
            SourceSummary {
                origin: origin.clone(),
                count,
                from_peer,
                display_name,
            }
        })
        .collect();
    // Stable order so message-metadata diffs and tests don't churn.
    sources.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.origin.cmp(&b.origin)));

    let mut thin_folders: Vec<ThinFolder> = source_map
        .iter()
        .filter_map(|(origin, &count)| {
            let meta = folder_meta.get(origin)?;
            if count >= FOLDER_THIN_COVERAGE_THRESHOLD {
                return None;
            }
            Some(ThinFolder {
                corpus_id: origin.clone(),
                display_name: meta.display_name.clone(),
                chunks: count,
                skipped_files: meta.skipped_count,
                failed_files: meta.failed_count,
            })
        })
        .collect();
    thin_folders.sort_by(|a, b| {
        a.chunks
            .cmp(&b.chunks)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });

    let coverage = if thin_folders.is_empty() {
        None
    } else {
        Some(CoverageNote {
            kind: "thin".to_string(),
            thin_threshold: FOLDER_THIN_COVERAGE_THRESHOLD,
            thin_folders,
        })
    };

    (sources, coverage)
}

/// Build a "what I don't have" prompt-time note for any folder
/// corpora that contributed retrieval AND have non-zero
/// `failed_files` / `skipped_by_extension`. Empty string when every
/// matched folder is fully indexed.
///
/// Folder-ingest v1 §6.3: the model should be honest about the
/// user's coverage gap (encrypted PDFs, unsupported formats) the
/// moment it might affect the answer. Putting this in the synthesis
/// system message — capped at the top two folders by gap magnitude
/// — keeps the prompt budget bounded while making the gap legible
/// without the user having to dig through the folder-detail UI.
pub(crate) fn build_coverage_gaps_note(
    chunks: &[ScoredChunk],
    folder_meta: &HashMap<String, FolderMetadata>,
) -> String {
    let mut by_id: BTreeMap<String, &FolderMetadata> = BTreeMap::new();
    for c in chunks {
        let Some(m) = folder_meta.get(&c.corpus_id) else {
            continue;
        };
        if m.failed_count == 0 && m.skipped_count == 0 {
            continue;
        }
        by_id.insert(c.corpus_id.clone(), m);
    }
    if by_id.is_empty() {
        return String::new();
    }
    let mut ranked: Vec<(String, &FolderMetadata)> = by_id.into_iter().collect();
    ranked.sort_by(|a, b| {
        let total_a = a.1.skipped_count + a.1.failed_count;
        let total_b = b.1.skipped_count + b.1.failed_count;
        total_b
            .cmp(&total_a)
            .then_with(|| a.1.display_name.cmp(&b.1.display_name))
    });
    ranked.truncate(2);

    let mut lines = Vec::new();
    for (_id, m) in ranked {
        let mut bits = Vec::new();
        if m.skipped_count > 0 {
            let ext_part = if m.top_skipped_extensions.is_empty() {
                String::new()
            } else {
                let exts: Vec<String> = m
                    .top_skipped_extensions
                    .iter()
                    .map(|e| format!(".{e}"))
                    .collect();
                format!(" ({})", exts.join(", "))
            };
            bits.push(format!(
                "{} files in unsupported formats{}",
                m.skipped_count, ext_part
            ));
        }
        if m.failed_count > 0 {
            bits.push(format!("{} files we couldn't extract", m.failed_count));
        }
        lines.push(format!(
            "- Their \"{}\" folder has {}.",
            m.display_name,
            bits.join(", ")
        ));
    }
    format!(
        "GAP NOTE — what the user has but you don't see:\n{}\n\
         If the answer might depend on these gaps, mention them honestly.",
        lines.join("\n"),
    )
}

#[cfg(test)]
mod folder_attribution_tests {
    use super::*;
    // The per-chunk cap the formatter applies — imported rather than
    // restated so the truncation test cannot drift from the value the
    // prompt actually uses (§10.6).
    use crate::runtime::text_utils::MAX_CHUNK_CHARS;

    fn folder(name: &str, failed: usize, skipped: usize, top_ext: &[&str]) -> FolderMetadata {
        FolderMetadata {
            display_name: name.to_string(),
            failed_count: failed,
            skipped_count: skipped,
            top_skipped_extensions: top_ext.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn chunk(corpus_id: &str, title: &str) -> ScoredChunk {
        ScoredChunk {
            content: "body".into(),
            title: Some(title.into()),
            url: None,
            corpus_id: corpus_id.into(),
            score: 0.5,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    /// The budget-eviction accounting must be OBSERVABLE, and it must
    /// be observable in the failing direction — a test that only ever
    /// sees `admitted.len() == chunks.len()` proves nothing, because
    /// the bug being guarded is silence about the DROPPED chunks
    /// (`ARCH_PRINCIPLES.md` §18.1: a check with no failing input you
    /// can name is not a check).
    ///
    /// Named failing input: 10 chunks whose rendered parts each cost
    /// well over 100 chars, against a 300-char budget. If the accounting
    /// ever reports all 10 as delivered, the soak journal's evidence
    /// counts silently overstate what the model saw — the exact defect
    /// that made the 2026-08-06 baseline incomparable.
    #[test]
    fn budget_eviction_is_reported_not_silent() {
        let chunks: Vec<ScoredChunk> = (0..10)
            .map(|i| {
                let mut c = chunk("corpus-a", &format!("Title {i}"));
                c.content = "x".repeat(200);
                c
            })
            .collect();

        let tight = format_scored_chunks_counted(&chunks, 300, None, None, None, None);
        assert!(
            tight.admitted.len() < chunks.len(),
            "a 300-char budget must evict most of 10 chunks of ~200 chars each; \
             admitted {} of {}",
            tight.admitted.len(),
            chunks.len()
        );
        assert!(
            tight.admitted.len() > 0,
            "the budget must still admit the first chunk — admitting zero would \
             mean the model got no evidence at all, a different bug"
        );
        // The reported indices must correspond to real input positions,
        // so a consumer can tag exactly which chunks were delivered.
        assert!(
            tight.admitted.iter().all(|(i, _)| *i < chunks.len()),
            "admitted indices must index the input slice: {:?}",
            tight.admitted.iter().map(|(i, _)| *i).collect::<Vec<_>>()
        );
        // The reported body must be what the prompt carried, not the
        // chunk's full content — otherwise a consumer judging "did the
        // model have the answer?" reads text the model never saw, which
        // is the specific defect this field exists to close. Named
        // failing input: 200-char chunks are below MAX_CHUNK_CHARS, so
        // here the body is delivered whole; the truncating leg is
        // covered by `admitted_body_is_truncated_like_the_prompt`.
        assert!(
            tight
                .admitted
                .iter()
                .all(|(i, body)| body == &chunks[*i].content),
            "under the per-chunk cap the admitted body must equal the chunk content"
        );

        // Control leg: with a budget that cannot bite, nothing is
        // dropped. Without this, the assertion above would also pass on
        // a formatter that always reported one chunk.
        let roomy = format_scored_chunks_counted(&chunks, 1_000_000, None, None, None, None);
        assert_eq!(
            roomy.admitted.len(),
            chunks.len(),
            "an unbounded budget must admit every chunk"
        );
        assert_eq!(
            roomy.admitted.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            (0..chunks.len()).collect::<Vec<_>>(),
            "admitted indices must cover the whole input when nothing is evicted"
        );
    }

    /// The loss that actually bites is per-chunk TRUNCATION, not chunk
    /// eviction, and the two are easy to confuse — which is why this
    /// test exists separately from the eviction one.
    ///
    /// Named failing input, taken from the live soak baseline rather
    /// than invented: on 2026-08-06 the three most evidence-heavy chaos
    /// turns evicted ZERO chunks (0/20, 0/19, 0/20) while every passage
    /// exceeded the 600-char cap — one turn resolving 214,129 chars of
    /// evidence into a prompt that could hold at most 12,000. A
    /// consumer reading only `admitted.len()` would conclude the model
    /// received everything. So: one chunk far above the cap, a budget
    /// that cannot bite, and the assertion that the reported body is
    /// the TRUNCATED one.
    #[test]
    fn admitted_body_is_truncated_like_the_prompt() {
        let mut c = chunk("corpus-a", "Long passage");
        c.content = "y".repeat(MAX_CHUNK_CHARS * 8);
        let chunks = vec![c];

        let out = format_scored_chunks_counted(&chunks, 1_000_000, None, None, None, None);
        assert_eq!(out.admitted.len(), 1, "a roomy budget must admit the chunk");

        let (_, body) = &out.admitted[0];
        // Compared against `truncate_chunk_content` itself, not against a
        // hand-computed bound: the cap has an ellipsis convention (a word-
        // boundary cut plus "...") and restating it here would be a second
        // implementation of the same decider — the exact §10.6 smell the
        // whole `admitted` body exists to avoid.
        assert_eq!(
            body,
            &crate::runtime::text_utils::truncate_chunk_content(&chunks[0].content),
            "the admitted body must be exactly what the prompt's own truncation produced"
        );
        assert!(
            body.chars().count() <= MAX_CHUNK_CHARS + 4,
            "and that truncation must be bounded by the cap (plus its ellipsis), \
             got {} chars against a cap of {MAX_CHUNK_CHARS}",
            body.chars().count()
        );
        assert!(
            body.chars().count() < chunks[0].content.chars().count(),
            "this input is 8x the cap — a body equal to the full content would \
             mean the reported evidence is NOT what the prompt carried"
        );
        assert!(
            out.text.contains(body.as_str()),
            "the reported body must be the text that actually went into the prompt"
        );
    }

    #[test]
    fn corpus_chunk_with_url_heads_source_not_web() {
        // A corpus chunk carries its source-article URL but has a
        // chunk_id (corpus handle) — it must head `[Source:]`, never
        // `[Web:]`. A genuine web-fetch result (URL, no chunk_id) still
        // heads `[Web:]`. Regression: corpus passages were headed
        // `[Web: …]`, so the synthesis model cited local corpora as web.
        let corpus = ScoredChunk {
            content: "Compatibilism holds that…".into(),
            title: Some("incompatibilism-arguments".into()),
            url: Some("https://plato.stanford.edu/entries/incompatibilism".into()),
            corpus_id: "sep".into(),
            score: 0.9,
            metadata: HashMap::new(),
            chunk_id: Some(42),
            source_doc_id: None,
            vector_distance: None,
        };
        let web = ScoredChunk {
            content: "Breaking news…".into(),
            title: Some("Live Result".into()),
            url: Some("https://example.com/news".into()),
            corpus_id: "web".into(),
            score: 0.5,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        };
        let out = format_scored_chunks_with_kinds(&[corpus, web], 100_000, None, None, None, None);
        assert!(
            out.contains("[Source: incompatibilism-arguments]"),
            "corpus chunk WITH a URL must head [Source:], got:\n{out}"
        );
        assert!(
            !out.contains("[Web: incompatibilism-arguments]"),
            "corpus chunk must NOT be mislabeled [Web:]"
        );
        assert!(
            out.contains("[Web: Live Result]"),
            "genuine web-fetch (no chunk_id) must still head [Web:]"
        );
    }

    #[test]
    fn provenance_sources_carry_folder_display_name() {
        let mut source_map: HashMap<String, usize> = HashMap::new();
        source_map.insert("folder_abc".into(), 5);
        source_map.insert("sep".into(), 3);
        let mut folder_meta: HashMap<String, FolderMetadata> = HashMap::new();
        folder_meta.insert("folder_abc".into(), folder("Case Files", 0, 0, &[]));

        let (sources, coverage) =
            build_provenance_components(&source_map, &HashMap::new(), &folder_meta, None);
        let folder_src = sources
            .iter()
            .find(|s| s.origin == "folder_abc")
            .expect("folder source present");
        assert_eq!(folder_src.display_name.as_deref(), Some("Case Files"));
        let sep_src = sources.iter().find(|s| s.origin == "sep").unwrap();
        assert!(
            sep_src.display_name.is_none(),
            "non-folder corpora must not get a display_name"
        );
        // Above the threshold, no chip surfaces.
        assert!(coverage.is_none(), "5 chunks is above thin threshold");
    }

    #[test]
    fn thin_folder_triggers_coverage_chip() {
        let mut source_map: HashMap<String, usize> = HashMap::new();
        source_map.insert("folder_thin".into(), 1);
        source_map.insert("sep".into(), 12); // non-folder, irrelevant to chip
        let mut folder_meta: HashMap<String, FolderMetadata> = HashMap::new();
        folder_meta.insert("folder_thin".into(), folder("Research Notes", 0, 0, &[]));

        let (_sources, coverage) =
            build_provenance_components(&source_map, &HashMap::new(), &folder_meta, None);
        let cov = coverage.expect("thin folder must produce a CoverageNote");
        assert_eq!(cov.kind, "thin");
        assert_eq!(cov.thin_folders.len(), 1);
        assert_eq!(cov.thin_folders[0].display_name, "Research Notes");
        assert_eq!(cov.thin_folders[0].chunks, 1);
    }

    #[test]
    fn non_folder_corpora_never_trip_coverage() {
        let mut source_map: HashMap<String, usize> = HashMap::new();
        source_map.insert("sep".into(), 1);
        source_map.insert("wikipedia".into(), 0);
        // No folder_meta entries.
        let (_, coverage) =
            build_provenance_components(&source_map, &HashMap::new(), &HashMap::new(), None);
        assert!(
            coverage.is_none(),
            "non-folder corpora returning thin retrieval must NOT surface a chip"
        );
    }

    #[test]
    fn gap_note_enumerates_skipped_and_failed() {
        let chunks = vec![chunk("folder_gap", "doc.pdf")];
        let mut folder_meta: HashMap<String, FolderMetadata> = HashMap::new();
        folder_meta.insert(
            "folder_gap".into(),
            folder("Case Files", 2, 9, &["pages", "key"]),
        );
        let note = build_coverage_gaps_note(&chunks, &folder_meta);
        assert!(
            note.contains("\"Case Files\""),
            "must name the folder: {note}"
        );
        assert!(
            note.contains("9 files in unsupported formats"),
            "must enumerate skipped count: {note}"
        );
        assert!(note.contains(".pages"), "must list top extensions: {note}");
        assert!(
            note.contains("2 files we couldn't extract"),
            "must enumerate failed count: {note}"
        );
    }

    #[test]
    fn gap_note_empty_when_folders_have_no_gaps() {
        let chunks = vec![chunk("folder_clean", "doc.pdf")];
        let mut folder_meta: HashMap<String, FolderMetadata> = HashMap::new();
        folder_meta.insert("folder_clean".into(), folder("Clean", 0, 0, &[]));
        let note = build_coverage_gaps_note(&chunks, &folder_meta);
        assert!(note.is_empty(), "no gaps means no prompt overhead");
    }

    #[test]
    fn gap_note_caps_at_two_folders() {
        let chunks = vec![
            chunk("a", "x.pdf"),
            chunk("b", "y.pdf"),
            chunk("c", "z.pdf"),
        ];
        let mut folder_meta: HashMap<String, FolderMetadata> = HashMap::new();
        folder_meta.insert("a".into(), folder("Aaa", 1, 1, &["one"]));
        folder_meta.insert("b".into(), folder("Bbb", 1, 1, &["two"]));
        folder_meta.insert("c".into(), folder("Ccc", 99, 99, &["three"]));
        let note = build_coverage_gaps_note(&chunks, &folder_meta);
        // Highest-magnitude folder must appear; one of the smaller two
        // is dropped to keep the prompt overhead bounded.
        assert!(
            note.contains("\"Ccc\""),
            "highest-gap folder must be present: {note}"
        );
        let appears = ["\"Aaa\"", "\"Bbb\""]
            .iter()
            .filter(|s| note.contains(*s))
            .count();
        assert!(
            appears <= 1,
            "at most one of the two smaller folders should appear; note={note}"
        );
    }
}

#[cfg(test)]
mod formatter_stream_section_tests {
    //! Move 5 Stage 5 — formatter sub-buckets the corpus bucket by
    //! `metadata["articulation"]` so the synthesis model sees the
    //! three streams as named sections. Chunks without the tag fall
    //! through to the catch-all "From knowledge base" section
    //! (no-regression for un-meta-tagged retrieval).

    use super::format_scored_chunks_with_kinds;
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(
        corpus: &str,
        title: &str,
        content: &str,
        axis: Option<&str>,
        stab: Option<&str>,
    ) -> ScoredChunk {
        let mut metadata = HashMap::new();
        if let Some(a) = axis {
            metadata.insert("articulation".into(), a.into());
        }
        if let Some(s) = stab {
            metadata.insert("stability".into(), s.into());
        }
        ScoredChunk {
            content: content.into(),
            title: Some(title.into()),
            url: None,
            corpus_id: corpus.into(),
            score: 1.0,
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn meta_atlas_tags_produce_three_sub_sections() {
        let chunks = vec![
            chunk(
                "wikipedia",
                "Albert Einstein",
                "Einstein was a German-born theoretical physicist…",
                Some("inventory"),
                Some("frozen"),
            ),
            chunk(
                "sep-einstein-philscience",
                "Einstein's Philosophy of Science",
                "Einstein argued that physics must explain…",
                Some("argument"),
                Some("frozen"),
            ),
            chunk(
                "conversation-history",
                "Yesterday's discussion",
                "We talked about Einstein's gravity earlier…",
                Some("trace"),
                Some("rolling"),
            ),
        ];
        let out = format_scored_chunks_with_kinds(&chunks, 4096, None, None, None, None);
        assert!(out.contains("## Broad map (inventory)"));
        assert!(out.contains("## Articulated claims (arguments)"));
        assert!(out.contains("## Lived practice (traces)"));
        // Each section's header includes [corpus_id ... · stability].
        assert!(out.contains("[wikipedia: Albert Einstein · frozen]"));
        assert!(
            out.contains("[sep-einstein-philscience: Einstein's Philosophy of Science · frozen]")
        );
        assert!(out.contains("[conversation-history: Yesterday's discussion · rolling]"));
        // Catch-all bucket is NOT rendered (no untagged corpus chunks).
        assert!(!out.contains("## From knowledge base"));
    }

    #[test]
    fn untagged_chunks_fall_through_to_catch_all() {
        let chunks = vec![chunk("wikipedia", "Some article", "Body", None, None)];
        let out = format_scored_chunks_with_kinds(&chunks, 4096, None, None, None, None);
        assert!(out.contains("## From knowledge base"));
        assert!(!out.contains("## Broad map"));
    }

    #[test]
    fn mixed_tagged_and_untagged_render_both_buckets() {
        let chunks = vec![
            chunk("wiki", "Tagged", "x", Some("inventory"), Some("frozen")),
            chunk("wiki", "Untagged", "y", None, None),
        ];
        let out = format_scored_chunks_with_kinds(&chunks, 4096, None, None, None, None);
        assert!(out.contains("## Broad map (inventory)"));
        assert!(out.contains("## From knowledge base"));
    }

    /// Today-anchor unit tests. Pins the contract that the system
    /// message + refine prompt both surface a current-date line plus
    /// shape-level recency-reasoning discipline.
    mod today_anchor_tests {
        use crate::runtime::today_anchor_block;

        #[test]
        fn renders_the_supplied_iso_date_verbatim() {
            let out = today_anchor_block("2026-05-19");
            assert!(
                out.contains("Current date: 2026-05-19."),
                "expected anchor line, got: {out}"
            );
        }

        #[test]
        fn includes_recency_reasoning_discipline() {
            // SHAPE-level guidance only — these phrases are about
            // "compare to today, flag stale" patterns, not specific
            // products or events. Per `feedback_no_teaching_to_test`
            // the discipline lives in the prompt; bank vocabulary
            // does not.
            let out = today_anchor_block("2026-05-19");
            assert!(out.contains("compare"), "missing compare-instruction");
            assert!(
                out.contains("date that has already passed"),
                "missing stale-prediction handling"
            );
            assert!(
                out.contains("event happened") || out.contains("prediction was wrong"),
                "missing the two-paths instruction"
            );
        }
    }
}
