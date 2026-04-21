use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::error::Result;
use crate::skills::SkillRegistry;
use crate::traits::{InferenceProvider, Router, StateStore};
use crate::types::*;

/// Classification result from the two-pass router.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub intent: Intent,
    pub confidence: f64,
}

/// Structured output from Pass 1.
#[derive(Debug, Default, serde::Deserialize)]
struct CoarseClassification {
    #[serde(default)]
    intent: String,
    #[serde(default)]
    confidence: f32,
}

/// Outcome of the SimpleQuery self-assessment gate.
#[derive(Debug)]
enum SelfAssessment {
    /// Answer directly from model weights — question is certain and not fact-specific.
    Confident,
    /// Check local corpora first — question involves specific names, lists, or statistics.
    Uncertain,
    /// Local corpus unlikely to help (current events, live data) — suggest web search.
    NeedsWebSearch,
}

const SELF_ASSESSMENT_PROMPT: &str = r#"You are about to answer this question from memory:

"{message}"

Installed knowledge sources: {corpus_list}

Before answering, assess your confidence honestly.

Ask yourself:
1. Does this question ask for a SPECIFIC LIST, ROSTER, or ENUMERATION of items?
   (squad members, episode list, ingredients, rankings)
2. Does this question ask for a SPECIFIC STATISTIC, RECORD, or DATE
   that has a single correct answer?
3. Might a reasonable person fact-check this answer?
4. Could one of the installed knowledge sources have a more accurate
   answer than your training data?

Respond with exactly ONE word:

CONFIDENT   — You are certain of the full, complete, accurate answer
              and it does not involve specific lists or statistics
              that might be wrong.

UNCERTAIN   — The question involves specific facts, lists, names, or
              statistics where you might be incomplete or wrong.
              A local knowledge source should be checked first.

WEB         — The question requires current information (today's news,
              live scores, current prices) that no local corpus
              could have.

Answer:"#;

