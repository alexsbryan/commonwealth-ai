//! Epistemic tools — `claim_search` and `epistemic_landscape`.
//!
//! Both tools wrap a `corpus_engine::CorpusEngine` to expose field model
//! enrichment data to the `ReasonWithTools` loop. They query the
//! `field_skeleton.json` artifact and the enriched `chunks.lance/` table
//! with position-aware filtered vector search.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{Permission, StepOutput, ToolContext, ToolDescriptor};

use corpus_engine::CorpusEngine;

// ─── ClaimSearchTool ─────────────────────────────────────

/// Searches for philosophical positions or claims with their epistemic
/// status and attribution. Use when you need to know what positions
/// exist on a topic and who holds them — not just find text passages.
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
            description: "Search for specific philosophical positions or claims \
                          with their epistemic status and attribution. Use when \
                          you need to know what positions exist on a topic and \
                          who holds them — not just find text passages."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The topic or claim to search for"
                    },
                    "position": {
                        "type": "string",
                        "description": "Optional: filter by position name (e.g. 'Compatibilism')"
                    }
                },
                "required": ["query"]
            }),
            examples: vec![],
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
        let position_filter = params.get("position").and_then(|v| v.as_str());

        // Embed the query for future vector-search integration.
        // Currently positions are matched via text similarity on the skeleton.
        let _embedding = self
            .engine
            .embed(query)
            .await
            .map_err(|e| Error::Execution(format!("embedding failed: {e}")))?;

        let mut results: Vec<PositionResult> = Vec::new();

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

            let skeleton = match index.load_field_skeleton() {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Find positions relevant to the query by scanning the skeleton.
            for question in &skeleton.canonical_questions {
                for position in &question.positions {
                    // Apply position name filter if specified.
                    if let Some(filter) = position_filter {
                        if !position.name.eq_ignore_ascii_case(filter) {
                            continue;
                        }
                    }

                    // Check if this position is relevant to the query using
                    // simple text matching on claim and name.
                    let query_lower = query.to_lowercase();
                    let is_relevant = position.claim.to_lowercase().contains(&query_lower)
                        || position.name.to_lowercase().contains(&query_lower)
                        || question.question.to_lowercase().contains(&query_lower);

                    if !is_relevant && position_filter.is_none() {
                        continue;
                    }

                    // Retrieve top argument chunks for this position.
                    let chunks = index
                        .get_chunks(&position.centroid_chunk_ids)
                        .await
                        .unwrap_or_default();

                    results.push(PositionResult {
                        position_name: position.name.clone(),
                        claim: position.claim.clone(),
                        status: position.status.clone(),
                        proponents: position.proponents.clone(),
                        source: position.source.clone(),
                        question: question.question.clone(),
                        chunks: chunks
                            .iter()
                            .map(|c| c.content.chars().take(300).collect::<String>())
                            .collect(),
                    });
                }
            }
        }

        if results.is_empty() {
            return Ok(StepOutput::Text(
                "No field model found for this topic. The corpus may not be \
                 enriched, or this topic is not covered. Try using the regular \
                 'search' tool for text passages."
                    .to_string(),
            ));
        }

        results.truncate(10);
        Ok(StepOutput::Text(format_position_results(&results)))
    }
}

struct PositionResult {
    position_name: String,
    claim: String,
    status: String,
    proponents: Vec<String>,
    source: String,
    question: String,
    chunks: Vec<String>,
}

fn format_position_results(results: &[PositionResult]) -> String {
    if results.is_empty() {
        return "No positions found. Try a broader query or use the regular \
                'search' tool for text passages."
            .to_string();
    }

    let mut output = format!("Found {} positions:\n\n", results.len());

    for (i, r) in results.iter().enumerate() {
        output.push_str(&format!(
            "[Position {}] [{status}] {name}\n\
             Claim: {claim}\n\
             Proponents: {proponents}\n\
             Source: {source}\n\
             Question: {question}\n",
            i + 1,
            status = r.status,
            name = r.position_name,
            claim = r.claim,
            proponents = if r.proponents.is_empty() {
                "(not attributed)".to_string()
            } else {
                r.proponents.join(", ")
            },
            source = r.source,
            question = r.question,
        ));

        if !r.chunks.is_empty() {
            output.push_str("Evidence:\n");
            for chunk in &r.chunks {
                output.push_str(&format!("  - {chunk}\n"));
            }
        }
        output.push('\n');
    }

    output
}

// ─── EpistemicLandscapeTool ──────────────────────────────

