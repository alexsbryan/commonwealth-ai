//! Reusable live-path runner for grounded-calibration benches (chaos-monkey
//! and the Fidelity Flywheel).
//!
//! Drives the SAME desktop chat path (`Runtime::handle_message_stream`), sealed
//! to one corpus via `enabled_corpora`, then recovers the retrieved chunks +
//! routing provenance from the persisted assistant message. Every probe set
//! (I1–I5) flows through this one runner, so the loop exercises the real router
//! + retrieval + synthesis — not a stub. Generalized out of the chaos bench's
//! `run_synth` so reuse is proven from day one.
//!
//! The forced-choice judges (`classify_abstain`, `classify_caveat`) live here
//! too: they are the *observation* step that turns a free-text reply into the
//! `AgentAction` / caveat signal the (pure) verifier consumes. They are
//! objective single-letter classifications the chaos bench already gates on.

use futures::StreamExt as _;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

use crate::chat_cmd::bootstrap::ChatSession;

/// What the live path produced for one probe.
///
/// (Phase 5 will add `coarse_intent` here — recovered from the persisted
/// `provenance` metadata — once the F-MISROUTE register check actually reads
/// it; kept minimal until then so there's no speculative dead field.)
pub struct LiveAnswer {
    /// Think-stripped visible answer (what a user would read).
    pub visible: String,
    /// Retrieved chunk text recovered from the persisted assistant message.
    pub retrieved_chunk_texts: Vec<String>,
}

/// Drive the desktop chat path, sealed to `corpus` via `enabled_corpora`.
/// Best-effort: a seeding/stream failure degrades to an empty answer (the
/// caller scores it as an abstention / miss) rather than aborting the battery.
pub async fn run_live(session: &ChatSession, corpus: &str, question: &str) -> LiveAnswer {
    let conv_id = uuid::Uuid::new_v4().to_string();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Seal retrieval to the bank's corpus so ABSENT questions genuinely have
    // nothing to find.
    let _ = session.store.insert_empty_conversation(&conv_id, created_at, None).await;
    let _ = session
        .store
        .set_conversation_enabled_corpora(&conv_id, Some(vec![corpus.to_string()]))
        .await;

    let raw = match session.runtime.handle_message_stream(question, &conv_id).await {
        Ok(handle) => {
            let mut stream = handle.stream;
            let mut buf = String::new();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => buf.push_str(&chunk),
                    Err(e) => {
                        eprintln!("    [live] stream error: {e}");
                        break;
                    }
                }
            }
            buf
        }
        Err(sovereign_core::error::Error::NotImplemented(_)) => {
            match session.runtime.handle_message(question, &conv_id).await {
                Ok(resp) => resp.message.content,
                Err(e) => {
                    eprintln!("    [live] fallback failed: {e}");
                    String::new()
                }
            }
        }
        Err(e) => {
            eprintln!("    [live] stream start: {e}");
            String::new()
        }
    };

    // Recover retrieved chunk text from the persisted assistant message.
    let retrieved_chunk_texts = session
        .store
        .get_conversation(&conv_id)
        .await
        .ok()
        .and_then(|c| c.messages.last().and_then(|m| m.metadata.clone()))
        .and_then(|m| m.get("retrieved_chunks").and_then(|v| v.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    ["text", "content", "passage_preview", "preview", "snippet"]
                        .iter()
                        .find_map(|k| c.get(*k).and_then(|v| v.as_str()))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let visible = strip_think(&raw);
    LiveAnswer { visible, retrieved_chunk_texts }
}

/// Drive the BARE model — the "true baseline" control. NONE of Commonwealth's
/// value-add: no system prompt, no retrieval injection, no router / synthesis /
/// presenter pipeline. Just `{user: question} → model → answer`, at the same
/// model + temperature as `run_live`, so the ONLY variables removed are our
/// prompting and retrieval. The delta (`run_live` − `run_naked`) is exactly the
/// measured value-add. `retrieved_chunk_texts` is empty by definition (no
/// retrieval), so grounding sub-metrics (citation_fidelity, distractor) score
/// against an empty set — that's the point: the naked model has no sources.
pub async fn run_naked(
    provider: &dyn InferenceProvider,
    model: &str,
    question: &str,
    max_tokens: usize,
) -> LiveAnswer {
    let req = CompletionRequest {
        prompt: question.to_string(),
        system_message: None,
        preferred_speed: Speed::Slow,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    let raw = match provider.complete(&req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            eprintln!("    [naked] complete failed: {e}");
            String::new()
        }
    };
    LiveAnswer {
        visible: strip_think(&raw),
        retrieved_chunk_texts: Vec::new(),
    }
}

/// Remove `<think>…</think>` reasoning blocks; keep the visible answer.
pub fn strip_think(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</think>") {
            rest = &rest[start + end + "</think>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Forced-choice answer-vs-abstain classifier. `Some(true)` = abstained.
pub async fn classify_abstain(judge: &dyn InferenceProvider, model: &str, answer: &str) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(true);
    }
    let prompt = format!(
        "An assistant was asked a question and replied:\n\n\"\"\"\n{}\n\"\"\"\n\n\
         Did the assistant provide a substantive answer, or did it decline / say it \
         does not have the information?\n\n\
         Answer with exactly one letter — A = gave a substantive answer, B = declined / lacks the information.",
        answer.chars().take(1200).collect::<String>()
    );
    forced_choice_ab(judge, model, &prompt).await.map(|(a, b)| b > a)
}

/// Forced-choice provenance-caveat classifier for out-of-domain answers.
/// `Some(true)` = the reply explicitly flags the answer is general knowledge /
/// NOT drawn from the provided passages.
pub async fn classify_caveat(judge: &dyn InferenceProvider, model: &str, answer: &str) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(false);
    }
    let prompt = format!(
        "An assistant answered a question whose answer is NOT in the provided source passages \
         (it comes from the assistant's general knowledge). Here is the reply:\n\n\"\"\"\n{}\n\"\"\"\n\n\
         Did the assistant EXPLICITLY flag that this answer comes from its general knowledge / is \
         NOT drawn from the provided passages or corpus?\n\n\
         Answer with exactly one letter — A = yes, it flagged the answer as general knowledge / not from the sources, B = no, it gave the answer with no such provenance caveat.",
        answer.chars().take(1200).collect::<String>()
    );
    forced_choice_ab(judge, model, &prompt).await.map(|(a, b)| a > b)
}

/// One forced-choice A/B logprob pass. Returns `(p_A, p_B)`.
async fn forced_choice_ab(judge: &dyn InferenceProvider, model: &str, prompt: &str) -> Option<(f64, f64)> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        system_message: Some("You are a careful classifier. Answer with a single letter.".into()),
        preferred_speed: Speed::Medium,
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    match judge.complete(&req).await {
        Ok(resp) => {
            let m: std::collections::HashMap<String, f64> = serde_json::from_str(resp.text.trim()).ok()?;
            Some((m.get("A").copied().unwrap_or(0.0), m.get("B").copied().unwrap_or(0.0)))
        }
        Err(e) => {
            eprintln!("    [judge] {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_think_removes_reasoning_blocks() {
        assert_eq!(strip_think("<think>plan</think>The answer"), "The answer");
        assert_eq!(strip_think("bare answer"), "bare answer");
        assert_eq!(strip_think("<think>unterminated"), "");
    }
}
