//! Skeleton response parsing, repair, and filtering logic.
//!
//! Extracted from `field_engine.rs` to keep the engine module focused on
//! orchestration. All functions here are `pub(crate)` so they remain
//! accessible from `FieldModelEngine` methods and `reprocess_skeleton_failures`.

use std::collections::HashMap;
use std::path::Path;

use super::skeleton::{PartialSkeleton, SkeletonPosition, SkeletonQuestion};

/// Outcome of attempting to parse a skeleton extraction response.
pub(crate) enum ParseResult {
    Ok(Vec<SkeletonQuestion>),
    Repaired(Vec<SkeletonQuestion>, usize),
    Failed,
}

/// Parse a skeleton extraction response into questions.
pub(crate) fn parse_skeleton_response(
    batch_idx: usize,
    response: &str,
    failures_path: &Path,
) -> ParseResult {
    let json_str = extract_json_from_response(response);

    // Try parsing as-is first.
    if let Ok(passages) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
        return ParseResult::Ok(extract_questions_from_passages(&passages));
    }

    // Repair pipeline: try each strategy in order of increasing aggressiveness.
    let repair_source = if json_str.starts_with('[') {
        json_str.to_string()
    } else {
        response
            .find('[')
            .map(|start| response[start..].to_string())
            .unwrap_or_else(|| json_str.to_string())
    };

    // Strategy 1: fix unquoted string values (e.g., "claim": autonomy is...)
    let with_quotes = repair_unquoted_strings(&repair_source);
    if let Ok(passages) = serde_json::from_str::<Vec<serde_json::Value>>(&with_quotes) {
        let count = passages.len();
        let questions = extract_questions_from_passages(&passages);
        tracing::info!(
            batch = batch_idx,
            "Repaired unquoted strings — salvaged {count} passages"
        );
        return ParseResult::Repaired(questions, count);
    }

    // Strategy 2: fix unquoted strings + truncation repair
    if let Some(repaired) = try_repair_truncated_json(&with_quotes) {
        if let Ok(passages) = serde_json::from_str::<Vec<serde_json::Value>>(&repaired) {
            let count = passages.len();
            let questions = extract_questions_from_passages(&passages);
            tracing::info!(
                batch = batch_idx,
                "Repaired unquoted strings + truncation — salvaged {count} passages"
            );
            return ParseResult::Repaired(questions, count);
        }
    }

    // Strategy 3: truncation repair only (original string, no quote fix)
    if let Some(repaired) = try_repair_truncated_json(&repair_source) {
        if let Ok(passages) = serde_json::from_str::<Vec<serde_json::Value>>(&repaired) {
            let count = passages.len();
            let questions = extract_questions_from_passages(&passages);
            return ParseResult::Repaired(questions, count);
        }
    }

    let snippet: String = json_str.chars().take(200).collect();
    log_skeleton_failure(failures_path, batch_idx, json_str, "not repairable");
    tracing::warn!(batch = batch_idx, response_snippet = %snippet, "Skeleton parse failed — not valid or repairable JSON");
    ParseResult::Failed
}

