//! `EnrichmentEngine` — runs claim and relationship extraction prompts
//! against an existing `CorpusIndex`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::index::{CorpusIndex, EnrichmentState};
use crate::progress::{IngestProgress, ProgressCallback};
use crate::recipe::EnrichmentConfig;
use crate::types::{EmbedFn, InferenceFn};

use super::claims::{EpistemicStatus, ExtractedClaim};
use super::filter::is_chunk_eligible;
use super::relationships::{ClaimRelationship, RelationshipType};

/// Prompt version tag. When the prompt changes this string must be bumped so
/// that `save_enrichment_state` records the new version and future tooling can
/// detect stale claims.
const EXTRACTION_PROMPT_VERSION: &str = "v2";

/// Number of passages bundled into a single inference call.
const BATCH_SIZE: usize = 4;

/// Persisted record of a chunk whose enrichment failed at the parse stage.
///
/// Written to `_enrichment_failures.ndjson` inside the corpus directory.
/// Because only the raw inference response is stored (not re-generated),
/// retries are cheap: reload the file, run a better parser, embed, store.
/// Backwards-compatible: old indices have no failures file → empty list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentFailure {
    pub chunk_id: u64,
    pub corpus_id: String,
    /// Chunk title, needed to reconstruct `ExtractedClaim.source_entry` on retry.
    pub source_entry: Option<String>,
    /// `"parse"` — inference succeeded but JSON could not be extracted.
    pub error_kind: String,
    /// The raw inference response that failed to parse.
    pub raw_response: String,
    /// Unix timestamp (seconds) of the failed attempt.
    pub attempted_at: u64,
}

/// Runs the optional enrichment phase of the ingest pipeline.
///
/// Phase 1: walk every chunk in the index, ask the inference model to
/// extract propositional claims, embed each claim, return the list.
///
/// Phase 2 (optional): for each claim pair from the *same* source entry that
/// scores above a similarity threshold, ask the inference model whether
/// they are in any epistemic relationship.
pub struct EnrichmentEngine {
    embed: EmbedFn,
    /// Primary slot — used for relationship extraction and as fallback.
    inference: InferenceFn,
    /// Fast slot (e.g. Qwen3-1.7B) — used for claim extraction.
    /// Falls back to `inference` when `None`.
    fast_inference: Option<InferenceFn>,
}

impl EnrichmentEngine {
    /// Create an engine with only the primary inference slot.
    pub fn new(embed: EmbedFn, inference: InferenceFn) -> Self {
        Self { embed, inference, fast_inference: None }
    }

    /// Create an engine with both a primary and a fast inference slot.
    ///
    /// Claim extraction is routed through `fast_inference` (typically a
    /// small, fast model like Qwen3-1.7B).  Relationship extraction and
    /// everything else uses `inference` (the primary/large slot).
    pub fn new_with_fast(
        embed: EmbedFn,
        inference: InferenceFn,
        fast_inference: Option<InferenceFn>,
    ) -> Self {
        Self { embed, inference, fast_inference }
    }

    /// Return the fast slot if set, otherwise fall back to the primary slot.
    fn effective_fast(&self) -> &InferenceFn {
        self.fast_inference.as_ref().unwrap_or(&self.inference)
    }