/// Returns true when the message contains surface signals that the answer
/// involves specific enumerable facts — names, rosters, lists, statistics —
/// where weight-only answers commonly hallucinate.
fn has_enumerable_markers(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "who was", "who were", "who played", "who scored", "who won",
        "which players", "starting lineup", "starting 11", "starting xi",
        "full squad", "full cast", "full list", "list of",
        "how many", "what year", "what date", "when was", "when did",
        "how tall", "how far", "what is the population",
        "what was the score", "what were the results",
        "episodes in", "seasons of", "members of",
        "ingredients in", "steps to", "requirements for",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn parse_self_assessment(raw: &str) -> SelfAssessment {
    let upper = raw.trim().to_uppercase();
    if upper.contains("UNCERTAIN") {
        SelfAssessment::Uncertain
    } else if upper.contains("WEB") {
        SelfAssessment::NeedsWebSearch
    } else if upper.contains("CONFIDENT") {
        SelfAssessment::Confident
    } else {
        // Safe fallback: assume uncertain, prefer local search.
        SelfAssessment::Uncertain
    }
}

/// LLM-based router that uses the Fast inference slot to classify messages.
///
/// Uses a two-pass approach for reliability:
/// - Pass 1: Coarse binary — needs tools? needs deep reasoning?
/// - Pass 2: Refine within the chosen branch to a specific Intent.
///
/// Each pass is a simple, focused question that a 1-3B model can answer reliably.
pub struct LlmRouter {
    inference: Arc<dyn InferenceProvider>,
    store: Arc<dyn StateStore>,
    skills: Arc<SkillRegistry>,
}

impl LlmRouter {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        store: Arc<dyn StateStore>,
        skills: Arc<SkillRegistry>,
    ) -> Self {
        Self {
            inference,
            store,
            skills,
        }
    }

    /// Pass 1: Coarse classification into one of three buckets.
    /// Each is a simple yes/no-like question the small model handles well.
    fn build_pass1_prompt(
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
        corrections: &[RoutingCorrection],
        routing_hints: &crate::skills::MergedRoutingHints,
    ) -> String {
        let context_str = Self::format_context_summary(context);
        let has_tools = !available_tools.is_empty();

        let corrections_note = if corrections.is_empty() {
            String::new()
        } else {
            let examples: String = corrections
                .iter()
                .take(3)
                .map(|c| format!("- A message was wrongly classified as {}", c.classified_as))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "\n\nPrevious classification mistakes (avoid these):\n{examples}"
            )
        };

        let skill_hints = if routing_hints.trigger_phrases.is_empty() {
            String::new()
        } else {
            let phrases: String = routing_hints
                .trigger_phrases
                .iter()
                .map(|(phrase, _)| format!("\"{phrase}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\n\nActive skill hints: If the message relates to {phrases}, prefer ACTION (C)."
            )
        };

        let corpus_list = context.installed_corpora_display();
        let tool_list = if has_tools {
            available_tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "none".to_string()
        };
        let lookup_note = if context.installed_corpora.is_empty() {
            "  Note: no local knowledge sources are installed, so LOOKUP is less useful."
        } else {
            ""
        };

        format!(
            r#"Classify this message into exactly ONE category.

Installed knowledge sources: {corpus_list}
Other available tools: {tool_list}

Categories:

SIMPLE
  Answerable from general world knowledge without needing to look
  anything up. Pure reasoning, math, definitions, logic puzzles,
  things that have exactly one universally known answer.
  NOTE: contested philosophical, ethical, or metaphysical topics
  (free will, consciousness, moral realism, God's existence,
  political philosophy) are NOT SIMPLE — use REASONING.
  Examples: "What is 12 × 14?", "What does 'ephemeral' mean?",
            "If all A are B and all B are C, are all A C?"

LOOKUP
  A factual question where a specific, correct answer EXISTS and
  could plausibly be wrong if answered from memory alone.
  Includes: names, dates, statistics, records, lists, lineups,
  specific events, anything that changes over time.
  When installed knowledge sources are available, prefer LOOKUP
  over SIMPLE for ANY factual question involving specific details.
  When in doubt between SIMPLE and LOOKUP: choose LOOKUP.{lookup_note}
  Examples: "Who was in the Arsenal Invincibles squad?",
            "What year was the Eiffel Tower built?",
            "How many episodes are in Breaking Bad season 3?"

REASONING
  Analysis, synthesis, comparison, creative work, or multi-step
  thinking where no single lookup would answer it.
  Examples: "Compare Wenger's 4-4-2 to his 4-2-3-1",
            "Write a short poem about autumn",
            "Explain why inflation causes interest rate rises"

ACTION
  Requires a tool with external reach or side-effects: web search,
  email, calendar, file system, shell, or MCP tools.
  Only use ACTION when no installed knowledge source could answer
  the question — web search costs money per call.
  IMPORTANT: Questions about current events, today's news, live
  prices/scores, or anything time-sensitive are ACTION.
  Examples: "Search the web for today's Arsenal news",
            "Send an email to my team",
            "What time is it in Tokyo right now?"
  NOT ACTION: processing content that's already in the prompt or
  conversation is REASONING. "Summarize this", "Explain this passage",
  "Paraphrase the excerpt", "Compare these sections" are REASONING,
  not ACTION, even though they use imperative verbs.

Conversation context: {context_str}
User message: "{message}"{corrections_note}{skill_hints}

Respond with JSON only:
{{"intent": "SIMPLE|LOOKUP|REASONING|ACTION", "confidence": 0.0}}"#,
        )
    }

    /// Pass 2: Refine within the ACTION branch — is it a single tool call or a multi-step plan?
    fn build_pass2_action_prompt(
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> String {
        let context_str = Self::format_context_summary(context);
        let tools_str = available_tools
            .iter()
            .map(|t| format!("{}: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"The user wants to perform an action. Is this a single tool call or a multi-step task?

A) SINGLE — One tool call can handle this (e.g., search the web, read a file)
B) MULTI — This needs multiple steps or planning (e.g., research + summarize + email)
C) KNOWLEDGE — The user is asking about their own documents/files

Conversation context: {context_str}
Available tools:
{tools_str}
User message: "{message}"

Reply with ONLY the letter: A, B, or C"#
        )
    }

    /// Build a summary of conversation context for the classification prompt.
    /// Includes working memory (current goal, facts) and recent messages.
    fn format_context_summary(context: &ConversationContext) -> String {
        let mut parts = Vec::new();

        // Include working memory if available — this gives the Router
        // visibility into the conversational arc, not just the last 2 messages.
        if let Some(wm) = &context.working_memory {
            if let Some(goal) = &wm.current_goal {
                parts.push(format!("Current goal: {goal}"));
            }
            if !wm.facts.is_empty() {
                let facts = wm.facts.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
                parts.push(format!("Known facts: {facts}"));
            }
        }

        // Recent messages (last 3 for slightly better context than 2).
        let recent: Vec<String> = context
            .conversation
            .messages
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                };
                format!("{role}: {}", &m.content[..m.content.len().min(150)])
            })
            .collect();

        if !recent.is_empty() {
            parts.push(format!("Recent messages:\n{}", recent.join("\n")));
        }

        if parts.is_empty() {
            "None".to_string()
        } else {
            parts.join("\n")
        }
    }

    /// Heuristic check: does this message require deep reasoning?
    ///
    /// Small fast models (0.5B–3B) are unreliable at distinguishing "is free will
    /// compatible with determinism?" (DeepQuery) from "what is free will?" (SimpleQuery).
    /// This function catches the obvious cases so the LLM only handles genuinely
    /// ambiguous ones.
    fn needs_deep_reasoning(message: &str) -> bool {
        let lower = message.to_lowercase();

        // Explicit analysis/comparison directives — unambiguous DeepQuery signals.
        let analysis_markers = [
            "compare", "contrast", "analyze", "analyse",
            "explain how", "explain why", "explain the difference",
            "what are the arguments", "what are the implications",
            "evaluate", "critically", "assess",
            "discuss", "debate",
            "reconcile", "how does", "why does", "in what ways",
            "pros and cons", "advantages and disadvantages",
            "relationship between", "difference between",
            "summarize the", "summarise the",
            "history of", "overview of", "evolution of",
            "how have", "how has",
        ];

        // Complex conceptual domains where even short questions require reasoning.
        let complex_domains = [
            "free will", "determinism", "compatibilism", "incompatibilism",
            "consciousness", "qualia", "hard problem",
            "epistemology", "ontology", "metaphysics", "phenomenology",
            "moral realism", "ethics", "morality", "normative",
            "political philosophy", "social contract", "justice",
            "dialectic", "existentialism", "absurdism",
            "artificial general intelligence", "alignment problem",
            "philosophy of mind", "philosophy of language",
            "emergence", "supervenience", "reduction",
        ];

        // Compatibility/tension questions — always require reasoning regardless of domain.
        let tension_markers = [
            "compatible", "incompatible",
            "consistent with", "inconsistent with",
            "reconcile", "tension between",
            "can both", "are both",
        ];

        let word_count = message.split_whitespace().count();

        // Explicit analysis directive → always deep.
        if analysis_markers.iter().any(|m| lower.contains(m)) {
            return true;
        }

        // Compatibility/tension question on any subject → always deep.
        if tension_markers.iter().any(|m| lower.contains(m)) {
            return true;
        }

        // Complex philosophical/technical domain + non-trivial question length → deep.
        // (Excludes "what is X?" which is short and definitional.)
        if complex_domains.iter().any(|d| lower.contains(d)) && word_count > 5 {
            return true;
        }

        false
    }

    /// Heuristic check: does this message likely need current/real-time information?
    /// This catches cases that small models miss in classification.
    fn needs_current_info(message: &str) -> bool {
        let lower = message.to_lowercase();

        // Recent/current year references (2024-2030 covers the near window).
        let has_recent_year = (2024..=2030).any(|y| lower.contains(&y.to_string()));

        // Temporal keywords that suggest the answer changes over time.
        let temporal_keywords = [
            "latest",
            "recent",
            "current",
            "today",
            "yesterday",
            "this week",
            "this month",
            "this year",
            "right now",
            "just happened",
            "breaking",
            "news",
            "score",
            "price",
            "stock",
            "weather",
            "who won",
            "who is winning",
            "election",
            "results",
        ];
        let has_temporal = temporal_keywords.iter().any(|kw| lower.contains(kw));

        // Search-imperative phrases.
        let search_keywords = [
            "search for",
            "look up",
            "find out",
            "google",
            "search the web",
            "web search",
        ];
        let has_search_request = search_keywords.iter().any(|kw| lower.contains(kw));

        has_recent_year || has_temporal || has_search_request
    }

    /// Heuristic check: is this message asking the model to *process* content
    /// it already has (summarize, explain, compare, paraphrase, etc.) rather
    /// than reach outside to fetch or mutate something?
    ///
    /// Small Fast-slot models occasionally latch onto imperative verbs like
    /// "summarize this document" and classify them as ACTION (shell/file-system
    /// category) with high confidence. This pre-check short-circuits that:
    /// these are reasoning operations, full stop.
    fn looks_like_content_processing(message: &str) -> bool {
        let lower = message.to_lowercase();

        // Verb phrases that signal the user wants the model to operate on
        // content in the prompt or the conversation, not reach outside.
        //
        // A trailing space or punctuation in the pattern forces a word
        // boundary on the right — `describe ` doesn't match `described`,
        // `explain ` doesn't match `explainer`.
        const CONTENT_VERBS: &[&str] = &[
            "summarize",
            "summarise",
            "summary of",
            "paraphrase",
            "rephrase",
            "explain ",
            "explain the ",
            "explain this ",
            "describe ",
            "analyse",
            "analyze",
            "compare ",
            "contrast ",
            "critique ",
            "interpret ",
            "outline ",
            "elaborate",
        ];

        CONTENT_VERBS.iter().any(|v| lower.contains(v))
    }

    /// Call the fast model with a classification prompt.
    async fn classify_call(&self, prompt: String) -> Result<String> {
        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a message classifier. Respond with exactly one letter.".to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(5),
            temperature: Some(0.0),
            structured_output: None,
            think_budget: Some(0),  // suppress thinking — prevents Qwen <think> consuming the 5-token budget
            top_k: None,
            top_p: None,
            oicp: None,
                tools: None,
                tool_choice: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] classify raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Call the fast model for a JSON-output classification prompt (Pass 1 + self-assessment).
    async fn classify_call_json(&self, prompt: String, max_tokens: usize) -> Result<String> {
        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a message classifier. Respond with valid JSON only.".to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(max_tokens),
            temperature: Some(0.0),
            structured_output: None,
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
                tools: None,
                tool_choice: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] classify_json raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Refine the ACTION coarse classification via Pass 2.
    async fn pass2_refine(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Intent> {
        if available_tools.is_empty() {
            return Ok(Intent::ComplexTask);
        }
        let pass2_prompt = Self::build_pass2_action_prompt(message, context, available_tools);
        let pass2_response = self.classify_call(pass2_prompt).await?;
        let refined = Self::parse_letter(&pass2_response);
        Ok(match refined {
            'A' => {
                let tool = available_tools
                    .first()
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                Intent::SimpleAction { tool }
            }
            'C' => Intent::KnowledgeQuery,
            _ => Intent::ComplexTask,
        })
    }

    /// Called when Pass 1 returns SIMPLE. Runs a fast self-assessment to decide
    /// whether to answer directly from weights or escalate to KnowledgeQuery.
    async fn assess_simple_query(
        &self,
        message: &str,
        context: &ConversationContext,
        confidence: f32,
    ) -> Result<(Intent, Option<String>)> {
        // Fast path: high-confidence, no enumerable-fact markers → commit to SimpleQuery.
        if confidence >= 0.92 && !has_enumerable_markers(message) {
            return Ok((Intent::SimpleQuery, None));
        }

        // Slow path: run a self-assessment on the Fast slot (~100ms extra latency).
        let assessment = self.self_assess(message, context).await?;
        let label = format!("{assessment:?}");
        let intent = match assessment {
            SelfAssessment::Confident => Intent::SimpleQuery,
            SelfAssessment::Uncertain => Intent::KnowledgeQuery,
            SelfAssessment::NeedsWebSearch => Intent::SimpleAction {
                tool: ToolId::from("web_search"),
            },
        };
        Ok((intent, Some(label)))
    }

    async fn self_assess(
        &self,
        message: &str,
        context: &ConversationContext,
    ) -> Result<SelfAssessment> {
        let corpus_list = context.installed_corpora_display();
        let prompt = SELF_ASSESSMENT_PROMPT
            .replace("{message}", message)
            .replace("{corpus_list}", &corpus_list);
        let raw = self.classify_call_json(prompt, 10).await?;
        Ok(parse_self_assessment(&raw))
    }

    /// Parse a JSON coarse-classification response: `{"intent": "SIMPLE|...", "confidence": 0.9}`.
    fn parse_coarse(raw: &str) -> CoarseClassification {
        // Strip <think>...</think> blocks that Qwen3 emits even with think_budget=0.
        let after_think = if let (Some(start), Some(end)) = (raw.find("<think>"), raw.find("</think>")) {
            if end > start {
                &raw[end + "</think>".len()..]
            } else {
                raw
            }
        } else {
            raw
        };
        // Strip markdown code fences if present.
        let cleaned = after_think
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        serde_json::from_str(cleaned).unwrap_or_default()
    }

    /// Parse a letter response (A/B/C) from the model.
    /// Looks for a standalone A, B, or C — first as a single-char token,
    /// then as the first character of the response.
    fn parse_letter(response: &str) -> char {
        let cleaned = response.trim().to_uppercase();

        // Try to find a standalone letter (surrounded by non-alpha or at boundaries).
        for token in cleaned.split(|c: char| !c.is_alphabetic()) {
            if token.len() == 1 {
                let ch = token.chars().next().unwrap();
                if matches!(ch, 'A' | 'B' | 'C') {
                    return ch;
                }
            }
        }

        // Fallback: first character.
        cleaned
            .chars()
            .find(|c| matches!(c, 'A' | 'B' | 'C'))
            .unwrap_or('A')
    }

    /// Legacy single-word parser (kept for compatibility with simpler prompts).
    pub fn parse_intent(response: &str, available_tools: &[ToolDescriptor]) -> Intent {
        let cleaned = response.trim().to_lowercase();
        let category = cleaned
            .split(|c: char| !c.is_alphabetic())
            .find(|w| {
                matches!(
                    *w,
                    "simple"
                        | "deep"
                        | "knowledge"
                        | "action"
                        | "complex"
                        | "continuation"
                )
            })
            .unwrap_or("simple");

        match category {
            "deep" => Intent::DeepQuery,
            "knowledge" => Intent::KnowledgeQuery,
            "action" => {
                let tool = available_tools
                    .first()
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                Intent::SimpleAction { tool }
            }
            "complex" => Intent::ComplexTask,
            "continuation" => Intent::ComplexTask,
            _ => Intent::SimpleQuery,
        }
    }

    /// Check whether the conversation's topic context suggests a routing override.
    ///
    /// Returns `Some(Intent)` when the topic context is strong enough to override
    /// the normal two-pass classification. This prevents general knowledge questions
    /// in an established conversation from being routed to corpus retrieval that
    /// will find nothing and refuse.
    ///
    /// The key insight: after 2+ turns on a topic, a follow-up question that doesn't
    /// reference the anchored document's specific content is likely a general knowledge
    /// question that should be answered directly (SimpleQuery or DeepQuery), not
    /// sent through KnowledgeQuery retrieval.
    fn check_topic_continuity(message: &str, context: &ConversationContext) -> Option<Intent> {
        let tc = context.topic_context.as_ref()?;

        // Need at least 2 turns of established context for an override.
        if tc.turn_depth < 2 {
            return None;
        }

        let msg_lower = message.to_lowercase();

        // If there's an anchored document and the message references it
        // specifically (uses the filename, "the document", "chapter", "page"),
        // let normal routing handle it — it's a document query.
        if tc.anchored_source.is_some() {
            let doc_reference_patterns = [
                "the document", "the paper", "the article", "the book",
                "chapter", "page", "paragraph", "section",
                "the author writes", "according to the text",
            ];
            if doc_reference_patterns.iter().any(|p| msg_lower.contains(p)) {
                return None;
            }
        }

        // Detect general knowledge follow-ups: questions that are about the
        // broader domain but not about the specific document content.
        // These use domain terms but not document-specific references.
        let general_knowledge_signals = [
            // Question words + broad domain terms suggest general knowledge.
            "what are the", "what is the", "how does", "how do",
            "core differences", "main differences", "key differences",
            "compare", "contrast", "relationship between",
            "explain", "define", "describe",
        ];

        let is_general = general_knowledge_signals
            .iter()
            .any(|p| msg_lower.contains(p));

        // Pronoun-heavy short follow-ups in an established conversation
        // are likely continuations that can be answered from general knowledge.
        let pronoun_patterns = ["he ", "she ", "they ", "it ", "his ", "her ", "their ", "that "];
        let has_pronouns = pronoun_patterns.iter().any(|p| msg_lower.starts_with(p));
        let is_short = message.split_whitespace().count() <= 12;

        if is_general || (has_pronouns && is_short) {
            // Determine whether this needs deep reasoning or a simple answer.
            if Self::needs_deep_reasoning(message) {
                tracing::info!(
                    topic = ?tc.topic,
                    turn_depth = tc.turn_depth,
                    "Topic continuity override → DeepQuery (general knowledge follow-up)"
                );
                Some(Intent::DeepQuery)
            } else {
                tracing::info!(
                    topic = ?tc.topic,
                    turn_depth = tc.turn_depth,
                    "Topic continuity override → SimpleQuery (general knowledge follow-up)"
                );
                Some(Intent::SimpleQuery)
            }
        } else {
            None
        }
    }
}