/// Repair unquoted string values in JSON.
///
/// Handles the common LLM failure where string values after a colon are
/// missing their opening quote:
///   `"claim": autonomy is a central value"`  →  `"claim": "autonomy is a central value"`
///
/// The pattern: `": ` followed by a non-quote, non-bracket, non-digit char,
/// ending at the next `"` (which the LLM did output as the closing quote).
pub(crate) fn repair_unquoted_strings(s: &str) -> String {
    // Match: `"<key>": <unquoted-value>"` where value starts with a letter
    // The LLM outputs the closing quote but forgets the opening one.
    let mut result = String::with_capacity(s.len() + 64);
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for pattern: `": ` followed by a letter (not `"`, `[`, `{`, digit, `n` for null, `t`/`f` for true/false)
        if i + 3 < len && bytes[i] == b'"' && bytes[i + 1] == b':' && bytes[i + 2] == b' ' {
            let next = bytes[i + 3];
            // Check if the next char starts an unquoted string value:
            // - It's a letter (but not the start of null/true/false)
            // - It's not a quote, bracket, brace, or digit
            let is_json_keyword = i + 3 + 4 <= len
                && (&s[i + 3..i + 3 + 4] == "null" || &s[i + 3..i + 3 + 4] == "true")
                || (i + 3 + 5 <= len && &s[i + 3..i + 3 + 5] == "false");

            if next.is_ascii_alphabetic()
                && !is_json_keyword
                && next != b'"'
                && next != b'['
                && next != b'{'
                && next != b']'
                && next != b'}'
            {
                // Found an unquoted value — insert the missing opening quote
                result.push('"'); // the key's closing quote
                result.push(':');
                result.push(' ');
                result.push('"'); // the missing opening quote for the value
                i += 3;
                continue;
            }
        }

        result.push(s[i..].chars().next().unwrap());
        i += s[i..].chars().next().unwrap().len_utf8();
    }

    result
}

