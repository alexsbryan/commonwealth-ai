//! Conversation tiered-retrieval briefing surface.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Renders per-conversation T3 RAPTOR summaries into a prompt block
//! that sits *alongside* the standard chunk-formatted output from
//! [`runtime::format_scored_chunks_with_kinds`]. The model gets:
//!
//! - Conversation **overviews** (the conv title, which is the
//!   first-message-of-conversation truncated by the threaded_turns
//!   chunker — opt-3 in the spec, no LLM call to generate).
//! - **Cluster signposts** at level 0 of each conv's RAPTOR tree —
//!   "this conversation had a cluster about X, with these distinctive
//!   phrases" — so the synthesis model has scene-scale orientation
//!   beyond raw chunks.
//!
//! ## Adaptive policy
//!
//! Briefing budget scales with how concentrated the retrieval hits
//! are across conversations:
//!
//! - **Deep mode (top-3, full signposts)** — fires when the 3 most-hit
//!   conversations account for ≥ 70% of retrieved chunks. Reader
//!   intuition: "the user's question is about a small number of
//!   conversations, render those in detail."
//! - **Shallow mode (top-8, overview-only)** — fires otherwise. Reader
//!   intuition: "the user's question is broad (e.g. 'summarize my
//!   conversations about React'); render many shallowly so they get
//!   one-line context per source."
//!
//! The deep/shallow split is the only branch — there's no third mode.
//! When fewer than 3 (deep) or 8 (shallow) convs are in the hit set,
//! the cap collapses naturally.
//!
//! ## Architectural future
//!
//! Today this is conv-specific (reads from `conv_skeletons` /
//! `conv_raptor_nodes` directly). The shape implied by the public
//! functions — `briefing_for_source(corpus_id, source_doc_id)` and
//! `leaf_cluster_for_chunk(corpus_id, chunk_id)` — is the
//! `TieredRetrievalSurface` trait that future ports (vault, SEP,
//! corpus-wide RAPTOR) should impl. Consolidation deferred until
//! the second port lands and the trait shape is grounded in two
//! real consumers, not one. See `CONV_TIERED_PORT.md` §"Retrieval
//! surface — next session's trait" for the planned extraction.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::ScoredChunk;

use crate::conv_tiered::{
    ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredReader, VaultThemeRow,
};

/// Display-category strings that route through the tiered (RAPTOR +
/// chunk_entities + PPR) retrieval path. Watched folders join
/// conversations on this list because both populate the conv_raptor_*
/// / chunk_entities tables under the same shape — `conv_uuid` keyed
/// on `source_doc_id` in both cases (one RAPTOR tree per
/// conversation export, one per file inside a watched folder).
///
/// `"vault"` is the third category — Obsidian-style vault corpora,
/// added when the vault port replaced the legacy `obsidian_atlas`
/// pipeline with the tiered surface. Vault corpora reuse the
/// watched-folder dispatch shape (one RAPTOR tree per note) but
/// surface in the briefing as "Vault context" so the user sees the
/// right label.
pub const TIERED_DISPLAY_CATEGORIES: &[&str] = &["conversation", "watched_folder", "vault"];

/// True if `cat` names a tiered-enrichment-bearing corpus category.
/// Used by both `build_conv_tiered_briefings` and
/// `rerank_conv_chunks_via_ppr` to decide which chunks participate
/// in the tiered retrieval surface.
pub fn is_tiered_category(cat: &str) -> bool {
    TIERED_DISPLAY_CATEGORIES.iter().any(|c| *c == cat)
}

/// For a tiered category, return the conv_uuid key used to bucket
/// chunks into per-source graphs. Both conversations and watched
/// folders key by `source_doc_id`: conversation corpora put one
/// chat export per source_doc; watched-folder corpora put one
/// file per source_doc. The per-doc shape keeps RAPTOR trees and
/// PPR entity graphs scoped to a single topic source rather than
/// collapsing heterogeneous files into one bag.
///
/// `_corpus_id` is retained in the signature for future categories
/// that need it (and for symmetry with call sites that already
/// pass it); the watched-folder branch no longer reads it.
pub fn tiered_group_key<'a>(
    _category: &str,
    _corpus_id: &'a str,
    source_doc_id: Option<&'a str>,
) -> Option<&'a str> {
    source_doc_id
}

