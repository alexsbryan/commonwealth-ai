use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::skills::MergedMemoryConfig;
use crate::traits::{InferenceProvider, StateStore};
use crate::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ─── Working Memory Compression ───────────────────────────────

/// Compress recent conversation messages into a structured WorkingMemory.
/// Uses the Fast slot for low latency since this runs on every message.
pub async fn compress_working_memory(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    previous: Option<&WorkingMemory>,
) -> Result<WorkingMemory> {
    tracing::debug!(
        messages = messages.len(),
        has_previous = previous.is_some(),
        "memory: compress_working_memory — begin"
    );

    if messages.len() < 2 {
        tracing::debug!("memory: compress_working_memory — not enough messages, skipping");
        return Ok(previous.cloned().unwrap_or(WorkingMemory {
            current_goal: None,
            facts: Vec::new(),
            active_documents: Vec::new(),
        }));
    }

    // Format last 8 messages.
    let recent: Vec<String> = messages
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{role}: {}", &m.content[..m.content.len().min(300)])
        })
        .collect();

    let mut context_prefix = String::new();
    if let Some(prev) = previous {
        if let Some(goal) = &prev.current_goal {
            context_prefix.push_str(&format!("Previous goal: {goal}\n"));
        }
        if !prev.facts.is_empty() {
            context_prefix.push_str(&format!(
                "Known facts: {}\n",
                prev.facts.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
            ));
        }
    }

    let prompt = format!(
        "{context_prefix}Conversation:\n{}\n\n\
         Produce a JSON object with:\n\
         - \"goal\": the user's current goal (string or null)\n\
         - \"facts\": array of short factual statements established so far\n\n\
         Respond with only the JSON object.",
        recent.join("\n")
    );

    let request = CompletionRequest {
        prompt,
        system_message: Some(
            "Extract the user's goal and key facts from the conversation. Respond with JSON only."
                .to_string(),
        ),
        preferred_speed: Speed::Fast,
        max_tokens: Some(200),
        temperature: Some(0.1),
        structured_output: None,
            think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
    };

    let response = inference.complete(&request).await?;
    let result = parse_working_memory(&response.text, previous);
    if let Ok(ref wm) = result {
        tracing::debug!(
            has_goal = wm.current_goal.is_some(),
            fact_count = wm.facts.len(),
            "memory: compress_working_memory — done"
        );
    }
    result
}

/// Parse working memory from LLM response, with fallback.
fn parse_working_memory(
    text: &str,
    previous: Option<&WorkingMemory>,
) -> Result<WorkingMemory> {
    // Try full JSON parse first.
    if let Some(json_str) = extract_json_object(text) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
            let goal = val
                .get("goal")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let facts: Vec<String> = val
                .get("facts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            return Ok(WorkingMemory {
                current_goal: goal,
                facts,
                active_documents: Vec::new(),
            });
        }
    }

    // Fallback: return previous or empty.
    Ok(previous.cloned().unwrap_or(WorkingMemory {
        current_goal: None,
        facts: Vec::new(),
        active_documents: Vec::new(),
    }))
}

/// Extract a JSON object substring from text.
fn extract_json_object(text: &str) -> Option<String> {
    // Try ```json ... ``` fence.
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // Try bare { ... }.
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

// ─── Long-Term Memory Extraction ──────────────────────────────

/// Extract durable facts from a conversation for long-term storage.
/// Uses the Primary (Slow) slot for better extraction quality.
pub async fn extract_long_term_memories(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    memory_rules: &MergedMemoryConfig,
) -> Result<Vec<Memory>> {
    tracing::debug!(
        messages = messages.len(),
        "memory: extract_long_term_memories — begin"
    );

    if messages.len() < 4 {
        tracing::debug!("memory: extract_long_term_memories — not enough messages, skipping");
        return Ok(Vec::new());
    }

    // Format conversation.
    let conversation_text: String = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{role}: {}", &m.content[..m.content.len().min(500)])
        })
        .collect::<Vec<_>>()
        .join("\n");

    let addenda = if memory_rules.extraction_addenda.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nAdditional extraction rules:\n{}",
            memory_rules.extraction_addenda.join("\n")
        )
    };

    let prompt = format!(
        "Given this conversation, extract any durable facts about the user that would be \
         useful in future conversations. Only extract clearly true, persistently relevant \
         things (preferences, profession, location, tools used, etc.). Do not extract \
         transient requests or conversation-specific details.\n\n\
         Conversation:\n{conversation_text}{addenda}\n\n\
         Return a JSON array of strings, each a single fact. Return [] if none."
    );

    let request = CompletionRequest {
        prompt,
        system_message: Some(
            "You extract durable user facts from conversations. Respond with a JSON array of strings only."
                .to_string(),
        ),
        preferred_speed: Speed::Slow,
        max_tokens: Some(300),
        temperature: Some(0.3),
        structured_output: None,
            think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
    };

    let response = inference.complete(&request).await?;
    let result = parse_extracted_memories(&response.text);
    if let Ok(ref memories) = result {
        tracing::info!(
            extracted = memories.len(),
            "memory: extract_long_term_memories — done"
        );
    }
    result
}