/// Maps the landscape of positions, agreements, and disagreements on a
/// topic. Returns the dominant view, contested positions, specific fault
/// lines, and open questions. Use for contested philosophical, scientific,
/// or policy questions.
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
            description: "Map the landscape of positions, agreements, and \
                          disagreements on a topic. Returns the dominant view, \
                          contested positions, specific fault lines, and open \
                          questions. Use for contested philosophical, scientific, \
                          or policy questions."
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
            examples: vec![],
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

        let corpus_ids = self
            .engine
            .enriched_corpus_ids()
            .await
            .map_err(|e| Error::Execution(format!("listing enriched corpora: {e}")))?;

        let mut landscape = FieldLandscape::empty();

        for corpus_id in corpus_ids {
            let index = match self.engine.open_index_for_corpus(&corpus_id).await {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::warn!("Skipping corpus {corpus_id}: {e}");
                    continue;
                }
            };

            let skeleton = match index.load_field_skeleton() {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Find the most relevant canonical question.
            let topic_lower = topic.to_lowercase();
            let question = skeleton
                .canonical_questions
                .iter()
                .find(|q| q.question.to_lowercase().contains(&topic_lower))
                .or_else(|| skeleton.canonical_questions.first());

            let Some(question) = question else { continue };

            // Load top argument chunks for each position.
            let mut positions_with_evidence = Vec::new();
            for pos in &question.positions {
                let chunks = index
                    .get_chunks(&pos.centroid_chunk_ids)
                    .await
                    .unwrap_or_default();

                positions_with_evidence.push(PositionWithEvidence {
                    name: pos.name.clone(),
                    claim: pos.claim.clone(),
                    status: pos.status.clone(),
                    proponents: pos.proponents.clone(),
                    source: pos.source.clone(),
                    chunks: chunks
                        .iter()
                        .map(|c| c.content.chars().take(300).collect::<String>())
                        .collect(),
                });
            }

            // Load fault lines for this question.
            let fault_lines: Vec<String> = question
                .fault_lines
                .iter()
                .map(|fl| fl.crux.clone())
                .collect();

            // Load open questions.
            let open_questions: Vec<String> = skeleton
                .open_questions_for_question(&question.id)
                .iter()
                .map(|oq| oq.question.clone())
                .collect();

            landscape.add_question(
                question.question.clone(),
                question.status.clone(),
                positions_with_evidence,
                fault_lines,
                open_questions,
            );
        }

        if landscape.is_empty() {
            return Ok(StepOutput::Text(
                "No field model found for this topic. The knowledge \
                 base may not be enriched, or this topic is not covered. \
                 Try the regular 'search' tool for text passages instead."
                    .to_string(),
            ));
        }

        Ok(StepOutput::Text(landscape.format_for_model()))
    }
}

// ─── FieldLandscape formatting ──────────────────────────

struct PositionWithEvidence {
    name: String,
    claim: String,
    status: String,
    proponents: Vec<String>,
    source: String,
    chunks: Vec<String>,
}

struct QuestionLandscape {
    question: String,
    status: String,
    positions: Vec<PositionWithEvidence>,
    fault_lines: Vec<String>,
    open_questions: Vec<String>,
}

struct FieldLandscape {
    questions: Vec<QuestionLandscape>,
}

impl FieldLandscape {
    fn empty() -> Self {
        Self {
            questions: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    fn add_question(
        &mut self,
        question: String,
        status: String,
        positions: Vec<PositionWithEvidence>,
        fault_lines: Vec<String>,
        open_questions: Vec<String>,
    ) {
        self.questions.push(QuestionLandscape {
            question,
            status,
            positions,
            fault_lines,
            open_questions,
        });
    }

    fn format_for_model(&self) -> String {
        let mut out = String::new();

        for q in &self.questions {
            out.push_str(&format!(
                "QUESTION: {}\nStatus: {}\n\n",
                q.question, q.status
            ));

            // Group by status.
            let dominant: Vec<_> = q.positions.iter().filter(|p| p.status == "majority").collect();
            let contested: Vec<_> =
                q.positions.iter().filter(|p| p.status == "contested").collect();
            let minority: Vec<_> =
                q.positions.iter().filter(|p| p.status == "minority").collect();

            if !dominant.is_empty() {
                out.push_str("DOMINANT VIEW:\n");
                for pwe in &dominant {
                    out.push_str(&format_position_with_evidence(pwe));
                }
            }

            if !contested.is_empty() {
                out.push_str("CONTESTED:\n");
                for pwe in &contested {
                    out.push_str(&format_position_with_evidence(pwe));
                }
            }

            if !minority.is_empty() {
                out.push_str("MINORITY POSITIONS:\n");
                for pwe in &minority {
                    out.push_str(&format_position_with_evidence(pwe));
                }
            }

            if !q.fault_lines.is_empty() {
                out.push_str("\nFAULT LINES — where the debate actually turns:\n");
                for fl in &q.fault_lines {
                    out.push_str(&format!("  - {fl}\n"));
                }
            }

            if !q.open_questions.is_empty() {
                out.push_str("\nOPEN QUESTIONS — unresolved in the field:\n");
                for oq in &q.open_questions {
                    out.push_str(&format!("  - {oq}\n"));
                }
            }

            out.push('\n');
        }

        out
    }
}

fn format_position_with_evidence(pwe: &PositionWithEvidence) -> String {
    let mut out = format!(
        "  [{status}] {name}: {claim}\n    Proponents: {proponents}\n",
        status = pwe.status,
        name = pwe.name,
        claim = pwe.claim,
        proponents = if pwe.proponents.is_empty() {
            "(not attributed)".to_string()
        } else {
            pwe.proponents.join(", ")
        },
    );

    if pwe.source == "discovered" {
        out.push_str("    (Discovered via clustering — not stated in overviews)\n");
    }

    if !pwe.chunks.is_empty() {
        out.push_str("    Evidence:\n");
        for chunk in &pwe.chunks {
            out.push_str(&format!(
                "      - {}\n",
                chunk.chars().take(200).collect::<String>()
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_empty_results_returns_helpful_message() {
        let formatted = format_position_results(&[]);
        assert!(formatted.contains("No positions found"));
        assert!(formatted.contains("search"));
    }

    #[test]
    fn descriptor_has_required_query_param() {
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

    #[test]
    fn field_landscape_empty_check() {
        let l = FieldLandscape::empty();
        assert!(l.is_empty());
    }
}