#[async_trait]
impl Router for LlmRouter {
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<RoutingOutcome> {
        let start = Instant::now();

        // Fetch recent routing corrections for few-shot self-correction.
        let corrections = self
            .store
            .get_routing_corrections(3)
            .await
            .unwrap_or_default();

        // Get active skill routing hints.
        let routing_hints = self.skills.routing_hints();

        // Pre-check 0: topic continuity — if the conversation has established
        // context (2+ turns on a topic), check whether this message is a general
        // knowledge follow-up that should bypass corpus retrieval.
        if let Some(override_intent) = Self::check_topic_continuity(message, context) {
            let latency_ms = start.elapsed().as_millis() as i64;
            let mut hasher = DefaultHasher::new();
            message.hash(&mut hasher);
            let hash = format!("{:x}", hasher.finish());
            let intent_str = format!("{override_intent:?}");
            let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
            let _ = self.store.log_routing_meta(&hash, "TOPIC_CONTINUITY", None).await;

            eprintln!(
                "[router] \"{}\" → {:?} (topic continuity override)",
                &message[..message.len().min(50)],
                override_intent,
            );

            return Ok(RoutingOutcome {
                intent: override_intent,
                coarse_intent: Some("TOPIC_CONTINUITY".to_string()),
                self_assessment: None,
            });
        }

        // Pre-check 1: temporal/current-info → force ACTION (search).
        // Small models are unreliable at detecting these.
        let has_search = available_tools.iter().any(|t| t.name.contains("search"));
        let force_action = has_search && Self::needs_current_info(message);

        // Pre-check 2: content-processing signal → force REASONING. Catches
        // "summarize this", "explain this passage", "compare these sections"
        // etc. which the Fast model sometimes misreads as ACTION because of
        // the imperative verb. Content processing never needs external reach.
        let force_content_reasoning = !force_action
            && Self::looks_like_content_processing(message);

        // Pre-check 3: deep reasoning signal → force REASONING before the LLM sees it.
        // This catches philosophical, analytical, and compatibility questions that
        // small fast models frequently mis-classify as SimpleQuery.
        let force_deep = !force_action
            && !force_content_reasoning
            && Self::needs_deep_reasoning(message);

        // Pass 1: Coarse classification (skipped for pre-checked cases).
        let coarse = if force_action {
            CoarseClassification { intent: "ACTION".to_string(), confidence: 1.0 }
        } else if force_content_reasoning {
            CoarseClassification { intent: "REASONING".to_string(), confidence: 1.0 }
        } else if force_deep {
            CoarseClassification { intent: "REASONING".to_string(), confidence: 1.0 }
        } else {
            let pass1_prompt = Self::build_pass1_prompt(
                message,
                context,
                available_tools,
                &corrections,
                &routing_hints,
            );
            let pass1_response = self.classify_call_json(pass1_prompt, 40).await?;
            Self::parse_coarse(&pass1_response)
        };

        let (intent, self_assessment_outcome) = match coarse.intent.as_str() {
            "LOOKUP" => (Intent::KnowledgeQuery, None),
            "REASONING" => (Intent::DeepQuery, None),
            "ACTION" => (self.pass2_refine(message, context, available_tools).await?, None),
            "SIMPLE" => {
                self.assess_simple_query(message, context, coarse.confidence).await?
            }
            _ => {
                // Parse failure or unknown intent — default to local search (never confabulate).
                tracing::warn!(
                    raw = %coarse.intent,
                    "Router Pass 1 parse failed; defaulting to KnowledgeQuery"
                );
                (Intent::KnowledgeQuery, None)
            }
        };

        let latency_ms = start.elapsed().as_millis() as i64;

        // Log routing decision.
        let mut hasher = DefaultHasher::new();
        message.hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());
        let intent_str = format!("{intent:?}");
        let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
        let _ = self.store.log_routing_meta(
            &hash,
            &coarse.intent,
            self_assessment_outcome.as_deref(),
        ).await;

