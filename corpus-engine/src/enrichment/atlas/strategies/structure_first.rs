//! `structure_first` — deterministic Wikipedia atlas ingestion.
//!
//! Walks an already-indexed Wikipedia corpus's LanceDB chunks and
//! emits atoms + edges from the structural metadata each chunk
//! carries (`section_path`, `outgoing_links`, `wikidata_qid`,
//! `page_id`). Pure Rust, no LLM calls, single pass with one
//! resolution sub-pass.
//!
//! What it produces (depth = `Structural`):
//!   - One [`Entity`] per article in the source corpus, with
//!     `first_appearance` pointing at the article's lead chunk and
//!     `description` holding the lead's first sentence (truncated).
//!     `entity_type` falls back to `EntityType::Other("article")` —
//!     the structure alone doesn't tell us whether the subject is a
//!     person, place, or concept; a later `StructuralClassified`
//!     pass can refine this from the title without re-reading the
//!     body.
//!   - One placeholder [`Entity`] per off-corpus wikilink target
//!     (the link points at an article the operator hasn't ingested).
//!     Placeholders have an empty `description` and `first_appearance`
//!     pointing at the chunk that mentioned them — keeps the link
//!     graph dense for retrieval; the brief assembler can flag the
//!     low depth.
//!   - One [`Edge`] of type `Involves` per (article → wikilink target)
//!     pair, deduped per article. `provenance =
//!     EdgeProvenance::WikilinkStructural` is the explicit flag the
//!     spec reserves for this strategy.
//!
//! What it deliberately does NOT produce in v1:
//!   - `Event` atoms — date-in-title heuristics produce too many
//!     false positives ("1936-1939 Arab revolt" is a span, not a
//!     point event; "World War II" has no date in title at all).
//!     Defer event extraction to a later LLM-budgeted Tier 2 pass.
//!   - `Claim` / `State` / `Configuration` atoms — these need
//!     text-level inference that the structural metadata can't
//!     provide.
//!   - `Composes` edges (section → article) — sections aren't atoms
//!     in the v2 schema; the per-entity `first_appearance` chunk_ref
//!     already pins the article to its lead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;

use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
use crate::enrichment::atlas::ingestion::{
    AtlasData, AtlasIngestion, AtlasIngestionConfig,
};
use crate::enrichment::atlas::registry::AtlasIngestionRegistry;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::extractors::wikipedia_types::WikipediaChunkMetadata;
use crate::progress::{IngestProgress, ProgressCallback};
use crate::types::{EmbedFn, InferenceFn};

/// Per-strategy config deserialised from
/// [`AtlasIngestionConfig::strategy_config`]. Operator-supplied;
/// CLI surfaces `--source-corpus`, `--limit-articles`,
/// `--include-functions`, and `--include-private` flags. The latter
/// two are honoured only on the code-corpus branch.
#[derive(Debug, Clone, Deserialize, Default)]
struct StructureFirstConfig {
    /// Corpus id to read chunks from (e.g. `"wikipedia"`). Required.
    pub source_corpus_id: String,
    /// Cap on the number of articles to process. `None` means "all".
    /// Articles are sorted by `source_doc_id` for stable ordering.
    #[serde(default)]
    pub limit_articles: Option<usize>,
    /// First-sentence cap for the article-entity `description` field.
    /// 280 keeps it tweet-sized; the brief assembler doesn't need the
    /// full lead.
    #[serde(default = "default_lead_chars")]
    pub lead_description_chars: usize,
    /// Code-corpus branch: emit Entity atoms for `pub fn` / `pub
    /// method` items as well. Off by default — function-tier atoms
    /// inflate the demo atlas without paying back.
    #[serde(default)]
    pub include_functions: bool,
    /// Code-corpus branch: include non-`pub` items. Off by default —
    /// public surface is the architectural shape; private internals
    /// are implementation detail.
    #[serde(default)]
    pub include_private: bool,
}

fn default_lead_chars() -> usize {
    280
}

pub struct StructureFirstIngestion;

impl StructureFirstIngestion {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StructureFirstIngestion {
    fn default() -> Self {
        Self::new()
    }
}

impl AtlasIngestion for StructureFirstIngestion {
    fn id(&self) -> &'static str {
        "structure_first"
    }