/// Parse extracted memories from LLM response.
fn parse_extracted_memories(text: &str) -> Result<Vec<Memory>> {
    let current_time = now();

    // Try to find JSON array.
    let json_str = if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                &text[start..=end]
            } else {
                return Ok(Vec::new());
            }
        } else {
            return Ok(Vec::new());
        }
    } else {
        return Ok(Vec::new());
    };

    let facts: Vec<String> = serde_json::from_str(json_str).unwrap_or_default();

    Ok(facts
        .into_iter()
        .filter(|f| f.len() > 3)
        .map(|content| Memory {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            source: "conversation_extraction".to_string(),
            confidence: 1.0,
            created_at: current_time,
            last_used: current_time,
            version: current_time,
            deleted_at: None,
            source_conversation_id: None,
        })
        .collect())
}

// ─── Memory Prompt Injection ──────────────────────────────────

/// Format memories for injection into system prompts.
pub fn format_memories_for_prompt(memories: &[Memory]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    let items: Vec<String> = memories.iter().map(|m| format!("- {}", m.content)).collect();
    Some(format!("Known facts about the user:\n{}", items.join("\n")))
}

// ─── Contradiction Detection ──────────────────────────────────

/// Detect which existing memories are contradicted by a new fact.
/// Returns IDs of memories that should be deleted.
pub async fn detect_contradictions(
    inference: &dyn InferenceProvider,
    new_memory: &Memory,
    existing: &[Memory],
) -> Result<Vec<String>> {
    if existing.is_empty() {
        return Ok(Vec::new());
    }

    let numbered: String = existing
        .iter()
        .enumerate()
        .map(|(i, m)| format!("{}. {}", i + 1, m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "New fact: \"{}\"\n\n\
         Existing facts:\n{numbered}\n\n\
         Which existing facts, if any, are CONTRADICTED by the new fact? \
         A contradiction means the new fact makes the old fact false. \
         Return a JSON array of the numbers of contradicted facts, or [] if none.",
        new_memory.content,
    );

    let request = CompletionRequest {
        prompt,
        system_message: Some(
            "Identify contradictions. Respond with a JSON array of numbers only.".to_string(),
        ),
        preferred_speed: Speed::Fast,
        max_tokens: Some(50),
        temperature: Some(0.0),
        structured_output: None,
            think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
    };

    let response = inference.complete(&request).await?;

    // Parse array of indices.
    let indices: Vec<usize> = if let Some(start) = response.text.find('[') {
        if let Some(end) = response.text.rfind(']') {
            serde_json::from_str(&response.text[start..=end]).unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Map 1-based indices back to memory IDs.
    let ids: Vec<String> = indices
        .into_iter()
        .filter_map(|i| existing.get(i.wrapping_sub(1)).map(|m| m.id.clone()))
        .collect();

    Ok(ids)
}

// ─── Confidence Decay ─────────────────────────────────────────

const DEFAULT_DECAY_RATE: f64 = 0.10; // 10% per month
const DEFAULT_PRUNE_THRESHOLD: f64 = 0.2;

/// Calculate decayed confidence for a memory based on time since last use.
/// `decay_rate` is the fraction lost per month (default 0.10 = 10%).
pub fn apply_confidence_decay(memory: &Memory, now: i64) -> f64 {
    apply_confidence_decay_with_rate(memory, now, DEFAULT_DECAY_RATE)
}

/// Calculate decayed confidence with a custom decay rate.
pub fn apply_confidence_decay_with_rate(memory: &Memory, now: i64, decay_rate: f64) -> f64 {
    let months_elapsed = (now - memory.last_used) as f64 / (30.0 * 86400.0);
    let retention = 1.0 - decay_rate.clamp(0.0, 1.0);
    memory.confidence * retention.powf(months_elapsed)
}

/// Prune memories with decayed confidence below threshold.
/// Uses default decay rate (10%/month) and prune threshold (0.2).
pub async fn prune_decayed_memories(store: &dyn StateStore, now_ts: i64) -> Result<usize> {
    prune_decayed_memories_with_config(store, now_ts, DEFAULT_DECAY_RATE, DEFAULT_PRUNE_THRESHOLD)
        .await
}

/// Prune memories with configurable decay rate and threshold.
pub async fn prune_decayed_memories_with_config(
    store: &dyn StateStore,
    now_ts: i64,
    decay_rate: f64,
    prune_threshold: f64,
) -> Result<usize> {
    let all = store.get_all_memories().await?;
    let mut pruned = 0;

    for memory in &all {
        let decayed = apply_confidence_decay_with_rate(memory, now_ts, decay_rate);
        if decayed < prune_threshold {
            store.delete_memory(&memory.id).await?;
            pruned += 1;
        } else if (decayed - memory.confidence).abs() > 0.01 {
            store
                .update_memory_confidence(&memory.id, decayed)
                .await?;
        }
    }

    Ok(pruned)
}

// ─── Save with Contradiction Check ────────────────────────────

/// Save a new memory, first checking for duplicates and contradictions.
pub async fn save_with_contradiction_check(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    new_memory: Memory,
) -> Result<()> {
    let existing = store.get_all_memories().await?;

    // Check for exact duplicate content.
    let new_lower = new_memory.content.trim().to_lowercase();
    if existing.iter().any(|m| m.content.trim().to_lowercase() == new_lower) {
        return Ok(());
    }

    // Detect and delete contradictions.
    let contradicted_ids = detect_contradictions(inference, &new_memory, &existing).await?;
    for id in &contradicted_ids {
        store.delete_memory(id).await?;
    }

    store.save_memory(&new_memory).await
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_decay_one_month() {
        let mem = Memory {
            id: "1".to_string(),
            content: "test".to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
        };
        let one_month = 30 * 86400;
        let decayed = apply_confidence_decay(&mem, one_month);
        assert!((decayed - 0.9).abs() < 0.001);
    }

    #[test]
    fn confidence_decay_six_months() {
        let mem = Memory {
            id: "1".to_string(),
            content: "test".to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
        };
        let six_months = 6 * 30 * 86400;
        let decayed = apply_confidence_decay(&mem, six_months);
        assert!((decayed - 0.531).abs() < 0.01);
    }

    #[test]
    fn confidence_decay_24_months_below_threshold() {
        let mem = Memory {
            id: "1".to_string(),
            content: "test".to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
        };
        let two_years = 24 * 30 * 86400;
        let decayed = apply_confidence_decay(&mem, two_years);
        assert!(decayed < 0.2, "expected < 0.2, got {decayed}");
    }

    #[test]
    fn format_memories_empty_returns_none() {
        assert!(format_memories_for_prompt(&[]).is_none());
    }

    #[test]
    fn format_memories_returns_bullet_list() {
        let memories = vec![
            Memory {
                id: "1".to_string(),
                content: "User prefers Rust".to_string(),
                source: "test".to_string(),
                confidence: 1.0,
                created_at: 0,
                last_used: 0,
                version: 0,
                deleted_at: None,
                source_conversation_id: None,
            },
            Memory {
                id: "2".to_string(),
                content: "User is a backend engineer".to_string(),
                source: "test".to_string(),
                confidence: 1.0,
                created_at: 0,
                last_used: 0,
                version: 0,
                deleted_at: None,
                source_conversation_id: None,
            },
        ];
        let result = format_memories_for_prompt(&memories).unwrap();
        assert!(result.contains("Known facts about the user:"));
        assert!(result.contains("- User prefers Rust"));
        assert!(result.contains("- User is a backend engineer"));
    }

    #[test]
    fn parse_working_memory_valid_json() {
        let text = r#"{"goal": "build a web app", "facts": ["User knows Rust", "Using Axum"]}"#;
        let wm = parse_working_memory(text, None).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("build a web app"));
        assert_eq!(wm.facts.len(), 2);
        assert_eq!(wm.facts[0], "User knows Rust");
    }

    #[test]
    fn parse_working_memory_json_fence() {
        let text = "Here is the result:\n```json\n{\"goal\": \"test\", \"facts\": []}\n```";
        let wm = parse_working_memory(text, None).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("test"));
    }

    #[test]
    fn parse_working_memory_fallback() {
        let text = "I don't understand the request";
        let prev = WorkingMemory {
            current_goal: Some("previous goal".to_string()),
            facts: vec!["old fact".to_string()],
            active_documents: vec![],
        };
        let wm = parse_working_memory(text, Some(&prev)).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("previous goal"));
    }

    #[test]
    fn parse_extracted_memories_valid() {
        let text = r#"["User prefers Rust", "User lives in Portland"]"#;
        let mems = parse_extracted_memories(text).unwrap();
        assert_eq!(mems.len(), 2);
        assert_eq!(mems[0].content, "User prefers Rust");
        assert_eq!(mems[0].confidence, 1.0);
        assert_eq!(mems[0].source, "conversation_extraction");
    }

    #[test]
    fn parse_extracted_memories_empty() {
        let text = "[]";
        let mems = parse_extracted_memories(text).unwrap();
        assert!(mems.is_empty());
    }

    #[test]
    fn parse_extracted_memories_garbage() {
        let text = "I found some facts about the user";
        let mems = parse_extracted_memories(text).unwrap();
        assert!(mems.is_empty());
    }
}
