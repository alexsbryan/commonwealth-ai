//! Catalog-corpus retrieval helpers.
//!
//! When a query lands on a corpus whose `kind = Catalog`, the
//! returned chunk is metadata about a work (title, author, subjects)
//! — *not* the work's full text. The runtime needs to surface this
//! distinct affordance instead of synthesising as if it had read the
//! work. Hence: partition retrieval results into full-text hits and
//! catalog-aware hits, and let synthesis present the latter as an
//! "I know of this — want me to read it?" offer.
//!
//! ## Where catalog metadata comes from
//!
//! The Gutenberg catalog extractor stamps a string-valued JSON
//! object onto each chunk. After round-tripping through the index
//! this becomes a `HashMap<String, String>` on `ScoredChunk.metadata`
//! with keys like `gutenberg_id`, `authors`, `year`, `subjects`,
//! `language`. [`CatalogHit`] hydrates from that map plus the
//! catalog corpus's `[catalog]` recipe block (download URL template,
//! ingest/enrich estimates).
//!
//! ## Already-ingested suppression
//!
//! Per-work corpora produced by an on-demand ingest carry
//! `parent_corpus_id == <catalog>` and an id of the shape
//! `<catalog>-<work_id>`. When such a corpus is already installed,
//! [`CatalogHit::already_ingested_corpus_id`] is set so the runtime
//! can skip the offer (the user has already had the work read) and
//! fall back to the existing full-text path against that per-work
//! corpus.

use std::collections::HashMap;

use corpus_engine::recipe::CatalogConfig;
use corpus_engine::types::{CorpusKind, IndexInfo, ScoredChunk};

/// Index-side context needed to resolve a `Catalog`-kind chunk into
/// a [`CatalogHit`]. Built once per partition call from
/// `CorpusEngine::installed_indexes()` + the catalog corpora's
/// recipe `[catalog]` blocks.
#[derive(Debug, Clone)]
pub struct CatalogResolutionContext {
    /// Catalog corpus id → `CatalogConfig` (download URL template,
    /// content recipe, throughput estimates). Sourced from the
    /// recipe at install time and cached per-process.
    pub catalogs: HashMap<String, CatalogConfig>,
    /// `parent_corpus_id` → set of `<parent>-<work_id>` corpus ids
    /// that have already been ingested. Used to mark a catalog hit
    /// as `already_ingested` so we stop offering to read it again.
    pub ingested_works: HashMap<String, Vec<String>>,
}

impl CatalogResolutionContext {
    pub fn empty() -> Self {
        Self {
            catalogs: HashMap::new(),
            ingested_works: HashMap::new(),
        }
    }

    /// Build a context from `installed_indexes()` plus a per-catalog
    /// `CatalogConfig` lookup the caller supplies (typically by
    /// loading each catalog corpus's recipe via
    /// `RecipeRegistry::fetch_recipe`).
    pub fn from_indexes(
        indexes: &[IndexInfo],
        catalog_configs: HashMap<String, CatalogConfig>,
    ) -> Self {
        let mut ingested_works: HashMap<String, Vec<String>> = HashMap::new();
        for idx in indexes {
            if let Some(parent) = idx.parent_corpus_id.as_deref() {
                ingested_works
                    .entry(parent.to_string())
                    .or_default()
                    .push(idx.corpus_id.clone());
            }
        }
        Self {
            catalogs: catalog_configs,
            ingested_works,
        }
    }
}

/// A retrieval hit on a catalog corpus, fully resolved into
/// everything synthesis needs to surface a "want me to read this?"
/// offer or to fire an on-demand ingest deterministically.
#[derive(Debug, Clone)]
pub struct CatalogHit {
    pub catalog_corpus_id: String,
    pub work_id: String,
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<String>,
    pub subjects: Option<String>,
    pub language: Option<String>,
    pub download_url: String,
    pub content_recipe: String,
    pub estimated_ingest_minutes: Option<u32>,
    pub estimated_enrich_minutes: Option<u32>,
    /// `Some(corpus_id)` when an on-demand ingest of this work is
    /// already on disk. The runtime should suppress the ingest
    /// offer in that case and route the user to the per-work
    /// corpus's full-text retrieval instead.
    pub already_ingested_corpus_id: Option<String>,
    pub score: f32,
}