    /// Phase 1: extract claims from every eligible chunk in the index.
    ///
    /// Changes from the original single-chunk loop:
    /// - Pre-filters chunks with `is_chunk_eligible()` (zero inference cost).
    /// - Batches `BATCH_SIZE` passages into a single inference call.
    /// - Routes to the fast slot via `effective_fast()`.
    /// - Checkpoints after every batch; resumes from `last_chunk_id` on restart.
    pub async fn extract_claims(
        &self,
        index: &CorpusIndex,
        config: &EnrichmentConfig,
        progress: &Option<ProgressCallback>,
    ) -> Result<Vec<ExtractedClaim>> {
        let corpus_id = index.corpus_id().to_string();

        // ── Checkpoint / resume ────────────────────────────────────────────────
        let checkpoint = index.load_enrichment_state();
        let resume_chunk_id: Option<u64> = checkpoint
            .as_ref()
            .filter(|s| s.status == "in_progress" && s.phase == "claims")
            .and_then(|s| s.last_chunk_id);

        if let Some(ref s) = checkpoint {
            if s.status == "complete" && s.phase == "claims" {
                eprintln!("[{corpus_id}] Phase 1 already complete — skipping claim extraction");
                // Claims are already stored in LanceDB; Phase 2 loads them directly.
                return Ok(Vec::new());
            }
        }

        // ── Load all chunks ────────────────────────────────────────────────────
        let all_chunks = index.all_chunks().await?;
        let total_all = all_chunks.len() as u64;

        // ── Filter eligible chunks ─────────────────────────────────────────────
        // Step A: skip already-processed chunks (resume path).
        // Step B: eligibility heuristic (zero inference cost).
        let eligible_chunks: Vec<_> = all_chunks
            .iter()
            .filter(|c| {
                if let Some(resume_id) = resume_chunk_id {
                    if c.id <= resume_id {
                        return false;
                    }
                }
                is_chunk_eligible(&c.content, c.title.as_deref())
            })
            .collect();

        let filtered_count = total_all.saturating_sub(eligible_chunks.len() as u64);
        eprintln!(
            "[{corpus_id}] {total_all} total chunks, {} eligible after filter \
             ({filtered_count} skipped)",
            eligible_chunks.len(),
        );

        // ── Fresh start: drop any stale claims from a prior run ───────────────
        // If there is no in-progress checkpoint we are starting from scratch,
        // not resuming.  Appending to an existing table would create duplicates.
        if resume_chunk_id.is_none() {
            if let Err(e) = index.drop_claims_tables().await {
                eprintln!("[{corpus_id}] Warning: could not clear old claims table: {e}");
            } else {
                eprintln!("[{corpus_id}] Cleared existing claims/relationships tables for fresh run");
            }
        }

        // ── Write initial checkpoint ───────────────────────────────────────────
        let _ = index.save_enrichment_state(
            &EnrichmentState {
                status: "in_progress".to_string(),
                last_chunk_id: resume_chunk_id,
                phase: "claims".to_string(),
                relationships_last_article: None,
            },
            EXTRACTION_PROMPT_VERSION,
        );

        let total = eligible_chunks.len() as u64;
        let mut claims = Vec::new();
        let mut next_id: u64 = 0;
        let mut flush_offset: usize = 0;
        const FLUSH_EVERY: usize = 50; // flush every N claims (not chunks)

        // Observability counters.
        let mut claims_found: u64 = 0;
        let mut inference_errors: u64 = 0;
        let mut parse_errors: u64 = 0;
        let mut window_start = std::time::Instant::now();
        let mut window_batches: u64 = 0;
        let mut chunks_per_sec: f32 = 0.0;
        const REPORT_EVERY: usize = 10; // batches (= BATCH_SIZE * 10 chunks)

        // Retry queue for chunks whose batch slot failed to parse.
        let mut retry_queue: Vec<&crate::index::StoredChunk> = Vec::new();

        // ── Batch loop ─────────────────────────────────────────────────────────
        for (batch_idx, batch) in eligible_chunks.chunks(BATCH_SIZE).enumerate() {
            window_batches += 1;

            if batch_idx > 0 && batch_idx % REPORT_EVERY == 0 {
                let secs = window_start.elapsed().as_secs_f32().max(0.001);
                chunks_per_sec = (window_batches * BATCH_SIZE as u64) as f32 / secs;
                window_start = std::time::Instant::now();
                window_batches = 0;
                let processed = batch_idx * BATCH_SIZE;
                eprintln!(
                    "[{corpus_id}] claims {processed}/{total} | {claims_found} found | \
                     {inference_errors} inf-err | {parse_errors} parse-err | {chunks_per_sec:.1} chunks/s",
                );
            }

            if let Some(ref cb) = progress {
                cb(IngestProgress::ExtractingClaims {
                    current: (batch_idx * BATCH_SIZE) as u64 + 1,
                    total,
                    claims_found,
                    inference_errors,
                    parse_errors,
                    chunks_per_sec,
                });
            }

            // ── Build batched prompt ───────────────────────────────────────────
            let prompt = build_batch_prompt(&config.claim_extraction_prompt, batch);

            let response = match (self.effective_fast())(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    inference_errors += 1;
                    eprintln!("[{corpus_id}] batch {batch_idx}: inference error — {e}");
                    // Queue all chunks in this batch for individual retry.
                    retry_queue.extend(batch.iter().copied());
                    continue;
                }
            };

            // ── Parse batch response ───────────────────────────────────────────
            let parsed = parse_batch_response(&response);

            for (slot_idx, chunk) in batch.iter().enumerate() {
                let slot_key = format!("{}", slot_idx + 1);
                let raw_claims = match parsed.get(&slot_key) {
                    Some(v) => v.as_slice(),
                    None => {
                        // This slot was missing or malformed — queue for retry.
                        retry_queue.push(chunk);
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
                        claim: raw.claim.clone(),
                        source_chunk_id: chunk.id,
                        source_chunk_hash: Some(crate::engine::blake3_hex(&chunk.content)),
                        corpus_id: corpus_id.clone(),
                        epistemic_status: status,
                        hedging_language: raw.hedging_language.clone(),
                        attributed_to: raw.attributed_to.clone(),
                        source_entry: chunk.title.clone(),
                        embedding,
                    });
                    next_id += 1;
                    claims_found += 1;
                }
            }