/// Renderable briefing for a single conversation. Carries the parts
/// the prompt-assembly path stitches together.
#[derive(Debug, Clone)]
pub struct ConvBriefing {
    pub conv_uuid: String,
    pub overview: String,
    pub hit_count: usize,
    pub chunk_count: i64,
    /// Top RAPTOR signposts for this conv, ordered most-distinctive
    /// first (highest cluster_coherence). Empty in shallow mode and
    /// for Tiny convs (synthetic node only — overview already carries
    /// the signal).
    pub signposts: Vec<ClusterSignpost>,
    /// Display category that drove this briefing's inclusion —
    /// `"conversation"` for chat exports, `"watched_folder"` for
    /// folder corpora. The renderer uses this to pick the header
    /// label ("Conversation context" vs "Watched folder context")
    /// and the per-bullet framing ("hits across N chunks" works
    /// uniformly).
    pub source_category: String,
}

/// One leaf-cluster signpost. Drives the bullet rendering inside a
/// `ConvBriefing`.
#[derive(Debug, Clone)]
pub struct ClusterSignpost {
    pub summary: String,
    pub primary_entities: Vec<String>,
    pub cluster_coherence: f32,
}

/// Render mode chosen by the adaptive policy. Public so the prompt
/// assembler can include the mode label in trace logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefingMode {
    /// 3 convs, full signposts. Fires when top-3 ≥ 70% of hits.
    Deep,
    /// 8 convs, overview-only. Default fallback.
    Shallow,
    /// No conv-tiered hits in the retrieval set; nothing to render.
    Empty,
}

impl BriefingMode {
    pub fn label(&self) -> &'static str {
        match self {
            BriefingMode::Deep => "deep_top3",
            BriefingMode::Shallow => "shallow_top8",
            BriefingMode::Empty => "empty",
        }
    }
}

/// Output of [`build_conv_tiered_briefings`]. The caller concatenates
/// `rendered` ahead of the regular chunk-formatted prompt block.
#[derive(Debug, Clone)]
pub struct ConvBriefingPayload {
    pub rendered: String,
    pub mode: BriefingMode,
    pub conv_count: usize,
    pub per_conv_hit_distribution: Vec<(String, usize)>,
}

impl ConvBriefingPayload {
    /// Empty payload — caller renders nothing.
    pub fn empty() -> Self {
        Self {
            rendered: String::new(),
            mode: BriefingMode::Empty,
            conv_count: 0,
            per_conv_hit_distribution: Vec::new(),
        }
    }
}

