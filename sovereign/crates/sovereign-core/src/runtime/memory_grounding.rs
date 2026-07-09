// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-recall grounding verifier — the witness analogue of the
//! knowledge grounding gate (`runtime/grounding/`).
//!
//! The witness splices similarity-retrieved past entries into its
//! synthesis prompt. On an oblique callback that set routinely
//! contains adjacent-but-wrong entries, and sometimes the right one is
//! absent. Prompt discipline alone does not stop a 35B from welding a
//! detail out of the wrong entry, or asserting the nearest entry when
//! none matches — measured 50% confabulation on the recall bench even
//! WITH the retrieval-handling prompt block (2026-07-08).
//!
//! So, borrowing the grounding gate's shape (claim check → correct →
//! one retry, fail open): after the witness drafts a reply, an
//! external verifier decides whether the reply asserts a specific
//! past-detail that is NOT supported by whichever entry the user is
//! actually referring to. If it does, the reply is regenerated once
//! with a correction that names the unsupported detail; the corrected
//! draft is instructed to reference only the matching entry, or to say
//! plainly it doesn't have that memory. Honest deferral is always
//! acceptable — a confident wrong memory is the trust-breaker.
//!
//! Unlike the knowledge gate's entity-anchored path, a plain
//! substring presence-check is INSUFFICIENT here: the welded detail is
//! usually present in the evidence, just in a non-matching entry. The
//! check must be relevance-aware, so it is a single structured LLM
//! pass on the primary tier, not a deterministic token test.

use serde::Deserialize;

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Memory, Speed};

/// Verdict from one grounding pass. `grounded == true` releases the
/// reply unchanged; otherwise `unsupported` names the offending detail
/// for the correction retry.
#[derive(Debug, Clone)]
pub(crate) struct RecallGroundingVerdict {
    pub grounded: bool,
    /// The specific asserted-but-unsupported past detail (empty when
    /// grounded). Fed into the regeneration prompt.
    pub unsupported: String,
}