    fn name(&self) -> &'static str {
        "Structure-first (deterministic Wikipedia parser)"
    }

    fn ingest<'a>(
        &'a self,
        corpus: Arc<CorpusEngine>,
        _embed_fn: EmbedFn,
        _inference_fn: Option<InferenceFn>,
        config: AtlasIngestionConfig,
        progress: Arc<ProgressCallback>,
    ) -> Pin<Box<dyn Future<Output = Result<AtlasData>> + Send + 'a>> {
        Box::pin(async move {
            let cfg: StructureFirstConfig = if config.strategy_config.is_null() {
                return Err(Error::InvalidInput(
                    "structure_first requires {\"source_corpus_id\": \"...\"} \
                     in strategy_config"
                        .into(),
                ));
            } else {
                serde_json::from_value(config.strategy_config.clone()).map_err(|e| {
                    Error::InvalidInput(format!(
                        "structure_first config: {e} (expected \
                         {{source_corpus_id, limit_articles?, \
                         lead_description_chars?}})"
                    ))
                })?
            };

            tracing::info!(
                source_corpus = %cfg.source_corpus_id,
                "structure_first: opening source corpus"
            );
            let index = corpus
                .open_index_for_corpus(&cfg.source_corpus_id)
                .await
                .map_err(|e| {
                    Error::Database(format!(
                        "open source corpus `{}`: {e}",
                        cfg.source_corpus_id
                    ))
                })?;

            tracing::info!("structure_first: streaming chunks");
            let chunks = index.all_chunks_full().await?;
            let total_chunks = chunks.len();
            tracing::info!(total_chunks, "structure_first: streamed chunks");
            // Report the chunk-stream completion through the typed
            // progress channel as an Extracting tick — keeps the
            // strategy's signal compatible with the engine-wide
            // progress consumer (CLI, desktop) without a bespoke
            // variant.
            (progress)(IngestProgress::Extracting {
                documents_processed: total_chunks as u64,
            });

            // ── Dispatch on corpus kind ─────────────────────────
            // Sniff the first chunk's metadata. Code chunks carry
            // `symbol_name` + `file_path` + `language`; Wikipedia
            // chunks carry `section_path` + `section_type` +
            // `outgoing_links`. Cheap O(1) signal; avoids a
            // separate recipe round-trip.
            #[cfg(feature = "treesitter")]
            {
                let is_code_corpus = chunks
                    .iter()
                    .find_map(|c| c.metadata_raw.as_deref())
                    .map(crate::enrichment::atlas::strategies::code_walk::metadata_looks_like_code)
                    .unwrap_or(false);
                if is_code_corpus {
                    drop(chunks); // free before code_walk re-streams.
                    let walk_cfg = crate::enrichment::atlas::strategies::code_walk::CodeWalkConfig {
                        source_corpus_id: cfg.source_corpus_id.clone(),
                        include_functions: cfg.include_functions,
                        include_private: cfg.include_private,
                    };
                    return crate::enrichment::atlas::strategies::code_walk::extract_code_corpus(
                        corpus, &walk_cfg, progress,
                    )
                    .await;
                }
            }

            // ── Aggregate per article ───────────────────────────
            // BTreeMap so output is stable across runs.
            let mut articles: BTreeMap<String, AggregatedArticle> = BTreeMap::new();
            let mut chunks_with_metadata = 0usize;
            let mut chunks_without_metadata = 0usize;

            for chunk in chunks {
                let Some(metadata_raw) = chunk.metadata_raw.as_deref() else {
                    chunks_without_metadata += 1;
                    continue;
                };
                let meta: WikipediaChunkMetadata = match serde_json::from_str(metadata_raw)
                {
                    Ok(m) => m,
                    Err(_) => {
                        chunks_without_metadata += 1;
                        continue;
                    }
                };
                chunks_with_metadata += 1;

                // Resolve canonical article title. Prefer chunk.title;
                // fall back to URL-derived title; skip if neither.
                let article_title = chunk
                    .title
                    .clone()
                    .or_else(|| {
                        chunk
                            .url
                            .as_deref()
                            .and_then(crate::extractors::wikipedia_types::wiki_title_from_url)
                    });
                let Some(article_title) = article_title else {
                    continue;
                };

                let entry = articles
                    .entry(article_title.clone())
                    .or_insert_with(|| AggregatedArticle::new(article_title.clone()));

                // Carry per-article fields from the first metadata
                // we see for the article. Wikipedia chunks repeat
                // these across sections.
                if entry.wikidata_qid.is_none() {
                    entry.wikidata_qid = meta.wikidata_qid.clone();
                }
                if entry.page_id.is_none() {
                    entry.page_id = meta.page_id;
                }
                if entry.url.is_none() {
                    entry.url = chunk.url.clone();
                }

                // Capture the lead chunk (first one we see whose
                // section_type is "lead" OR section_path empty/depth==0).
                let is_lead = meta.section_type == "lead"
                    || meta.section_depth == 0
                    || meta.section_path.is_empty();
                if is_lead && entry.lead.is_none() {
                    entry.lead = Some(LeadChunk {
                        chunk_id: chunk.id,
                        content: chunk.content.clone(),
                    });
                }

                // Outgoing wikilinks — dedupe per (article, target).
                // First-seen `link_text` wins. Track the first chunk
                // we saw the link in so the placeholder Entity has a
                // sensible `first_appearance`.
                //
                // Namespace filter: skip wikilinks targeting Wikipedia
                // meta namespaces (Help:, Wikipedia:, Template:,
                // Portal:, Category:, File:, User:, Special:, Talk:,
                // Draft:). The Vital-L5 corpus filter excludes them
                // by design — leaving them as placeholders pollutes
                // the link graph (Help:IPA/English collected 6557
                // inbound on the first pass, dwarfing real entities).
                for link in &meta.outgoing_links {
                    if is_meta_namespace(&link.target_title) {
                        continue;
                    }
                    entry
                        .outgoing
                        .entry(link.target_title.clone())
                        .or_insert(OutgoingLink {
                            link_text: link.link_text.clone(),
                            first_seen_chunk: chunk.id,
                        });
                }
            }

            tracing::info!(
                articles = articles.len(),
                chunks_with_metadata,
                chunks_without_metadata,
                "structure_first: aggregated articles"
            );

            // Apply article cap, sorted by source_doc_id-style key
            // (article_title is our stable key in this aggregation).
            let mut article_titles: Vec<String> = articles.keys().cloned().collect();
            if let Some(cap) = cfg.limit_articles {
                article_titles.truncate(cap);
            }
            let kept_article_set: HashSet<String> =
                article_titles.iter().cloned().collect();

            // ── Build Entity atoms ──────────────────────────────
            //
            // Two passes so wikilink targets that ARE in the kept set
            // resolve to the real Entity's AtomId (not a placeholder).
            //
            // Pass 1: assign AtomId to every kept article. Build the
            // title → AtomId map for wikilink resolution.
            let mut title_to_atom: HashMap<String, AtomId> =
                HashMap::with_capacity(article_titles.len());
            let mut entities: Vec<Entity> = Vec::with_capacity(article_titles.len());
            for (idx, title) in article_titles.iter().enumerate() {
                let agg = articles.get(title).expect("kept article must exist");
                let atom_id = AtomId::entity(idx + 1);
                title_to_atom.insert(title.clone(), atom_id.clone());

                let (first_chunk_id, lead_text) = match agg.lead.as_ref() {
                    Some(lead) => (lead.chunk_id, lead.content.as_str()),
                    None => {
                        // Article had no identifiable lead chunk; pin
                        // first_appearance at chunk 0 with empty
                        // description so the entity is still valid.
                        (
                            agg.outgoing
                                .values()
                                .next()
                                .map(|e| e.first_seen_chunk)
                                .unwrap_or(0),
                            "",
                        )
                    }
                };
                let description = first_sentence_truncated(
                    strip_leading_title(lead_text, title),
                    cfg.lead_description_chars,
                );

                entities.push(Entity {
                    id: atom_id,
                    canonical_name: title.clone(),
                    aliases: Vec::new(),
                    entity_type: structural_entity_type(),
                    first_appearance: ChunkRef::new(
                        first_chunk_id.to_string(),
                        Some(preview(lead_text, 120)),
                    ),
                    description,
                    defining_quote: None,
                    salience: 0.5, // structural pass has no centrality input yet
                    enrichment_depth: EnrichmentDepth::Structural,
                    affiliation: None,
                    role: None,
                    participants: Vec::new(),
                                    concept_kind: None,
});
            }

            // Pass 2: scan kept articles' outgoing links for targets
            // that aren't in `title_to_atom`. Each unique off-corpus
            // target becomes one placeholder Entity. Track the
            // (chunk_id, link_text) of the FIRST mention so the
            // placeholder anchors somewhere meaningful.
            let mut placeholder_targets: BTreeMap<String, (u64, String)> =
                BTreeMap::new();
            for title in &article_titles {
                let agg = articles.get(title).expect("kept article must exist");
                for (target, link) in &agg.outgoing {
                    if title_to_atom.contains_key(target)
                        || placeholder_targets.contains_key(target)
                    {
                        continue;
                    }
                    placeholder_targets
                        .insert(target.clone(), (link.first_seen_chunk, link.link_text.clone()));
                }
            }

            for (i, (target, (chunk_id, link_text))) in
                placeholder_targets.iter().enumerate()
            {
                let atom_id = AtomId::entity(article_titles.len() + i + 1);
                title_to_atom.insert(target.clone(), atom_id.clone());
                entities.push(Entity {
                    id: atom_id,
                    canonical_name: target.clone(),
                    aliases: if link_text != target {
                        vec![link_text.clone()]
                    } else {
                        Vec::new()
                    },
                    entity_type: structural_entity_type(),
                    first_appearance: ChunkRef::new(chunk_id.to_string(), None),
                    description: String::new(),
                    defining_quote: None,
                    salience: 0.0, // off-corpus, no in-corpus signal
                    enrichment_depth: EnrichmentDepth::Structural,
                    affiliation: None,
                    role: None,
                    participants: Vec::new(),
                                    concept_kind: None,
});
            }

            tracing::info!(
                total_entities = entities.len(),
                in_corpus = article_titles.len(),
                placeholders = placeholder_targets.len(),
                "structure_first: entities emitted"
            );

            // ── Build Involves edges ────────────────────────────
            let mut edges: Vec<Edge> = Vec::new();
            let mut next_edge_idx = 1usize;
            for title in &article_titles {
                let agg = articles.get(title).expect("kept article must exist");
                let source_atom = title_to_atom
                    .get(title)
                    .expect("kept article has an atom id")
                    .clone();
                for target_title in agg.outgoing.keys() {
                    let target_atom = title_to_atom
                        .get(target_title)
                        .expect("every wikilink target has an atom (real or placeholder)")
                        .clone();
                    edges.push(Edge {
                        id: EdgeId::new(next_edge_idx),
                        edge_type: EdgeType::Involves,
                        source: source_atom.clone(),
                        target: target_atom,
                        evidence: Vec::new(),
                        trigger_event: None,
                        sub_question: None,
                        // Wikilink presence is an objective signal —
                        // not LLM-inferred. Use 1.0; the brief
                        // assembler still calibrates language via
                        // EdgeProvenance::WikilinkStructural.
                        confidence: 1.0,
                        provenance: EdgeProvenance::WikilinkStructural,
                    });
                    next_edge_idx += 1;
                }
            }

            tracing::info!(
                involves_edges = edges.len(),
                "structure_first: edges emitted"
            );
            // Final progress tick: report total chunks scanned as
            // Complete so any consumer that gates on Complete can
            // close out the run.
            (progress)(IngestProgress::Complete {
                total_chunks: total_chunks as u64,
                duration_secs: 0,
            });

            // ── Compose AtlasData payload ───────────────────────
            let atom_envelopes: Vec<AtomEnvelope> = entities
                .into_iter()
                .map(AtomEnvelope::Entity)
                .collect();
            let atoms_file = AtomsFile::new(atom_envelopes);
            let edges_file = EdgesFile::new(edges);

            let placeholder_count = placeholder_targets.len();
            let in_corpus_count = article_titles.len();
            let edges_count = edges_file.edges.len();
            let schema_validation = serde_json::json!({
                "strategy": "structure_first",
                "stats": {
                    "in_corpus_articles": in_corpus_count,
                    "placeholder_entities": placeholder_count,
                    "involves_edges": edges_count,
                    "chunks_with_metadata": chunks_with_metadata,
                    "chunks_without_metadata": chunks_without_metadata,
                },
            });

            Ok(AtlasData {
                atoms: serde_json::to_value(&atoms_file)
                    .map_err(|e| Error::Serialization(format!("atoms serialise: {e}")))?,
                edges: serde_json::to_value(&edges_file)
                    .map_err(|e| Error::Serialization(format!("edges serialise: {e}")))?,
                trajectories: serde_json::json!({}),
                manifest: serde_json::json!({}),
                schema_validation,
                dominant_depth: EnrichmentDepth::Structural,
            })
        })
    }
}