/// Partition a flat search result into full-text hits and
/// catalog-aware hits, given the kind of each corpus.
///
/// Hits whose corpus is unknown to `index_kinds` are treated as
/// full-text (back-compat: legacy code calling search without the
/// new partitioning machinery can keep all results).
pub fn partition_hits_by_kind(
    hits: Vec<ScoredChunk>,
    index_kinds: &HashMap<String, CorpusKind>,
    ctx: &CatalogResolutionContext,
) -> (Vec<ScoredChunk>, Vec<CatalogHit>) {
    let mut full_text = Vec::with_capacity(hits.len());
    let mut catalog = Vec::new();
    for hit in hits {
        match index_kinds.get(&hit.corpus_id) {
            Some(CorpusKind::Catalog) => {
                if let Some(cat) = hydrate_catalog_hit(&hit, ctx) {
                    catalog.push(cat);
                } else {
                    // Catalog hit without a `CatalogConfig` in the
                    // resolution context — degenerate, but log and
                    // fall back to surfacing the chunk as full text
                    // rather than dropping it.
                    tracing::warn!(
                        corpus = %hit.corpus_id,
                        "catalog hit on corpus with no CatalogConfig; falling back to full-text formatting"
                    );
                    full_text.push(hit);
                }
            }
            _ => full_text.push(hit),
        }
    }
    (full_text, catalog)
}

fn hydrate_catalog_hit(hit: &ScoredChunk, ctx: &CatalogResolutionContext) -> Option<CatalogHit> {
    let cat = ctx.catalogs.get(&hit.corpus_id)?;
    // The id_field tells us which metadata key holds the work id.
    // Default to "gutenberg_id" / "id" / "source_id" / "title" if
    // the field isn't present (defensive — keeps a malformed
    // catalog index from blowing up retrieval).
    let work_id = hit
        .metadata
        .get(cat.id_field.as_str())
        .or_else(|| hit.metadata.get("gutenberg_id"))
        .or_else(|| hit.metadata.get("id"))
        .or_else(|| hit.metadata.get("source_id"))
        .cloned()?;

    let title = hit
        .title
        .clone()
        .or_else(|| hit.metadata.get("title").cloned())
        .unwrap_or_else(|| format!("Untitled work {}", work_id));

    let download_url = cat
        .download_url_template
        .replace("{id}", &work_id);

    let estimated_ingest_minutes = estimate_minutes(
        hit.metadata
            .get("estimated_words")
            .and_then(|s| s.parse::<u64>().ok()),
        cat.ingest_estimate_wpm,
    );
    let estimated_enrich_minutes = estimate_minutes(
        hit.metadata
            .get("estimated_words")
            .and_then(|s| s.parse::<u64>().ok()),
        cat.enrich_estimate_wpm,
    );

    let already_ingested_corpus_id = ctx
        .ingested_works
        .get(&hit.corpus_id)
        .and_then(|works| {
            let suffix = format!("-{}", work_id);
            works
                .iter()
                .find(|cid| cid.ends_with(&suffix))
                .cloned()
        });

    Some(CatalogHit {
        catalog_corpus_id: hit.corpus_id.clone(),
        work_id,
        title,
        authors: hit.metadata.get("authors").cloned(),
        year: hit.metadata.get("year").cloned(),
        subjects: hit.metadata.get("subjects").cloned(),
        language: hit.metadata.get("language").cloned(),
        download_url,
        content_recipe: cat.content_recipe.clone(),
        estimated_ingest_minutes,
        estimated_enrich_minutes,
        already_ingested_corpus_id,
        score: hit.score,
    })
}