/// Build the conv-tiered briefing block from a retrieval hit set.
///
/// `display_categories` maps `corpus_id → category` (typically
/// `"conversation"` for conv corpora). Only chunks from corpora whose
/// category is `"conversation"` AND that carry a `source_doc_id` are
/// considered. Empty result → empty payload, no allocations.
///
/// The store is consulted only for the chosen conversations (top-3
/// or top-8), so a query that hits 50 different conversations still
/// only fetches state for 8 of them at most.
pub async fn build_conv_tiered_briefings(
    store: &Arc<dyn ConvTieredReader>,
    chunks: &[ScoredChunk],
    display_categories: Option<&HashMap<String, String>>,
) -> ConvBriefingPayload {
    let display = match display_categories {
        Some(d) => d,
        None => return ConvBriefingPayload::empty(),
    };

    // ── 1. Tally hits per (corpus_id, conv_uuid). ──────────────
    // Conversation hits get a key `(corpus_id, source_doc_id)`. We
    // tally across all conv corpora simultaneously — a query that
    // straddles `conversations-anthropic` and `conversations-personal`
    // surfaces both in one briefing block.
    let mut hits: HashMap<(String, String), usize> = HashMap::new();
    for c in chunks {
        let Some(category) = display.get(&c.corpus_id) else {
            continue;
        };
        if !is_tiered_category(category) {
            continue;
        }
        let Some(conv_uuid) = tiered_group_key(
            category,
            &c.corpus_id,
            c.source_doc_id.as_deref(),
        ) else {
            continue;
        };
        *hits
            .entry((c.corpus_id.clone(), conv_uuid.to_string()))
            .or_insert(0) += 1;
    }
    if hits.is_empty() {
        return ConvBriefingPayload::empty();
    }

    // ── 2. Sort by hit count, descending. ───────────────────────
    let mut ranked: Vec<((String, String), usize)> = hits.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let total_hits: usize = ranked.iter().map(|(_, n)| *n).sum();
    let top_3_hits: usize = ranked.iter().take(3).map(|(_, n)| *n).sum();
    let concentration = if total_hits > 0 {
        top_3_hits as f64 / total_hits as f64
    } else {
        0.0
    };

    // ── 3. Pick mode + slice. ──────────────────────────────────
    let mode = if concentration >= 0.70 {
        BriefingMode::Deep
    } else {
        BriefingMode::Shallow
    };
    let cap = match mode {
        BriefingMode::Deep => 3,
        BriefingMode::Shallow => 8,
        BriefingMode::Empty => 0,
    };
    let selected: Vec<((String, String), usize)> =
        ranked.iter().take(cap).cloned().collect();

    // ── 4. Bulk fetch conv_skeletons by corpus. ────────────────
    // Group selected by corpus so the IN-list per corpus stays tight.
    let mut by_corpus: HashMap<String, Vec<String>> = HashMap::new();
    for ((corpus_id, conv_uuid), _) in &selected {
        by_corpus
            .entry(corpus_id.clone())
            .or_default()
            .push(conv_uuid.clone());
    }
    let mut skeleton_map: HashMap<(String, String), ConvSkeletonRow> = HashMap::new();
    for (corpus_id, uuids) in &by_corpus {
        let rows = store
            .list_conv_skeletons_for_corpus(corpus_id, uuids)
            .await
            .unwrap_or_default();
        for row in rows {
            skeleton_map.insert((row.corpus_id.clone(), row.conv_uuid.clone()), row);
        }
    }

    // ── 5. Build per-conv briefing, fetching RAPTOR nodes only
    //      in Deep mode. ─────────────────────────────────────────
    let mut briefings: Vec<ConvBriefing> = Vec::with_capacity(selected.len());
    for ((corpus_id, conv_uuid), hit_count) in &selected {
        let skeleton = match skeleton_map.get(&(corpus_id.clone(), conv_uuid.clone())) {
            Some(s) if s.state == "Ready" || s.state == "MultiHopReady" => s,
            _ => continue,
        };
        let overview = skeleton
            .overview
            .clone()
            .unwrap_or_else(|| "(untitled conversation)".to_string());

        let signposts = if matches!(mode, BriefingMode::Deep) {
            fetch_signposts(store, corpus_id, conv_uuid).await
        } else {
            Vec::new()
        };

        // Stamp the source category so the renderer can pick the
        // right header. Falls back to "conversation" defensively —
        // every briefing here passed `is_tiered_category` upstream.
        let source_category = display
            .get(corpus_id)
            .cloned()
            .unwrap_or_else(|| "conversation".to_string());

        briefings.push(ConvBriefing {
            conv_uuid: conv_uuid.clone(),
            overview,
            hit_count: *hit_count,
            chunk_count: skeleton.chunk_count,
            signposts,
            source_category,
        });
    }

    // ── 6. Render. ─────────────────────────────────────────────
    let rendered = render_briefings(&briefings, mode);
    let conv_count = briefings.len();
    let per_conv_hit_distribution = selected
        .into_iter()
        .map(|((_, uuid), n)| (uuid, n))
        .collect();

    ConvBriefingPayload {
        rendered,
        mode,
        conv_count,
        per_conv_hit_distribution,
    }
}

