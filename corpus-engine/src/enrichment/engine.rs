//! `EnrichmentEngine` — runs claim and relationship extraction prompts
//! against an existing `CorpusIndex`.

use serde::Deserialize;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::progress::{IngestProgress, ProgressCallback};
use crate::recipe::EnrichmentConfig;
use crate::types::{EmbedFn, InferenceFn};

use super::claims::{EpistemicStatus, ExtractedClaim};
use super::relationships::{ClaimRelationship, RelationshipType};

/// Runs the optional enrichment phase of the ingest pipeline.
///
/// Phase 1: walk every chunk in the index, ask the inference model to
/// extract propositional claims, embed each claim, return the list.
///
/// Phase 2 (optional): for each claim pair from different entries that
/// scores above a similarity threshold, ask the inference model whether
/// they are in any epistemic relationship.
pub struct EnrichmentEngine {
    embed: EmbedFn,
    inference: InferenceFn,
}

impl EnrichmentEngine {
    pub fn new(embed: EmbedFn, inference: InferenceFn) -> Self {
        Self { embed, inference }
    }

    /// Phase 1: extract claims from every chunk in the index.
    pub async fn extract_claims(
        &self,
        index: &CorpusIndex,
        config: &EnrichmentConfig,
        progress: &Option<ProgressCallback>,
    ) -> Result<Vec<ExtractedClaim>> {
        let chunks = index.all_chunks().await?;
        let total = chunks.len() as u64;
        let mut claims = Vec::new();
        let mut next_id: u64 = 0;
        let corpus_id = index.corpus_id().to_string();

        // Observability counters.
        let mut claims_found: u64 = 0;
        let mut inference_errors: u64 = 0;
        let mut parse_errors: u64 = 0;
        let mut window_start = std::time::Instant::now();
        let mut window_chunks: u64 = 0;
        let mut chunks_per_sec: f32 = 0.0;
        const REPORT_EVERY: usize = 50;

        for (i, chunk) in chunks.iter().enumerate() {
            window_chunks += 1;

            // Recompute throughput and emit terminal summary every REPORT_EVERY chunks.
            if i > 0 && i % REPORT_EVERY == 0 {
                let secs = window_start.elapsed().as_secs_f32().max(0.001);
                chunks_per_sec = window_chunks as f32 / secs;
                window_start = std::time::Instant::now();
                window_chunks = 0;
                eprintln!(
                    "[{corpus_id}] claims {}/{total} | {claims_found} found | \
                     {inference_errors} inf-err | {parse_errors} parse-err | {chunks_per_sec:.1} chunks/s",
                    i + 1,
                );
            }

            if let Some(ref cb) = progress {
                cb(IngestProgress::ExtractingClaims {
                    current: i as u64 + 1,
                    total,
                    claims_found,
                    inference_errors,
                    parse_errors,
                    chunks_per_sec,
                });
            }

            let prompt = format!(
                "{}\n\n---\nPassage:\n{}\n---",
                config.claim_extraction_prompt, chunk.content,
            );

            let response = match (self.inference)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    inference_errors += 1;
                    eprintln!("[{corpus_id}] chunk {i}/{total}: inference error — {e}");
                    continue;
                }
            };

            let raw_claims = match parse_extracted_claims(&response) {
                Some(v) => v,
                None => {
                    parse_errors += 1;
                    eprintln!(
                        "[{corpus_id}] chunk {i}/{total}: parse failed — {:?}",
                        &response[..response.len().min(120)],
                    );
                    continue;
                }
            };

