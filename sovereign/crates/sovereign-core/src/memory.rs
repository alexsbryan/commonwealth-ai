use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::skills::{MergedMemoryConfig, SkillRegister};
use crate::traits::{InferenceProvider, MemoryScope, StateStore};
use crate::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Cosine similarity between two equally-sized embedding vectors.
/// Returns 0.0 when either norm is zero or lengths mismatch — the
/// caller treats that as "no signal" and falls back to FTS.
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
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Embedding-based memory recall — used on relational/witness paths
/// where keyword FTS misses the seed memories on abstract queries
/// (hard-mode H05: *"what kind of person am I?"* against concrete-
/// event memories shares zero keywords). Retrieves all live
/// memories, batch-embeds their content alongside the query, scores
/// by cosine similarity, applies the same confidence-decay floor as
/// FTS, returns top-K.
///
/// Falls back to the FTS path on any embedding error (empty query,
/// dim mismatch, batch failure) so the caller never sees a hard
/// failure — the retrieval just degrades to keyword.
///
/// Cost: 1 query embed + 1 batched embed of all live memories per
/// turn. For voice-eval scenarios with <10 seeds, this is ~50–200ms.
/// At production scale (hundreds of memories) the right next step is
/// schema-side caching of embeddings; this helper keeps the
/// architectural surface clean for that follow-up.
pub async fn recall_relevant_memories_embed(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    scope: &MemoryScope,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Scope-filtered fetch — the wall is enforced before the embed
    // batch so we never embed memories the caller isn't allowed to
    // see. (Embedding scoped memories would leak content via the
    // inference provider's logging/telemetry even if we filtered the
    // result.)
    let all = store
        .get_all_memories_for_scope(scope)
        .await
        .unwrap_or_default();
    if all.is_empty() {
        return Ok(Vec::new());
    }

    let query_emb = match inference.embed_query(query).await {
        Ok(e) if !e.is_empty() => e,
        _ => {
            tracing::debug!("memory: embed recall — query embed failed, falling back to FTS");
            return Ok(store
                .get_relevant_memories_for_scope(scope, query, limit)
                .await
                .unwrap_or_default());
        }
    };

    let texts: Vec<String> = all.iter().map(|m| m.content.clone()).collect();
    let embs = match inference.embed_batch(&texts).await {
        Ok(es) if es.len() == all.len() => es,
        _ => {
            tracing::debug!(
                memories = all.len(),
                "memory: embed recall — batch embed failed, falling back to FTS"
            );
            return Ok(store
                .get_relevant_memories_for_scope(scope, query, limit)
                .await
                .unwrap_or_default());
        }
    };

    let now_ts = now();
    let mut scored: Vec<(f32, Memory)> = embs
        .into_iter()
        .zip(all.into_iter())
        .filter_map(|(emb, m)| {
            // Same confidence-decay floor as FTS path
            // (sqlite::get_relevant_memories): drop memories whose
            // decayed confidence falls below 0.2.
            let months = (now_ts - m.last_used) as f64 / (30.0 * 86400.0);
            let decayed = m.confidence * 0.9_f64.powf(months.max(0.0));
            if decayed < 0.2 {
                return None;
            }
            let sim = cosine_similarity(&query_emb, &emb);
            Some((sim, m))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let top = scored.into_iter().map(|(_, m)| m).collect::<Vec<_>>();
    tracing::debug!(
        returned = top.len(),
        limit,
        "memory: embed recall — returning top-K by cosine"
    );
    Ok(top)
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
                    model_id: None,
                    enable_thinking: None,
    sampling_mode: None,
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
                    model_id: None,
                    enable_thinking: None,
    sampling_mode: None,
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
            source_skill_id: None,
        })
        .collect())
}

// ─── Memory Prompt Injection ──────────────────────────────────

/// Confidence threshold for the "directly stated" register —
/// memories at or above this are presented as things the user
/// said in their own words.
const RELATIONAL_DIRECT_THRESHOLD: f64 = 0.85;

/// Confidence threshold for the "inferred" register — memories
/// between this and `RELATIONAL_DIRECT_THRESHOLD` are presented as
/// patterns read across conversations rather than verbatim claims.
/// Memories below this threshold land in the "tentative" band.
const RELATIONAL_INFER_THRESHOLD: f64 = 0.5;