fn estimate_minutes(words: Option<u64>, wpm: Option<u32>) -> Option<u32> {
    let w = words?;
    let r = wpm?;
    if r == 0 {
        return None;
    }
    Some(((w as f64 / r as f64).ceil() as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake_catalog_config() -> CatalogConfig {
        CatalogConfig {
            id_field: "gutenberg_id".into(),
            download_url_template: "https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt".into(),
            content_recipe: "gutenberg-work".into(),
            estimated_words_field: None,
            ingest_estimate_wpm: Some(8000),
            enrich_estimate_wpm: Some(500),
        }
    }

    fn fake_catalog_hit(corpus: &str, work_id: &str, title: &str) -> ScoredChunk {
        let mut metadata = HashMap::new();
        metadata.insert("gutenberg_id".into(), work_id.into());
        metadata.insert("title".into(), title.into());
        metadata.insert("authors".into(), "Anon".into());
        metadata.insert("year".into(), "1900".into());
        metadata.insert("estimated_words".into(), "215000".into());
        ScoredChunk {
            content: format!("Title: {title}"),
            title: Some(title.into()),
            url: None,
            corpus_id: corpus.into(),
            score: 0.42,
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    fn fake_fulltext_hit(corpus: &str, content: &str) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some("ch 1".into()),
            url: None,
            corpus_id: corpus.into(),
            score: 0.88,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn partitions_catalog_and_fulltext_hits() {
        let mut configs = HashMap::new();
        configs.insert("gutenberg".into(), fake_catalog_config());
        let ctx = CatalogResolutionContext {
            catalogs: configs,
            ingested_works: HashMap::new(),
        };

        let mut kinds = HashMap::new();
        kinds.insert("gutenberg".into(), CorpusKind::Catalog);
        kinds.insert("wikipedia".into(), CorpusKind::Knowledge);

        let hits = vec![
            fake_catalog_hit("gutenberg", "2701", "Moby Dick"),
            fake_fulltext_hit("wikipedia", "Whaling article body."),
        ];

        let (ft, cat) = partition_hits_by_kind(hits, &kinds, &ctx);
        assert_eq!(ft.len(), 1);
        assert_eq!(ft[0].corpus_id, "wikipedia");
        assert_eq!(cat.len(), 1);
        let mb = &cat[0];
        assert_eq!(mb.catalog_corpus_id, "gutenberg");
        assert_eq!(mb.work_id, "2701");
        assert_eq!(mb.title, "Moby Dick");
        assert_eq!(mb.authors.as_deref(), Some("Anon"));
        assert_eq!(mb.download_url, "https://www.gutenberg.org/cache/epub/2701/pg2701.txt");
        assert_eq!(mb.content_recipe, "gutenberg-work");
        // 215000 / 8000 ≈ 27 min
        assert_eq!(mb.estimated_ingest_minutes, Some(27));
        // 215000 / 500 = 430 min
        assert_eq!(mb.estimated_enrich_minutes, Some(430));
        assert!(mb.already_ingested_corpus_id.is_none());
    }

    #[test]
    fn already_ingested_short_circuit() {
        let mut configs = HashMap::new();
        configs.insert("gutenberg".into(), fake_catalog_config());
        let mut ingested = HashMap::new();
        ingested.insert("gutenberg".into(), vec!["gutenberg-2701".into()]);
        let ctx = CatalogResolutionContext {
            catalogs: configs,
            ingested_works: ingested,
        };

        let mut kinds = HashMap::new();
        kinds.insert("gutenberg".into(), CorpusKind::Catalog);

        let hits = vec![fake_catalog_hit("gutenberg", "2701", "Moby Dick")];
        let (_, cat) = partition_hits_by_kind(hits, &kinds, &ctx);
        assert_eq!(cat.len(), 1);
        assert_eq!(
            cat[0].already_ingested_corpus_id.as_deref(),
            Some("gutenberg-2701")
        );
    }

    #[test]
    fn unknown_corpus_falls_back_to_fulltext() {
        let ctx = CatalogResolutionContext::empty();
        let kinds: HashMap<String, CorpusKind> = HashMap::new();
        let hits = vec![fake_fulltext_hit("unfamiliar", "body")];
        let (ft, cat) = partition_hits_by_kind(hits, &kinds, &ctx);
        assert_eq!(ft.len(), 1);
        assert!(cat.is_empty());
    }

    #[test]
    fn catalog_hit_without_config_falls_back_to_fulltext() {
        // The catalog corpus is registered as Kind=Catalog but no
        // CatalogConfig is supplied (e.g. recipe load failed).
        // We surface the chunk as full text rather than drop it
        // outright — it's better to over-report than lose a hit.
        let ctx = CatalogResolutionContext::empty();
        let mut kinds = HashMap::new();
        kinds.insert("gutenberg".into(), CorpusKind::Catalog);
        let hits = vec![fake_catalog_hit("gutenberg", "2701", "Moby Dick")];
        let (ft, cat) = partition_hits_by_kind(hits, &kinds, &ctx);
        assert!(cat.is_empty());
        assert_eq!(ft.len(), 1);
    }
}