            // ── Incremental flush ──────────────────────────────────────────────
            if claims.len() >= flush_offset + FLUSH_EVERY {
                let new_claims = &claims[flush_offset..];
                match index.store_claims(new_claims).await {
                    Ok(()) => {
                        eprintln!(
                            "[{corpus_id}] Flushed {} new claims (total: {})",
                            new_claims.len(), claims.len()
                        );
                        flush_offset = claims.len();
                    }
                    Err(e) => eprintln!("[{corpus_id}] Warning: claim flush failed: {e}"),
                }
            }

            // ── Write batch checkpoint ─────────────────────────────────────────
            if let Some(last_chunk) = batch.last() {
                let _ = index.save_enrichment_state(
                    &EnrichmentState {
                        status: "in_progress".to_string(),
                        last_chunk_id: Some(last_chunk.id),
                        phase: "claims".to_string(),
                        relationships_last_article: None,
                    },
                    EXTRACTION_PROMPT_VERSION,
                );
            }
        }

        // ── Individual retry queue ─────────────────────────────────────────────
        for chunk in &retry_queue {
            let passage = truncate_passage(&chunk.content);
            let prompt = format!(
                "{}\n\n---\nPassage:\n{}\n---",
                config.claim_extraction_prompt, passage,
            );

            let response = match (self.effective_fast())(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    inference_errors += 1;
                    eprintln!("[{corpus_id}] retry chunk {}: inference error — {e}", chunk.id);
                    continue;
                }
            };

            let raw_claims = match parse_extracted_claims(&response) {
                Some(v) => v,
                None => {
                    parse_errors += 1;
                    eprintln!(
                        "[{corpus_id}] retry chunk {}: parse failed — {:?}",
                        chunk.id,
                        &response[..response.len().min(120)],
                    );
                    let failure = EnrichmentFailure {
                        chunk_id: chunk.id,
                        corpus_id: corpus_id.clone(),
                        source_entry: chunk.title.clone(),
                        error_kind: "parse".to_string(),
                        raw_response: response,
                        attempted_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    };
                    let _ = index.append_enrichment_failure(&failure);
                    continue;
                }
            };

            for raw in raw_claims {
                let status = EpistemicStatus::parse(&raw.epistemic_status);
                let embedding = match (self.embed)(&raw.claim).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "Embedding failed during retry for chunk {}: {e}", chunk.id
                        );
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

        // ── Final flush ────────────────────────────────────────────────────────
        if claims.len() > flush_offset {
            let new_claims = &claims[flush_offset..];
            match index.store_claims(new_claims).await {
                Ok(()) => {
                    eprintln!(
                        "[{corpus_id}] Final flush: {} new claims to disk (total: {})",
                        new_claims.len(), claims.len()
                    );
                }
                Err(e) => eprintln!("[{corpus_id}] Warning: final claim flush failed: {e}"),
            }
        }

        // ── Mark Phase 1 complete ──────────────────────────────────────────────
        let _ = index.save_enrichment_state(
            &EnrichmentState {
                status: "complete".to_string(),
                last_chunk_id: eligible_chunks.last().map(|c| c.id),
                phase: "claims".to_string(),
                relationships_last_article: None,
            },
            EXTRACTION_PROMPT_VERSION,
        );

        eprintln!(
            "[{corpus_id}] Phase 1 complete — {claims_found} claims from {total} eligible \
             chunks ({filtered_count} filtered, {inference_errors} inf-err, {parse_errors} parse-err)",
        );

        Ok(claims)
    }

    /// Re-run claim extraction on previously-failed chunks using improved parsers.
    ///
    /// Loads `_enrichment_failures.ndjson`, attempts to parse each stored
    /// `raw_response` with `try_repair_truncated_claims()` (which handles
    /// truncated JSON arrays), embeds successfully-parsed claims, and removes
    /// resolved records from the file.
    ///
    /// Returns the new `ExtractedClaim`s; callers should pass them to
    /// `index.store_claims()`. Backwards-compatible: returns `Ok([])` if no
    /// failures file exists.
    pub async fn retry_parse_failures(
        &self,
        index: &CorpusIndex,
    ) -> Result<Vec<ExtractedClaim>> {
        let failures = index.load_enrichment_failures();
        // Clone parse failures so `failures` remains owned for the final filter.
        let parse_failures: Vec<EnrichmentFailure> = failures
            .iter()
            .filter(|f| f.error_kind == "parse")
            .cloned()
            .collect();

        if parse_failures.is_empty() {
            return Ok(Vec::new());
        }

        let corpus_id = index.corpus_id();
        eprintln!(
            "[{corpus_id}] Retrying {} parse failures with repair parser…",
            parse_failures.len(),
        );

        let mut resolved_chunk_ids: Vec<u64> = Vec::new();
        let mut new_claims: Vec<ExtractedClaim> = Vec::new();
        let mut next_id: u64 = 0;

        for failure in &parse_failures {
            let raw_claims = match try_repair_truncated_claims(&failure.raw_response) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };

            for raw in raw_claims {
                let status = EpistemicStatus::parse(&raw.epistemic_status);
                let embedding = match (self.embed)(&raw.claim).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "Embedding failed during retry for chunk {}: {e}",
                            failure.chunk_id
                        );
                        continue;
                    }
                };
                new_claims.push(ExtractedClaim {
                    id: next_id,
                    claim: raw.claim,
                    source_chunk_id: failure.chunk_id,
                    source_chunk_hash: None,
                    corpus_id: failure.corpus_id.clone(),
                    epistemic_status: status,
                    hedging_language: raw.hedging_language,
                    attributed_to: raw.attributed_to,
                    source_entry: failure.source_entry.clone(),
                    embedding,
                });
                next_id += 1;
            }
            resolved_chunk_ids.push(failure.chunk_id);
        }

        if !resolved_chunk_ids.is_empty() {
            let remaining: Vec<EnrichmentFailure> = failures
                .into_iter()
                .filter(|f| !resolved_chunk_ids.contains(&f.chunk_id))
                .collect();
            let _ = index.save_enrichment_failures(&remaining);
            eprintln!(
                "[{corpus_id}] Retry resolved {}/{} parse failures → {} new claims",
                resolved_chunk_ids.len(),
                parse_failures.len(),
                new_claims.len(),
            );
        }

        Ok(new_claims)
    }

    /// Phase 2: extract relationships between claims *within* the same source entry.
    ///
    /// Candidate pairs are found by vector-similarity search on the claim
    /// embeddings.  Only pairs from the same source entry (article) are
    /// considered.  Processing is grouped by source_entry so that the
    /// checkpoint cursor (`relationships_last_article`) can be written after
    /// each article completes, enabling crash-safe resume.
    pub async fn extract_relationships_within_article(
        &self,
        claims: &[ExtractedClaim],
        config: &EnrichmentConfig,
        progress: &Option<ProgressCallback>,
        index: &CorpusIndex,
    ) -> Result<Vec<ClaimRelationship>> {
        let prompt_template = match config.relationship_extraction_prompt.as_deref() {
            Some(t) => t,
            None => {
                tracing::warn!("extract_relationships_within_article called but no prompt configured");
                return Ok(Vec::new());
            }
        };

        // ── Phase 2 checkpoint / resume ────────────────────────────────────────
        let resume_article: Option<String> = index
            .load_enrichment_state()
            .filter(|s| s.phase == "relationships")
            .and_then(|s| s.relationships_last_article);

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

        // ── Group candidates by source_entry for per-article checkpoint ────────
        // Map: article_title → Vec<(a_idx, b_idx)>
        let mut by_article: Vec<(String, Vec<(usize, usize)>)> = {
            let mut map: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
            for &(a_idx, b_idx) in &candidates {
                let key = claims[a_idx]
                    .source_entry
                    .clone()
                    .unwrap_or_default();
                map.entry(key).or_default().push((a_idx, b_idx));
            }
            let mut v: Vec<_> = map.into_iter().collect();
            v.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic order
            v
        };

        // Skip articles already processed in a prior run.
        if let Some(ref last) = resume_article {
            by_article.retain(|(article, _)| article > last);
            eprintln!(
                "[{}] Phase 2 resume: skipping articles up to {:?}, {} remaining",
                index.corpus_id(), last, by_article.len()
            );
        }

        let total = candidates.len() as u64;
        let mut relationships = Vec::new();
        let mut next_id: u64 = 0;
        let mut processed_pairs: u64 = 0;

        for (article, pairs) in &by_article {
            for (i, &(a_idx, b_idx)) in pairs.iter().enumerate() {
                if processed_pairs.is_multiple_of(100) {
                    if let Some(ref cb) = progress {
                        cb(IngestProgress::ExtractingRelationships {
                            current: processed_pairs,
                            total,
                        });
                    }
                }

                let claim_a = &claims[a_idx];
                let claim_b = &claims[b_idx];

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
                        tracing::warn!(
                            "Inference failed for pair ({},{}): {e}",
                            claim_a.id, claim_b.id
                        );
                        processed_pairs += 1;
                        continue;
                    }
                };

                let raw = match parse_raw_relationship(&response) {
                    Some(r) => r,
                    None => { processed_pairs += 1; continue; }
                };

                let rel_type = match RelationshipType::parse(&raw.relationship) {
                    Some(t) => t,
                    None => { processed_pairs += 1; continue; }
                };

                if raw.confidence < 0.5 {
                    processed_pairs += 1;
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
                let _ = i;
                processed_pairs += 1;
            } // end pair loop

            // Write per-article checkpoint after all pairs for this article are done.
            let _ = index.save_enrichment_state(
                &EnrichmentState {
                    status: "in_progress".to_string(),
                    last_chunk_id: None,
                    phase: "relationships".to_string(),
                    relationships_last_article: Some(article.clone()),
                },
                EXTRACTION_PROMPT_VERSION,
            );
        } // end article loop

        Ok(relationships)
    }
}

