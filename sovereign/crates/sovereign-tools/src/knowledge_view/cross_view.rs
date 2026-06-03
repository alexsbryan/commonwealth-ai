//! Cross-view resonance detection.
//!
//! A theme that appears in one view is often connected to a theme
//! in another: the "meaningful work" question in personal memory
//! resonates with the "purpose of this project" question in
//! conversation history; an autonomy thread in inner-work sessions
//! resonates with governance decisions in the institutional notes.
//! These connections are exactly what a flat per-view digest cannot
//! see, and they're the feature that distinguishes KnowledgeView
//! from "a database with FTS bolted on".
//!
//! ## Design tension
//!
//! Per the spec: *"The connection is surfaced as a question, not a
//! conclusion."* The digest does not assert that two themes ARE
//! the same concern — it flags them as *possibly connected inquiry*
//! for the person to accept or reject. We enforce this by:
//!
//! 1. Only surfacing matches above a conservative similarity
//!    threshold (default 0.75 cosine similarity on BPE embeddings —
//!    strong enough that most matches are meaningful, weak enough
//!    that genuinely connected themes worded differently still show).
//! 2. Framing the output with "may resonate with" / "possibly
//!    connected" language, never "is about".
//! 3. Showing at most N matches; overwhelming the landscape with
//!    tentative connections would read as surveillance.
//!
//! ## Privacy
//!
//! The matching runs AFTER `splice_into`'s skill-based suppression:
//! if `conversation-history` is suppressed for a `local_only`
//! active skill, those items never enter the match set. The
//! cross-view digest therefore never leaks
//! conversational context into an inner-work session.
//!
//! ## Performance
//!
//! Embeddings are computed via the injected `EmbedFn`. For a
//! typical 3-view setup with ~10 items each, that's ~30 embed
//! calls per cross-view computation. Results are cached keyed by
//! a composite of each source skeleton's mtime; a cache hit
//! skips the embeddings entirely. The cache invalidates
//! automatically when any source view re-enriches.

use std::collections::HashMap;

use corpus_engine::enrichment::skeleton::FieldSkeleton;
use corpus_engine::error::Result as CorpusResult;
use corpus_engine::EmbedFn;

/// One item extracted from a view's skeleton for cross-view
/// matching. The item's text is what gets embedded; the view and
/// kind are carried alongside for display formatting.
#[derive(Debug, Clone)]
pub(crate) struct CrossViewItem {
    pub view_id: String,
    pub text: String,
    pub kind: CrossViewItemKind,
}

/// What aspect of a view's skeleton an item represents. Determines
/// how the item reads in the rendered digest ("theme", "open
/// question").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossViewItemKind {
    /// A canonical question — represents a cluster of thematic
    /// content the view has surfaced.
    ClusterTheme,
    /// An unresolved question the view has flagged as still-open.
    OpenQuestion,
}

impl CrossViewItemKind {
    fn label(self) -> &'static str {
        match self {
            Self::ClusterTheme => "theme",
            Self::OpenQuestion => "open question",
        }
    }
}

/// A match between two items in different views, ordered by
/// similarity. Used both in the rendered digest and in tests.
#[derive(Debug, Clone)]
pub(crate) struct CrossViewMatch {
    pub a: CrossViewItem,
    pub b: CrossViewItem,
    pub similarity: f32,
}

/// Similarity floor for reporting a match. Picked conservatively on
/// cl100k_base-like embeddings where 0.75 already filters out most
/// false positives while keeping "stability vs. autonomy" (personal)
/// ↔ "autonomy in governance design" (institutional) above the line.
pub(crate) const DEFAULT_MATCH_THRESHOLD: f32 = 0.75;

/// Cap on matches surfaced to the user per cross-view digest. The
/// digest is supposed to be a tentative signal, not an exhaustive
/// list.
pub(crate) const DEFAULT_TOP_N: usize = 5;

/// Extract matchable items from a skeleton. The order is:
///   1. Canonical questions (cluster themes)
///   2. Open questions
/// Empty-body items are dropped so the embedder doesn't waste calls.
pub(crate) fn extract_items(view_id: &str, skeleton: &FieldSkeleton) -> Vec<CrossViewItem> {
    let mut out = Vec::new();
    for q in &skeleton.canonical_questions {
        let text = q.question.trim();
        if !text.is_empty() {
            out.push(CrossViewItem {
                view_id: view_id.to_string(),
                text: text.to_string(),
                kind: CrossViewItemKind::ClusterTheme,
            });
        }
    }
    for oq in &skeleton.open_questions {
        let text = oq.question.trim();
        if !text.is_empty() {
            out.push(CrossViewItem {
                view_id: view_id.to_string(),
                text: text.to_string(),
                kind: CrossViewItemKind::OpenQuestion,
            });
        }
    }
    out
}