        eprintln!(
            "[router] \"{}\" → {:?} (coarse={}, confidence={:.2})",
            &message[..message.len().min(50)],
            intent,
            coarse.intent,
            coarse.confidence,
        );

        Ok(RoutingOutcome {
            intent,
            coarse_intent: Some(coarse.intent),
            self_assessment: self_assessment_outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_letter_extracts_correctly() {
        assert_eq!(LlmRouter::parse_letter("A"), 'A');
        assert_eq!(LlmRouter::parse_letter("B\n"), 'B');
        assert_eq!(LlmRouter::parse_letter("  c  "), 'C');
        assert_eq!(LlmRouter::parse_letter("The answer is B."), 'B');
        assert_eq!(LlmRouter::parse_letter("garbage"), 'A'); // default
    }

    #[test]
    fn parse_intent_simple() {
        assert!(matches!(
            LlmRouter::parse_intent("simple", &[]),
            Intent::SimpleQuery
        ));
    }

    #[test]
    fn parse_intent_deep() {
        assert!(matches!(
            LlmRouter::parse_intent("deep", &[]),
            Intent::DeepQuery
        ));
    }

    #[test]
    fn parse_intent_knowledge() {
        assert!(matches!(
            LlmRouter::parse_intent("knowledge", &[]),
            Intent::KnowledgeQuery
        ));
    }

    #[test]
    fn parse_intent_complex() {
        assert!(matches!(
            LlmRouter::parse_intent("complex", &[]),
            Intent::ComplexTask
        ));
    }

    #[test]
    fn parse_intent_action_with_tools() {
        let tools = vec![ToolDescriptor {
            id: "web_search".to_string(),
            name: "web_search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: crate::types::Effect::Read,
            idempotency: crate::types::Idempotency::Idempotent,
            latency: crate::types::Latency::Slow,
            scope: crate::types::Scope::External,
            output_schema: None,
        }];
        if let Intent::SimpleAction { tool } = LlmRouter::parse_intent("action", &tools) {
            assert_eq!(tool, "web_search");
        } else {
            panic!("Expected SimpleAction");
        }
    }

    #[test]
    fn parse_intent_garbage_defaults_to_simple() {
        assert!(matches!(
            LlmRouter::parse_intent("asdfghjkl", &[]),
            Intent::SimpleQuery
        ));
        assert!(matches!(
            LlmRouter::parse_intent("", &[]),
            Intent::SimpleQuery
        ));
    }

    #[test]
    fn context_summary_with_working_memory() {
        let ctx = ConversationContext {
            conversation: Conversation {
                id: "c1".to_string(),
                title: None,
                messages: vec![],
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
            },
            memories: vec![],
            working_memory: Some(WorkingMemory {
                current_goal: Some("researching EU AI Act".to_string()),
                facts: vec!["User is a policy analyst".to_string()],
                active_documents: vec![],
            }),
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
        };

        let summary = LlmRouter::format_context_summary(&ctx);
        assert!(summary.contains("researching EU AI Act"));
        assert!(summary.contains("policy analyst"));
    }

    #[test]
    fn context_summary_without_working_memory() {
        let ctx = ConversationContext {
            conversation: Conversation {
                id: "c1".to_string(),
                title: None,
                messages: vec![Message {
                    id: "m1".to_string(),
                    conversation_id: "c1".to_string(),
                    role: Role::User,
                    content: "Hello there".to_string(),
                    created_at: 0,
                    metadata: None,
                    version: 0,
                }],
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
        };

        let summary = LlmRouter::format_context_summary(&ctx);
        assert!(summary.contains("user: Hello there"));
    }

    #[test]
    fn needs_current_info_recent_year() {
        assert!(LlmRouter::needs_current_info("Who won the Nobel Prize in 2025?"));
        assert!(LlmRouter::needs_current_info("What happened in 2024?"));
        assert!(!LlmRouter::needs_current_info("What happened in 1969?"));
    }

    #[test]
    fn needs_current_info_temporal_keywords() {
        assert!(LlmRouter::needs_current_info("What is the latest news?"));
        assert!(LlmRouter::needs_current_info("What's the current price of Bitcoin?"));
        assert!(LlmRouter::needs_current_info("Who won the game today?"));
        assert!(LlmRouter::needs_current_info("What's the weather like?"));
        assert!(LlmRouter::needs_current_info("Who won the election?"));
    }

    #[test]
    fn needs_current_info_search_requests() {
        assert!(LlmRouter::needs_current_info("Search for restaurants near me"));
        assert!(LlmRouter::needs_current_info("Can you look up flight prices?"));
        assert!(LlmRouter::needs_current_info("Google the EU AI Act"));
    }

    #[test]
    fn needs_current_info_false_for_general() {
        assert!(!LlmRouter::needs_current_info("What is recursion?"));
        assert!(!LlmRouter::needs_current_info("Explain photosynthesis"));
        assert!(!LlmRouter::needs_current_info("Hello, how are you?"));
        assert!(!LlmRouter::needs_current_info("Write a poem about the ocean"));
    }

    // ── looks_like_content_processing ───────────────────────────

    #[test]
    fn content_processing_catches_summarize_variants() {
        assert!(LlmRouter::looks_like_content_processing(
            "Can you summarize this document?"
        ));
        assert!(LlmRouter::looks_like_content_processing(
            "SUMMARISE the argument"
        ));
        assert!(LlmRouter::looks_like_content_processing(
            "Give me a summary of this paper"
        ));
    }

    #[test]
    fn content_processing_catches_explain_and_analyse() {
        assert!(LlmRouter::looks_like_content_processing(
            "Explain this code snippet"
        ));
        assert!(LlmRouter::looks_like_content_processing(
            "Analyse these passages and tell me what stands out"
        ));
        assert!(LlmRouter::looks_like_content_processing(
            "compare contrast these two sections"
        ));
        assert!(LlmRouter::looks_like_content_processing(
            "paraphrase the opening paragraph"
        ));
    }

    #[test]
    fn content_processing_rejects_action_verbs() {
        assert!(!LlmRouter::looks_like_content_processing(
            "run my linter on these files"
        ));
        assert!(!LlmRouter::looks_like_content_processing(
            "send an email to my team"
        ));
        assert!(!LlmRouter::looks_like_content_processing(
            "search the web for today's Arsenal news"
        ));
        assert!(!LlmRouter::looks_like_content_processing(
            "what is the capital of France"
        ));
    }

    // ── has_enumerable_markers ──────────────────────────────────

    #[test]
    fn enumerable_markers_arsenal_invincibles() {
        assert!(has_enumerable_markers(
            "Who was in the starting 11 for the Arsenal Invincibles?"
        ));
    }

    #[test]
    fn enumerable_markers_math_question() {
        assert!(!has_enumerable_markers("What is 12 × 14?"));
    }

    #[test]
    fn enumerable_markers_definition() {
        assert!(!has_enumerable_markers("What does ephemeral mean?"));
    }

    #[test]
    fn enumerable_markers_various_positives() {
        assert!(has_enumerable_markers("Who won the Premier League in 2004?"));
        assert!(has_enumerable_markers("List of countries in the EU"));
        assert!(has_enumerable_markers("How many seasons of Breaking Bad are there?"));
        assert!(has_enumerable_markers("What year was the Eiffel Tower built?"));
        assert!(has_enumerable_markers("Members of the Beatles"));
    }

    // ── parse_coarse ────────────────────────────────────────────

    #[test]
    fn parse_coarse_valid_json() {
        let c = LlmRouter::parse_coarse(r#"{"intent":"LOOKUP","confidence":0.9}"#);
        assert_eq!(c.intent, "LOOKUP");
        assert!((c.confidence - 0.9).abs() < 0.01);
    }

    #[test]
    fn parse_coarse_with_markdown_fences() {
        let c = LlmRouter::parse_coarse("```json\n{\"intent\":\"SIMPLE\",\"confidence\":0.95}\n```");
        assert_eq!(c.intent, "SIMPLE");
    }

    #[test]
    fn parse_coarse_garbage_returns_default() {
        let c = LlmRouter::parse_coarse("I cannot classify this message.");
        assert_eq!(c.intent, "");
        assert_eq!(c.confidence, 0.0);
    }

    // ── parse_self_assessment ───────────────────────────────────

    #[test]
    fn parse_self_assessment_uncertain() {
        assert!(matches!(parse_self_assessment("UNCERTAIN"), SelfAssessment::Uncertain));
        assert!(matches!(parse_self_assessment("uncertain"), SelfAssessment::Uncertain));
    }

    #[test]
    fn parse_self_assessment_web() {
        assert!(matches!(parse_self_assessment("WEB"), SelfAssessment::NeedsWebSearch));
    }

    #[test]
    fn parse_self_assessment_confident() {
        assert!(matches!(parse_self_assessment("CONFIDENT"), SelfAssessment::Confident));
    }

    #[test]
    fn parse_self_assessment_garbage_defaults_to_uncertain() {
        // Safe fallback — prefer local search over confabulation.
        assert!(matches!(parse_self_assessment("???"), SelfAssessment::Uncertain));
        assert!(matches!(parse_self_assessment(""), SelfAssessment::Uncertain));
    }
}