// ─── Candidate pair finding ─────────────────────────────────

/// Find pairs of claims (a, b) where:
/// - They come from the **same** source entry (within-article).
/// - Their embedding cosine similarity is above `threshold`.
/// - We only include each unordered pair once (i < j).
///
/// Stops once `max_candidates` pairs have been found.
fn find_candidate_pairs(
    claims: &[ExtractedClaim],
    threshold: f32,
    max_candidates: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..claims.len() {
        for j in (i + 1)..claims.len() {
            // Skip pairs from *different* entries — we only want within-article.
            if claims[i].source_entry != claims[j].source_entry {
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

// ─── Batch prompt / response helpers ──────────────────────────────────────────

/// Build a numbered multi-passage prompt from a preamble and a slice of chunks.
///
/// Output format:
/// ```text
/// {preamble}
///
/// PASSAGES:
/// 1. {chunk1.content}
///
/// 2. {chunk2.content}
///
/// Respond with exactly:
/// {"1": [...], "2": [...]}
/// Each array: [{"claim": "...", "epistemic_status": "...", "hedging_language": "...", "attributed_to": "..."}]
/// Max 3 items per array. Empty array [] if no qualifying claims.
/// ```
fn build_batch_prompt(preamble: &str, chunks: &[&crate::index::StoredChunk]) -> String {
    let mut buf = String::with_capacity(4096);
    buf.push_str(preamble);
    buf.push_str("\n\nPASSAGES:\n");
    for (i, chunk) in chunks.iter().enumerate() {
        let passage = truncate_passage(&chunk.content);
        buf.push_str(&format!("{}. {}\n\n", i + 1, passage));
    }
    // Build expected keys.
    let keys: Vec<String> = (1..=chunks.len()).map(|n| format!("\"{n}\": [...]")).collect();
    buf.push_str(&format!(
        "Respond with exactly:\n{{{}}}\n\
         Each array: [{{\"claim\": \"...\", \"epistemic_status\": \"...\", \
         \"hedging_language\": \"...\", \"attributed_to\": \"...\"}}]\n\
         Max 3 items per array. Empty array [] if no qualifying claims.",
        keys.join(", ")
    ));
    buf
}

/// Parse a batch inference response into a map of slot key → claims.
///
/// Slot keys are `"1"` through `"4"` (or however many were in the batch).
/// Missing or malformed keys are omitted so callers can queue those chunks
/// for individual retry.
fn parse_batch_response(response: &str) -> HashMap<String, Vec<RawExtractedClaim>> {
    let cleaned = strip_think_tags(response);
    let s = cleaned.trim();

    // Try to extract a JSON object from the response.
    let json = extract_json_object_from_response(s).unwrap_or_else(|| s.to_string());

    let map: serde_json::Map<String, serde_json::Value> =
        match serde_json::from_str(&json) {
            Ok(m) => m,
            Err(_) => return HashMap::new(),
        };

    let mut out: HashMap<String, Vec<RawExtractedClaim>> = HashMap::new();
    for (k, v) in &map {
        // Each value should be an array of claim objects.
        if let Some(arr) = v.as_array() {
            let claims: Vec<RawExtractedClaim> = arr
                .iter()
                .filter_map(lenient_claim_from_value)
                .collect();
            out.insert(k.clone(), claims);
        }
    }
    out
}

/// Truncate a passage to 6000 chars at the last whitespace boundary.
fn truncate_passage(content: &str) -> &str {
    const MAX_PASSAGE_CHARS: usize = 6000;
    if content.len() <= MAX_PASSAGE_CHARS {
        return content;
    }
    let cutoff = content[..MAX_PASSAGE_CHARS]
        .rfind(|c: char| c.is_ascii_whitespace())
        .unwrap_or(MAX_PASSAGE_CHARS);
    &content[..cutoff]
}

/// Like `extract_json_from_response` but looks for a JSON *object* (`{…}`)
/// as the outermost structure (used by `parse_batch_response`).
fn extract_json_object_from_response(response: &str) -> Option<String> {
    // Fenced code block first.
    if let Some(start) = response.find("```") {
        let after = &response[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        if let Some(end) = body.find("```") {
            return Some(body[..end].trim().to_string());
        }
    }
    let first = response.find('{')?;
    let last = response.rfind('}')?;
    if last < first { return None; }
    Some(response[first..=last].trim().to_string())
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

// ─── Lenient field extraction helpers ────────────────────────────────────────

/// Extract `epistemic_status` from a JSON object with case-insensitive key
/// matching and tolerance for common model typos. Tries exact match first,
/// then any key whose lowercase form starts with `"epistemic"` that holds
/// a non-empty string value (booleans and nulls are skipped).
fn extract_epistemic_status(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    // Exact match (fast path — the well-behaved case).
    if let Some(v) = obj.get("epistemic_status") {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    // Case-insensitive fallback: first key starting with "epistemic" whose
    // value is a non-empty string.  Covers:
    //   epistemic_statuS, epistemic_state, epistemic_statment, epistemic_staus,
    //   epistemic_statement (when the value is a string, not a bool)
    for (k, v) in obj {
        if k.to_lowercase().starts_with("epistemic") {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Case-insensitive lookup for an optional string field.
/// Returns `None` for null, missing, or non-string values.
fn extract_opt_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<String> {
    let field_lc = field.to_lowercase();
    for (k, v) in obj {
        if k.to_lowercase() == field_lc {
            return v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
        }
    }
    None
}

/// Case-insensitive lookup for `attributed_to`, coercing an array of strings
/// into a single comma-joined string.
fn extract_attributed_to(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    for (k, v) in obj {
        if k.to_lowercase() == "attributed_to" {
            return match v {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                serde_json::Value::Array(arr) => {
                    let parts: Vec<&str> = arr
                        .iter()
                        .filter_map(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if parts.is_empty() { None } else { Some(parts.join(", ")) }
                }
                _ => None,
            };
        }
    }
    None
}

/// Convert a `serde_json::Value` to a `RawExtractedClaim` using lenient field
/// extraction.  Only `"claim"` must be present and non-empty; all other fields
/// fall back gracefully.
fn lenient_claim_from_value(v: &serde_json::Value) -> Option<RawExtractedClaim> {
    let obj = v.as_object()?;
    let claim = obj.get("claim")?.as_str()?.trim().to_string();
    if claim.is_empty() {
        return None;
    }
    Some(RawExtractedClaim {
        claim,
        epistemic_status: extract_epistemic_status(obj)
            .unwrap_or_else(|| "unclear".to_string()),
        hedging_language: extract_opt_string(obj, "hedging_language"),
        attributed_to: extract_attributed_to(obj),
    })
}

/// Parse a JSON array of claims out of an inference response, tolerating
/// markdown code fences, `<think>` blocks, field name typos/casing issues,
/// and `attributed_to` values that are arrays instead of strings.
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

    // Fast path: strict serde on the response as-is.
    if let Ok(claims) = serde_json::from_str::<Vec<RawExtractedClaim>>(s) {
        return Some(claims);
    }

    // Strip markdown fences / surrounding prose.
    let json = extract_json_from_response(s)
        .unwrap_or_else(|| s.to_string());

    // Strict serde on extracted JSON.
    if let Ok(claims) = serde_json::from_str::<Vec<RawExtractedClaim>>(&json) {
        return Some(claims);
    }

    // Lenient path: parse as generic Value array, then extract fields with
    // case-insensitive key matching and coercion for common model quirks:
    //   - epistemic_status typos/casing (epistemic_statuS, epistemic_statment, …)
    //   - attributed_to as array-of-strings instead of a single string
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
        let claims: Vec<RawExtractedClaim> = arr
            .iter()
            .filter_map(lenient_claim_from_value)
            .collect();
        return Some(claims);
    }

    None
}

/// Like `parse_extracted_claims` but additionally attempts to repair a
/// truncated JSON array (model ran out of `max_tokens` mid-generation).
///
/// Strategy: find the last complete `}` in the fragment and close the array
/// with `]`. This recovers all fully-generated claims even when the last one
/// was cut off. Falls back to the standard parser first; this function is only
/// invoked during retry so it never affects the hot ingest path.
fn try_repair_truncated_claims(response: &str) -> Option<Vec<RawExtractedClaim>> {
    // Standard parser first — handles normal and fenced responses.
    if let Some(claims) = parse_extracted_claims(response) {
        return Some(claims);
    }

    // Extract the JSON fragment (strips think tags and markdown fences).
    let cleaned = strip_think_tags(response);
    let fragment = extract_json_from_response(cleaned.trim())
        .unwrap_or_else(|| cleaned.trim().to_string());

    // Close the truncated array at the last complete object boundary.
    if let Some(last_brace) = fragment.rfind('}') {
        let repaired = format!("{}]", &fragment[..=last_brace]);
        if let Ok(claims) = serde_json::from_str::<Vec<RawExtractedClaim>>(&repaired) {
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

    // ── Lenient parser regression tests (real failure samples) ───────────────

    /// Chunk 5 / 13 pattern: one element uses `epistemic_statement` or
    /// `epistemic_state` instead of `epistemic_status`.
    #[test]
    fn lenient_handles_epistemic_statement_variant() {
        let resp = "<think>\n</think>\n\n```json\n[\
            {\"claim\": \"A\", \"epistemic_status\": \"unclear\", \"hedging_language\": null, \"attributed_to\": null},\
            {\"claim\": \"B\", \"epistemic_statement\": \"unclear\", \"hedging_language\": null, \"attributed_to\": null}\
        ]\n```";
        let claims = parse_extracted_claims(resp).unwrap();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[1].claim, "B");
        assert_eq!(claims[1].epistemic_status, "unclear");
    }

    /// Chunk 29 pattern: wrong case on key names (`epistemic_statuS`,
    /// `hedging_Language`, `Attributed_To`).
    #[test]
    fn lenient_handles_wrong_case_keys() {
        let resp = "<think>\n</think>\n\n```json\n[\
            {\"claim\": \"A\", \"epistemic_status\": \"minority\", \"hedging_language\": null, \"attributed_to\": null},\
            {\"claim\": \"B\", \"epistemic_statuS\": \"minority\", \"hedging_Language\": null, \"Attributed_To\": null}\
        ]\n```";
        let claims = parse_extracted_claims(resp).unwrap();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[1].epistemic_status, "minority");
        assert!(claims[1].hedging_language.is_none());
    }

    /// Chunk 18 / 35 pattern: `attributed_to` is a JSON array of strings
    /// rather than a single string.
    #[test]
    fn lenient_handles_attributed_to_array() {
        let resp = r#"<think>
</think>

```json
[
  {
    "claim": "Adams and Leverrier suggested an eighth planet.",
    "epistemic_status": "established",
    "hedging_language": null,
    "attributed_to": ["John Couch Adams", "Urbain Leverrier"]
  }
]
```"#;
        let claims = parse_extracted_claims(resp).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            claims[0].attributed_to.as_deref(),
            Some("John Couch Adams, Urbain Leverrier")
        );
    }

    /// Chunk 36 pattern: mixed issues — one element has both a bool
    /// `epistemic_statement` field and a correct `epistemic_status`, another
    /// has only the typo `epistemic_staus`.
    #[test]
    fn lenient_handles_mixed_typo_and_bool_fields() {
        let resp = r#"<think>
</think>

```json
[
  {
    "claim": "ABD2 needs supplementing.",
    "epistemic_status": "consensus",
    "hedging_language": null,
    "attributed_to": null
  },
  {
    "claim": "A satisfactory explanation requires a criterion.",
    "epistemic_statement": true,
    "epistemic_status": "established",
    "hedging_language": null,
    "attributed_to": null
  },
  {
    "claim": "We lack a criterion for satisfactoriness.",
    "epistemic_statment": true,
    "epistemic_staus": "contested",
    "hedging_language": null,
    "attributed_to": null
  }
]
```"#;
        let claims = parse_extracted_claims(resp).unwrap();
        assert_eq!(claims.len(), 3);
        assert_eq!(claims[1].epistemic_status, "established");
        assert_eq!(claims[2].epistemic_status, "contested");
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
    fn find_candidates_within_article_only() {
        // entry1 has 2 claims; entry2 has 1 claim.
        // Within-article filter: only the (entry1, entry1) pair should appear.
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
                source_entry: Some("entry1".into()), // same entry — should be a candidate
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
                source_entry: Some("entry2".into()), // different entry — skipped
                embedding: vec![1.0, 0.0],
            },
        ];
        let pairs = find_candidate_pairs(&claims, 0.5, 100);
        // Only the within-article pair (entry1[0], entry1[1]) should appear.
        assert_eq!(pairs.len(), 1);
        let (a, b) = pairs[0];
        assert_eq!(claims[a].source_entry, claims[b].source_entry);
    }

    #[test]
    fn find_candidates_respects_max() {
        // All 10 claims from the same entry so there are C(10,2)=45 candidate pairs.
        // max_candidates=5 must cap the output.
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
                source_entry: Some("same_entry".to_string()),
                embedding: vec![1.0, 0.0],
            })
            .collect();
        let pairs = find_candidate_pairs(&claims, 0.5, 5);
        assert!(pairs.len() <= 5);
    }
}
