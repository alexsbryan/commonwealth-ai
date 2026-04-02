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

        format!(
            r#"You are a message classifier. Given a user message, classify it into ONE category.

Categories:
A) SIMPLE — Can be answered directly from general knowledge in a sentence or two (greetings, basic facts, brief questions)
B) REASONING — Requires analysis, explanation, comparison, creative work, or detailed thought
C) ACTION — Requires doing something: searching the web, reading files, sending email, running code, or any multi-step task. IMPORTANT: Questions about recent events, current news, today's information, specific dates/years, prices, scores, or anything that may have changed since training data was collected are ACTION — they require a web search{tools_note}

Conversation context: {context_str}
User message: "{message}"{corrections_note}{skill_hints}

Reply with ONLY the letter: A, B, or C"#,
            tools_note = if has_tools {
                format!(
                    "\n   Available tools: {}",
                    available_tools
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            },
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
            oicp: None,
        };
        let response = self.inference.complete(&request).await?;
        Ok(response.text)
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
}

#[async_trait]
impl Router for LlmRouter {
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Intent> {
        let start = Instant::now();

        // Fetch recent routing corrections for few-shot self-correction.
        let corrections = self
            .store
            .get_routing_corrections(3)
            .await
            .unwrap_or_default();

        // Get active skill routing hints.
        let routing_hints = self.skills.routing_hints();

        // Pre-check: if the message needs current information and we have
        // a search tool, skip LLM classification and go straight to ACTION.
        // Small models (0.5B-3B) are unreliable at detecting temporal queries.
        let has_search = available_tools.iter().any(|t| t.name.contains("search"));
        let force_action = has_search && Self::needs_current_info(message);

        // Pass 1: Coarse classification (skipped if force_action).
        let coarse = if force_action {
            'C'
        } else {
            let pass1_prompt = Self::build_pass1_prompt(
                message,
                context,
                available_tools,
                &corrections,
                &routing_hints,
            );
            let pass1_response = self.classify_call(pass1_prompt).await?;
            Self::parse_letter(&pass1_response)
        };

        let intent = match coarse {
            'A' => Intent::SimpleQuery,
            'B' => Intent::DeepQuery,
            'C' => {
                // Pass 2: Refine the ACTION branch.
                if available_tools.is_empty() {
                    Intent::ComplexTask
                } else {
                    let pass2_prompt =
                        Self::build_pass2_action_prompt(message, context, available_tools);
                    let pass2_response = self.classify_call(pass2_prompt).await?;
                    let refined = Self::parse_letter(&pass2_response);
                    match refined {
                        'A' => {
                            let tool = available_tools
                                .first()
                                .map(|t| t.id.clone())
                                .unwrap_or_default();
                            Intent::SimpleAction { tool }
                        }
                        'C' => Intent::KnowledgeQuery,
                        _ => Intent::ComplexTask,
                    }
                }
            }
            _ => Intent::SimpleQuery,
        };

        let latency_ms = start.elapsed().as_millis() as i64;

        // Log routing decision.
        let mut hasher = DefaultHasher::new();
        message.hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());
        let intent_str = format!("{intent:?}");
        let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;

        eprintln!(
            "[router] \"{}\" → {:?} (pass1={coarse})",
            &message[..message.len().min(50)],
            intent,
        );

        Ok(intent)
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
            },
            memories: vec![],
            working_memory: Some(WorkingMemory {
                current_goal: Some("researching EU AI Act".to_string()),
                facts: vec!["User is a policy analyst".to_string()],
                active_documents: vec![],
            }),
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
                }],
                created_at: 0,
                updated_at: 0,
            },
            memories: vec![],
            working_memory: None,
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
}
