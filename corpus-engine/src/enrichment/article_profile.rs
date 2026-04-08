//! Per-article epistemic profiles (Layer 2.5).
//!
//! After the link graph is built, this module aggregates Layer 1 chunk
//! metadata and Layer 2 inlink counts into a single `ArticleEpistemicProfile`
//! per Wikipedia article. The profile drives Layer 3 LLM-enrichment candidate
//! selection and surfaces article-level editorial signals in search results.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::enrichment::relationships::ClaimRelationship;
use crate::error::Result;
use crate::index::{CorpusIndex, StoredChunkWithMetadata};

/// Chunk-level structural metadata stored by `WikipediaStructuredExtractor`
/// in the `InsertChunk.metadata` JSON field. Used by the link graph builder
/// and article profile builder to access section and editorial signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikipediaChunkMetadata {
    pub section_name: String,
    pub section_path: Vec<String>,
    pub section_depth: u32,
    /// "lead", "controversy", "factual", or "general"
    pub section_type: String,
    pub citation_needed_count: Option<i64>,
    pub pov_count: Option<i64>,
    pub clarification_needed_count: Option<i64>,
    pub update_count: Option<i64>,
    pub is_flagged_stable: Option<bool>,
    pub outgoing_links: Vec<WikiLink>,
    /// Wikipedia revision ID from `version.identifier` — used as delta key.
    #[serde(default)]
    pub revision_id: Option<i64>,
    /// Wikidata entity QID from `main_entity.identifier` (e.g. "Q42").
    #[serde(default)]
    pub wikidata_qid: Option<String>,
    /// Wikipedia page ID from the top-level `identifier` column.
    #[serde(default)]
    pub page_id: Option<i64>,
}

/// A directed link from a Wikipedia section to another article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLink {
    /// Article title of the link target (spaces, not underscores).
    pub target_title: String,
    /// The display text of the link as it appears in the section.
    pub link_text: String,
}

/// Aggregate epistemic profile for a single Wikipedia article.
///
/// Computed from:
/// - Layer 1: chunk metadata (maintenance tags, section classification)
/// - Layer 2: controversy inlink count from the link graph
///
/// Stored in the `article_profiles` LanceDB table. Used to select
/// candidates for Layer 3 LLM enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleEpistemicProfile {
    pub article_title: String,
    pub article_url: Option<String>,

    /// Overall editorial confidence (0.0–1.0). Computed from maintenance
    /// tag counts and section classification. Higher = more stable.
    pub editorial_confidence: f32,

    pub has_controversy_sections: bool,
    pub controversy_section_count: u32,

    /// Aggregate maintenance tag counts from article-level signals.
    pub citation_needed_count: u32,
    pub pov_count: u32,
    pub clarification_needed_count: u32,

    /// How many other articles link to this one from a controversy section.
    pub controversy_inlink_count: u32,

    /// True if this article is a good candidate for Layer 3 LLM enrichment.
    pub llm_enrichment_candidate: bool,
}

