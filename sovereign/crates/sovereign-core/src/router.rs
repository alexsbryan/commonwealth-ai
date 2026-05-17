use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::error::Result;
use crate::router_embed::EmbedRouter;
use crate::skills::SkillRegistry;
use crate::traits::{InferenceProvider, Router, StateStore};
use crate::types::*;

/// Classification result from the two-pass router.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub intent: Intent,
    pub confidence: f64,
}

/// Structured output from Pass 1. `rationale` is optional — small
/// fast models don't always emit it cleanly, and a missing field
/// defaults to `None` rather than failing the whole parse. Missing
/// rationale is fine; broken JSON loses us classification entirely,
/// so `serde(default)` tolerance matters more than rationale fidelity.
#[derive(Debug, Default, serde::Deserialize)]
struct CoarseClassification {
    #[serde(default)]
    intent: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    rationale: Option<String>,
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

/// Compute the `routing_log.message_hash` for a given user input.
/// Stable across router + runtime so PR4 redirect-signal updates
/// can correlate to the row written by `log_routing`. Public so
/// integration tests can reproduce the hash without reaching into
/// implementation internals.
/// Inherit a prior knowledge-family intent when the conversation has
/// established an active knowledge thread. Returns `Some(intent)` to
/// short-circuit downstream classification when:
///
///   1. There is at least one prior assistant turn (the conversation
///      is past the first exchange), AND
///   2. That prior turn classified as a knowledge-family intent
///      (`KnowledgeQuery` / `DeepQuery` / `ComparisonQuery`) as
///      persisted in `metadata.provenance.intent` or
///      `metadata.intent`.
///
/// Purely structural — no lexical pattern matching on the current
/// message. The earlier `looks_like_anaphoric_followup` keyed off a
/// hand-maintained list of lead words ("Who"/"After"/"Going back to"
/// fell outside it, dropping marathon turns onto the NotImplemented
/// stall path). We replace that with the principle: if the prior
/// turn was a knowledge answer, the next turn under the same
/// conversation_id is part of the same knowledge thread until the
/// downstream classifier produces strong evidence otherwise. This
/// pre-check fires BEFORE the embed router, so the embed router's
/// high-confidence non-knowledge verdicts (which run downstream of
/// the personal-recall heuristic in the wider stack) never see the
/// case where they'd otherwise hijack the thread.
///
/// Personal-recall framings ("Have I mentioned X before?") are
/// handled by the upstream `looks_like_personal_recall` pre-check
/// (where present) — it fires earlier and short-circuits before this
/// function is reached.
///
/// Surfaced by sovereign/bench/wikipedia_learn 2026-05-17 marathon
/// (v9→v10).
fn inherits_prior_knowledge_intent(
    context: &ConversationContext,
) -> Option<Intent> {
    let prior_assistant = context
        .conversation
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)?;
    // Streaming KnowledgeQuery persists snake_case `metadata.intent`
    // + CamelCase `metadata.provenance.intent`; other intents
    // persist CamelCase in `metadata.intent` directly. Normalize.
    let metadata = prior_assistant.metadata.as_ref()?;
    let raw_intent = metadata
        .get("provenance")
        .and_then(|p| p.get("intent"))
        .and_then(|v| v.as_str())
        .or_else(|| metadata.get("intent").and_then(|v| v.as_str()))?;
    let normalized = raw_intent.to_lowercase().replace('_', "");
    match normalized.as_str() {
        "knowledgequery" => Some(Intent::KnowledgeQuery),
        "deepquery" => Some(Intent::DeepQuery),
        "comparisonquery" => Some(Intent::ComparisonQuery),
        _ => None,
    }
}