/// Render the vault-wide synthesis briefing block — "Vault themes"
/// surfacing cross-note clusters that intersect the retrieval hits.
/// Returns an empty string when there are no vault-category hits OR
/// when no themes intersect the hit set, so the caller can safely
/// `.push_str(&out)` without checking.
///
/// Mechanism mirrors `build_conv_tiered_briefings`: scan chunks for
/// vault-category corpora, collect the unique source_doc_ids hit per
/// vault, fetch the corpus's `vault_themes` (already ordered by
/// `cluster_coherence DESC` in storage), keep themes whose
/// `member_source_doc_ids` intersect the hit set, cap render count
/// to keep prompt budget bounded.
///
/// Stitches in alongside `ConvBriefingPayload.rendered` — the
/// caller writes:
///
/// ```ignore
/// let conv_block = build_conv_tiered_briefings(store, chunks, cats).await;
/// let vault_block = build_vault_synthesis_briefings(store, chunks, cats).await;
/// prompt.push_str(&conv_block.rendered);
/// prompt.push_str(&vault_block);
/// ```
pub async fn build_vault_synthesis_briefings(
    store: &Arc<dyn ConvTieredReader>,
    chunks: &[ScoredChunk],
    display_categories: Option<&HashMap<String, String>>,
) -> String {
    // Max themes rendered per corpus. Keeps the synthesis block from
    // ballooning when a vault has dozens of marginally-related themes
    // that all intersect a broad query. Tight first, broad later — the
    // store already orders by cluster_coherence DESC.
    const MAX_THEMES_PER_CORPUS: usize = 5;

    let Some(display) = display_categories else {
        return String::new();
    };

    // Collect (corpus_id -> set<source_doc_id>) for vault-category hits.
    let mut vault_hits: HashMap<String, std::collections::HashSet<String>> =
        HashMap::new();
    for c in chunks {
        let Some(category) = display.get(&c.corpus_id) else {
            continue;
        };
        if category != "vault" {
            continue;
        }
        let Some(source_doc) = c.source_doc_id.as_deref() else {
            continue;
        };
        vault_hits
            .entry(c.corpus_id.clone())
            .or_default()
            .insert(source_doc.to_string());
    }
    if vault_hits.is_empty() {
        return String::new();
    }

    // For each vault corpus, fetch its themes + filter to those whose
    // member notes intersect the hit set.
    let mut matched: Vec<(String, VaultThemeRow, usize)> = Vec::new();
    for (corpus_id, hit_notes) in &vault_hits {
        let themes = match store.list_vault_themes_for_corpus(corpus_id).await {
            Ok(t) => t,
            Err(_) => continue,
        };
        let mut taken = 0usize;
        for theme in themes {
            let members: Vec<String> =
                serde_json::from_str(&theme.member_source_doc_ids_json)
                    .unwrap_or_default();
            let hit_overlap = members.iter().filter(|m| hit_notes.contains(*m)).count();
            if hit_overlap == 0 {
                continue;
            }
            matched.push((corpus_id.clone(), theme, hit_overlap));
            taken += 1;
            if taken >= MAX_THEMES_PER_CORPUS {
                break;
            }
        }
    }
    if matched.is_empty() {
        return String::new();
    }

    // Sort by hit-overlap DESC then coherence DESC so the themes the
    // query lit up most strongly come first, ties broken by store
    // ordering's coherence rank.
    matched.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.1.cluster_coherence.partial_cmp(&a.1.cluster_coherence)
                .unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out = String::from("## Vault themes\n");
    out.push_str(
        "Cross-note themes recurring across the retrieved vault notes. Use \
         these to ground synthesis in the vault's broader patterns, not just \
         the specific notes hit:\n\n",
    );
    for (_corpus, theme, hit_overlap) in matched {
        let member_count = serde_json::from_str::<Vec<String>>(
            &theme.member_source_doc_ids_json,
        )
        .map(|v| v.len())
        .unwrap_or(0);
        out.push_str(&format!(
            "- **{}** — {} hit note{} of {} member{}\n",
            sanitize_overview(theme.summary.trim()),
            hit_overlap,
            plural(hit_overlap),
            member_count,
            plural(member_count),
        ));
    }
    out.push('\n');
    out
}