/// Compute one `ArticleEpistemicProfile` per article by aggregating chunk
/// metadata and link-graph inlink counts.
///
/// Materialises all chunks into memory (same pattern as claim enrichment).
/// For full English Wikipedia (~6.4M articles, 20M+ chunks) this is a large
/// but one-time offline job; each chunk's metadata payload is a few hundred
/// bytes.
pub async fn compute_article_profiles(
    index: &CorpusIndex,
    relationships: &[ClaimRelationship],
) -> Result<Vec<ArticleEpistemicProfile>> {
    let chunks: Vec<StoredChunkWithMetadata> = index.all_chunks_with_raw_metadata().await?;

    // Group (id, url, parsed metadata) by article title.
    let mut by_title: HashMap<String, Vec<(u64, Option<String>, WikipediaChunkMetadata)>> =
        HashMap::new();

    for chunk in &chunks {
        let title = match &chunk.title {
            Some(t) if !t.is_empty() => t.clone(),
            _ => continue,
        };
        let meta: WikipediaChunkMetadata = match chunk
            .metadata_raw
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
        {
            Some(m) => m,
            None => continue,
        };
        by_title
            .entry(title)
            .or_default()
            .push((chunk.id, chunk.url.clone(), meta));
    }

    // Count how many controversy-sourced relationships point INTO each chunk.
    let mut chunk_inlink_counts: HashMap<u64, usize> = HashMap::new();
    for rel in relationships {
        *chunk_inlink_counts.entry(rel.claim_b_id).or_insert(0) += 1;
    }

    // Map chunk_id → article title so we can aggregate inlinks per article.
    let chunk_to_title: HashMap<u64, String> = chunks
        .iter()
        .filter_map(|c| c.title.as_ref().map(|t| (c.id, t.clone())))
        .collect();

    let mut article_inlinks: HashMap<String, u32> = HashMap::new();
    for (chunk_id, count) in &chunk_inlink_counts {
        if let Some(title) = chunk_to_title.get(chunk_id) {
            *article_inlinks.entry(title.clone()).or_insert(0) += *count as u32;
        }
    }

    let mut profiles = Vec::with_capacity(by_title.len());

    for (title, entries) in by_title {
        // Use first chunk's metadata for article-level maintenance tag counts
        // (they're the same for every chunk from the same article).
        let first_meta = &entries[0].2;
        let citation_needed = first_meta.citation_needed_count.unwrap_or(0).max(0) as u32;
        let pov = first_meta.pov_count.unwrap_or(0).max(0) as u32;
        let clarification = first_meta
            .clarification_needed_count
            .unwrap_or(0)
            .max(0) as u32;
        let is_stable = first_meta.is_flagged_stable.unwrap_or(false);

        let controversy_count = entries
            .iter()
            .filter(|(_, _, m)| m.section_type == "controversy")
            .count() as u32;

        let inbound = *article_inlinks.get(&title).unwrap_or(&0);

        // Editorial confidence score: starts at 1.0, penalised for flags.
        let mut confidence: f32 = 1.0;
        if pov > 0 {
            confidence -= 0.3;
        }
        if citation_needed > 5 {
            confidence -= 0.2;
        }
        if citation_needed > 10 {
            confidence -= 0.1;
        }
        if clarification > 3 {
            confidence -= 0.1;
        }
        if !is_stable {
            confidence -= 0.05;
        }
        if controversy_count > 0 {
            confidence -= 0.1;
        }
        confidence = confidence.clamp(0.0, 1.0);

        let llm_candidate = pov > 0
            || controversy_count > 0
            || inbound >= 3
            || citation_needed > 10;

        let article_url = entries[0].1.clone();

        profiles.push(ArticleEpistemicProfile {
            article_title: title,
            article_url,
            editorial_confidence: confidence,
            has_controversy_sections: controversy_count > 0,
            controversy_section_count: controversy_count,
            citation_needed_count: citation_needed,
            pov_count: pov,
            clarification_needed_count: clarification,
            controversy_inlink_count: inbound,
            llm_enrichment_candidate: llm_candidate,
        });
    }

    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_link_round_trips() {
        let link = WikiLink {
            target_title: "Elinor Ostrom".to_string(),
            link_text: "Ostrom".to_string(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let back: WikiLink = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target_title, "Elinor Ostrom");
    }

    #[test]
    fn metadata_round_trips() {
        let meta = WikipediaChunkMetadata {
            section_name: "Criticism".to_string(),
            section_path: vec!["Criticism".to_string()],
            section_depth: 0,
            section_type: "controversy".to_string(),
            citation_needed_count: Some(3),
            pov_count: Some(1),
            clarification_needed_count: Some(0),
            update_count: Some(0),
            is_flagged_stable: Some(false),
            outgoing_links: vec![WikiLink {
                target_title: "Evidence-based medicine".to_string(),
                link_text: "evidence-based medicine".to_string(),
            }],
            revision_id: Some(1234567890),
            wikidata_qid: Some("Q42".to_string()),
            page_id: Some(9876),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: WikipediaChunkMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.section_type, "controversy");
        assert_eq!(back.pov_count, Some(1));
        assert_eq!(back.outgoing_links.len(), 1);
    }
}