pub fn message_hash(message: &str) -> String {
    let mut hasher = DefaultHasher::new();
    message.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Emit up to three candidate alternative intents for the Ask move,
/// in ranked order. Pure function — no model call, no I/O. Target
/// cost under 15ms.
///
/// Rationale: small Fast-slot models fumble multi-option JSON output
/// (top-2 interpretations). A keyword heuristic is cheaper, more
/// reliable, and empirically good enough for the antifragile-routing
/// clarification UX — the user sees 2-3 clickable alternatives and
/// picks one, which is often enough to disambiguate "how does X
/// work" into "walk me through X" vs "show me X in the corpus" vs
/// "search the web for X".
///
/// Always excludes the primary intent so the UI doesn't show a
/// redundant "you meant this" chip. Availability-aware: never
/// suggests `SimpleAction { tool: "web_search" }` when the web
/// search tool isn't installed.
pub(crate) fn suggest_alternatives(
    message: &str,
    primary: &Intent,
    available_tools: &[ToolDescriptor],
) -> Vec<IntentCandidate> {
    let lower = message.to_lowercase();
    let mut out: Vec<IntentCandidate> = Vec::new();
    let has_web = available_tools
        .iter()
        .any(|t| t.name.contains("web_search") || t.name == "search");

    // Temporal / recency signal → offer web search.
    let wants_current = [
        "latest", "today", "current", "recent", "this week", "this month",
        "right now", "news", "price", "score", "weather",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if wants_current && has_web {
        if let Some(t) = available_tools
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            out.push(IntentCandidate {
                intent: Intent::SimpleAction {
                    tool: t.id.clone(),
                },
                confidence: 0.6,
            });
        }
    }

    // Deep-reasoning signal → DeepQuery.
    let wants_deep = [
        "how does", "explain", "walk me through", "compare", "contrast",
        "why does", "analyze", "analyse", "relationship between", "difference between",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if wants_deep {
        out.push(IntentCandidate {
            intent: Intent::DeepQuery,
            confidence: 0.55,
        });
    }

    // Corpus-lookup-y signal → KnowledgeQuery.
    let wants_lookup = [
        "according to", "in the", "from the", "the document", "chapter",
        "paper", "book", "find", "lookup", "look up",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if wants_lookup {
        out.push(IntentCandidate {
            intent: Intent::KnowledgeQuery,
            confidence: 0.5,
        });
    }

    // Definitional / short-factual → SimpleQuery.
    let wants_simple = lower.starts_with("what is ")
        || lower.starts_with("what does ")
        || lower.starts_with("define ")
        || lower.starts_with("meaning of ");
    if wants_simple {
        out.push(IntentCandidate {
            intent: Intent::SimpleQuery,
            confidence: 0.5,
        });
    }

    // Drop alternatives that match the primary (same discriminant) —
    // the redirect chip would be redundant otherwise.
    out.retain(|c| std::mem::discriminant(&c.intent) != std::mem::discriminant(primary));

    // Cap at 3 entries — more is UI noise. Order is preserved (most
    // confident signal first based on the above ordering).
    out.truncate(3);
    out
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
    /// Optional embedding-based intent classifier. When installed
    /// AND confident (top-similarity above threshold + sufficient
    /// margin), the embed router's verdict short-circuits the
    /// heuristic + LLM cascade. Ambiguous queries fall through to
    /// the existing path. Installed via `with_embed_router`.
    embed_router: Option<Arc<EmbedRouter>>,
    /// Optional binary classifier for the personal-vs-external scope
    /// axis. Independent of intent. When installed, called once per
    /// query (reusing the embed_router's query embedding when both
    /// are present) and the result populates
    /// `RouterClassification.scope`. Downstream retrieval
    /// (`prepare_knowledge_context` + `apply_atlas_grounding`) reads
    /// scope to restrict to user-owned corpora when the classifier
    /// fires `Some("personal")`.
    scope_classifier: Option<Arc<crate::scope_classifier::PersonalScopeClassifier>>,
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
            embed_router: None,
            scope_classifier: None,
        }
    }

    /// Install an `EmbedRouter` as the first pre-check. When the
    /// embed classifier returns a confident result the router skips
    /// every downstream heuristic + the LLM Pass 1/2 calls.
    pub fn with_embed_router(mut self, embed: Arc<EmbedRouter>) -> Self {
        self.embed_router = Some(embed);
        self
    }

    /// Install a personal-scope binary classifier. Called once per
    /// query (alongside the embed router when both are installed)
    /// and the result populates `RouterClassification.scope`.
    pub fn with_scope_classifier(
        mut self,
        classifier: Arc<crate::scope_classifier::PersonalScopeClassifier>,
    ) -> Self {
        self.scope_classifier = Some(classifier);
        self
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

═══ SITUATED CONTEXT (use for Gricean inference) ═══
{context_str}

Installed knowledge sources: {corpus_list}
Other available tools: {tool_list}

═══ INFERENCE INSTRUCTION ═══
Pick the category whose move-shape would make the user's message
*optimally relevant given the situated context above*. The
grammatical surface form does NOT pick the category — the user's
actual move does. Examples of surface ≠ intent:
  - "you there?" after a long-running task → PHATIC under DIRECTIVE,
    not LOOKUP. The user is checking presence, not asking a question.
  - "I'm stuck on this" while debugging → EXPRESSIVE with implicit
    help-request, not a SIMPLE statement of fact.
  - "shorter please" after a long answer → CONATION (act on prior
    turn), not a question about brevity.
Reason about what the user actually wants given that they said this
*now*, after seeing the assistant's last reply.

═══ Categories ═══

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
  A factual question where ONE specific, atomic answer exists and
  could plausibly be wrong if answered from memory alone. The full
  answer fits in a sentence or a short list; no aggregation across
  sources is required.
  Includes: names, dates, statistics, records, single-fact rosters,
  specific events, anything that changes over time.
  When installed knowledge sources are available, prefer LOOKUP
  over SIMPLE for atomic factual questions involving specific details.
  When in doubt between SIMPLE and LOOKUP: choose LOOKUP.{lookup_note}
  NOT LOOKUP: questions that ask for *causes*, *effects*, *reasons*,
  *contributions across multiple actors*, *how X drew on Y*, or *why
  X happened*. Those aggregate across sources and are REASONING. For
  *the differences between X and Y* / *compare X and Y*, use COMPARISON.
  Examples: "Who was in the Arsenal Invincibles squad?",
            "What year was the Eiffel Tower built?",
            "How many episodes are in Breaking Bad season 3?"

COMPARISON
  Two or more named entities (people, theories, technologies, places)
  being contrasted. The answer has a bounded structure: a handful of
  axes on which X and Y differ. Not an open-ended essay — fits in a
  short paragraph or three bullets.
  Signals: "difference between X and Y", "compare X and Y",
  "how does X differ from Y", "X vs Y".
  Examples: "What's the difference between TCP and UDP?",
            "How does a turbine engine differ from a piston engine?",
            "Compare cuneiform and hieroglyphic writing systems."

REASONING
  Open-ended analysis, synthesis, creative work, or multi-step
  thinking where no single lookup would answer it. A REASONING
  answer aggregates evidence across multiple facts/sources without
  the bounded contrast shape of COMPARISON.
  Includes: causes/effects/reasons questions, multi-aspect "how did
  X relate to Y" / "how did X draw on Y" questions, multi-actor
  attributions, anything that asks for the structure or logic behind
  a sequence of facts rather than a single fact.
  Examples: "Write a short poem about autumn",
            "Explain why inflation causes interest rate rises",
            "What were the main causes of the 2008 financial crisis?",
            "How did Roman trade networks shape early Christian spread?"

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

CONATION
  Imperative command directed at *the assistant about its last reply*.
  Short (typically 1-4 words), no question mark, no proper nouns,
  references the assistant's prior turn — not the world. The user is
  asking the system to *transform* what was just produced.
  Examples: "Stop", "Try again", "Shorter please",
            "Skip the boilerplate", "Walk me through it slower"
  NOT CONATION: questions about whether to stop something in the
  world ("should I stop the migration?") are ACTION or REASONING.

COMMISSION
  User committing to a future action, or asking the assistant to
  remember something. First-person future framing ("I'll", "I'm
  going to", "remind me to"). The intent is to *persist a
  commitment*, not get an immediate answer.
  Examples: "I'll fix the migration tomorrow",
            "I'm going to refactor the router later",
            "Remind me to review this Friday"
  NOT COMMISSION: questions about future events ("when will X
  happen?") are LOOKUP or REASONING.
  NOT COMMISSION: memory-reference framings ("Remember when …",
  "Last time we talked about …", "You mentioned X", "I told you
  about Y") are recall moves, NOT commitments — even when followed
  by "I want to come back to that" or similar tail framing. These
  are EXPRESSIVE (relational) or REASONING.

EXPRESSIVE
  User expressing how they're feeling about the current work,
  often with implicit help-request. Short, first-person, emotive
  vocabulary ("stuck", "frustrated", "no idea"). Surface looks like
  a feeling-statement but the actual move — given conversation
  context — is usually "help me unstick".
  Examples: "I'm stuck on this", "Ugh, broken again",
            "I have no idea where to start", "This is exhausting"
  NOT EXPRESSIVE: longer messages that embed a real question are
  the question type, not EXPRESSIVE.

User message: "{message}"{corrections_note}{skill_hints}

Respond with JSON only:
{{"intent": "SIMPLE|LOOKUP|COMPARISON|REASONING|ACTION|CONATION|COMMISSION|EXPRESSIVE"}}"#,
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

    /// Pass 2.5: ask the model to pick the *specific* tool from the
    /// available set. Schema-constrained downstream so the response
    /// is forced to `{"tool": "<one of the ids>"}`.
    fn build_pass2_tool_selection_prompt(
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> String {
        let context_str = Self::format_context_summary(context);
        let tools_str = available_tools
            .iter()
            .map(|t| format!("- {}: {}", t.id, t.description))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"The user wants a single tool call. Pick the BEST tool for this request.

Conversation context: {context_str}

Available tools:
{tools_str}

User message: "{message}"

Reply with JSON only:
{{"tool": "<one of the tool ids above, exactly>"}}"#
        )
    }

    /// Build a summary of conversation context for the classification prompt.
    /// Includes working memory (current goal, facts) and recent messages.
    fn format_context_summary(context: &ConversationContext) -> String {
        let mut parts = Vec::new();

        // Working memory — current_goal is the load-bearing situated
        // signal for Gricean inference (it's what tells the classifier
        // that "I'm stuck" is EXPRESSIVE about a real ongoing task,
        // not idle chitchat).
        if let Some(wm) = &context.working_memory {
            if let Some(goal) = &wm.current_goal {
                parts.push(format!("Current goal: {goal}"));
            }
            if !wm.facts.is_empty() {
                let facts = wm.facts.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
                parts.push(format!("Known facts: {facts}"));
            }
        }

        // Topic context — the per-turn-updated arc. `topic` and
        // `domain` add a second situated layer when working_memory
        // hasn't been distilled yet (early in a conversation).
        if let Some(tc) = &context.topic_context {
            if let Some(topic) = &tc.topic {
                parts.push(format!("Recent topic: {topic}"));
            }
            if let Some(anchor) = &tc.anchored_source {
                parts.push(format!("Anchored source: {anchor}"));
            }
        }

        // Last assistant turn — the *specific* prior reply, surfaced
        // separately from the recent-messages list so the classifier
        // can use it for Gricean reads ("is the user reacting to what
        // I just said?"). Truncated to 200 chars to stay budget-safe.
        let last_assistant: Option<&Message> = context
            .conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant);
        if let Some(m) = last_assistant {
            let snippet = &m.content[..m.content.len().min(200)];
            parts.push(format!("Last assistant turn: {snippet}"));
        }

        // Recent messages (last 3) — broader conversation arc beneath
        // the singled-out last_assistant entry above.
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
        // Iter6: added structural causal patterns ("how did X / why
        // did X" causal openers, "what were the consequences/causes
        // of X", "what led to X / what caused X", "contribute to" /
        // "contributed to"). These are general-purpose: they match
        // any causal question regardless of domain. Surface-y
        // patterns that only matched specific bank questions
        // ("is contested today", "shape the", "shaped the") were
        // explicitly NOT added — those were teaching to the test.
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
            // Structural causal patterns (general — any causal-shape
            // question, not bank-specific):
            "how did", "why did",
            "what were the consequences", "what were the effects",
            "what were the causes", "what were the implications",
            "what led to", "what caused",
            "contribute to", "contributed to",
            "influence on", "influenced the",
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

    /// Heuristic check: is this message a metalingual query — asking
    /// about how a *specific source* uses a term, rather than asking
    /// about the term's general meaning in the world?
    ///
    /// Jakobson's metalingual function: foregrounding the *code* (the
    /// words themselves), not the world the words point at. The
    /// distinguishing Gricean signal is the **source-anchor locator**
    /// — without one, the question is referential (KnowledgeQuery);
    /// with one, it's asking how *that source* uses the word.
    ///
    /// Locator families covered:
    /// - **System-internal**: "in this codebase / repo / project /
    ///   sovereign" → resolves to code corpora.
    /// - **Conversation-internal**: "earlier", "we mentioned", "you
    ///   said" → resolves to conversation history.
    /// - **Source-anchored**: "according to X", "per X", "X defines",
    ///   "in [author / framework]" → resolves to a named corpus.
    /// - **Ambient**: "here", "this" + definitional → resolves from
    ///   conversation context.
    /// Heuristic check: personal-recall content question — first-person
    /// (I / my / we / our) + a content-discourse verb (mentioned /
    /// talked / discussed / brought up / come up / said / told) without
    /// any source-anchor on the WORD itself. These are LOOKUP/REASONING
    /// against the conversation corpus, NOT metalingual about word use.
    ///
    /// Pre-empts the LLM Pass 1 because the 4B Fast slot reliably
    /// emits METALINGUAL with confidence 1.00 for "What books have I
    /// mentioned in our chats?" style framings — the surface verb
    /// "mentioned" looks like a word-usage signal, but the user is
    /// asking for the books (content), not how the chats use the
    /// word.
    ///
    /// High-precision floor:
    /// - First-person marker present (`i `, `my `, `we `, `our `,
    ///   `i've `, `i'm `, `we've `).
    /// - Content-discourse verb present (the conversation-internal
    ///   set the metalingual heuristic uses, but inverted to fire ON
    ///   instead of off).
    /// - No definitional-of-a-word marker (`mean`, `refers to`,
    ///   `definition`, `defines`, `stand for`) — those keep the
    ///   metalingual signal.
    /// - No system-internal locator (`in this codebase`, `in this
    ///   repo`) — those are real metalingual codebase-lookup
    ///   questions about the system's own vocabulary.
    fn looks_like_personal_recall(message: &str) -> bool {
        let lower = message.to_lowercase();

        const FIRST_PERSON_MARKERS: &[&str] = &[
            "have i ",
            "did i ",
            "do i ",
            "what did i ",
            "what have i ",
            "have we ",
            "did we ",
            "what did we ",
            "what have we ",
            // possessive
            " my ",
            " our ",
            "across my ",
            "across our ",
            // contractions
            "i've ",
            "we've ",
            "i'm ",
            "we're ",
        ];

        const CONTENT_DISCOURSE_VERBS: &[&str] = &[
            "mentioned",
            "mention",
            "talked",
            "talk about",
            "discussed",
            "discuss",
            "brought up",
            "bring up",
            "come up",
            "came up",
            "shared",
            "told",
            "tell",
            "said",
            "say",
        ];

        // Word-definition markers — keep metalingual signal for these.
        const DEFINITIONAL_OF_WORD: &[&str] = &[
            " mean ",
            " mean?",
            " means ",
            " means?",
            "refers to",
            "refer to",
            "definition",
            "defines",
            "stand for",
        ];

        // System-internal locators — keep metalingual for codebase
        // vocabulary questions.
        const SYSTEM_MARKERS: &[&str] = &[
            "in this codebase",
            "in this repo",
            "in this repository",
            "in this project",
            "in this code",
            "in our codebase",
            "in our repo",
            "in our system",
            "in the codebase",
            "in the repo",
            "in sovereign",
        ];

        let has_first_person = FIRST_PERSON_MARKERS.iter().any(|m| lower.contains(m));
        if !has_first_person {
            return false;
        }
        let has_content_verb = CONTENT_DISCOURSE_VERBS.iter().any(|m| lower.contains(m));
        if !has_content_verb {
            return false;
        }
        let has_word_def = DEFINITIONAL_OF_WORD.iter().any(|m| lower.contains(m));
        if has_word_def {
            return false;
        }
        let has_system = SYSTEM_MARKERS.iter().any(|m| lower.contains(m));
        if has_system {
            return false;
        }
        true
    }

    fn looks_like_metalingual(message: &str) -> bool {
        let lower = message.to_lowercase();

        // System-internal locators — the original code/project case.
        const SYSTEM_MARKERS: &[&str] = &[
            "in this codebase",
            "in this repo",
            "in this repository",
            "in this project",
            "in this code",
            "in our codebase",
            "in our repo",
            "in our system",
            "in the codebase",
            "in the repo",
            "in sovereign",
            "in the sovereign",
        ];

        // Conversation-internal locators — asking about something said
        // earlier in this thread.
        const CONVERSATION_MARKERS: &[&str] = &[
            "in this conversation",
            "in our conversation",
            " earlier ",
            "earlier you",
            "earlier i ",
            "we mentioned",
            "we discussed",
            "we discuss ",   // "did we discuss" past-construction
            "we talked",
            "did we",
            "did you",
            "you mentioned",
            "you said",
        ];

        // Source-anchored locators — "according to X / per X / X says /
        // X defines / X uses the term Y". Phrases here must be high-
        // precision; bare "how does" is too broad (false-positives on
        // "how does Darwin's theory account for...") and is omitted.
        const SOURCE_ANCHORS: &[&str] = &[
            "according to",
            " per ",
            " says",
            " defines",
            " uses the term",
            " use the term",
        ];

        let has_system = SYSTEM_MARKERS.iter().any(|m| lower.contains(m));
        let has_conversation = CONVERSATION_MARKERS.iter().any(|m| lower.contains(m));
        let has_source_anchor = SOURCE_ANCHORS.iter().any(|m| lower.contains(m));

        // Ambient: " here" / " this" as a deictic locator. The
        // downstream definitional-verb check is the actual precision
        // gate — we don't double-require "mean"/"refer" here, so
        // "how is gossip implemented here" qualifies despite having
        // a verb-side rather than noun-side definitional shape.
        // " this " is excluded when it's the start of an in-prompt
        // content reference ("this is broken"), which is content-
        // processing not metalingual.
        let has_ambient_here = lower.contains(" here?")
            || lower.contains(" here.")
            || lower.contains(" here,")
            || lower.contains(" here ");
        let has_ambient_this = lower.contains(" this ")
            && !lower.contains(" this is ")
            && !lower.contains(" this code")
            && !lower.contains(" this passage")
            && !lower.contains(" this section");
        let has_ambient = has_ambient_here || has_ambient_this;

        // Pure source-anchored questions ("according to SEP, what does X mean")
        // qualify even without an explicit definitional verb if the anchor
        // itself implies metalingual framing — but in practice they almost
        // always include a definitional verb downstream, so we still
        // require one for precision.
        let has_locator = has_system || has_conversation || has_source_anchor || has_ambient;
        if !has_locator {
            return false;
        }

        // Source-anchored questions ("according to X", "X says")
        // are AMBIGUOUS — they can be metalingual ("what does X
        // mean according to source Y") OR referential ("what's
        // the fact according to source Y"). String matching can't
        // tell; only the full-sentence LLM classifier can. So when
        // the only locator is source-anchored (no system/conversation
        // /ambient signal), abstain from forcing metalingual and let
        // the LLM decide downstream.
        //
        // System and conversation locators stay as overrides because
        // their signals are unambiguous: "in this codebase" / "what
        // did we discuss earlier" are always metalingual.
        if has_source_anchor && !has_system && !has_conversation && !has_ambient {
            return false;
        }

        // Definitional / structural / discourse verbs — what the user
        // is asking about the located term.
        //
        // Discourse verbs ("discuss", "say", "mention", "explain") are
        // included because, paired with a conversation-internal
        // locator ("what did we discuss earlier"), they're inherently
        // metalingual — asking about a prior linguistic act, not about
        // the world. They never fire without a locator since the
        // outer `has_locator` gate already rejected those cases.
        const DEFINITIONAL_VERBS: &[&str] = &[
            "what does", "what is", "what's", "what are",
            "what did", "what do",
            "how does", "how do", "how is",
            "mean", "refers to", "refer to", "stand for",
            "where is", "where's", "where do",
            "definition", "defines", "describe", "describes",
            "discuss", "discussed",
            "say", "said", "says",
            "mention", "mentioned",
            "explain", "explained",
            "talk", "talked",
        ];
        DEFINITIONAL_VERBS.iter().any(|m| lower.contains(m))
    }

    /// Heuristic check: is this message a conation move — short
    /// imperative directed at the assistant about its prior reply
    /// ("stop", "try again", "shorter please", "skip the boilerplate",
    /// "more detail")?
    ///
    /// High-precision: requires (a) ≤4 alphanumeric tokens, (b) no
    /// question mark, (c) no proper nouns (filters out "Compare X
    /// and Y"), (d) at least one imperative or style-modifier marker.
    /// The conation handler then operates on the situated artifact —
    /// the prior `QuerySession.classification` — without re-running
    /// the router.
    fn looks_like_conation(message: &str) -> bool {
        if message.contains('?') {
            return false;
        }
        let tokens: Vec<&str> = message
            .split_whitespace()
            .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
            .collect();
        if tokens.is_empty() || tokens.len() > 4 {
            return false;
        }
        // Reject mid-sentence proper nouns. First token treated as
        // sentence-initial so "Stop" capitalized at start is allowed.
        for (i, t) in tokens.iter().enumerate() {
            if i == 0 {
                continue;
            }
            let cleaned = t.trim_matches(|c: char| !c.is_alphabetic());
            if cleaned
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
            {
                return false;
            }
        }
        let lower = message.to_lowercase();
        const IMPERATIVE_MARKERS: &[&str] = &[
            "stop", "cancel", "abort", "halt",
            "try again", "retry", "regenerate", "redo", "again",
            "shorter", "terser", "concise", "be terse", "tldr",
            "longer", "more detail", "expand", "elaborate",
            "slower", "step by step", "walk through",
            "faster", "skip", "boilerplate",
            "less", "more",
        ];
        IMPERATIVE_MARKERS.iter().any(|m| lower.contains(m))
    }

    /// Heuristic check: is this message a commissive move — user
    /// committing to a future action or asking the assistant to
    /// remember something? "I'll fix this tomorrow", "I'm going to
    /// refactor X", "remind me to check Friday".
    ///
    /// Distinct from a question with first-person framing ("I want
    /// to know X" — that's still a question). Requires a future-
    /// commitment marker AND no question mark AND no memory-reference
    /// opener — see `looks_like_memory_reference` for the latter.
    fn looks_like_commissive(message: &str) -> bool {
        if message.contains('?') {
            return false;
        }
        // A memory-reference framing is a recall move, not a commit.
        // "Last time we talked about X, I'll come back to it" looks
        // commissive on the tail but the lead is recall — the witness
        // contract is the right surface.
        if Self::looks_like_memory_reference(message) {
            return false;
        }
        let lower = message.to_lowercase();
        const COMMITMENT_MARKERS: &[&str] = &[
            "i'll ", "i will ", "i'm going to ", "i am going to ",
            "i'm gonna ", "i plan to ", "i'll be ",
            "remind me to ", "remind me about ", "remind me later ",
            "remind me on ", "remind me in ",
        ];
        COMMITMENT_MARKERS.iter().any(|m| lower.contains(m))
    }

    /// Heuristic check: factual-lookup shape — the message opens
    /// with a single-clause "what is/was X" / "who is/was X" /
    /// "when did/was X" / "where is/was X". Iter6 pre-check added
    /// to bypass the LLM Pass 1 (median ~3s on the 4B fast slot)
    /// for unambiguous lookups.
    ///
    /// High-precision floor:
    /// - Lookup-verb opener (first ~6 words contain the trigger).
    /// - No analytical markers ("and what", "compared to",
    ///   "compatible", "and how", etc.) — those go through
    ///   `needs_deep_reasoning` first; this fires only when the
    ///   message is a clean single-clause lookup.
    /// - Skipped on long messages (>15 words) — the longer the
    ///   message, the more likely it embeds a multi-clause shape
    ///   the LLM should resolve.
    fn looks_like_factual_lookup(message: &str) -> bool {
        let lower = message.to_lowercase();
        let word_count = message.split_whitespace().count();
        if word_count > 15 {
            return false;
        }

        // Opener triggers — checked at the start of the message
        // (allowing leading punctuation but not nested clauses).
        const LOOKUP_OPENERS: &[&str] = &[
            "what is ", "what was ", "what are ", "what were ",
            "who is ", "who was ", "who are ", "who were ",
            "when is ", "when was ", "when did ", "when does ",
            "where is ", "where was ", "where are ",
            "which ", // "which X is Y" — selection lookup
            "name the ", "name a ", // direct lookup imperative
        ];
        // Trim leading punctuation/whitespace to test opener
        // positions like "What is X?" or "  who was X?".
        let trimmed = lower.trim_start_matches(|c: char| !c.is_alphanumeric());
        let has_opener = LOOKUP_OPENERS.iter().any(|m| trimmed.starts_with(m));
        if !has_opener {
            return false;
        }

        // Multi-clause / analytical exclusions. These drag the
        // message into deep-reasoning territory regardless of the
        // lookup opener.
        const ANALYTICAL_EXCLUSIONS: &[&str] = &[
            " and what ", " and how ", " and why ",
            " and were ", " and is ",
            " compared to ", " compared with ",
            " compatible ", " incompatible ",
            " differ ", " differs ", " differences ",
            " consequences ", " implications ", " causes ",
            " contributed ", " contribute to ",
            " led to ", " caused ", " influenced ",
            " significance ", // "what is the significance of X" — borderline
            " role of ", // "what was the role of X in Y" — multi-relation
            " contested ", " debated ",
            " arguments for ", " arguments against ",
        ];
        if ANALYTICAL_EXCLUSIONS.iter().any(|m| lower.contains(m)) {
            return false;
        }

        true
    }

    /// Heuristic check: does this message lead with (or contain) a
    /// memory-reference framing? "Remember when …", "Last time we
    /// talked about …", "You mentioned X", "I told you about Y".
    ///
    /// These are recall moves — the user is pointing at past shared
    /// state, not committing to anything. The relational/witness
    /// contract is the right surface; a Save-commitment classifier
    /// that catches the trailing "I want to come back to that" misses
    /// the lead. High-precision floor: substring match on phrases
    /// that don't appear inside other words.
    fn looks_like_memory_reference(message: &str) -> bool {
        let lower = message.to_lowercase();
        const MEMORY_REF_MARKERS: &[&str] = &[
            "remember when",
            "remember our",
            "remember the",
            "do you remember",
            "you remember",
            "last time we",
            "last time you",
            "you mentioned",
            "you said",
            "you told me",
            "we talked about",
            "we discussed",
            "what we discussed",
            "i told you",
            "i mentioned",
            "going back to what",
            "back to what you",
            "come back to that",
            "come back to what",
        ];
        MEMORY_REF_MARKERS.iter().any(|m| lower.contains(m))
    }

    /// Heuristic check: is this message an expressive move — user
    /// stating how they're feeling about the current work, often with
    /// implicit help-request? "I'm stuck on this bug", "ugh, broken
    /// again", "I have no idea where to start".
    ///
    /// High-precision floor only — Pass 1 (situated) handles the
    /// borderline cases. We require an emotive marker AND no question
    /// mark AND short message length, because longer messages tend to
    /// embed a real question that the LLM should classify.
    fn looks_like_expressive(message: &str) -> bool {
        if message.contains('?') {
            return false;
        }
        let token_count = message.split_whitespace().count();
        if token_count > 15 {
            return false;
        }
        let lower = message.to_lowercase();

        // Multi-word markers — substring match is safe because they
        // include word characters that can't appear inside other words
        // ("im stuck" can't accidentally match anywhere outside an
        // intended occurrence).
        const PHRASE_MARKERS: &[&str] = &[
            "i'm stuck", "im stuck",
            "i'm bored", "im bored",
            "i'm frustrated", "im frustrated",
            "i'm exhausted", "im exhausted",
            "i'm tired", "im tired",
            "i give up", "i gave up",
            "i have no idea", "no idea where to start",
            "this is frustrating", "this is broken",
            "this is annoying", "this is exhausting",
        ];
        if PHRASE_MARKERS.iter().any(|m| lower.contains(m)) {
            return true;
        }

        // Single-word interjections — must match as a whole word, not
        // a substring (otherwise "ugh" matches inside "through",
        // "argh" inside "marginal", "sigh" inside "signing"). Split
        // on non-alphabetic so punctuation doesn't anchor us to the
        // wrong boundary.
        const WORD_MARKERS: &[&str] = &["ugh", "argh", "sigh", "agh", "blah"];
        for word in lower.split(|c: char| !c.is_alphabetic()) {
            if WORD_MARKERS.contains(&word) {
                return true;
            }
        }
        false
    }

    /// Heuristic check: is this message a two-entity comparison shape
    /// ("difference between X and Y", "X vs Y", "how do X and Y differ",
    /// "compare X and Y")? High-precision — requires both a comparison
    /// verb/preposition AND a conjunction/separator AND no in-prompt-content
    /// markers ("this", "these", "the passage", etc.) so it doesn't
    /// poach `looks_like_content_processing`'s territory.
    fn looks_like_comparison(message: &str) -> bool {
        let lower = message.to_lowercase();

        // In-prompt-content markers — if present, this is "compare these
        // sections" not "compare X and Y", so let content_processing handle.
        const IN_PROMPT_MARKERS: &[&str] = &[
            "this passage", "this section", "this chapter", "this paragraph",
            "this code", "this snippet", "this text", "this excerpt",
            "this document", "this article", "this paper",
            "these passages", "these sections", "these paragraphs",
            "these snippets", "these excerpts", "the passage above",
            "the section above", "the snippet above", "the excerpt above",
            "above and below", "the above", "the below",
        ];
        if IN_PROMPT_MARKERS.iter().any(|m| lower.contains(m)) {
            return false;
        }

        // Unambiguous comparison signals — any one of these is sufficient.
        const STRONG_SIGNALS: &[&str] = &[
            "difference between", "differences between",
            "differ from", "differs from", "differing from",
            " vs ", " vs.", " versus ",
        ];
        if STRONG_SIGNALS.iter().any(|m| lower.contains(m)) {
            return true;
        }

        // "differ" anywhere in the message is a strong contrast signal as
        // long as it's not part of "different" (a softer adjective).
        // We check for whitespace/punctuation boundaries on both sides.
        if lower.split(|c: char| !c.is_alphabetic()).any(|w| w == "differ") {
            return true;
        }

        // "compare X and Y" / "contrast X and Y" — comparison verb plus a
        // conjunction. Loose, but the in-prompt filter above already
        // ruled out content-processing cases.
        let has_compare_verb = lower.contains("compare ")
            || lower.contains("contrast ")
            || lower.contains("comparison of ")
            || lower.contains("compared to ")
            || lower.contains("compared with ");
        let has_conjunction = lower.contains(" and ") || lower.contains(" or ");
        if has_compare_verb && has_conjunction {
            return true;
        }

        false
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
                        model_id: None,
                        enable_thinking: None,
        sampling_mode: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] classify raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Call the fast model for a JSON-output classification prompt (Pass 1 + self-assessment).
    ///
    /// Populates `structured_output` with the classifier schema —
    /// `JsonConstraint` (in `sovereign-inference::json_constraint`)
    /// masks logits to force the model to emit only schema-conforming
    /// bytes. Without this, small fast-slot models (Qwen3.5-2B) write
    /// reasoning prose ("Let me analyse this message carefully…")
    /// instead of JSON, which `parse_coarse` can't recover from.
    ///
    /// The schema is `{"intent": <enum>}` only — the decode cost of
    /// every extra schema field is paid on every routing turn. A
    /// `rationale` field invites the model to chain-of-thought in
    /// JSON before committing to the intent, which both costs ~20-30
    /// tokens and biases the answer toward analytical labels on
    /// emotive inputs. A `confidence` field has nowhere to land —
    /// `assess_simple_query` always runs self_assess on the SIMPLE
    /// branch, so there's no threshold to gate.
    async fn classify_call_json(&self, prompt: String, max_tokens: usize) -> Result<String> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "enum": [
                        "SIMPLE",
                        "LOOKUP",
                        "COMPARISON",
                        "REASONING",
                        "ACTION",
                        "CONATION",
                        "COMMISSION",
                        "EXPRESSIVE",
                        "METALINGUAL",
                    ],
                },
            },
            "required": ["intent"],
        });
        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a message classifier. Respond with valid JSON only.".to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(max_tokens),
            temperature: Some(0.0),
            structured_output: Some(schema),
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
                tools: None,
                tool_choice: None,
                        model_id: None,
                        enable_thinking: None,
        sampling_mode: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] classify_json raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Schema-constrained call for Pass 2.5 tool selection. The
    /// schema's `tool` enum is the set of registered tool ids — the
    /// constraint enforcer mathematically can't emit anything else.
    async fn classify_call_tool_json(
        &self,
        prompt: String,
        tool_ids: &[String],
    ) -> Result<String> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "enum": tool_ids,
                },
            },
            "required": ["tool"],
        });
        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a tool router. Respond with valid JSON only.".to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(64),
            temperature: Some(0.0),
            structured_output: Some(schema),
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
                tools: None,
                tool_choice: None,
                        model_id: None,
                        enable_thinking: None,
        sampling_mode: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] tool_select raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Refine the ACTION coarse classification via Pass 2.
    ///
    /// Pass 2 is a category check (single-tool / multi-step / knowledge).
    /// When it returns `A` (single tool), we run Pass 2.5 — a follow-up
    /// LLM call constrained to the actual tool ids — to *pick* the
    /// specific tool. Hardcoding `available_tools.first()` here was the
    /// original bug: the agent's autonomous tool selection picked
    /// whatever tool happened to register first (e.g. `ShellTool`)
    /// instead of the LLM-fit tool (e.g. `wikipedia_fetch`).
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
                let tool = self
                    .pass2_select_tool(message, context, available_tools)
                    .await?;
                Intent::SimpleAction { tool }
            }
            'C' => Intent::KnowledgeQuery,
            _ => Intent::ComplexTask,
        })
    }

    /// Pass 2.5: pick the specific tool the user wants, constrained to
    /// the registered tool ids. Schema-constrained output forces the
    /// model into `{"tool": "<id>"}` where `<id>` is one of the actual
    /// available tool ids — `JsonConstraint` masks logits so the small
    /// fast-slot model can't hallucinate a tool name.
    ///
    /// On parse failure or off-schema output (rare given the constraint),
    /// fall back to the first tool — preserving the prior behaviour as a
    /// floor rather than as the default.
    async fn pass2_select_tool(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<ToolId> {
        debug_assert!(!available_tools.is_empty());
        if available_tools.len() == 1 {
            // No selection to make.
            return Ok(available_tools[0].id.clone());
        }
        let prompt =
            Self::build_pass2_tool_selection_prompt(message, context, available_tools);
        let tool_ids: Vec<String> =
            available_tools.iter().map(|t| t.id.clone()).collect();
        let raw = self.classify_call_tool_json(prompt, &tool_ids).await?;
        Ok(Self::parse_tool_selection(&raw, &tool_ids)
            .unwrap_or_else(|| available_tools[0].id.clone()))
    }

    /// Called when Pass 1 returns SIMPLE. Runs a fast self-assessment to decide
    /// whether to answer directly from weights or escalate to KnowledgeQuery.
    ///
    /// The `_confidence` parameter is vestigial: Pass 1's schema no
    /// longer emits a confidence field (it cost ~20 decode tokens
    /// per turn and had no use beyond a fast-path here). With no
    /// signal to gate on, every SIMPLE classification falls through
    /// to self_assess — ~100ms extra on the SIMPLE branch only.
    async fn assess_simple_query(
        &self,
        message: &str,
        context: &ConversationContext,
        _confidence: f32,
    ) -> Result<(Intent, Option<String>)> {
        // Always run self-assessment on the Fast slot (~100ms extra latency).
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

    /// Parse a Pass 2.5 tool-selection JSON response. Returns
    /// `Some(tool_id)` only when the parsed `tool` field is one of
    /// `valid_ids`. Tolerant of `<think>` blocks and code fences for
    /// the rare case the schema constraint is bypassed (e.g. legacy
    /// providers without `JsonConstraint` wired in).
    fn parse_tool_selection(raw: &str, valid_ids: &[String]) -> Option<String> {
        let after_think = if let (Some(start), Some(end)) =
            (raw.find("<think>"), raw.find("</think>"))
        {
            if end > start {
                &raw[end + "</think>".len()..]
            } else {
                raw
            }
        } else {
            raw
        };
        let cleaned = after_think
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        #[derive(serde::Deserialize)]
        struct ToolSelection {
            tool: String,
        }
        let parsed: ToolSelection = serde_json::from_str(cleaned).ok()?;
        if valid_ids.iter().any(|id| id == &parsed.tool) {
            Some(parsed.tool)
        } else {
            None
        }
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
    ) -> Result<RouterClassification> {
        let start = Instant::now();

        // Fetch recent routing corrections for few-shot self-correction.
        let corrections = self
            .store
            .get_routing_corrections(3)
            .await
            .unwrap_or_default();

        // Get active skill routing hints.
        let routing_hints = self.skills.routing_hints();

        // Pre-check -3: personal-recall content question. Fires BEFORE
        // the embed router because the embed router's nearest-exemplar
        // matching reliably picks ExpressiveQuery or MetalingualQuery
        // for "Have I ever discussed X" / "Questions I keep coming back
        // to" shapes — both intents skip retrieval entirely, defeating
        // the whole point of the conversation-history corpus. The
        // heuristic is high-precision: first-person marker
        // (`have i `, `did i `, `my `, `our `, `i've `, ...) +
        // content-discourse verb (`mentioned` / `talked` / `discussed`
        // / `said` / `told`) with no word-definition or system-locator
        // override. We compute the scope hint here too (when the
        // classifier is installed) so retrieval sees the personal
        // bias without paying a second embed call later.
        //
        // Must fire BEFORE `inherits_prior_knowledge_intent` —
        // the inherit pre-check assumes personal-recall already
        // short-circuited (see fn doc-comment).
        if Self::looks_like_personal_recall(message) {
            let latency_ms = start.elapsed().as_millis() as i64;
            let hash = message_hash(message);
            let _ = self.store.log_routing(&hash, "KnowledgeQuery", latency_ms).await;
            let _ = self
                .store
                .log_routing_meta(&hash, "PERSONAL_RECALL", None)
                .await;
            // Compute scope hint via the classifier (needs a fresh
            // embedding — the embed router didn't run, so no shared
            // vector to reuse). Skipped if no classifier installed.
            let scope = match self.scope_classifier.as_ref() {
                Some(cls) => cls.classify(message, &*self.inference).await.ok().flatten(),
                None => None,
            };
            eprintln!(
                "[router] \"{}\" → KnowledgeQuery (personal-recall heuristic; scope={:?})",
                &message[..message.len().min(50)],
                scope,
            );
            return Ok(RouterClassification {
                primary: IntentCandidate {
                    intent: Intent::KnowledgeQuery,
                    confidence: 1.0,
                },
                alternatives: Vec::new(),
                rationale: Some(
                    "first-person + content-discourse verb → personal-corpus lookup".to_string(),
                ),
                coarse_intent: Some("PERSONAL_RECALL".to_string()),
                self_assessment: None,
                timing: None,
                scope,
            });
        }

        // Pre-check -1: embedding-based intent classification.
        // When installed AND confident (top-similarity + margin both
        // pass the configured thresholds), the embed router commits
        // to an intent without consulting the heuristic stack or the
        // LLM Pass 1/2 calls. Falls through silently when ambiguous.
        //
        // This replaces the brittle string-match heuristics
        // (`looks_like_metalingual` etc.) for the common case while
        // still letting them act as a backstop for queries the
        // embed router declines.
        // Scope is an axis ORTHOGONAL to intent — answered by its own
        // binary classifier (`PersonalScopeClassifier`) running off
        // the same query embedding the intent router uses. Stashed
        // here so every return path below — embed-router verdict,
        // topic-continuity, LLM-classifier fallback — sets it on the
        // returned RouterClassification.
        //
        // The previous design routed scope through per-intent
        // exemplar tags (`scope = "personal"` on individual rows in
        // exemplars.toml). That collapsed because k=1 NN gives ONE
        // intent its tag, and bench questions reliably landed near
        // exemplars from OTHER intents, erasing the scope hint. See
        // `scope_classifier.rs` module docs for the full post-mortem.
        let mut scope_hint: Option<String> = None;

        if let Some(embed) = self.embed_router.as_ref() {
            match embed
                .classify_returning_embedding(message, &*self.inference)
                .await
            {
                Ok((intent_verdict, query_embedding)) => {
                    // Run scope classifier off the same embedding —
                    // single embed call serves both decisions.
                    if let Some(scope_cls) = self.scope_classifier.as_ref() {
                        scope_hint = scope_cls.classify_from_embedding(&query_embedding);
                    }
                    if let Some(verdict) = intent_verdict {
                        let latency_ms = start.elapsed().as_millis() as i64;
                        let hash = message_hash(message);
                        let intent_str = format!("{:?}", verdict.intent);
                        let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
                        let _ = self
                            .store
                            .log_routing_meta(&hash, "EMBED_ROUTER", None)
                            .await;
                        eprintln!(
                            "[router] \"{}\" → {:?} (embed: sim={:.3} margin={:.3} nearest={:?} scope={:?})",
                            &message[..message.len().min(50)],
                            verdict.intent,
                            verdict.top_sim,
                            verdict.margin,
                            verdict.nearest_exemplar,
                            scope_hint,
                        );
                        return Ok(RouterClassification {
                            primary: IntentCandidate {
                                intent: verdict.intent,
                                // Embed verdicts that clear both gates
                                // are high-confidence by construction.
                                // Pin to 0.95 (not 1.0) so downstream
                                // policy still treats this as "very
                                // likely" rather than "unconditionally
                                // certain"; leaves room for
                                // user-driven interpretation-redirect.
                                confidence: 0.95,
                            },
                            alternatives: Vec::new(),
                            rationale: Some(format!(
                                "embed router: nearest exemplar {:?} (cosine {:.3}, margin {:.3})",
                                verdict.nearest_exemplar, verdict.top_sim, verdict.margin
                            )),
                            coarse_intent: Some("EMBED_ROUTER".to_string()),
                            self_assessment: None,
                            timing: None,
                            scope: scope_hint.clone(),
                        });
                    }
                    // Intent ambiguous — fall through. scope_hint is
                    // already set (or None) above and survives
                    // independently of the intent decision.
                }
                Err(e) => {
                    tracing::warn!(
                        target: "router.embed",
                        error = %e,
                        "embed-router classify failed; falling through"
                    );
                }
            }
        } else if let Some(scope_cls) = self.scope_classifier.as_ref() {
            // No embed router installed — pay the scope embed call on
            // its own. Rare path; production always installs both.
            match scope_cls.classify(message, &*self.inference).await {
                Ok(s) => scope_hint = s,
                Err(e) => tracing::warn!(
                    target: "router.scope",
                    error = %e,
                    "scope classifier failed; treating as None"
                ),
            }
        }

        // Pre-check 0: topic continuity — if the conversation has established
        // context (2+ turns on a topic), check whether this message is a general
        // knowledge follow-up that should bypass corpus retrieval.
        if let Some(override_intent) = Self::check_topic_continuity(message, context) {
            let latency_ms = start.elapsed().as_millis() as i64;
            let hash = message_hash(message);
            let intent_str = format!("{override_intent:?}");
            let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
            let _ = self.store.log_routing_meta(&hash, "TOPIC_CONTINUITY", None).await;

            eprintln!(
                "[router] \"{}\" → {:?} (topic continuity override)",
                &message[..message.len().min(50)],
                override_intent,
            );

            // Topic-continuity override is a deterministic heuristic:
            // treat it as maximum confidence so `decide_policy` commits
            // without prompting the user.
            return Ok(RouterClassification {
                primary: IntentCandidate {
                    intent: override_intent,
                    confidence: 1.0,
                },
                alternatives: Vec::new(),
                rationale: Some(
                    "topic continuity: general-knowledge follow-up in an established conversation"
                        .to_string(),
                ),
                coarse_intent: Some("TOPIC_CONTINUITY".to_string()),
                self_assessment: None,
                timing: None,
                scope: scope_hint.clone(),
            });
        }

        // Iter6: time the pre-check chain, the LLM Pass 1 (when it
        // fires), and the parse step separately so the iter5
        // routing slice (median 6s) can be diagnosed.
        let precheck_start = Instant::now();

        // Pre-check 0: conation shape → force CONATION. Short imperative
        // directed at the assistant about the prior turn ("stop", "try
        // again", "shorter please"). Highest-precision pre-check; fires
        // first so it pre-empts e.g. "stop" being read as ACTION (cancel
        // an in-flight search would itself be conation routed correctly).
        let force_conation = Self::looks_like_conation(message);

        // Pre-check 1: temporal/current-info → force ACTION (search).
        // Small models are unreliable at detecting these.
        let has_search = available_tools.iter().any(|t| t.name.contains("search"));
        let force_action = !force_conation
            && has_search
            && Self::needs_current_info(message);

        // Pre-check 1a: personal-recall content question → force
        // LOOKUP. First-person + content-discourse verb without a
        // word-definition or system-locator marker means the user is
        // asking what was SAID in the chats, not how the chats USE a
        // word. Runs BEFORE force_metalingual so the LLM Pass 1's
        // overzealous METALINGUAL emission on "What books have I
        // mentioned" is pre-empted. See `looks_like_personal_recall`
        // for the heuristic. Pairs with the centroid scope classifier
        // (`scope_classifier.rs`) which marks these queries personal
        // for retrieval-time corpus restriction.
        let force_personal_recall = !force_conation
            && !force_action
            && Self::looks_like_personal_recall(message);

        // Pre-check 1b: metalingual shape → force METALINGUAL. Question
        // about the system's own vocabulary ("what does X mean in this
        // codebase"). Runs BEFORE comparison so "what's the difference
        // between plan and task in this codebase" routes metalingual,
        // not comparison-against-Wikipedia.
        let force_metalingual = !force_conation
            && !force_action
            && !force_personal_recall
            && Self::looks_like_metalingual(message);

        // Pre-check 1c: commissive shape → force COMMISSION. First-
        // person future commitment ("I'll fix it tomorrow", "remind me
        // to check Friday"). Distinct from comparison/metalingual; can
        // fire after either without conflict.
        let force_commissive = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && Self::looks_like_commissive(message);

        // Pre-check 2: comparison shape → force COMPARISON. Two-entity
        // contrast ("difference between X and Y", "X vs Y", "compare X
        // and Y") — bounded shape that's served by the fast slot with a
        // constrained prompt rather than the open-ended REASONING path.
        // Runs BEFORE content-processing so `compare X and Y` (world
        // entities) doesn't get poached by the `compare ` content verb.
        let force_comparison = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && Self::looks_like_comparison(message);

        // Pre-check 2b: expressive shape → force EXPRESSIVE. Short
        // first-person feeling-statement, often with implicit help-
        // request ("I'm stuck on this bug", "ugh, broken again").
        // The heuristic catches surface-clear cases; situated Pass 1
        // (and the tail-case refiner) handle the borderline ones.
        let force_expressive_short = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && !force_comparison
            && Self::looks_like_expressive(message);

        // Pre-check 2c: memory-reference framing → force EXPRESSIVE.
        // "Remember when …", "You mentioned X", "Last time we talked
        // about Y" — the user is pointing at past shared state. The
        // relational/witness contract is the right surface; the LLM
        // Pass 1 otherwise misclassifies these as COMMISSION when a
        // trailing "I want to come back to that" appears (hard-mode
        // H02). EXPRESSIVE routes to `handle_expressive_query`, which
        // runs the witness path on Relational skills.
        let force_expressive_memref = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && !force_comparison
            && !force_expressive_short
            && Self::looks_like_memory_reference(message);

        let force_expressive = force_expressive_short || force_expressive_memref;

        // Pre-check 3: content-processing signal → force REASONING. Catches
        // "summarize this", "explain this passage", "compare these sections"
        // etc. which the Fast model sometimes misreads as ACTION because of
        // the imperative verb. Content processing never needs external reach.
        let force_content_reasoning = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && !force_comparison
            && !force_expressive
            && Self::looks_like_content_processing(message);

        // Pre-check 4: deep reasoning signal → force REASONING before the LLM sees it.
        // This catches philosophical, analytical, and compatibility questions that
        // small fast models frequently mis-classify as SimpleQuery.
        let force_deep = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && !force_comparison
            && !force_expressive
            && !force_content_reasoning
            && Self::needs_deep_reasoning(message);

        // Pre-check 5: factual-lookup shape → force LOOKUP. Single-
        // clause "what is/was X", "who is/was X", "when did X",
        // "where is/was X" patterns. Iter6: each catch saves a ~3s
        // LLM Pass 1 call. High-precision floor — requires the
        // lookup verb at the message start (no "and" / "but" /
        // multi-clause), no analytical markers (those would have
        // caught earlier in `force_deep`).
        let force_lookup = !force_conation
            && !force_action
            && !force_personal_recall
            && !force_metalingual
            && !force_commissive
            && !force_comparison
            && !force_expressive
            && !force_content_reasoning
            && !force_deep
            && Self::looks_like_factual_lookup(message);

        // Iter6: pre-check chain done. Cap timer here — anything
        // after this is either zero-cost branch selection or the
        // LLM Pass 1.
        let precheck_ms = precheck_start.elapsed().as_millis() as u64;
        let mut llm_ms: u64 = 0;
        let mut parse_ms: u64 = 0;
        let mut used_llm = false;

        // Pass 1: Coarse classification (skipped for pre-checked cases).
        let coarse = if force_conation {
            CoarseClassification {
                intent: "CONATION".to_string(),
                confidence: 1.0,
                rationale: Some("short imperative on prior turn → conation".to_string()),
            }
        } else if force_action {
            CoarseClassification {
                intent: "ACTION".to_string(),
                confidence: 1.0,
                rationale: Some("current/time-sensitive signal → external tool".to_string()),
            }
        } else if force_personal_recall {
            CoarseClassification {
                intent: "LOOKUP".to_string(),
                confidence: 1.0,
                rationale: Some(
                    "first-person + content-discourse verb → personal-corpus lookup".to_string(),
                ),
            }
        } else if force_metalingual {
            CoarseClassification {
                intent: "METALINGUAL".to_string(),
                confidence: 1.0,
                rationale: Some("in-system definitional signal → codebase lookup".to_string()),
            }
        } else if force_commissive {
            CoarseClassification {
                intent: "COMMISSION".to_string(),
                confidence: 1.0,
                rationale: Some("first-person future commitment → persist".to_string()),
            }
        } else if force_comparison {
            CoarseClassification {
                intent: "COMPARISON".to_string(),
                confidence: 1.0,
                rationale: Some("two-entity contrast signal → bounded comparison".to_string()),
            }
        } else if force_expressive {
            let rationale = if force_expressive_memref {
                "memory-reference framing → relational witness path"
            } else {
                "emotive marker, short first-person → expressive"
            };
            CoarseClassification {
                intent: "EXPRESSIVE".to_string(),
                confidence: 1.0,
                rationale: Some(rationale.to_string()),
            }
        } else if force_content_reasoning {
            CoarseClassification {
                intent: "REASONING".to_string(),
                confidence: 1.0,
                rationale: Some("content-processing verb on in-prompt material".to_string()),
            }
        } else if force_deep {
            CoarseClassification {
                intent: "REASONING".to_string(),
                confidence: 1.0,
                rationale: Some("analytical/compatibility signal → deep reasoning".to_string()),
            }
        } else if force_lookup {
            CoarseClassification {
                intent: "LOOKUP".to_string(),
                confidence: 1.0,
                rationale: Some("factual-lookup shape (what/who/when/where) → knowledge query".to_string()),
            }
        } else {
            let pass1_prompt = Self::build_pass1_prompt(
                message,
                context,
                available_tools,
                &corrections,
                &routing_hints,
            );
            // 60-token budget: JSON + confidence + short rationale clause.
            // 60-token budget: gemma-4-E4B writes terse rationales
            // ("one short clause" per the prompt) and 60 fits the
            // typical JSON wrapper + clause comfortably. Qwen3.5-2B
            // can run out at this budget on verbose rationales — bump
            // to 120 ONLY if a small fast slot is in use (set the
            // bumped value via a knob if/when we wire one up).
            used_llm = true;
            let llm_start = Instant::now();
            // 16-token budget. Schema is `{"intent": "<enum>"}` —
            // masker forces structural tokens; longest enum value is
            // "EXPRESSIVE" (5 tokens for most BPEs) plus 4 wrapper
            // tokens. 16 is generous slack.
            let pass1_response = self.classify_call_json(pass1_prompt, 16).await?;
            llm_ms = llm_start.elapsed().as_millis() as u64;
            let parse_start = Instant::now();
            let parsed = Self::parse_coarse(&pass1_response);
            parse_ms = parse_start.elapsed().as_millis() as u64;
            parsed
        };

        let (intent, self_assessment_outcome) = match coarse.intent.as_str() {
            "LOOKUP" => (Intent::KnowledgeQuery, None),
            "COMPARISON" => (Intent::ComparisonQuery, None),
            "METALINGUAL" => (Intent::MetalingualQuery, None),
            "CONATION" => (Intent::ConationQuery, None),
            "COMMISSION" => (Intent::CommissiveQuery, None),
            "EXPRESSIVE" => (Intent::ExpressiveQuery, None),
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
        let hash = message_hash(message);
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

        // Confidence source: pre-check heuristics (force_action,
        // force_content_reasoning, force_deep) pin `coarse.confidence`
        // to 1.0 at the match-arm above where they're constructed;
        // otherwise the LLM Pass 1 asserted confidence flows through.
        //
        // Empirical issue (v26 bench audit, 2026-05-17): the Pass 1
        // schema only emits `{"intent": "<enum>"}` — no confidence
        // field. `parse_coarse` defaults the missing field to 0.0,
        // which then maps every LLM-routed turn to MoveKind::Ask
        // (the clarification-card placeholder). New-thread T0s like
        // "What did Christopher Columbus do?" and "When and where did
        // Buddhism originate?" routed correctly to LOOKUP/REASONING
        // but were Ask'd into a non-answer placeholder. Treat a
        // successfully-parsed intent as the LLM's commit signal
        // regardless of the absent confidence field; pre-check
        // heuristics still pin 1.0 explicitly when they fire.
        let primary_confidence = if coarse.intent.is_empty() {
            1.0
        } else if coarse.confidence > 0.0 {
            coarse.confidence
        } else {
            1.0
        };

        // PR2: populate alternatives for every classification. The
        // runtime surfaces them only on the Ask move (low-confidence
        // clarification card); on Commit/Propose they're carried
        // along for telemetry + potential next-step use.
        let primary_intent_ref = &intent;
        let alternatives = suggest_alternatives(message, primary_intent_ref, available_tools);

        Ok(RouterClassification {
            primary: IntentCandidate {
                intent,
                confidence: primary_confidence,
            },
            alternatives,
            rationale: coarse.rationale.clone(),
            coarse_intent: Some(coarse.intent),
            self_assessment: self_assessment_outcome,
            timing: Some(crate::types::RoutingTiming {
                precheck_ms,
                llm_ms,
                parse_ms,
                used_llm,
            }),
            scope: scope_hint,
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
    fn parse_tool_selection_extracts_valid_id() {
        let valid = vec!["wikipedia_fetch".to_string(), "shell".to_string()];
        let raw = r#"{"tool": "wikipedia_fetch"}"#;
        assert_eq!(
            LlmRouter::parse_tool_selection(raw, &valid),
            Some("wikipedia_fetch".to_string())
        );
    }

    #[test]
    fn parse_tool_selection_rejects_unknown_id() {
        let valid = vec!["wikipedia_fetch".to_string(), "shell".to_string()];
        let raw = r#"{"tool": "made_up_tool"}"#;
        assert_eq!(LlmRouter::parse_tool_selection(raw, &valid), None);
    }

    #[test]
    fn parse_tool_selection_strips_think_block() {
        let valid = vec!["wikipedia_fetch".to_string(), "shell".to_string()];
        let raw = "<think>this is the wikipedia case</think>{\"tool\": \"wikipedia_fetch\"}";
        assert_eq!(
            LlmRouter::parse_tool_selection(raw, &valid),
            Some("wikipedia_fetch".to_string())
        );
    }

    #[test]
    fn parse_tool_selection_strips_code_fences() {
        let valid = vec!["wikipedia_fetch".to_string(), "shell".to_string()];
        let raw = "```json\n{\"tool\": \"shell\"}\n```";
        assert_eq!(
            LlmRouter::parse_tool_selection(raw, &valid),
            Some("shell".to_string())
        );
    }

    #[test]
    fn parse_tool_selection_handles_garbage() {
        let valid = vec!["wikipedia_fetch".to_string()];
        assert_eq!(LlmRouter::parse_tool_selection("not json", &valid), None);
        assert_eq!(LlmRouter::parse_tool_selection("", &valid), None);
        assert_eq!(
            LlmRouter::parse_tool_selection(r#"{"wrong_field": "x"}"#, &valid),
            None
        );
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
            temporal_tensions: Vec::new(),
            compacted_history: None,
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
            temporal_tensions: Vec::new(),
            compacted_history: None,
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

    // ── suggest_alternatives (Ask-move heuristic) ───────────────

    fn web_search_tool() -> ToolDescriptor {
        ToolDescriptor {
            id: "web_search".to_string(),
            name: "web_search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::External,
            output_schema: None,
        }
    }

    #[test]
    fn alternatives_temporal_suggests_web_search_when_tool_available() {
        let alts = suggest_alternatives(
            "what's the latest on the election?",
            &Intent::DeepQuery,
            &[web_search_tool()],
        );
        let has_web = alts.iter().any(|c| matches!(&c.intent, Intent::SimpleAction { tool } if tool == "web_search"));
        assert!(has_web, "temporal + web tool should surface SimpleAction: {alts:?}");
    }

    #[test]
    fn alternatives_temporal_without_tool_skips_web() {
        let alts = suggest_alternatives(
            "what's the latest on the election?",
            &Intent::DeepQuery,
            &[],
        );
        let has_web = alts.iter().any(|c| matches!(&c.intent, Intent::SimpleAction { .. }));
        assert!(!has_web, "no web tool → no SimpleAction alternative");
    }

    #[test]
    fn alternatives_how_does_suggests_deep() {
        let alts = suggest_alternatives(
            "how does the scheduler work?",
            &Intent::SimpleQuery,
            &[],
        );
        assert!(alts.iter().any(|c| matches!(c.intent, Intent::DeepQuery)));
    }

    #[test]
    fn alternatives_excludes_primary() {
        // Primary is DeepQuery; even though the message carries a
        // "how does" signal, the DeepQuery alternative must be
        // omitted because it equals the primary.
        let alts = suggest_alternatives(
            "how does the scheduler work?",
            &Intent::DeepQuery,
            &[],
        );
        assert!(!alts.iter().any(|c| matches!(c.intent, Intent::DeepQuery)));
    }

    #[test]
    fn alternatives_definitional_suggests_simple() {
        let alts = suggest_alternatives("what is a mesh?", &Intent::DeepQuery, &[]);
        assert!(alts.iter().any(|c| matches!(c.intent, Intent::SimpleQuery)));
    }

    #[test]
    fn alternatives_capped_at_three() {
        // Message hits all signals at once.
        let alts = suggest_alternatives(
            "what is the latest on how does the scheduler work — look up the paper",
            &Intent::SimpleQuery,
            &[web_search_tool()],
        );
        assert!(alts.len() <= 3, "alternatives must be capped: {alts:?}");
    }

    #[test]
    fn alternatives_empty_on_vague_message() {
        // No trigger keywords at all.
        let alts = suggest_alternatives(
            "hmm interesting point",
            &Intent::SimpleQuery,
            &[web_search_tool()],
        );
        assert!(alts.is_empty(), "nothing matched → no alternatives: {alts:?}");
    }
}