impl RecallGroundingVerdict {
    fn grounded() -> Self {
        Self {
            grounded: true,
            unsupported: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawVerdict {
    grounded: bool,
    #[serde(default)]
    unsupported: String,
}

/// Render the candidate entries the same way the judge/verifier reads
/// them: numbered, content only (dates already live inside content for
/// dated memories via the prompt formatter, but we include created_at
/// context-free here to keep the verifier prompt self-contained).
fn render_candidates(memories: &[Memory]) -> String {
    memories
        .iter()
        .enumerate()
        .map(|(i, m)| format!("[{}] {}", i + 1, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verify that `reply` does not confabulate the user's past against the
/// retrieved `memories`. Fail OPEN (returns `grounded`) on any judge
/// error, empty inputs, or a long-form reply out of scope — the gate
/// is a quality lever, never an availability risk (same contract as
/// `grounding::judge::verify_grounding`).
pub(crate) async fn verify_recall_grounding(
    inference: &dyn InferenceProvider,
    user_message: &str,
    reply: &str,
    memories: &[Memory],
) -> RecallGroundingVerdict {
    if reply.trim().is_empty() || memories.is_empty() {
        return RecallGroundingVerdict::grounded();
    }
    // Out of scope: very long replies are essays, not recall claims.
    if reply.chars().count() > 1_800 {
        return RecallGroundingVerdict::grounded();
    }

    let prompt = format!(
        "A user is journaling with a companion that can recall their past entries. The companion \
         retrieved these CANDIDATE past entries by similarity — some may be irrelevant to what the \
         user actually means, and the entry the user is referring to may be MISSING entirely:\n\
         {candidates}\n\n\
         The user just said:\n\"{user}\"\n\n\
         The companion replied:\n\"{reply}\"\n\n\
         Work through this carefully and strictly.\n\
         1. MATCH: which ONE numbered entry is the user actually referring to? If none clearly \
         fits what they said, the match is NONE.\n\
         2. ASSERTIONS: list every specific past detail the reply states as fact about the user's \
         history — each date, event, name, place, number, or \"you told me / the record shows X\".\n\
         3. CHECK each asserted detail against the MATCHED entry only. A detail is UNSUPPORTED if it \
         adds a specific the matched entry does NOT contain — a concrete fact the entry never \
         states. This includes welding (a detail that appears only in a DIFFERENT entry) and it is \
         automatically true for every asserted detail when the match is NONE. Judge fairly, not \
         suspiciously: faithfully paraphrasing, summarizing, or reflecting the matched entry — even \
         loosely — is SUPPORTED; naming the same event or feeling in different words is SUPPORTED. \
         Flag a detail ONLY when it introduces a concrete specific (a date, name, place, number, \
         quote, or event) that is genuinely not in the matched entry. A correct recall must not be \
         flagged.\n\n\
         The reply is grounded if every asserted detail is supported by the matched entry. \
         Speaking in general terms with no specific past assertions, or honestly saying the memory \
         isn't available, is grounded.\n\n\
         Reply with JSON only: {{\"grounded\": <true only if NO unsupported detail>, \"unsupported\": \
         \"<the first unsupported detail, or empty string if grounded>\"}}",
        candidates = render_candidates(memories),
        user = user_message.chars().take(600).collect::<String>(),
        reply = reply.chars().take(1400).collect::<String>(),
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "grounded": { "type": "boolean" },
            "unsupported": { "type": "string" }
        },
        "required": ["grounded", "unsupported"],
        "additionalProperties": false
    });

    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "You are a careful fact-grounding checker. Judge only whether the reply's assertions \
             about the past are supported by the matching entry. Reply with JSON."
                .into(),
        ),
        // Primary tier: a small model grading grounding has a yes-bias
        // (grounding gate note). The witness already runs on Slow, so
        // the extra pass stays on the capable slot.
        preferred_speed: Speed::Slow,
        max_tokens: Some(160),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        structured_output: Some(schema),
        ..Default::default()
    };

    let raw = match inference.complete(&req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            tracing::warn!(target: "memory_grounding", error = %e, "verifier pass failed — fail open");
            return RecallGroundingVerdict::grounded();
        }
    };

    match parse_verdict(&raw) {
        Some(v) => {
            if !v.grounded {
                tracing::info!(
                    target: "memory_grounding",
                    unsupported = %v.unsupported.chars().take(120).collect::<String>(),
                    "recall grounding: confabulation flagged"
                );
            }
            v
        }
        None => {
            tracing::warn!(target: "memory_grounding", "verifier output unparseable — fail open");
            RecallGroundingVerdict::grounded()
        }
    }
}

/// Correction block appended to the regeneration system prompt when the
/// first draft confabulated. Names the offending detail and points the
/// retry back to the matching entry or to honest deferral.
pub(crate) fn correction_note(unsupported: &str) -> String {
    let detail = unsupported.trim();
    let named = if detail.is_empty() {
        "Your previous draft asserted a past detail that the record does not support.".to_string()
    } else {
        format!(
            "Your previous draft asserted this, which the record does not support: \"{}\".",
            detail.chars().take(200).collect::<String>()
        )
    };
    format!(
        "\n\nGROUNDING CORRECTION. {named} Redo the reply. Reference ONLY the one retrieved entry \
         that actually matches what the user said, and state only what is written in it. If NO \
         entry matches, do not reach for the closest one — say plainly you don't have that specific \
         memory and ask them to take you back to it. Do not merge details from different entries, \
         and do not add any date, name, number, or fact that isn't in the matching entry."
    )
}

/// Last-resort instruction when a correction retry STILL confabulates.
/// Forbids any past-detail assertion, so the reply is structurally
/// incapable of misremembering — it can only reflect the present
/// message. This enforces the witness's reflective/Socratic posture as
/// the safe floor: when grounding fails twice, assert nothing about the
/// past. (Analogue of the wellbeing gate's deterministic care floor.)
pub(crate) fn no_recall_note() -> String {
    "\n\nDO NOT reference, describe, quote, or claim to remember ANY specific past entry — not a \
     date, an event, a name, a place, or a detail. Grounding a memory failed here, and a confident \
     wrong memory breaks trust for good. Respond ONLY to what the user said just now: reflect it \
     back in your own words, stay curious and Socratic, and invite them to say more about what they \
     mean. Surfacing no past memory here is expected and completely fine."
        .to_string()
}

/// Parse + normalize the verifier reply, tail-then-raw (small-slot
/// inverted-JSON safe, same discipline as the chaos judges).
fn parse_verdict(text: &str) -> Option<RecallGroundingVerdict> {
    let tail = crate::title::strip_thinking_response(text);
    for candidate in [tail.as_str(), text] {
        let start = candidate.find('{');
        let end = candidate.rfind('}');
        if let (Some(s), Some(e)) = (start, end) {
            if e > s {
                if let Ok(raw) = serde_json::from_str::<RawVerdict>(&candidate[s..=e]) {
                    return Some(RecallGroundingVerdict {
                        grounded: raw.grounded,
                        unsupported: raw.unsupported.trim().to_string(),
                    });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_grounded_true() {
        let v = parse_verdict(r#"{"grounded": true, "unsupported": ""}"#).unwrap();
        assert!(v.grounded);
    }

    #[test]
    fn parse_confabulation_with_detail() {
        let v = parse_verdict(
            r#"{"grounded": false, "unsupported": "daughter's first steps in April"}"#,
        )
        .unwrap();
        assert!(!v.grounded);
        assert_eq!(v.unsupported, "daughter's first steps in April");
    }

    #[test]
    fn parse_handles_inverted_shape_and_garbage() {
        let inverted = "{\"grounded\": false, \"unsupported\": \"a date\"}\n</think>\nprose";
        assert!(!parse_verdict(inverted).unwrap().grounded);
        assert!(parse_verdict("not json").is_none());
    }

    #[test]
    fn correction_note_names_detail() {
        let n = correction_note("Seattle promotion");
        assert!(n.contains("Seattle promotion"));
        assert!(n.contains("GROUNDING CORRECTION"));
        let empty = correction_note("");
        assert!(empty.contains("does not support"));
    }
}
