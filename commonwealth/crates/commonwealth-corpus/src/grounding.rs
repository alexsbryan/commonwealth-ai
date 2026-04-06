use corpus_engine::{CorpusEngine, ScoredChunk};

use serde::{Deserialize, Serialize};

/// Configuration for knowledge grounding of non-OICP requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub corpora: Vec<String>,
    #[serde(default = "default_max_chunks")]
    pub max_chunks: usize,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    #[serde(default = "default_min_relevance")]
    pub min_relevance: f32,
    #[serde(default = "default_true")]
    pub citation_instructions: bool,
}

fn default_true() -> bool {
    true
}
fn default_max_chunks() -> usize {
    5
}
fn default_max_context_tokens() -> usize {
    3000
}
fn default_min_relevance() -> f32 {
    0.65
}

impl Default for GroundingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            corpora: Vec::new(),
            max_chunks: 5,
            max_context_tokens: 3000,
            min_relevance: 0.65,
            citation_instructions: true,
        }
    }
}

/// Search local indexes and return relevant chunks for grounding.
pub async fn search_for_grounding(
    engine: &CorpusEngine,
    query_embedding: &[f32],
    query_text: &str,
    config: &GroundingConfig,
) -> corpus_engine::Result<Vec<ScoredChunk>> {
    let mut results = Vec::new();

    for info in engine.installed_indexes().await? {
        // Filter to configured corpora if specified.
        if !config.corpora.is_empty() && !config.corpora.contains(&info.corpus_id) {
            continue;
        }

        match engine.open_index(&info.path).await {
            Ok(index) => {
                let hits = index
                    .search(query_embedding, query_text, config.max_chunks)
                    .await?;
                results.extend(hits);
            }
            Err(e) => {
                tracing::warn!("Failed to open index {}: {}", info.path.display(), e);
            }
        }
    }

    // Filter by relevance threshold.
    results.retain(|r| r.score >= config.min_relevance);
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(config.max_chunks);

    Ok(results)
}

/// Format grounded chunks as a knowledge context block for injection
/// into a system prompt.
pub fn format_knowledge_context(chunks: &[ScoredChunk], cite: bool) -> String {
    if chunks.is_empty() {
        return String::new();
    }

    let mut ctx = String::from("<knowledge_context>\n");

    for (i, chunk) in chunks.iter().enumerate() {
        ctx.push_str(&format!("[{}] ", i + 1));
        if let Some(ref title) = chunk.title {
            ctx.push_str(&format!("{}: ", title));
        }
        ctx.push_str(&chunk.content);
        if let Some(ref url) = chunk.url {
            ctx.push_str(&format!(" ({})", url));
        }
        ctx.push('\n');
    }

    ctx.push_str("</knowledge_context>\n");

    if cite {
        ctx.push_str(
            "\nWhen using information from the knowledge context above, cite the source using [N] notation.\n",
        );
    }

    ctx
}