/// Format memories for injection into system prompts. The `register`
/// argument determines the surface shape:
///
/// * `Factual` — flat bulleted list under the heading "Known facts
///   about the user:". Pre-existing behavior; preserved for the
///   default voice contract.
/// * `Relational` — three confidence-banded sections that the model
///   can render into its three epistemic registers (history /
///   inference / guess). Memories whose `source_conversation_id` is
///   set get a `[YYYY-MM-DD]` prefix derived from `created_at`, so
///   the model can produce situated phrasing like "you told me on
///   2026-03-12 that…" instead of flat assertions.
///
/// Returns `None` when `memories` is empty.
pub fn format_memories_for_prompt(
    memories: &[Memory],
    register: SkillRegister,
) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    match register {
        SkillRegister::Factual => format_factual(memories),
        SkillRegister::Relational => format_relational(memories),
    }
}

fn format_factual(memories: &[Memory]) -> Option<String> {
    let items: Vec<String> = memories
        .iter()
        .map(|m| format!("- {}", m.content))
        .collect();
    Some(format!("Known facts about the user:\n{}", items.join("\n")))
}

fn format_relational(memories: &[Memory]) -> Option<String> {
    let mut directly: Vec<&Memory> = Vec::new();
    let mut inferred: Vec<&Memory> = Vec::new();
    let mut tentative: Vec<&Memory> = Vec::new();
    for m in memories {
        if m.confidence >= RELATIONAL_DIRECT_THRESHOLD {
            directly.push(m);
        } else if m.confidence >= RELATIONAL_INFER_THRESHOLD {
            inferred.push(m);
        } else {
            tentative.push(m);
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if !directly.is_empty() {
        sections.push(format!(
            "What you've told me directly:\n{}",
            render_band(&directly).join("\n")
        ));
    }
    if !inferred.is_empty() {
        sections.push(format!(
            "What I've inferred from earlier conversations:\n{}",
            render_band(&inferred).join("\n")
        ));
    }
    if !tentative.is_empty() {
        sections.push(format!(
            "Tentative — flag these as guesses if you surface them:\n{}",
            render_band(&tentative).join("\n")
        ));
    }

    Some(sections.join("\n\n"))
}

fn render_band(memories: &[&Memory]) -> Vec<String> {
    memories
        .iter()
        .map(|m| {
            let date_prefix = m
                .source_conversation_id
                .as_ref()
                .and_then(|_| format_unix_date(m.created_at))
                .map(|d| format!("[{d}] "))
                .unwrap_or_default();
            format!(
                "- {date_prefix}{}   (confidence {:.2})",
                m.content, m.confidence
            )
        })
        .collect()
}

/// Render a Unix timestamp (seconds) as `YYYY-MM-DD` in UTC.
/// Returns `None` for negative timestamps and timestamps that don't
/// resolve to a valid date — both treated as missing-date cases so
/// the renderer can fall through to an undated bullet.
fn format_unix_date(ts: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
}

// ─── Temporal Tension Detection ───────────────────────────────

/// Maximum number of memories considered in a single tension
/// pre-pass. Bounds the Quick-slot inference cost so adding the
/// pre-pass doesn't dominate per-turn latency. Picked at 5 because
/// the directly-stated band is by construction a small set per
/// retrieval call (top-K=5 in current production), and at K=5 the
/// classifier batch fits comfortably under the Fast slot's
/// 1024-token budget.
const MAX_TENSION_CANDIDATES: usize = 5;

/// Maximum char length of the user-message excerpt that's spliced
/// alongside a tension. Bounds the prompt size so a long pasted
/// passage doesn't bloat every turn's system prompt. The model
/// only needs the gist of what the user just said; the full
/// message is in the conversation history immediately below.
const TENSION_EXCERPT_CHAR_CAP: usize = 240;

/// JSON shape the Quick-slot classifier is asked to return, one
/// item per candidate memory.
#[derive(Debug, serde::Deserialize)]
struct TensionClassification {
    index: usize,
    relation: String,
}

/// Detect tensions between the user's current message and prior
/// directly-stated memories. Implements principle 5 ("surface
/// contradictions across time") of the relational voice contract.
///
/// Inputs:
/// * `inference` — provider used to make a single Fast-slot call.
/// * `current_message` — what the user just said.
/// * `memories` — the memories already loaded into the
///   conversation context (from FTS retrieval). Filtered here to
///   the directly-stated band (`confidence ≥ RELATIONAL_DIRECT_THRESHOLD`)
///   so guesses and inferences don't seed false-positive tensions.
///
/// Behaviour:
/// * Returns `Ok(Vec::new())` when there are no candidate memories
///   — common case for casual chat, costs zero inference.
/// * Issues one Fast-slot batched JSON-classifier call. Soft-fails
///   on parse error (returns empty rather than blocking the turn).
/// * Returns at most `MAX_TENSION_CANDIDATES` tensions.
///
/// The function is register-agnostic — the *caller* (the Runtime)
/// is responsible for skipping it for factual skills. Keeping the
/// gate in the caller avoids threading `SkillRegister` through
/// memory's public surface and keeps this fn unit-testable in
/// isolation.
pub async fn detect_temporal_tensions(
    inference: &dyn InferenceProvider,
    current_message: &str,
    memories: &[Memory],
) -> Result<Vec<TemporalTension>> {
    let candidates: Vec<&Memory> = memories
        .iter()
        .filter(|m| m.confidence >= RELATIONAL_DIRECT_THRESHOLD)
        .filter(|m| m.deleted_at.is_none())
        .take(MAX_TENSION_CANDIDATES)
        .collect();

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let listing = candidates
        .iter()
        .enumerate()
        .map(|(i, m)| {
            // Memory.content is user-derived but JSON-escape it
            // before splicing into the prompt — defensive against
            // quotes / newlines that would corrupt the listing.
            format!(
                "{{\"index\": {i}, \"memory\": {}}}",
                serde_json::to_string(&m.content).unwrap_or_else(|_| "\"\"".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");

    let prompt = format!(
        "You are a tension-detector for a situated conversation. Compare each \
prior memory the user expressed against the user's current message. Classify \
each pairing as exactly one of:\n\
- \"tension\" — the new statement materially contradicts the prior memory, OR \
describes the same subject in a way that would benefit from gentle surfacing \
(e.g., \"I'm leaving the job\" vs. \"I want to grow here\").\n\
- \"consistent\" — the new statement reinforces or naturally extends the prior memory.\n\
- \"neutral\" — the topics don't relate enough to evaluate.\n\n\
Bias toward \"neutral\" when uncertain. \"tension\" should be a deliberate \
flag, not a default.\n\n\
User's current message:\n{current_message}\n\n\
Prior memories (JSON):\n[\n{listing}\n]\n\n\
Reply with a JSON array, one entry per memory, in the original order:\n\
[{{\"index\": <i>, \"relation\": \"consistent|neutral|tension\"}}, ...]"
    );

    let schema = serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "index": { "type": "integer", "minimum": 0 },
                "relation": { "type": "string", "enum": ["consistent", "neutral", "tension"] }
            },
            "required": ["index", "relation"],
            "additionalProperties": false
        }
    });

    let mut request = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
    request.structured_output = Some(schema);
    request.max_tokens = Some(512);

    let response = inference.complete(&request).await?;
    let parsed = parse_tension_classifications(&response.text);

    let excerpt = excerpt_message(current_message);
    let tensions: Vec<TemporalTension> = parsed
        .into_iter()
        .filter(|item| item.relation == "tension")
        .filter_map(|item| {
            candidates
                .get(item.index)
                .map(|m| TemporalTension {
                    memory_id: m.id.clone(),
                    prior_content: m.content.clone(),
                    prior_created_at: m.created_at,
                    prior_has_source_conversation: m.source_conversation_id.is_some(),
                    current_excerpt: excerpt.clone(),
                })
        })
        .collect();

    Ok(tensions)
}

fn excerpt_message(msg: &str) -> String {
    if msg.chars().count() <= TENSION_EXCERPT_CHAR_CAP {
        msg.to_string()
    } else {
        let head: String = msg.chars().take(TENSION_EXCERPT_CHAR_CAP).collect();
        format!("{head}…")
    }
}

/// Parse the Quick-slot classifier's response. Soft-fail policy:
/// any deviation from the schema yields an empty `Vec`, NOT an
/// error — a malformed pre-pass response must never block a turn,
/// it just suppresses the tension-surfacing cue and the model
/// continues without it.
fn parse_tension_classifications(text: &str) -> Vec<TensionClassification> {
    // Try the raw text first — the structured_output path should
    // produce a clean JSON array directly.
    if let Ok(items) = serde_json::from_str::<Vec<TensionClassification>>(text.trim()) {
        return items;
    }
    // Fallback: extract bracketed array from a possibly-fenced or
    // prose-wrapped response.
    if let Some(arr) = extract_json_array(text) {
        if let Ok(items) = serde_json::from_str::<Vec<TensionClassification>>(&arr) {
            return items;
        }
    }
    Vec::new()
}

/// Extract a `[...]` JSON array from text that may contain code
/// fences or trailing prose. Mirrors `extract_json_object` but for
/// arrays.
fn extract_json_array(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
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
                    model_id: None,
                    enable_thinking: None,
    sampling_mode: None,
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

/// Default uniform decay rate: 10% confidence loss per month.
/// Exposed so the Runtime's prune path can construct an explicit
/// `prune_decayed_memories_with_config` call when it has an entity
/// inventory available.
pub const DEFAULT_DECAY_RATE: f64 = 0.10;
/// Confidence floor below which a memory is dropped during prune.
pub const DEFAULT_PRUNE_THRESHOLD: f64 = 0.2;

/// Inventory of entity names that mark a memory as relationally /
/// strategically relevant. Memories whose `content` mentions any
/// inventory name decay at half the configured rate per
/// requirements §5 (relationship-weighted decay).
///
/// Names are stored lowercased + trimmed; matching is whole-word
/// case-insensitive (substring `Sarah` does NOT match `Sarahkov`).
/// The set is rebuilt from the personal + conversational atlas's
/// `atoms.json` files at the end of each enrichment cycle.
pub type EntityInventory = std::collections::HashSet<String>;

/// Calculate decayed confidence for a memory based on time since last use.
/// `decay_rate` is the fraction lost per month (default 0.10 = 10%).
///
/// Convenience wrapper — no entity inventory, full decay applied.
pub fn apply_confidence_decay(memory: &Memory, now: i64) -> f64 {
    apply_confidence_decay_with_rate_and_inventory(memory, now, DEFAULT_DECAY_RATE, None)
}

/// Calculate decayed confidence with a custom decay rate.
///
/// Convenience wrapper — no entity inventory, full rate applied.
/// Use [`apply_confidence_decay_with_rate_and_inventory`] when an
/// entity inventory is available so relationship-weighted decay
/// kicks in.
pub fn apply_confidence_decay_with_rate(memory: &Memory, now: i64, decay_rate: f64) -> f64 {
    apply_confidence_decay_with_rate_and_inventory(memory, now, decay_rate, None)
}

/// Full-fat decay: rate halved when the memory mentions any name in
/// the inventory.
///
/// The fixed-half rule (not a separately configurable parameter) is
/// per requirements §5.2 — "a fixed ratio, not a configurable
/// parameter." A skill that overrides `confidence_decay_per_month`
/// to 15% sees entity-linked memories decay at 7.5%.
///
/// `inventory = None` short-circuits to the unweighted formula —
/// callers without an inventory loaded yet (first run, enrichment
/// disabled) keep the default 10%/month behaviour.
pub fn apply_confidence_decay_with_rate_and_inventory(
    memory: &Memory,
    now: i64,
    decay_rate: f64,
    inventory: Option<&EntityInventory>,
) -> f64 {
    let effective_rate = match inventory {
        Some(inv) if memory_mentions_any_entity(&memory.content, inv) => decay_rate / 2.0,
        _ => decay_rate,
    };
    let months_elapsed = (now - memory.last_used) as f64 / (30.0 * 86400.0);
    let retention = 1.0 - effective_rate.clamp(0.0, 1.0);
    memory.confidence * retention.powf(months_elapsed)
}

/// Whole-word case-insensitive substring check. `entities` is a set
/// of lowercased names; `content` is split on non-alphanumeric and
/// each token is compared.
///
/// Multi-word names (e.g. "Sarah Chen", "API migration") are
/// detected by joining adjacent tokens up to the longest matching
/// run — we walk the memory's tokens and try each prefix slice
/// against the inventory. Linear in `tokens.len() *
/// max_inventory_words`; entity names are typically 1–3 words and
/// memories are short enough that this is well below the noise
/// floor.
fn memory_mentions_any_entity(content: &str, entities: &EntityInventory) -> bool {
    if entities.is_empty() {
        return false;
    }
    let tokens: Vec<String> = content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return false;
    }
    // Longest-match-wins: try 4-word, 3-word, 2-word, 1-word
    // windows. Most personal entities are 1–2 words; the cap at 4
    // covers full-name "First Middle Last Suffix" cases.
    const MAX_WINDOW: usize = 4;
    for start in 0..tokens.len() {
        let max_w = MAX_WINDOW.min(tokens.len() - start);
        for w in (1..=max_w).rev() {
            let candidate = tokens[start..start + w].join(" ");
            if entities.contains(&candidate) {
                return true;
            }
        }
    }
    false
}

/// Build an [`EntityInventory`] from a slice of raw entity names.
/// Names are lowercased + trimmed; empty strings are dropped.
/// Convenience for callers that have a `Vec<String>` from
/// `atoms.json` and want to feed it into the decay path.
pub fn entity_inventory_from_names<I, S>(names: I) -> EntityInventory
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter_map(|n| {
            let trimmed = n.as_ref().trim().to_lowercase();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect()
}

/// Prune memories with decayed confidence below threshold.
/// Uses default decay rate (10%/month) and prune threshold (0.2).
/// No entity inventory — decay is uniform.
pub async fn prune_decayed_memories(store: &dyn StateStore, now_ts: i64) -> Result<usize> {
    prune_decayed_memories_with_config(
        store,
        now_ts,
        DEFAULT_DECAY_RATE,
        DEFAULT_PRUNE_THRESHOLD,
        None,
    )
    .await
}

/// Prune memories with configurable decay rate, threshold, and an
/// optional entity inventory for relationship-weighted decay.
pub async fn prune_decayed_memories_with_config(
    store: &dyn StateStore,
    now_ts: i64,
    decay_rate: f64,
    prune_threshold: f64,
    inventory: Option<&EntityInventory>,
) -> Result<usize> {
    let all = store.get_all_memories().await?;
    let mut pruned = 0;

    for memory in &all {
        let decayed = apply_confidence_decay_with_rate_and_inventory(
            memory, now_ts, decay_rate, inventory,
        );
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

    fn mem_with_content(content: &str) -> Memory {
        Memory {
            id: "1".to_string(),
            content: content.to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            source_skill_id: None,
        }
    }

    #[test]
    fn confidence_decay_one_month() {
        let mem = mem_with_content("test");
        let one_month = 30 * 86400;
        let decayed = apply_confidence_decay(&mem, one_month);
        assert!((decayed - 0.9).abs() < 0.001);
    }

    // ── Relationship-weighted decay ───────────────────────────────

    #[test]
    fn entity_linked_memory_decays_at_half_rate() {
        // One month at default 10%/month: unweighted lands at 0.90,
        // entity-linked lands at 0.95.
        let mem = mem_with_content(
            "Discussed Q3 strategy with Sarah Chen at the offsite.",
        );
        let inventory = entity_inventory_from_names(["Sarah Chen", "Mike Torres"]);
        let one_month = 30 * 86400;

        let weighted = apply_confidence_decay_with_rate_and_inventory(
            &mem, one_month, 0.10, Some(&inventory),
        );
        let unweighted = apply_confidence_decay_with_rate_and_inventory(
            &mem, one_month, 0.10, None,
        );

        assert!((weighted - 0.95).abs() < 0.001, "weighted={weighted}");
        assert!((unweighted - 0.90).abs() < 0.001, "unweighted={unweighted}");
    }

    #[test]
    fn unmatched_memory_decays_at_full_rate_even_with_inventory() {
        let mem = mem_with_content("Just thinking about software architecture.");
        let inventory = entity_inventory_from_names(["Sarah Chen", "API migration"]);
        let one_month = 30 * 86400;
        let decayed = apply_confidence_decay_with_rate_and_inventory(
            &mem, one_month, 0.10, Some(&inventory),
        );
        // No match → full decay → 0.90.
        assert!((decayed - 0.90).abs() < 0.001);
    }

    #[test]
    fn whole_word_match_does_not_match_substring_within_a_word() {
        // "Sarah" must not match "Sarahkov" (a different person).
        let mem = mem_with_content("Read about Sarahkov, the historian.");
        let inventory = entity_inventory_from_names(["Sarah"]);
        assert!(!memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn match_is_case_insensitive() {
        let mem = mem_with_content("Brief chat with sarah about pricing.");
        let inventory = entity_inventory_from_names(["Sarah"]);
        assert!(memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn multi_word_entity_name_matches() {
        let mem = mem_with_content(
            "The API migration is on track for end of Q2.",
        );
        let inventory = entity_inventory_from_names(["API migration"]);
        assert!(memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn empty_inventory_short_circuits_to_full_decay() {
        let mem = mem_with_content("Sarah Chen mentioned the Q3 push.");
        let inventory: EntityInventory = EntityInventory::new();
        let one_month = 30 * 86400;
        let decayed = apply_confidence_decay_with_rate_and_inventory(
            &mem, one_month, 0.10, Some(&inventory),
        );
        assert!((decayed - 0.90).abs() < 0.001);
    }

    #[test]
    fn skill_overridden_rate_is_halved_when_entity_matches() {
        // A skill with confidence_decay_per_month = 0.15 should see
        // entity-linked memories at 7.5% per month.
        let mem = mem_with_content("Sarah Chen flagged a budget concern.");
        let inventory = entity_inventory_from_names(["Sarah Chen"]);
        let one_month = 30 * 86400;
        let weighted = apply_confidence_decay_with_rate_and_inventory(
            &mem, one_month, 0.15, Some(&inventory),
        );
        // Effective rate 0.075 → retention 0.925 → after 1 month 0.925.
        assert!((weighted - 0.925).abs() < 0.001, "weighted={weighted}");
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
            source_skill_id: None,
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
            source_skill_id: None,
        };
        let two_years = 24 * 30 * 86400;
        let decayed = apply_confidence_decay(&mem, two_years);
        assert!(decayed < 0.2, "expected < 0.2, got {decayed}");
    }

    #[test]
    fn format_memories_empty_returns_none() {
        assert!(format_memories_for_prompt(&[], SkillRegister::Factual).is_none());
        assert!(format_memories_for_prompt(&[], SkillRegister::Relational).is_none());
    }

    #[test]
    fn factual_register_returns_pre_existing_flat_bullet_list() {
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
                source_skill_id: None,
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
                source_skill_id: None,
            },
        ];
        let result =
            format_memories_for_prompt(&memories, SkillRegister::Factual).unwrap();
        assert!(result.contains("Known facts about the user:"));
        assert!(result.contains("- User prefers Rust"));
        assert!(result.contains("- User is a backend engineer"));
        // Banded headings must NOT appear in factual format.
        assert!(!result.contains("What you've told me directly"));
        assert!(!result.contains("What I've inferred"));
    }

    fn mem(
        id: &str,
        content: &str,
        confidence: f64,
        created_at: i64,
        source_conv: Option<&str>,
    ) -> Memory {
        Memory {
            id: id.to_string(),
            content: content.to_string(),
            source: "test".to_string(),
            confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: source_conv.map(|s| s.to_string()),
            source_skill_id: None,
        }
    }

    #[test]
    fn relational_register_splits_into_three_confidence_bands() {
        // 2026-03-12 00:00:00 UTC = 1773273600
        let directly = mem("d", "I want to leave the job", 0.92, 1_773_273_600, Some("c-mar"));
        // 2026-04-08 00:00:00 UTC = 1775606400
        let inferred = mem("i", "Work and meaning are linked for you", 0.62, 1_775_606_400, Some("c-apr"));
        let tentative = mem("t", "You may be avoiding conflict with Mark", 0.35, 0, None);

        let result = format_memories_for_prompt(
            &[directly, inferred, tentative],
            SkillRegister::Relational,
        )
        .unwrap();

        assert!(result.contains("What you've told me directly:"));
        assert!(result.contains("What I've inferred from earlier conversations:"));
        assert!(result.contains("Tentative — flag these as guesses"));
        assert!(result.contains("[2026-03-12]"));
        assert!(result.contains("[2026-04-08]"));
        assert!(result.contains("(confidence 0.92)"));
        assert!(result.contains("(confidence 0.62)"));
        assert!(result.contains("(confidence 0.35)"));
        // The flat-list factual heading must NOT appear in relational format.
        assert!(!result.contains("Known facts about the user:"));
    }

    #[test]
    fn relational_register_omits_date_when_no_source_conversation() {
        let undated = mem("u", "User prefers Rust", 0.95, 1_773_273_600, None);
        let result = format_memories_for_prompt(
            &[undated],
            SkillRegister::Relational,
        )
        .unwrap();
        // Date should not appear because source_conversation_id is None,
        // even though created_at would resolve to a valid date.
        assert!(!result.contains("[2026-03-12]"));
        assert!(result.contains("- User prefers Rust"));
    }

    #[test]
    fn relational_register_skips_empty_bands() {
        let only_directly = mem("d", "I told you X", 0.95, 1_773_273_600, Some("c"));
        let result = format_memories_for_prompt(
            &[only_directly],
            SkillRegister::Relational,
        )
        .unwrap();
        // Only the band that has content should render.
        assert!(result.contains("What you've told me directly:"));
        assert!(!result.contains("What I've inferred"));
        assert!(!result.contains("Tentative —"));
    }

    #[test]
    fn relational_register_band_thresholds_are_exact() {
        // 0.85 — exactly on the directly threshold (inclusive).
        let m_85 = mem("a", "boundary directly", 0.85, 0, None);
        // 0.5 — exactly on the inferred threshold (inclusive).
        let m_50 = mem("b", "boundary inferred", 0.50, 0, None);
        // 0.4999... — just below inferred threshold.
        let m_49 = mem("c", "tentative", 0.49, 0, None);

        let result = format_memories_for_prompt(
            &[m_85, m_50, m_49],
            SkillRegister::Relational,
        )
        .unwrap();
        // The directly band lists "boundary directly".
        let directly_idx = result.find("What you've told me directly:").unwrap();
        let inferred_idx = result.find("What I've inferred").unwrap();
        let tentative_idx = result.find("Tentative —").unwrap();
        let directly_block = &result[directly_idx..inferred_idx];
        let inferred_block = &result[inferred_idx..tentative_idx];
        let tentative_block = &result[tentative_idx..];

        assert!(directly_block.contains("boundary directly"));
        assert!(inferred_block.contains("boundary inferred"));
        assert!(tentative_block.contains("tentative"));
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

    // ─── R3: Temporal-tension detection ───────────────────────

    use crate::error::Error;
    use crate::traits::InferenceProvider;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities, Speed};
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Minimal mock inference provider for the tension-detector
    /// tests. Returns whatever was preset; records the prompt the
    /// caller sent so the tests can pin the prompt shape.
    struct ScriptedInference {
        response_text: String,
        last_prompt: Mutex<Option<String>>,
    }

    impl ScriptedInference {
        fn new(response_text: &str) -> Self {
            Self {
                response_text: response_text.to_string(),
                last_prompt: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl InferenceProvider for ScriptedInference {
        async fn complete(
            &self,
            request: &CompletionRequest,
        ) -> Result<CompletionResponse> {
            *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
            Ok(CompletionResponse {
                text: self.response_text.clone(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "scripted".into(),
                latency_ms: 0,
                oicp_meta: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("ScriptedInference: streaming unused".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn relational_mem(
        id: &str,
        content: &str,
        confidence: f64,
        created_at: i64,
        source_conv: Option<&str>,
    ) -> Memory {
        Memory {
            id: id.to_string(),
            content: content.to_string(),
            source: "test".into(),
            confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: source_conv.map(|s| s.to_string()),
            source_skill_id: None,
        }
    }

    #[tokio::test]
    async fn detect_tensions_returns_empty_when_no_candidate_memories() {
        let infer = ScriptedInference::new("[]");
        let out = detect_temporal_tensions(&infer, "anything", &[]).await.unwrap();
        assert!(out.is_empty());
        // The provider must NOT have been called when there are no
        // candidates (zero-cost guarantee for casual chat).
        assert!(infer.last_prompt.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn detect_tensions_skips_low_confidence_memories() {
        // 0.6 < RELATIONAL_DIRECT_THRESHOLD (0.85) — should be filtered.
        let infer = ScriptedInference::new("[]");
        let mems = vec![relational_mem("a", "guess", 0.6, 0, None)];
        let out = detect_temporal_tensions(&infer, "anything", &mems).await.unwrap();
        assert!(out.is_empty());
        // No directly-stated candidates → no inference call.
        assert!(infer.last_prompt.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn detect_tensions_returns_only_tension_classifications() {
        // Three candidates; classifier marks only the middle one as tension.
        let infer = ScriptedInference::new(
            r#"[
                {"index": 0, "relation": "consistent"},
                {"index": 1, "relation": "tension"},
                {"index": 2, "relation": "neutral"}
            ]"#,
        );
        let mems = vec![
            relational_mem("m0", "I love my job", 0.95, 1_773_273_600, Some("c1")),
            relational_mem("m1", "I want to leave the job", 0.92, 1_773_273_600, Some("c2")),
            relational_mem("m2", "I cook on Sundays", 0.90, 0, None),
        ];
        let out = detect_temporal_tensions(&infer, "this is a place I want to grow", &mems)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].memory_id, "m1");
        assert_eq!(out[0].prior_content, "I want to leave the job");
        assert!(out[0].prior_has_source_conversation);
        // Excerpt is the user message, possibly truncated; here it's short.
        assert_eq!(out[0].current_excerpt, "this is a place I want to grow");
    }

    #[tokio::test]
    async fn detect_tensions_soft_fails_on_garbage_response() {
        // Model output that doesn't parse as JSON — must NOT error,
        // just return empty so the turn proceeds.
        let infer = ScriptedInference::new("I'm not sure what you mean.");
        let mems = vec![relational_mem("m", "I told you X", 0.95, 0, Some("c"))];
        let out = detect_temporal_tensions(&infer, "current", &mems).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn detect_tensions_handles_fenced_response() {
        let infer = ScriptedInference::new(
            "Here's the classification:\n```json\n[{\"index\": 0, \"relation\": \"tension\"}]\n```",
        );
        let mems = vec![relational_mem("m", "I told you X", 0.95, 0, Some("c"))];
        let out = detect_temporal_tensions(&infer, "current", &mems).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].memory_id, "m");
    }

    #[tokio::test]
    async fn detect_tensions_truncates_long_messages() {
        let infer = ScriptedInference::new(r#"[{"index": 0, "relation": "tension"}]"#);
        let mems = vec![relational_mem("m", "prior", 0.95, 0, None)];
        let long_message = "x".repeat(500);
        let out = detect_temporal_tensions(&infer, &long_message, &mems).await.unwrap();
        assert_eq!(out.len(), 1);
        // Excerpt cap is TENSION_EXCERPT_CHAR_CAP (240) + ellipsis.
        assert!(out[0].current_excerpt.chars().count() <= TENSION_EXCERPT_CHAR_CAP + 1);
        assert!(out[0].current_excerpt.ends_with('…'));
    }

    #[tokio::test]
    async fn detect_tensions_caps_at_max_candidates() {
        let infer = ScriptedInference::new(
            // Classifier asked to evaluate 5 (capped); we send 7.
            r#"[
                {"index": 0, "relation": "tension"},
                {"index": 1, "relation": "tension"},
                {"index": 2, "relation": "tension"},
                {"index": 3, "relation": "tension"},
                {"index": 4, "relation": "tension"}
            ]"#,
        );
        let mems: Vec<Memory> = (0..7)
            .map(|i| relational_mem(&format!("m{i}"), &format!("memory {i}"), 0.95, 0, None))
            .collect();
        let out = detect_temporal_tensions(&infer, "current", &mems).await.unwrap();
        // At most MAX_TENSION_CANDIDATES (5), regardless of memories supplied.
        assert!(out.len() <= MAX_TENSION_CANDIDATES);
    }
}