            for raw in raw_claims {
                let status = EpistemicStatus::parse(&raw.epistemic_status);
                let embedding = match (self.embed)(&raw.claim).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("Embedding failed for claim '{}': {e}", raw.claim);
                        continue;
                    }
                };

                claims.push(ExtractedClaim {
                    id: next_id,
                    claim: raw.claim,
                    source_chunk_id: chunk.id,
                    source_chunk_hash: Some(crate::engine::blake3_hex(&chunk.content)),
                    corpus_id: corpus_id.clone(),
                    epistemic_status: status,
                    hedging_language: raw.hedging_language,
                    attributed_to: raw.attributed_to,
                    source_entry: chunk.title.clone(),
                    embedding,
                });
                next_id += 1;
                claims_found += 1;
            }
        }

        eprintln!(
            "[{corpus_id}] claims extraction complete — {claims_found} claims from {total} chunks \
             ({inference_errors} inf-err, {parse_errors} parse-err)",
        );

        Ok(claims)
    }

    /// Phase 2: extract relationships between claims from different entries.
    ///
    /// Candidate pairs are found by vector-similarity search on the claim
    /// embeddings. Pairs from the same source entry are skipped (they would
    /// describe the same position from the same author). Each candidate pair
    /// is sent to the inference model with the relationship extraction prompt.
    pub async fn extract_relationships(
        &self,
        claims: &[ExtractedClaim],
        config: &EnrichmentConfig,
        progress: &Option<ProgressCallback>,
    ) -> Result<Vec<ClaimRelationship>> {
        let prompt_template = match config.relationship_extraction_prompt.as_deref() {
            Some(t) => t,
            None => {
                tracing::warn!("extract_relationships called but no prompt configured");
                return Ok(Vec::new());
            }
        };

        let candidates = find_candidate_pairs(
            claims,
            config.relationship_similarity_threshold,
            config.max_relationship_candidates,
        );

        if let Some(ref cb) = progress {
            cb(IngestProgress::FoundCandidatePairs {
                count: candidates.len(),
            });
        }

        let total = candidates.len() as u64;
        let mut relationships = Vec::new();
        let mut next_id: u64 = 0;

        for (i, (a_idx, b_idx)) in candidates.iter().enumerate() {
            if i.is_multiple_of(100) {
                if let Some(ref cb) = progress {
                    cb(IngestProgress::ExtractingRelationships {
                        current: i as u64,
                        total,
                    });
                }
            }

            let claim_a = &claims[*a_idx];
            let claim_b = &claims[*b_idx];

            let prompt = prompt_template
                .replace("{claim_a}", &claim_a.claim)
                .replace(
                    "{source_a}",
                    claim_a.source_entry.as_deref().unwrap_or("unknown"),
                )
                .replace(
                    "{attributed_a}",
                    claim_a.attributed_to.as_deref().unwrap_or("the article"),
                )
                .replace("{claim_b}", &claim_b.claim)
                .replace(
                    "{source_b}",
                    claim_b.source_entry.as_deref().unwrap_or("unknown"),
                )
                .replace(
                    "{attributed_b}",
                    claim_b.attributed_to.as_deref().unwrap_or("the article"),
                );

            let response = match (self.inference)(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Inference failed for pair ({},{}): {e}", claim_a.id, claim_b.id);
                    continue;
                }
            };

            let raw = match parse_raw_relationship(&response) {
                Some(r) => r,
                None => continue,
            };

            // The model can return "none" to indicate no relationship.
            let rel_type = match RelationshipType::parse(&raw.relationship) {
                Some(t) => t,
                None => continue, // includes "none"
            };

            if raw.confidence < 0.5 {
                continue;
            }

            relationships.push(ClaimRelationship {
                id: next_id,
                claim_a_id: claim_a.id,
                claim_b_id: claim_b.id,
                relationship: rel_type,
                connecting_issue: raw.connecting_issue,
                evidence_chunk_ids: vec![claim_a.source_chunk_id, claim_b.source_chunk_id],
                confidence: raw.confidence,
            });
            next_id += 1;
        }

        Ok(relationships)
    }
}

// ─── Candidate pair finding ─────────────────────────────────