// ── Aggregation scratch types ────────────────────────────────────

#[derive(Debug)]
struct AggregatedArticle {
    #[allow(dead_code)]
    title: String,
    wikidata_qid: Option<String>,
    page_id: Option<i64>,
    url: Option<String>,
    lead: Option<LeadChunk>,
    /// target_title → (link_text, first_seen_chunk_id)
    outgoing: BTreeMap<String, OutgoingLink>,
}

impl AggregatedArticle {
    fn new(title: String) -> Self {
        Self {
            title,
            wikidata_qid: None,
            page_id: None,
            url: None,
            lead: None,
            outgoing: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct LeadChunk {
    chunk_id: u64,
    content: String,
}

#[derive(Debug)]
struct OutgoingLink {
    link_text: String,
    first_seen_chunk: u64,
}

// ── Helpers ──────────────────────────────────────────────────────

/// Default `entity_type` for structural-pass entities. The structure
/// alone doesn't tell us whether the article subject is a person,
/// place, etc. — `Other("article")` keeps the type slot honest until
/// a Tier-1.5 classifier upgrades it.
fn structural_entity_type() -> EntityType {
    EntityType::Other("article".to_string())
}

/// True if `title` names a Wikipedia meta-namespace page that the
/// L5 vital-articles filter excludes by design (Help:, Wikipedia:,
/// Template:, Portal:, Category:, File:, User:, Special:, Talk:,
/// Draft:, MediaWiki:, Module:, Book:, TimedText:). Case-sensitive
/// match on the namespace prefix as MediaWiki canonicalises it.
fn is_meta_namespace(title: &str) -> bool {
    const META_PREFIXES: &[&str] = &[
        "Help:",
        "Wikipedia:",
        "Template:",
        "Portal:",
        "Category:",
        "File:",
        "Image:",
        "User:",
        "User talk:",
        "Special:",
        "Talk:",
        "Draft:",
        "MediaWiki:",
        "Module:",
        "Book:",
        "TimedText:",
    ];
    META_PREFIXES.iter().any(|p| title.starts_with(p))
}

/// Wikipedia lead chunks routinely begin with the article title on
/// its own line (the chunker preserves the heading). Strip it so the
/// entity description doesn't waste characters re-stating
/// `canonical_name`. Match is case-sensitive and trims trailing
/// whitespace + newlines after the title.
fn strip_leading_title<'a>(text: &'a str, title: &str) -> &'a str {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(title) {
        rest.trim_start_matches(|c: char| c.is_whitespace())
    } else {
        trimmed
    }
}