/// Pull the top leaf clusters (level 0) for one conv, ranked by
/// cluster_coherence descending so the most-distinctive signposts
/// surface first. Tiny convs return a single synthetic node — its
/// summary == overview, so render code skips it to avoid repetition.
async fn fetch_signposts(
    store: &Arc<dyn ConvTieredReader>,
    corpus_id: &str,
    conv_uuid: &str,
) -> Vec<ClusterSignpost> {
    let nodes = match store.list_conv_raptor_nodes(corpus_id, conv_uuid).await {
        Ok(n) => n,
        Err(_) => return Vec::new(),
    };
    // Filter to leaf clusters (level 0). For Tiny convs the only
    // node IS the synthetic node which has empty primary_entities
    // and cluster_coherence = 1.0 — render code dedupes it against
    // the overview.
    let mut leaves: Vec<&ConvRaptorNodeRow> =
        nodes.iter().filter(|n| n.level == 0).collect();
    leaves.sort_by(|a, b| {
        b.cluster_coherence
            .partial_cmp(&a.cluster_coherence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    leaves
        .into_iter()
        .take(4)
        .map(|row| ClusterSignpost {
            summary: row.summary.clone(),
            primary_entities: serde_json::from_str::<Vec<String>>(
                &row.primary_entities_json,
            )
            .unwrap_or_default(),
            cluster_coherence: row.cluster_coherence as f32,
        })
        .collect()
}

/// Format the briefings into a prompt block. Deep mode renders bullets
/// per cluster signpost; shallow mode renders one bullet per conv.
///
/// Header text adapts to the source categories present in the
/// briefing set — pure-conversation hits read "Conversation context",
/// pure-vault hits read "Vault context", mixed sets join the labels
/// with `&`. Deep-mode framing stays uniform because the rendering
/// shape ("source title — hits across chunks") works for every
/// tiered source type.
fn render_briefings(briefings: &[ConvBriefing], mode: BriefingMode) -> String {
    if briefings.is_empty() {
        return String::new();
    }
    let labels = source_labels_present(briefings);
    let header = format!("## {} context", join_with_and(&labels.singular));
    let deep_intro = format!(
        "These {} carry most of the retrieved chunks. Each is summarised \
         with its top cluster signposts so you can ground responses in \
         the source's own structure:\n\n",
        join_with_and(&labels.plural)
    );
    let shallow_intro = format!(
        "{} contributing to the retrieved chunks (ordered by hit count):\n\n",
        capitalise_first(&join_with_and(&labels.plural))
    );
    let mut s = String::new();
    s.push_str(&header);
    s.push('\n');
    match mode {
        BriefingMode::Deep => {
            s.push_str(&deep_intro);
            for b in briefings {
                s.push_str(&format!(
                    "**{}** — {} hit{} across {} chunk{}\n",
                    sanitize_overview(&b.overview),
                    b.hit_count,
                    plural(b.hit_count),
                    b.chunk_count,
                    plural(b.chunk_count as usize),
                ));
                for sp in &b.signposts {
                    // Drop synthetic-tiny nodes (summary == overview):
                    // they add noise without information.
                    if sp.summary.trim() == b.overview.trim() {
                        continue;
                    }
                    let entities = if sp.primary_entities.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", sp.primary_entities.join(", "))
                    };
                    s.push_str(&format!("  - {}{}\n", sp.summary.trim(), entities));
                }
                s.push('\n');
            }
        }
        BriefingMode::Shallow => {
            s.push_str(&shallow_intro);
            for b in briefings {
                s.push_str(&format!(
                    "- **{}** — {} hit{} across {} chunk{}\n",
                    sanitize_overview(&b.overview),
                    b.hit_count,
                    plural(b.hit_count),
                    b.chunk_count,
                    plural(b.chunk_count as usize),
                ));
            }
            s.push('\n');
        }
        BriefingMode::Empty => {}
    }
    s
}

/// Singular + plural human labels for each source-category present in
/// the briefing set. Keyed off the in-set categories so a brief that
/// only contains vault hits doesn't drag the word "conversation" into
/// the header. Order is stable: conversation → vault → watched folder
/// → other (alphabetical), so a mixed brief reads
/// "Conversation & vault context", not "Vault & conversation context".
struct SourceLabels {
    singular: Vec<&'static str>,
    plural: Vec<&'static str>,
}

fn source_labels_present(briefings: &[ConvBriefing]) -> SourceLabels {
    let mut seen: Vec<&str> = briefings
        .iter()
        .map(|b| b.source_category.as_str())
        .collect();
    seen.sort();
    seen.dedup();
    let ordering = |cat: &str| -> usize {
        match cat {
            "conversation" => 0,
            "vault" => 1,
            "watched_folder" => 2,
            _ => 3,
        }
    };
    seen.sort_by_key(|c| (ordering(c), *c));
    let mut singular = Vec::new();
    let mut plural = Vec::new();
    for cat in seen {
        let (s, p) = label_pair(cat);
        singular.push(s);
        plural.push(p);
    }
    SourceLabels { singular, plural }
}

fn label_pair(category: &str) -> (&'static str, &'static str) {
    match category {
        "conversation" => ("Conversation", "conversations"),
        "vault" => ("Vault", "vaults"),
        "watched_folder" => ("Watched folder", "watched folders"),
        // Unknown categories shouldn't reach the renderer (gated by
        // `is_tiered_category` upstream) but if one slips through,
        // fall back to a neutral generic label rather than panicking.
        _ => ("Source", "sources"),
    }
}

fn join_with_and(parts: &[&str]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        2 => format!("{} & {}", parts[0], parts[1]),
        n => {
            // 3+ items: Oxford-style comma list with `&` before the last.
            let head = parts[..n - 1].join(", ");
            format!("{}, & {}", head, parts[n - 1])
        }
    }
}

fn capitalise_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Briefings render conv titles inline; the threaded_turns chunker
/// truncates them at ~80 chars on ingest but a few may carry stray
/// whitespace, markdown, or newlines that would break the bullet
/// layout. One-pass sanitise: collapse whitespace, strip leading
/// markdown markers, cap at 140 chars with ellipsis.
fn sanitize_overview(raw: &str) -> String {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_start_matches(['#', '*', '-', ' ']);
    if trimmed.chars().count() > 140 {
        let truncated: String = trimmed.chars().take(137).collect();
        format!("{truncated}…")
    } else {
        trimmed.to_string()
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mk_chunk(corpus: &str, conv: Option<&str>) -> ScoredChunk {
        ScoredChunk {
            content: "x".into(),
            title: None,
            url: None,
            corpus_id: corpus.into(),
            score: 0.5,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: conv.map(|s| s.to_string()),
            vector_distance: None,
        }
    }

    #[test]
    fn empty_payload_when_no_chunks() {
        // Async-free check via the empty constructor itself.
        let p = ConvBriefingPayload::empty();
        assert_eq!(p.mode, BriefingMode::Empty);
        assert!(p.rendered.is_empty());
        assert_eq!(p.conv_count, 0);
    }

    #[test]
    fn sanitize_overview_strips_md_and_caps_length() {
        assert_eq!(sanitize_overview("  ## Title  "), "Title");
        assert_eq!(
            sanitize_overview("Multiple\nWhitespace   normalised"),
            "Multiple Whitespace normalised"
        );
        let long = "x".repeat(200);
        let out = sanitize_overview(&long);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 138);
    }

    #[test]
    fn plural_helper() {
        assert_eq!(plural(0), "s");
        assert_eq!(plural(1), "");
        assert_eq!(plural(2), "s");
    }

    #[test]
    fn briefing_mode_labels_stable() {
        assert_eq!(BriefingMode::Deep.label(), "deep_top3");
        assert_eq!(BriefingMode::Shallow.label(), "shallow_top8");
        assert_eq!(BriefingMode::Empty.label(), "empty");
    }

    fn mk_briefing(category: &str) -> ConvBriefing {
        ConvBriefing {
            conv_uuid: "u".into(),
            overview: "Untitled".into(),
            hit_count: 1,
            chunk_count: 1,
            signposts: Vec::new(),
            source_category: category.into(),
        }
    }

    #[test]
    fn vault_only_briefing_renders_vault_header() {
        let briefings = vec![mk_briefing("vault")];
        let out = render_briefings(&briefings, BriefingMode::Shallow);
        assert!(out.starts_with("## Vault context"), "header was: {out}");
        assert!(out.contains("Vaults contributing"), "intro was: {out}");
    }

    #[test]
    fn mixed_categories_render_in_canonical_order() {
        // Conversation comes before vault comes before watched_folder
        // by source_labels_present's ordering, regardless of insertion
        // order.
        let briefings = vec![
            mk_briefing("watched_folder"),
            mk_briefing("vault"),
            mk_briefing("conversation"),
        ];
        let out = render_briefings(&briefings, BriefingMode::Shallow);
        assert!(
            out.starts_with("## Conversation, Vault, & Watched folder context"),
            "header was: {out}"
        );
    }

    #[test]
    fn unknown_category_falls_back_to_neutral_label() {
        // Defensive: a category that slipped through (shouldn't happen
        // because is_tiered_category gates upstream) renders neutrally
        // instead of panicking.
        let briefings = vec![mk_briefing("alien_corpus")];
        let out = render_briefings(&briefings, BriefingMode::Shallow);
        assert!(out.starts_with("## Source context"), "header was: {out}");
    }

    #[test]
    fn vault_is_a_tiered_category() {
        assert!(is_tiered_category("vault"));
        assert!(is_tiered_category("conversation"));
        assert!(is_tiered_category("watched_folder"));
        assert!(!is_tiered_category("personal"));
    }

    /// Minimal `ConvTieredReader` for unit-testing the vault
    /// synthesis briefing render path. Returns whatever themes the
    /// test pre-loads; all other trait methods return empty results.
    #[derive(Default)]
    struct StubVaultReader {
        themes: HashMap<String, Vec<VaultThemeRow>>,
    }

    #[async_trait::async_trait]
    impl crate::conv_tiered::ConvTieredReader for StubVaultReader {
        async fn list_conv_skeletons_for_corpus(
            &self,
            _corpus_id: &str,
            _conv_uuids: &[String],
        ) -> crate::error::Result<Vec<ConvSkeletonRow>> {
            Ok(Vec::new())
        }
        async fn list_conv_raptor_nodes(
            &self,
            _corpus_id: &str,
            _conv_uuid: &str,
        ) -> crate::error::Result<Vec<ConvRaptorNodeRow>> {
            Ok(Vec::new())
        }
        async fn list_chunk_entities_for_conv(
            &self,
            _corpus_id: &str,
            _conv_uuid: &str,
        ) -> crate::error::Result<Vec<crate::conv_tiered::ChunkEntityRow>>
        {
            Ok(Vec::new())
        }
        async fn get_chunk_entity_progress(
            &self,
            _corpus_id: &str,
        ) -> crate::error::Result<
            Option<crate::conv_tiered::ChunkEntityProgressRow>,
        > {
            Ok(None)
        }
        async fn list_vault_themes_for_corpus(
            &self,
            corpus_id: &str,
        ) -> crate::error::Result<Vec<VaultThemeRow>> {
            Ok(self.themes.get(corpus_id).cloned().unwrap_or_default())
        }
    }

    fn mk_vault_chunk(corpus: &str, note: &str) -> ScoredChunk {
        mk_chunk(corpus, Some(note))
    }

    fn mk_theme(corpus: &str, theme_id: &str, summary: &str, members: &[&str], coherence: f32) -> VaultThemeRow {
        VaultThemeRow {
            corpus_id: corpus.into(),
            theme_id: theme_id.into(),
            summary: summary.into(),
            summary_embedding: vec![0.0; 4],
            member_source_doc_ids_json: serde_json::to_string(
                &members.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
            .unwrap(),
            cluster_coherence: coherence,
            created_at: 0,
        }
    }

    #[tokio::test]
    async fn vault_synthesis_renders_intersecting_themes() {
        let mut themes = HashMap::new();
        themes.insert(
            "vault-1".to_string(),
            vec![
                mk_theme("vault-1", "t0", "Markets vs commons", &["note-a", "note-b"], 0.9),
                mk_theme("vault-1", "t1", "Unrelated theme", &["note-z"], 0.85),
            ],
        );
        let reader: Arc<dyn ConvTieredReader> = Arc::new(StubVaultReader { themes });
        let mut cats = HashMap::new();
        cats.insert("vault-1".to_string(), "vault".to_string());
        let chunks = vec![mk_vault_chunk("vault-1", "note-a")];
        let out =
            build_vault_synthesis_briefings(&reader, &chunks, Some(&cats)).await;
        assert!(out.contains("## Vault themes"), "header missing: {out}");
        assert!(out.contains("Markets vs commons"), "theme missing: {out}");
        assert!(
            !out.contains("Unrelated theme"),
            "non-intersecting theme should not render: {out}"
        );
    }

    #[tokio::test]
    async fn vault_synthesis_returns_empty_with_no_vault_hits() {
        let reader: Arc<dyn ConvTieredReader> = Arc::new(StubVaultReader::default());
        let mut cats = HashMap::new();
        cats.insert("conv-1".to_string(), "conversation".to_string());
        let chunks = vec![mk_chunk("conv-1", Some("uuid"))];
        let out = build_vault_synthesis_briefings(&reader, &chunks, Some(&cats)).await;
        assert!(out.is_empty(), "non-vault hits should render nothing: {out}");
    }

    // Module-internal helper: deciding hit concentration given a chunk
    // distribution. The async build path uses this same arithmetic.
    fn mode_for_distribution(counts: &[usize]) -> BriefingMode {
        let total: usize = counts.iter().sum();
        if total == 0 {
            return BriefingMode::Empty;
        }
        let mut sorted = counts.to_vec();
        sorted.sort_by(|a, b| b.cmp(a));
        let top3: usize = sorted.iter().take(3).sum();
        let conc = top3 as f64 / total as f64;
        if conc >= 0.70 {
            BriefingMode::Deep
        } else {
            BriefingMode::Shallow
        }
    }

    #[test]
    fn deep_mode_when_top3_dominate() {
        // 90% of hits in top 3.
        assert_eq!(mode_for_distribution(&[10, 8, 7, 1, 1, 1, 1, 1]), BriefingMode::Deep);
    }

    #[test]
    fn shallow_mode_when_diffuse() {
        // Even spread across 10 convs — top-3 at 30%.
        assert_eq!(mode_for_distribution(&[3, 3, 3, 2, 2, 2, 2, 1, 1, 1]), BriefingMode::Shallow);
    }

    #[test]
    fn deep_when_only_3_convs_hit() {
        // Three convs, hit distribution doesn't matter — concentration = 1.0.
        assert_eq!(mode_for_distribution(&[5, 3, 1]), BriefingMode::Deep);
    }

    #[test]
    fn mk_chunk_helper_compiles() {
        // Smoke test — ensure the test scaffolding actually constructs
        // chunks (catches ScoredChunk field-add regressions in tests).
        let c = mk_chunk("conversations-anthropic", Some("abc-123"));
        assert_eq!(c.corpus_id, "conversations-anthropic");
        assert_eq!(c.source_doc_id.as_deref(), Some("abc-123"));
    }
}