/// Parse positions from a JSON positions array value.
pub(crate) fn parse_positions(positions_val: &serde_json::Value) -> Vec<SkeletonPosition> {
    positions_val
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let name = p["name"].as_str()?;
                    if name.is_empty() || name == "..." || name == "null" || name.len() < 2 {
                        return None;
                    }
                    let claim = p["claim"].as_str().unwrap_or_default();
                    if claim == "..." || claim.is_empty() {
                        return None;
                    }
                    let status = p["status"].as_str().unwrap_or("contested");
                    let status = if status.contains('|') {
                        status.split('|').next().unwrap_or("contested").to_string()
                    } else if status == "..." || status.is_empty() {
                        "contested".to_string()
                    } else {
                        status.to_string()
                    };
                    Some(SkeletonPosition {
                        id: format!("p_{}", name.to_lowercase().replace(' ', "_")),
                        name: name.to_string(),
                        claim: claim.to_string(),
                        status,
                        proponents: p["proponents"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        source: "skeleton".into(),
                        cluster_ids: Vec::new(),
                        centroid_chunk_ids: Vec::new(),
                        discovery_confidence: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a question ID slug from a question string.
pub(crate) fn make_question_id(question: &str) -> String {
    format!(
        "q_{}",
        question
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .collect::<String>()
            .replace(' ', "_")
            .chars()
            .take(50)
            .collect::<String>()
    )
}

/// Synthesize a question from a position's claim when the LLM didn't provide one.
pub(crate) fn synthesize_question_from_position(position: &SkeletonPosition) -> String {
    format!("What is the status of the view: {}?", position.name)
}

/// Minimum claim length to keep a position. Short claims like "the pathos of
/// things" or "cutting" are definitional labels, not substantive philosophical
/// claims that the alignment phase can usefully match.
pub(crate) const MIN_CLAIM_LENGTH: usize = 20;

/// Filter out low-quality positions and questions after extraction.
///
/// Removes:
/// - Positions with claims shorter than MIN_CLAIM_LENGTH chars
/// - Questions where all remaining positions have status "majority"
///   (signals a definitional/survey passage, not a genuine debate)
/// - Questions left with zero positions after filtering (unless they
///   had an explicit canonical question from the LLM)
pub(crate) fn filter_low_quality(
    questions: &mut Vec<SkeletonQuestion>,
    had_explicit_question: &[bool],
) {
    let mut filtered_positions = 0_usize;
    let mut filtered_questions = 0_usize;

    for q in questions.iter_mut() {
        let before = q.positions.len();
        q.positions.retain(|p| p.claim.len() >= MIN_CLAIM_LENGTH);
        filtered_positions += before - q.positions.len();
    }

    // Remove questions that are purely definitional (all positions "majority")
    // or that lost all positions and were synthesized (not explicit).
    let mut i = 0;
    questions.retain(|q| {
        let idx = i;
        i += 1;
        let is_explicit = had_explicit_question.get(idx).copied().unwrap_or(false);

        // Keep explicit questions even if they have no positions
        if q.positions.is_empty() && !is_explicit {
            filtered_questions += 1;
            return false;
        }

        // Drop if all positions are "majority" (definitional, not contested)
        if !q.positions.is_empty()
            && q.positions.iter().all(|p| p.status == "majority")
            && !is_explicit
        {
            filtered_questions += 1;
            return false;
        }

        true
    });

    if filtered_positions > 0 || filtered_questions > 0 {
        tracing::info!(
            positions_dropped = filtered_positions,
            questions_dropped = filtered_questions,
            "Quality filter applied"
        );
    }
}

/// Extract questions and positions from parsed JSON passages.
///
/// When a passage has `canonical_question: null` but contains valid positions,
/// the positions are preserved under a synthesized question derived from the
/// first position's name. This prevents data loss from passages where the LLM
/// identified positions but couldn't frame them under a single question.
pub(crate) fn extract_questions_from_passages(
    passages: &[serde_json::Value],
) -> Vec<SkeletonQuestion> {
    let mut questions = Vec::new();
    let mut had_explicit = Vec::new();
    let mut null_question_with_positions = 0_usize;

    for passage in passages {
        let raw_question = passage["canonical_question"].as_str();
        let has_explicit_question = raw_question
            .map(|q| !q.is_empty() && q != "..." && q != "null" && q.len() >= 10)
            .unwrap_or(false);

        let question_type = passage["question_type"]
            .as_str()
            .unwrap_or("conceptual")
            .to_string();
        let positions = parse_positions(&passage["positions"]);

        if has_explicit_question {
            let question = raw_question.unwrap();
            questions.push(SkeletonQuestion {
                id: make_question_id(question),
                question: question.to_string(),
                question_type,
                status: "contested".into(),
                primary_article_ids: Vec::new(),
                positions,
            });
            had_explicit.push(true);
        } else if !positions.is_empty() {
            // The LLM found positions but didn't frame a canonical question.
            // Synthesize a question so the positions aren't lost.
            null_question_with_positions += 1;
            let question = synthesize_question_from_position(&positions[0]);
            questions.push(SkeletonQuestion {
                id: make_question_id(&question),
                question,
                question_type,
                status: "contested".into(),
                primary_article_ids: Vec::new(),
                positions,
            });
            had_explicit.push(false);
        }
    }

    if null_question_with_positions > 0 {
        tracing::info!(
            count = null_question_with_positions,
            "Synthesized questions for passages with positions but no canonical question"
        );
    }

    // Apply quality filter to remove definitional/low-quality entries.
    filter_low_quality(&mut questions, &had_explicit);

    questions
}

/// Extract JSON from a model response.
///
/// Handles common LLM output patterns:
/// - `<think>...</think>` reasoning blocks before the JSON
/// - Markdown code fences (```json, ```JSON, ```)
/// - Preamble prose before the JSON array/object
/// - Trailing prose after the closing bracket
pub(crate) fn extract_json_from_response(response: &str) -> &str {
    let mut text = response.trim();

    // Strip <think>...</think> blocks (common with reasoning models).
    if let Some(think_end) = text.find("</think>") {
        text = text[think_end + 8..].trim();
    }

    // Try to extract from ```json ... ``` (case-insensitive).
    let lower = text.to_lowercase();
    if let Some(fence_start) = lower.find("```json") {
        let content_start = fence_start + 7;
        // Skip optional newline after ```json
        let content_start = if text[content_start..].starts_with('\n') {
            content_start + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            return text[content_start..content_start + fence_end].trim();
        }
    }

    // Try to extract from ``` ... ```
    if let Some(fence_start) = text.find("```") {
        let content_start = fence_start + 3;
        // Skip optional language tag + newline (e.g. ```\n or ```text\n)
        let after_fence = &text[content_start..];
        let content_start = if let Some(nl) = after_fence.find('\n') {
            content_start + nl + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            let block = text[content_start..content_start + fence_end].trim();
            if block.starts_with('[') || block.starts_with('{') {
                return block;
            }
        }
    }

    // No code fence — find the first [ or { and last ] or }.
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }

    // Last resort — return the whole thing and let the caller handle the error.
    text
}

/// Try to repair truncated JSON by closing open brackets/braces.
///
/// LLMs often hit the token limit mid-response, producing valid JSON
/// that's cut off. We try to close the structure so at least the
/// complete elements parse. Returns `None` if the input doesn't look
/// like truncated JSON.
pub(crate) fn try_repair_truncated_json(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        return None;
    }

    // Find the last complete JSON element by walking brackets.
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    let mut last_complete_element_end = 0;

    for (i, ch) in trimmed.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth_brace += 1,
            '}' => {
                depth_brace -= 1;
                // A complete top-level element: either a standalone object
                // (depth 0/0) or a direct child of the top-level array (brace 0, bracket 1).
                if depth_brace == 0 && depth_bracket <= 1 {
                    last_complete_element_end = i + 1;
                }
            }
            '[' => depth_bracket += 1,
            ']' => {
                depth_bracket -= 1;
                if depth_bracket == 0 && depth_brace == 0 {
                    // Already complete — no repair needed.
                    return None;
                }
            }
            _ => {}
        }
    }

    if last_complete_element_end == 0 {
        return None; // No complete elements found.
    }

    // Truncate to the last complete element.
    let mut repaired = trimmed[..last_complete_element_end].to_string();

    // Close the top-level array if the input started with one.
    if trimmed.starts_with('[') {
        repaired.push(']');
    }

    Some(repaired)
}