/// Find pairs of claims (a, b) where:
/// - They come from different source entries.
/// - Their embedding cosine similarity is above `threshold`.
/// - We only include each unordered pair once (a.id < b.id).
///
/// Stops once `max_candidates` pairs have been found.
///
/// Uses brute-force similarity rather than LanceDB ANN because the claim
/// set is typically much smaller than the chunk set, and brute-force
/// over a few thousand claims is fast and deterministic.
fn find_candidate_pairs(
    claims: &[ExtractedClaim],
    threshold: f32,
    max_candidates: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..claims.len() {
        for j in (i + 1)..claims.len() {
            if claims[i].source_entry == claims[j].source_entry {
                continue;
            }
            if claims[i].embedding.is_empty() || claims[j].embedding.is_empty() {
                continue;
            }
            let sim = cosine_similarity(&claims[i].embedding, &claims[j].embedding);
            if sim >= threshold {
                out.push((i, j));
                if out.len() >= max_candidates {
                    return out;
                }
            }
        }
    }
    out
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

// ─── Response parsing ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawExtractedClaim {
    claim: String,
    epistemic_status: String,
    #[serde(default)]
    hedging_language: Option<String>,
    #[serde(default)]
    attributed_to: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRelationship {
    relationship: String,
    #[serde(default)]
    connecting_issue: Option<String>,
    #[serde(default)]
    confidence: f32,
}

/// Parse a JSON array of claims out of an inference response, tolerating
/// markdown code fences and `<think>` blocks.
///
/// Returns `Some(claims)` on success (including empty arrays — the model
/// legitimately found no claims). Returns `None` only when JSON extraction
/// failed entirely, so callers can distinguish a real parse error from a
/// valid empty result.
fn parse_extracted_claims(response: &str) -> Option<Vec<RawExtractedClaim>> {
    let cleaned = strip_think_tags(response);
    let s = cleaned.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    if let Ok(claims) = serde_json::from_str::<Vec<RawExtractedClaim>>(s) {
        return Some(claims);
    }
    if let Some(json) = extract_json_from_response(s) {
        if let Ok(claims) = serde_json::from_str::<Vec<RawExtractedClaim>>(&json) {
            return Some(claims);
        }
    }
    None
}

fn parse_raw_relationship(response: &str) -> Option<RawRelationship> {
    let cleaned = strip_think_tags(response);
    let s = cleaned.trim();
    if let Ok(r) = serde_json::from_str::<RawRelationship>(s) {
        return Some(r);
    }
    if let Some(json) = extract_json_from_response(s) {
        if let Ok(r) = serde_json::from_str::<RawRelationship>(&json) {
            return Some(r);
        }
    }
    None
}

/// Remove all `<think>…</think>` blocks from a model response.
/// Qwen3 and similar models may emit these even when thinking is nominally
/// disabled; stripping them before JSON extraction prevents false-positive
/// parse-error logs and keeps the JSON extractor from finding stray brackets
/// inside the think block.
fn strip_think_tags(s: &str) -> String {
    if !s.contains("<think>") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(rel_end) => {
                rest = &rest[start + rel_end + "</think>".len()..];
            }
            None => break, // unclosed tag — drop the rest
        }
    }
    out.push_str(rest);
    out
}