/// Embed every item using the provided `EmbedFn`. Results are
/// index-aligned with the input. Failed embeddings for a single
/// item do NOT abort the whole batch — the item is skipped via an
/// all-zero vector (which cosine_similarity correctly reports as 0
/// against any other vector, so it matches nothing).
pub(crate) async fn embed_items(items: &[CrossViewItem], embed: &EmbedFn) -> Vec<Vec<f32>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match (embed)(&item.text).await {
            Ok(v) => out.push(v),
            Err(e) => {
                tracing::debug!(
                    view_id = %item.view_id,
                    error = %e,
                    "cross_view: embed failed for item; skipping via zero vector"
                );
                out.push(Vec::new());
            }
        }
    }
    out
}

/// Find matches above `threshold` between items from different
/// views. Matches within a single view are excluded (intra-view
/// clustering is already the domain's job). Each cross-view pair
/// (ordered by view id) contributes at most one match per item on
/// side A: the single best partner on side B. This keeps the
/// digest from dumping five near-duplicates of the same pairing.
pub(crate) fn find_matches(
    items: &[CrossViewItem],
    embeddings: &[Vec<f32>],
    threshold: f32,
) -> Vec<CrossViewMatch> {
    debug_assert_eq!(items.len(), embeddings.len());
    let mut out: Vec<CrossViewMatch> = Vec::new();

    // Index items by view so we can iterate pairs of views, not
    // pairs of items (smaller search space per pass).
    let mut by_view: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        by_view.entry(item.view_id.clone()).or_default().push(i);
    }

    let mut view_ids: Vec<&String> = by_view.keys().collect();
    view_ids.sort(); // deterministic pair ordering

    for i in 0..view_ids.len() {
        for j in (i + 1)..view_ids.len() {
            let a_idxs = &by_view[view_ids[i]];
            let b_idxs = &by_view[view_ids[j]];

            for &ai in a_idxs {
                let mut best: Option<(usize, f32)> = None;
                for &bi in b_idxs {
                    let sim = cosine_similarity(&embeddings[ai], &embeddings[bi]);
                    if sim < threshold {
                        continue;
                    }
                    match best {
                        Some((_, cur)) if cur >= sim => {}
                        _ => best = Some((bi, sim)),
                    }
                }
                if let Some((bi, sim)) = best {
                    out.push(CrossViewMatch {
                        a: items[ai].clone(),
                        b: items[bi].clone(),
                        similarity: sim,
                    });
                }
            }
        }
    }

    // Sort: highest similarity first.
    out.sort_by(|l, r| {
        r.similarity
            .partial_cmp(&l.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Format a list of matches into the digest body. Returns `None`
/// when no matches survive the budget so `splice_into` can skip
/// inserting an empty section.
///
/// Output shape matches the rest of the landscape digest — a
/// header plus `— body` bullets under a `Cross-view connections:`
/// section. The per-match phrasing is deliberately tentative.
pub(crate) fn format_digest(
    matches: &[CrossViewMatch],
    budget_tokens: usize,
    estimate_tokens: impl Fn(&str) -> usize,
) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("Cross-view connections:\n\n  (possible connected inquiries — not assertions)\n");

    let mut shown = 0usize;
    for m in matches.iter().take(DEFAULT_TOP_N) {
        let line = format!(
            "    — {:?} \"{a_text}\" ({a_view}) may resonate with {b_kind} \"{b_text}\" ({b_view})\n",
            m.a.kind.label(),
            a_text = m.a.text,
            a_view = friendly_view(&m.a.view_id),
            b_kind = m.b.kind.label(),
            b_text = m.b.text,
            b_view = friendly_view(&m.b.view_id),
        );
        if estimate_tokens(&out) + estimate_tokens(&line) > budget_tokens {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    if shown == 0 {
        // No bullets fit — don't emit a header-only section.
        return None;
    }
    Some(out)
}

fn friendly_view(view_id: &str) -> &'static str {
    // Single source of truth lives on `ViewKind`. Unknown ids fall
    // through to "other" rather than propagating a string that would
    // look out of place in the rendered digest.
    super::view_kind::ViewKind::from_id(view_id)
        .map(|k| k.short_label())
        .unwrap_or("other")
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let (x, y) = (x as f64, y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

/// Top-level entry point a `KnowledgeViewManager` calls to generate
/// the cross-view digest body. Returns `None` when fewer than two
/// views have enriched skeletons available, when no matches clear
/// the threshold, or when every match gets truncated by the token
/// budget.
///
/// `skeletons` is keyed by view_id and must already have been
/// filtered by splice_into's skill-aware suppression logic —
/// callers must not pass a skeleton for a view that's supposed to
/// be invisible in the current session.
pub(crate) async fn build_cross_view_digest(
    skeletons: &[(String, FieldSkeleton)],
    embed: &EmbedFn,
    budget_tokens: usize,
    threshold: f32,
    estimate_tokens: impl Fn(&str) -> usize,
) -> CorpusResult<Option<String>> {
    if skeletons.len() < 2 {
        return Ok(None);
    }

    let mut items: Vec<CrossViewItem> = Vec::new();
    for (view_id, sk) in skeletons {
        items.extend(extract_items(view_id, sk));
    }
    if items.len() < 2 {
        return Ok(None);
    }

    let embeddings = embed_items(&items, embed).await;
    let matches = find_matches(&items, &embeddings, threshold);

    // Glassbox: summarise the decision. `tracing=debug` is enough for
    // an operator to answer "why did theme X resonate with theme Y?"
    // without logging raw embedding vectors. Each accepted match also
    // emits a trace-level line so the full set is recoverable when
    // someone flips on trace for an investigation.
    tracing::debug!(
        threshold,
        input_views = skeletons.len(),
        input_items = items.len(),
        accepted_matches = matches.len(),
        budget_tokens,
        "cross_view: match decisions"
    );
    for m in &matches {
        tracing::trace!(
            similarity = m.similarity,
            a_view = %m.a.view_id,
            a_text = %m.a.text,
            b_view = %m.b.view_id,
            b_text = %m.b.text,
            "cross_view: accepted match"
        );
    }

    Ok(format_digest(&matches, budget_tokens, estimate_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::clustering::FieldModelStats;
    use corpus_engine::enrichment::skeleton::{
        CanonicalQuestion, SkeletonFaultLine, SkeletonOpenQuestion, SkeletonPosition,
    };

    fn fixture_skeleton(corpus_id: &str, domain_id: &str) -> FieldSkeleton {
        FieldSkeleton {
            schema_version: 1,
            corpus_id: corpus_id.into(),
            generated_at: "2026-04-20T00:00:00Z".into(),
            extraction_method: "test".into(),
            prompt_version: "v1".into(),
            domain_id: domain_id.into(),
            canonical_questions: vec![CanonicalQuestion {
                id: "q1".into(),
                question: "What does meaningful work look like?".into(),
                status: "contested".into(),
                question_type: "normative".into(),
                primary_entries: vec![],
                positions: vec![SkeletonPosition {
                    id: "p1".into(),
                    name: "Purpose-driven".into(),
                    claim: "meaningful work serves others".into(),
                    status: "held".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                }],
                fault_lines: vec![SkeletonFaultLine {
                    id: "f1".into(),
                    between_positions: vec!["p1".into(), "p2".into()],
                    crux: "stability vs. autonomy".into(),
                    key_chunk_ids: vec![],
                    confidence: 0.8,
                    source: "detected".into(),
                    resolution_condition: None,
                }],
            }],
            open_questions: vec![SkeletonOpenQuestion {
                id: "oq1".into(),
                question: "what kind of life do I actually want".into(),
                status: "open".into(),
                related_question_id: None,
                representative_chunk_ids: vec![],
            }],
            field_stats: FieldModelStats::default(),
        }
    }

    #[test]
    fn extract_items_gathers_cluster_themes_and_open_questions() {
        let sk = fixture_skeleton("personal-knowledge", "personal");
        let items = extract_items("personal-knowledge", &sk);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, CrossViewItemKind::ClusterTheme);
        assert!(items[0].text.contains("meaningful work"));
        assert_eq!(items[1].kind, CrossViewItemKind::OpenQuestion);
    }

    #[test]
    fn extract_items_drops_empty_strings() {
        let mut sk = fixture_skeleton("x", "personal");
        sk.canonical_questions[0].question = "   ".into();
        let items = extract_items("x", &sk);
        // The empty cluster theme drops; the open question survives.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, CrossViewItemKind::OpenQuestion);
    }

    #[test]
    fn cosine_similarity_identity_is_one() {
        let v = vec![0.1f32, 0.2, 0.3];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-4);
    }

    #[test]
    fn cosine_similarity_handles_zero_vec() {
        let a = vec![0.0f32; 4];
        let b = vec![0.1f32, 0.2, 0.3, 0.4];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_rejects_mismatched_lengths() {
        assert_eq!(cosine_similarity(&[0.1f32, 0.2], &[0.1f32, 0.2, 0.3]), 0.0);
    }

    #[test]
    fn find_matches_only_crosses_views() {
        // Two items per view; each pair of same-view items has
        // identical text so same-view cosine sim is 1.0. But the
        // matcher must NOT emit intra-view matches.
        let items = vec![
            CrossViewItem {
                view_id: "A".into(),
                text: "x".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            CrossViewItem {
                view_id: "A".into(),
                text: "x".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            CrossViewItem {
                view_id: "B".into(),
                text: "x".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
        ];
        let embeddings = vec![vec![1.0, 0.0], vec![1.0, 0.0], vec![1.0, 0.0]];
        let matches = find_matches(&items, &embeddings, 0.5);
        // Expect A↔B matches only; no A↔A.
        assert!(matches.iter().all(|m| m.a.view_id != m.b.view_id));
    }

    #[test]
    fn find_matches_keeps_only_best_partner_per_item() {
        // Item A0 has two B candidates; B1 is closer than B0. Only
        // the B1 match should survive.
        let items = vec![
            CrossViewItem {
                view_id: "A".into(),
                text: "subject".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            CrossViewItem {
                view_id: "B".into(),
                text: "weak match".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            CrossViewItem {
                view_id: "B".into(),
                text: "strong match".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
        ];
        let embeddings = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.8, 0.6, 0.0],   // sim with A0 ≈ 0.8
            vec![0.99, 0.14, 0.0], // sim with A0 ≈ 0.99
        ];
        let matches = find_matches(&items, &embeddings, 0.5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].b.text, "strong match");
    }

    #[test]
    fn find_matches_threshold_filters_low_similarity() {
        let items = vec![
            CrossViewItem {
                view_id: "A".into(),
                text: "alpha".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            CrossViewItem {
                view_id: "B".into(),
                text: "beta".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
        ];
        // Near-orthogonal embeddings → similarity well below 0.75.
        let embeddings = vec![vec![1.0, 0.0], vec![0.2, 1.0]];
        let matches = find_matches(&items, &embeddings, 0.75);
        assert!(matches.is_empty(), "low-similarity pair must be filtered");
    }

    #[test]
    fn format_digest_frames_matches_tentatively() {
        // Fabricate a match and verify the output uses the
        // "possibly connected" language the spec requires.
        let matches = vec![CrossViewMatch {
            a: CrossViewItem {
                view_id: super::super::manager::VIEW_PERSONAL_KNOWLEDGE.into(),
                text: "meaningful work".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            b: CrossViewItem {
                view_id: super::super::manager::VIEW_INSTITUTIONAL_NOTES.into(),
                text: "purpose of this project".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            similarity: 0.88,
        }];
        let body =
            format_digest(&matches, 300, |s| s.split_whitespace().count()).expect("match formats");
        assert!(body.contains("Cross-view connections"));
        assert!(body.contains("possible connected inquiries"));
        assert!(body.contains("may resonate with"));
        assert!(body.contains("meaningful work"));
        assert!(body.contains("purpose of this project"));
        assert!(body.contains("personal"));
        assert!(body.contains("institutional"));
        // Must NOT assert — the match is a possibility, not a fact.
        assert!(!body.contains("is about"));
        assert!(!body.contains("is the same"));
    }

    #[test]
    fn format_digest_returns_none_for_empty_matches() {
        assert!(format_digest(&[], 300, |_| 0).is_none());
    }

    #[test]
    fn format_digest_returns_none_when_budget_too_tight() {
        let matches = vec![CrossViewMatch {
            a: CrossViewItem {
                view_id: "personal-knowledge".into(),
                text: "long match text that overshoots budget by itself".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            b: CrossViewItem {
                view_id: "institutional-notes".into(),
                text: "more long match text also way over".into(),
                kind: CrossViewItemKind::ClusterTheme,
            },
            similarity: 0.9,
        }];
        // Token budget of 1 — can't fit even the header.
        let body = format_digest(&matches, 1, |s| s.split_whitespace().count());
        assert!(body.is_none());
    }
}