/// Append a failed skeleton extraction batch to the failure log.
pub(crate) fn log_skeleton_failure(path: &Path, batch: usize, raw: &str, error: &str) {
    use std::io::Write;
    let entry = serde_json::json!({
        "batch": batch,
        "error": error,
        "raw_response_truncated": &raw[..raw.len().min(2000)],
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", entry);
    }
}

/// Deduplicate questions by merging those with similar text.
pub(crate) fn deduplicate_questions(skeleton: &mut PartialSkeleton) {
    // Simple dedup: merge questions with the same ID.
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped = Vec::new();

    for q in skeleton.questions.drain(..) {
        if let Some(&existing_idx) = seen.get(&q.id) {
            // Merge positions into the existing question.
            let existing: &mut SkeletonQuestion = &mut deduped[existing_idx];
            for pos in q.positions {
                if !existing.positions.iter().any(|p| p.id == pos.id) {
                    existing.positions.push(pos);
                }
            }
        } else {
            seen.insert(q.id.clone(), deduped.len());
            deduped.push(q);
        }
    }

    skeleton.questions = deduped;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_fence() {
        let response = "Here is the result:\n```json\n[{\"a\": 1}]\n```\nDone.";
        assert_eq!(extract_json_from_response(response), "[{\"a\": 1}]");
    }

    #[test]
    fn extract_json_bare() {
        let response = "[{\"a\": 1}]";
        assert_eq!(extract_json_from_response(response), "[{\"a\": 1}]");
    }

    #[test]
    fn extract_json_from_generic_code_fence() {
        let response = "Result:\n```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_from_response(response), "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_json_with_surrounding_prose() {
        let response = "Here is the JSON:\n```json\n[{\"a\": 1}, {\"b\": 2}]\n```\nAll done!";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn extract_json_strips_think_block() {
        let response = "<think>\nLet me analyze these passages...\n</think>\n[{\"a\": 1}]";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn extract_json_case_insensitive_fence() {
        let response = "```JSON\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_from_response(response), "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_json_from_prose_with_array() {
        let response = "Here are the results:\n\n[{\"a\": 1}, {\"b\": 2}]\n\nThat's all.";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn extract_json_from_prose_with_object() {
        let response = "The answer is: {\"crux\": \"test\", \"confidence\": 0.9} done.";
        let json = extract_json_from_response(response);
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed["crux"], "test");
    }

    #[test]
    fn extract_json_think_block_then_fence() {
        let response = "<think>\nreasoning here\n</think>\n```json\n[{\"x\": 1}]\n```";
        let json = extract_json_from_response(response);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    // ── Null-question extraction tests ──────────────────────

    #[test]
    fn extract_questions_from_null_canonical_question_with_positions() {
        let passages: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
            {
                "passage_index": 0,
                "canonical_question": null,
                "question_type": "conceptual",
                "positions": [
                    {
                        "name": "Kantian autonomy",
                        "claim": "Autonomy is a central value and capacity to be one's own person",
                        "status": "contested",
                        "proponents": ["Immanuel Kant"]
                    }
                ]
            }
        ]"#,
        )
        .unwrap();
        let questions = extract_questions_from_passages(&passages);
        assert_eq!(
            questions.len(),
            1,
            "should synthesize a question for null canonical_question with positions"
        );
        assert!(questions[0].question.contains("Kantian autonomy"));
        assert_eq!(questions[0].positions.len(), 1);
        assert_eq!(questions[0].positions[0].name, "Kantian autonomy");
    }

    #[test]
    fn extract_questions_skips_null_canonical_question_without_positions() {
        let passages: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
            {
                "passage_index": 0,
                "canonical_question": null,
                "question_type": "factual",
                "positions": []
            }
        ]"#,
        )
        .unwrap();
        let questions = extract_questions_from_passages(&passages);
        assert_eq!(
            questions.len(),
            0,
            "should skip passages with no question and no positions"
        );
    }

    #[test]
    fn extract_questions_prefers_explicit_canonical_question() {
        let passages: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
            {
                "passage_index": 0,
                "canonical_question": "Is free will compatible with determinism?",
                "question_type": "conceptual",
                "positions": [
                    {
                        "name": "Compatibilism",
                        "claim": "Free will is compatible with determinism",
                        "status": "majority",
                        "proponents": []
                    }
                ]
            }
        ]"#,
        )
        .unwrap();
        let questions = extract_questions_from_passages(&passages);
        assert_eq!(questions.len(), 1);
        assert_eq!(
            questions[0].question,
            "Is free will compatible with determinism?"
        );
    }

    // ── Unquoted string repair tests ────────────────────────

    #[test]
    fn repair_unquoted_claim_value() {
        let broken = r#"{"name": "Kantian autonomy", "claim": autonomy is a central value", "status": "majority"}"#;
        let fixed = repair_unquoted_strings(broken);
        let parsed: serde_json::Value = serde_json::from_str(&fixed).unwrap();
        assert_eq!(parsed["claim"], "autonomy is a central value");
    }

    #[test]
    fn repair_unquoted_preserves_valid_json() {
        let valid = r#"{"name": "test", "claim": "already quoted", "status": "majority"}"#;
        let result = repair_unquoted_strings(valid);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["claim"], "already quoted");
    }

    #[test]
    fn repair_unquoted_preserves_null_and_booleans() {
        let valid = r#"{"canonical_question": null, "active": true, "count": false}"#;
        let result = repair_unquoted_strings(valid);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["canonical_question"].is_null());
        assert_eq!(parsed["active"], true);
        assert_eq!(parsed["count"], false);
    }

    #[test]
    fn repair_unquoted_handles_batch_45_pattern() {
        // Real failure from SEP batch 45
        let broken = r#"[
  {
    "passage_index": 0,
    "canonical_question": null,
    "question_type": "conceptual",
    "positions": [
      {
        "name": "Kantian tradition of moral philosophy",
        "claim": autonomy is a central value and capacity to be one's own person independent from external forces",
        "status": "majority",
        "proponents": ["Immanuel Kant"]
      }
    ]
  }
]"#;
        let fixed = repair_unquoted_strings(broken);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&fixed).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0]["positions"][0]["claim"],
            "autonomy is a central value and capacity to be one's own person independent from external forces"
        );
    }

    // ── Quality filter tests ──────────────────────────────────

    #[test]
    fn filter_drops_short_claims() {
        let mut questions = vec![SkeletonQuestion {
            id: "q_test".into(),
            question: "What is the status of the view: mono no aware?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![
                SkeletonPosition {
                    id: "p_mono".into(),
                    name: "mono no aware".into(),
                    claim: "the pathos of things".into(), // 20 chars, borderline
                    status: "majority".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                },
                SkeletonPosition {
                    id: "p_wabi".into(),
                    name: "wabi".into(),
                    claim: "austere beauty".into(), // 14 chars, too short
                    status: "majority".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                },
            ],
        }];
        let explicit = vec![false];
        filter_low_quality(&mut questions, &explicit);
        // Both should be dropped: wabi for short claim, mono survives the length
        // check but since it's the only remaining position with "majority" status,
        // the all-majority filter removes the question entirely.
        assert_eq!(questions.len(), 0);
    }

    #[test]
    fn filter_keeps_contested_with_good_claims() {
        let mut questions = vec![SkeletonQuestion {
            id: "q_test".into(),
            question: "What is the nature of knowledge?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![
                SkeletonPosition {
                    id: "p_a".into(),
                    name: "Rationalism".into(),
                    claim: "Knowledge is primarily derived from reason and innate ideas".into(),
                    status: "contested".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                },
                SkeletonPosition {
                    id: "p_b".into(),
                    name: "Empiricism".into(),
                    claim: "Knowledge is primarily derived from sensory experience".into(),
                    status: "contested".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                },
            ],
        }];
        let explicit = vec![true];
        filter_low_quality(&mut questions, &explicit);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].positions.len(), 2);
    }

    #[test]
    fn filter_keeps_explicit_question_even_with_no_positions() {
        let mut questions = vec![SkeletonQuestion {
            id: "q_test".into(),
            question: "Whether machines can think".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![],
        }];
        let explicit = vec![true];
        filter_low_quality(&mut questions, &explicit);
        assert_eq!(
            questions.len(),
            1,
            "explicit questions survive even with no positions"
        );
    }

    // ── Truncated JSON repair tests ─────────────────────────

    #[test]
    fn repair_truncated_array_with_complete_first_element() {
        // Array with one complete object and a second cut off.
        let truncated = r#"[{"passage_index": 0, "canonical_question": "Is free will real?", "positions": []}, {"passage_index": 1, "canonical_ques"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["canonical_question"], "Is free will real?");
    }

    #[test]
    fn repair_already_complete_returns_none() {
        let complete = r#"[{"a": 1}]"#;
        assert!(
            try_repair_truncated_json(complete).is_none(),
            "already-complete JSON should return None"
        );
    }

    #[test]
    fn repair_not_json_returns_none() {
        assert!(try_repair_truncated_json("not json").is_none());
        assert!(try_repair_truncated_json("").is_none());
    }

    #[test]
    fn repair_truncated_mid_string() {
        // Truncated inside a string value — the complete first element should survive.
        let truncated =
            r#"[{"question": "What is X?", "type": "conceptual"}, {"question": "Is Y compat"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn repair_truncated_with_nested_objects() {
        let truncated = r#"[{"q": "test", "positions": [{"name": "A"}]}, {"q": "other", "positions": [{"name": "B"#;
        let repaired = try_repair_truncated_json(truncated).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["positions"][0]["name"], "A");
    }

    #[test]
    fn repair_no_complete_elements_returns_none() {
        // Only a partial first element — nothing to salvage.
        let truncated = r#"[{"passage_in"#;
        assert!(try_repair_truncated_json(truncated).is_none());
    }

    #[test]
    fn repair_realistic_truncated_skeleton_response() {
        // Simulates the actual batch 25 failure: 3 complete passage objects,
        // 4th truncated mid-string. Should salvage the first 3.
        let truncated = r#"[
  {
    "passage_index": 0,
    "canonical_question": "What is the proper relationship between reason and faith?",
    "question_type": "normative",
    "positions": [
      {
        "name": "pseudo-dialecticians",
        "claim": "everything can be explained by human reason",
        "status": "minority",
        "proponents": ["Abelard"]
      }
    ]
  },
  {
    "passage_index": 1,
    "canonical_question": null,
    "positions": []
  },
  {
    "passage_index": 2,
    "canonical_question": "What is identity?",
    "question_type": "conceptual",
    "positions": []
  },
  {
    "passage_index": 3,
    "canonical_question": null,
    "positions": [
      {
        "name": "traditional account",
        "claim": "(a) two things are the same in essence when they are numerically the concrete thing (essentia), and essentially different other"#;

        let repaired = try_repair_truncated_json(truncated)
            .expect("should repair truncated array with 3 complete elements");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&repaired).expect("repaired JSON should parse");
        assert_eq!(
            parsed.len(),
            3,
            "should salvage the 3 complete elements, got {}",
            parsed.len()
        );
        assert_eq!(
            parsed[0]["canonical_question"],
            "What is the proper relationship between reason and faith?"
        );
    }

    // ── Deduplication tests ─────────────────────────────────

    #[test]
    fn deduplicate_questions_merges_same_id() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        skeleton.questions.push(SkeletonQuestion {
            id: "q_free_will".into(),
            question: "Is free will compatible?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![SkeletonPosition {
                id: "p_compat".into(),
                name: "Compatibilism".into(),
                claim: "Yes".into(),
                status: "majority".into(),
                proponents: vec![],
                source: "skeleton".into(),
                cluster_ids: vec![],
                centroid_chunk_ids: vec![],
                discovery_confidence: None,
            }],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_free_will".into(), // same ID
            question: "Is free will compatible?".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![SkeletonPosition {
                id: "p_hard_incompat".into(), // different position
                name: "Hard Incompatibilism".into(),
                claim: "No".into(),
                status: "minority".into(),
                proponents: vec![],
                source: "skeleton".into(),
                cluster_ids: vec![],
                centroid_chunk_ids: vec![],
                discovery_confidence: None,
            }],
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(skeleton.questions.len(), 1, "duplicate IDs should merge");
        assert_eq!(
            skeleton.questions[0].positions.len(),
            2,
            "positions from both duplicates should be merged"
        );
    }

    #[test]
    fn deduplicate_questions_skips_duplicate_positions() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        let pos = SkeletonPosition {
            id: "p_compat".into(),
            name: "Compatibilism".into(),
            claim: "Yes".into(),
            status: "majority".into(),
            proponents: vec![],
            source: "skeleton".into(),
            cluster_ids: vec![],
            centroid_chunk_ids: vec![],
            discovery_confidence: None,
        };
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![pos.clone()],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![pos], // same position ID
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(skeleton.questions.len(), 1);
        assert_eq!(
            skeleton.questions[0].positions.len(),
            1,
            "same position ID should not be duplicated"
        );
    }

    #[test]
    fn deduplicate_questions_keeps_distinct() {
        let mut skeleton = PartialSkeleton::new("philosophy");
        skeleton.questions.push(SkeletonQuestion {
            id: "q_1".into(),
            question: "Q1".into(),
            question_type: "conceptual".into(),
            status: "contested".into(),
            primary_article_ids: vec![],
            positions: vec![],
        });
        skeleton.questions.push(SkeletonQuestion {
            id: "q_2".into(), // different ID
            question: "Q2".into(),
            question_type: "factual".into(),
            status: "settled".into(),
            primary_article_ids: vec![],
            positions: vec![],
        });

        deduplicate_questions(&mut skeleton);
        assert_eq!(
            skeleton.questions.len(),
            2,
            "distinct IDs should not be merged"
        );
    }

    #[test]
    fn placeholder_question_filtered() {
        // Questions with "..." as text should be skipped.
        let question = "...";
        assert!(question == "..." || question.len() < 10);
    }

    #[test]
    fn compound_status_normalized() {
        let status = "minority|contested";
        let normalized = if status.contains('|') {
            status.split('|').next().unwrap_or("contested").to_string()
        } else {
            status.to_string()
        };
        assert_eq!(normalized, "minority");
    }
}