/// Strip markdown code fences and return the inner JSON, if present.
/// Looks for the first `[` or `{` and the matching last `]` or `}`.
fn extract_json_from_response(response: &str) -> Option<String> {
    // Try to find a fenced code block first.
    if let Some(start) = response.find("```") {
        let after = &response[start + 3..];
        // Skip optional language tag.
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        if let Some(end) = body.find("```") {
            return Some(body[..end].trim().to_string());
        }
    }

    // Otherwise look for the first JSON-shaped substring.
    let first_array = response.find('[');
    let first_obj = response.find('{');
    let start = match (first_array, first_obj) {
        (Some(a), Some(o)) => Some(a.min(o)),
        (Some(a), None) => Some(a),
        (None, Some(o)) => Some(o),
        _ => None,
    }?;

    let last_array = response.rfind(']');
    let last_obj = response.rfind('}');
    let end = match (last_array, last_obj) {
        (Some(a), Some(o)) => Some(a.max(o)),
        (Some(a), None) => Some(a),
        (None, Some(o)) => Some(o),
        _ => None,
    }?;

    if end < start {
        return None;
    }
    Some(response[start..=end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mock_embed_zero() -> EmbedFn {
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
    }

    fn mock_inference_canned(json: String) -> InferenceFn {
        Arc::new(move |_prompt: &str| {
            let json = json.clone();
            Box::pin(async move { Ok(json) })
        })
    }

    #[test]
    fn extract_json_handles_plain_array() {
        let resp = r#"[{"claim": "x", "epistemic_status": "consensus"}]"#;
        let parsed = parse_extracted_claims(resp).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].claim, "x");
    }

    #[test]
    fn extract_json_handles_code_fence() {
        let resp = "Here you go:\n```json\n[{\"claim\": \"y\", \"epistemic_status\": \"contested\"}]\n```\n";
        let parsed = parse_extracted_claims(resp).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].claim, "y");
        assert_eq!(parsed[0].epistemic_status, "contested");
    }

    #[test]
    fn extract_json_handles_surrounding_prose() {
        let resp = r#"The claims are: [{"claim": "z", "epistemic_status": "majority"}] thank you"#;
        let parsed = parse_extracted_claims(resp).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].claim, "z");
    }

    #[test]
    fn empty_array_is_some_not_none() {
        // Model legitimately found no claims — should be Some([]), not a parse error.
        let resp = "<think>\n</think>\n\n```json\n[]\n```";
        let parsed = parse_extracted_claims(resp).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn strip_think_tags_removes_block() {
        let s = "<think>\nsome reasoning\n</think>\n\nActual response";
        assert_eq!(strip_think_tags(s).trim(), "Actual response");
    }

    #[test]
    fn strip_think_tags_empty_block() {
        let s = "<think>\n</think>\n\n```json\n[]\n```";
        let stripped = strip_think_tags(s);
        assert!(!stripped.contains("<think>"));
        assert!(stripped.contains("```json"));
    }

    #[test]
    fn parse_fails_on_truly_unparseable() {
        let resp = "Sorry, I cannot help with that.";
        assert!(parse_extracted_claims(resp).is_none());
    }

    #[test]
    fn parse_relationship_with_none_returns_some_but_unfiltered() {
        let resp = r#"{"relationship": "none", "confidence": 0.0}"#;
        let raw = parse_raw_relationship(resp).unwrap();
        assert_eq!(raw.relationship, "none");
        // The caller filters this out via RelationshipType::parse returning None.
        assert!(RelationshipType::parse(&raw.relationship).is_none());
    }

    #[test]
    fn cosine_similarity_basic() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        let c = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
    }

    #[test]
    fn find_candidates_skips_same_entry() {
        let claims = vec![
            ExtractedClaim {
                id: 1,
                claim: "a".into(),
                source_chunk_id: 1,
                source_chunk_hash: None,
                corpus_id: "test".into(),
                epistemic_status: EpistemicStatus::Contested,
                hedging_language: None,
                attributed_to: None,
                source_entry: Some("entry1".into()),
                embedding: vec![1.0, 0.0],
            },
            ExtractedClaim {
                id: 2,
                claim: "b".into(),
                source_chunk_id: 2,
                source_chunk_hash: None,
                corpus_id: "test".into(),
                epistemic_status: EpistemicStatus::Contested,
                hedging_language: None,
                attributed_to: None,
                source_entry: Some("entry1".into()), // same entry
                embedding: vec![1.0, 0.0],
            },
            ExtractedClaim {
                id: 3,
                claim: "c".into(),
                source_chunk_id: 3,
                source_chunk_hash: None,
                corpus_id: "test".into(),
                epistemic_status: EpistemicStatus::Contested,
                hedging_language: None,
                attributed_to: None,
                source_entry: Some("entry2".into()),
                embedding: vec![1.0, 0.0],
            },
        ];
        let pairs = find_candidate_pairs(&claims, 0.5, 100);
        // Only (entry1, entry2) pairs, not (entry1, entry1).
        assert_eq!(pairs.len(), 2);
        // Each pair must straddle the entry boundary.
        for (a, b) in &pairs {
            assert_ne!(claims[*a].source_entry, claims[*b].source_entry);
        }
    }

    #[test]
    fn find_candidates_respects_max() {
        let claims: Vec<_> = (0..10)
            .map(|i| ExtractedClaim {
                id: i,
                claim: format!("c{i}"),
                source_chunk_id: i,
                source_chunk_hash: None,
                corpus_id: "test".into(),
                epistemic_status: EpistemicStatus::Contested,
                hedging_language: None,
                attributed_to: None,
                source_entry: Some(format!("entry{i}")),
                embedding: vec![1.0, 0.0],
            })
            .collect();
        let pairs = find_candidate_pairs(&claims, 0.5, 5);
        assert!(pairs.len() <= 5);
    }
}
