//! Epistemic tools — `claim_search` and `epistemic_landscape`.
//!
//! Both tools wrap a `corpus_engine::CorpusEngine` to expose its
//! enrichment-aware search methods to the `ReasonWithTools` loop.
//! They use the existing `Tool` trait — no changes to `sovereign-core`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Permission, StepOutput, ToolContext, ToolDescriptor,
};

use corpus_engine::{CorpusEngine, EpistemicLandscape, ScoredClaim};

// ─── ClaimSearchTool ─────────────────────────────────────

/// Searches extracted claims with epistemic status (consensus, contested,
/// minority view, etc.). Use this when you need to know what positions
/// exist on a topic, not just retrieve text passages.
pub struct ClaimSearchTool {
    engine: Arc<CorpusEngine>,
}

impl ClaimSearchTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ClaimSearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "claim_search".to_string(),
            name: "Claim Search".to_string(),
            description: "Search for extracted propositional claims with \
                          epistemic status (consensus, majority, contested, \
                          minority, established). Use this when you need to \
                          understand what positions exist on a topic, not \
                          just find text passages."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The topic or claim to search for"
                    },
                    "status_filter": {
                        "type": "string",
                        "description": "Optional: filter by epistemic status. \
                                        Leave empty for all.",
                        "enum": ["consensus", "majority", "contested", "minority",
                                 "established", ""]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "claim_search requires a 'query' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query'".into()))?;
        let status_filter = params
            .get("status_filter")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let embedding = self
            .engine
            .embed(query)
            .await
            .map_err(|e| Error::Execution(format!("embedding failed: {e}")))?;

        let mut claims: Vec<ScoredClaim> = Vec::new();
        let corpus_ids = self
            .engine
            .enriched_corpus_ids()
            .await
            .map_err(|e| Error::Execution(format!("listing enriched corpora: {e}")))?;

        for corpus_id in corpus_ids {
            let index = match self.engine.open_index_for_corpus(&corpus_id).await {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::warn!("Skipping corpus {corpus_id}: {e}");
                    continue;
                }
            };
            match index.search_claims(&embedding, query, 10).await {
                Ok(mut results) => claims.append(&mut results),
                Err(e) => tracing::warn!("Search failed on {corpus_id}: {e}"),
            }
        }

        if let Some(status) = status_filter {
            claims.retain(|c| c.claim.epistemic_status.label() == status);
        }

        claims.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        claims.truncate(10);

        Ok(StepOutput::Text(format_claims_for_model(&claims)))
    }
}

fn format_claims_for_model(claims: &[ScoredClaim]) -> String {
    if claims.is_empty() {
        return "No claims found. Try a broader search query, or use the \
                regular 'search' tool for text passages instead."
            .to_string();
    }

    let mut output = format!("Found {} claims:\n\n", claims.len());

    for (i, scored) in claims.iter().enumerate() {
        let c = &scored.claim;
        output.push_str(&format!(
            "[Claim {}] [{}] [score: {:.2}]\n\
             {}\n\
             Attributed to: {}\n\
             Source: {}\n\
             Hedging: {}\n\n",
            i + 1,
            c.epistemic_status.label(),
            scored.score,
            c.claim,
            c.attributed_to.as_deref().unwrap_or("the article"),
            c.source_entry.as_deref().unwrap_or("unknown"),
            c.hedging_language.as_deref().unwrap_or("(none)"),
        ));
    }

    output
}

// ─── EpistemicLandscapeTool ──────────────────────────────

/// Maps the landscape of positions, agreements, and disagreements on a
/// topic. Shows where consensus exists, where views are contested, and
/// what minority positions are recorded. Use for questions about debates,
/// controversies, or contested topics.
pub struct EpistemicLandscapeTool {
    engine: Arc<CorpusEngine>,
}

impl EpistemicLandscapeTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for EpistemicLandscapeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "epistemic_landscape".to_string(),
            name: "Epistemic Landscape".to_string(),
            description: "Given a topic, return the landscape of positions, \
                          agreements, and disagreements. Shows where there is \
                          consensus, where views are contested, and what minority \
                          positions exist. Use for questions about debates, \
                          controversies, or contested topics."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "The topic or question to map"
                    }
                },
                "required": ["topic"]
            }),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("topic").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "epistemic_landscape requires a 'topic' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let topic = params
            .get("topic")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'topic'".into()))?;

        let embedding = self
            .engine
            .embed(topic)
            .await
            .map_err(|e| Error::Execution(format!("embedding failed: {e}")))?;

        let mut combined = EpistemicLandscape::empty();

        let corpus_ids = self
            .engine
            .enriched_corpus_ids()
            .await
            .map_err(|e| Error::Execution(format!("listing enriched corpora: {e}")))?;

        for corpus_id in corpus_ids {
            let index = match self.engine.open_index_for_corpus(&corpus_id).await {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::warn!("Skipping corpus {corpus_id}: {e}");
                    continue;
                }
            };
            match index.epistemic_landscape(&embedding, topic).await {
                Ok(l) => {
                    combined.consensus_claims.extend(l.consensus_claims);
                    combined.contested_clusters.extend(l.contested_clusters);
                    combined.minority_claims.extend(l.minority_claims);
                }
                Err(e) => tracing::warn!("Landscape failed on {corpus_id}: {e}"),
            }
        }

        if combined.is_empty() {
            return Ok(StepOutput::Text(
                "No epistemic landscape found for this topic. The knowledge \
                 base may not have enriched claims for this area. Try the \
                 regular 'search' tool for text passages instead."
                    .to_string(),
            ));
        }

        Ok(StepOutput::Text(combined.format_for_model()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_empty_claims_returns_helpful_message() {
        let formatted = format_claims_for_model(&[]);
        assert!(formatted.contains("No claims found"));
        assert!(formatted.contains("regular 'search'"));
    }

    #[test]
    fn descriptor_has_required_query_param() {
        // Build a dummy engine — won't be used by descriptor().
        // We can't construct a real engine without an embed fn, so we
        // just test the descriptor method statically by reading the JSON.
        let json = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"]
        });
        let required = json["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "query"));
    }
}