/// Take the first sentence of `text` and cap it at `max_chars`. Used
/// as the article-entity's `description`. A "sentence" is anything up
/// to the first `. `, `? `, `! `, or end of string. We don't try to
/// be clever — Wikipedia leads are usually well-formed.
fn first_sentence_truncated(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let end = trimmed
        .find(". ")
        .or_else(|| trimmed.find("? "))
        .or_else(|| trimmed.find("! "))
        .map(|i| i + 1) // include the punctuation
        .unwrap_or(trimmed.len());
    let candidate = &trimmed[..end];
    if candidate.chars().count() <= max_chars {
        candidate.to_string()
    } else {
        // Cap by chars (not bytes — Wikipedia has multi-byte UTF-8).
        candidate.chars().take(max_chars).collect::<String>() + "…"
    }
}

/// Short preview string for `ChunkRef::passage_preview`. ~120 chars.
fn preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect::<String>() + "…"
    }
}

/// Hook the registry calls. Mirrors the pattern in
/// `pipelines::literary_atlas::register_extraction_first`.
pub fn register_structure_first(registry: &mut AtlasIngestionRegistry) {
    registry.register("structure_first", || {
        Arc::new(StructureFirstIngestion::new())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_name_are_stable() {
        let s = StructureFirstIngestion::new();
        assert_eq!(s.id(), "structure_first");
        assert!(s.name().contains("Structure-first"));
    }

    #[test]
    fn strip_leading_title_drops_title_and_blank_lines() {
        let body = "Albert Einstein\n\nAlbert Einstein was a physicist.";
        assert_eq!(
            strip_leading_title(body, "Albert Einstein"),
            "Albert Einstein was a physicist."
        );
    }

    #[test]
    fn strip_leading_title_noop_when_lead_doesnt_start_with_title() {
        let body = "Born in 1879, the physicist Albert Einstein…";
        assert_eq!(
            strip_leading_title(body, "Albert Einstein"),
            "Born in 1879, the physicist Albert Einstein…"
        );
    }

    #[test]
    fn strip_leading_title_handles_quoted_title() {
        let body = "\"Weird Al\" Yankovic\n\nAlfred Matthew \"Weird Al\" Yankovic is an American comedy musician.";
        assert_eq!(
            strip_leading_title(body, "\"Weird Al\" Yankovic"),
            "Alfred Matthew \"Weird Al\" Yankovic is an American comedy musician."
        );
    }

    #[test]
    fn first_sentence_truncated_extracts_lead_sentence() {
        let s = first_sentence_truncated(
            "Albert Einstein was a German-born theoretical physicist. He developed \
             the theory of relativity.",
            280,
        );
        assert_eq!(
            s,
            "Albert Einstein was a German-born theoretical physicist."
        );
    }

    #[test]
    fn first_sentence_truncated_handles_no_period() {
        let s = first_sentence_truncated("Untouched lead", 280);
        assert_eq!(s, "Untouched lead");
    }

    #[test]
    fn first_sentence_truncated_caps_long_lead() {
        let long = "x".repeat(500);
        let s = first_sentence_truncated(&long, 50);
        assert_eq!(s.chars().count(), 51); // 50 chars + ellipsis
        assert!(s.ends_with('…'));
    }

    #[test]
    fn is_meta_namespace_catches_help_and_wikipedia_namespaces() {
        assert!(is_meta_namespace("Help:IPA/English"));
        assert!(is_meta_namespace("Wikipedia:Manual of Style"));
        assert!(is_meta_namespace("Template:Cite_book"));
        assert!(is_meta_namespace("Category:Living people"));
        assert!(is_meta_namespace("File:Example.jpg"));
        assert!(is_meta_namespace("Portal:History"));
        assert!(is_meta_namespace("User:Alice"));
        assert!(is_meta_namespace("Special:Search"));
    }

    #[test]
    fn is_meta_namespace_passes_real_articles() {
        assert!(!is_meta_namespace("World War II"));
        assert!(!is_meta_namespace("Albert Einstein"));
        assert!(!is_meta_namespace("U.S. Constitution"));
        // Article titles that just happen to contain a colon must not
        // be falsely classed as meta. "Help:" is a hard-coded prefix
        // so colon-containing real titles like "Java: A Beginner's
        // Guide" must pass through unaffected.
        assert!(!is_meta_namespace("Java: A Beginner's Guide"));
    }

    #[test]
    fn structural_entity_type_is_other_article() {
        match structural_entity_type() {
            EntityType::Other(s) => assert_eq!(s, "article"),
            other => panic!("expected Other(\"article\"), got {other:?}"),
        }
    }

    #[test]
    fn register_structure_first_populates_registry() {
        let mut r = AtlasIngestionRegistry::new();
        register_structure_first(&mut r);
        let s = r.get("structure_first").expect("must register");
        assert_eq!(s.id(), "structure_first");
    }
}
