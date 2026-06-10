// SPDX-License-Identifier: AGPL-3.0-or-later
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

const SELF_ASSESSMENT_PROMPT: &str = include_str!("../assets/self_assessment_prompt.md");

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
/// Surfaced by sovereign/bench/wikipedia_learn 2026-05-17 marathon
/// (v9→v10).
fn inherits_prior_knowledge_intent(context: &ConversationContext) -> Option<Intent> {
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
        "latest",
        "today",
        "current",
        "recent",
        "this week",
        "this month",
        "right now",
        "news",
        "price",
        "score",
        "weather",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if wants_current && has_web {
        if let Some(t) = available_tools
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            tracing::debug!(
                tool = %t.id,
                "router:web_search_candidate — temporal signal + web tool present, offering web_search"
            );
            out.push(IntentCandidate {
                intent: Intent::SimpleAction { tool: t.id.clone() },
                confidence: 0.6,
            });
        }
    }

    // Deep-reasoning signal → DeepQuery.
    let wants_deep = [
        "how does",
        "explain",
        "walk me through",
        "compare",
        "contrast",
        "why does",
        "analyze",
        "analyse",
        "relationship between",
        "difference between",
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
        "according to",
        "in the",
        "from the",
        "the document",
        "chapter",
        "paper",
        "book",
        "find",
        "lookup",
        "look up",
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
    /// Optional binary effort classifier (high/low). When installed it runs
    /// off the same query embedding as the scope classifier and supplies the
    /// deterministic effort signal the flaky 4B coarse verdict lacks — a
    /// `High` verdict escalates a referential `Answer` to `DeepQuery` (primary
    /// slot). See `effort_classifier.rs` + `docs/QUERY_TAXONOMY_MECE.md`.
    effort_classifier: Option<Arc<crate::effort_classifier::EffortClassifier>>,
    /// Optional binary classifier for the current-info (time-sensitive
    /// → external search) axis. When installed it REPLACES the
    /// `needs_current_info` keyword heuristic that drives the
    /// `force_action` pre-check — a semantic centroid decision instead
    /// of substring matching, so "…history of Lebanon from antiquity to
    /// today" no longer trips on the bare word "today". Falls back to
    /// the keyword heuristic when absent (tests / minimal configs).
    /// Installed via `with_current_info_classifier`.
    current_info_classifier:
        Option<Arc<crate::current_info_classifier::CurrentInfoClassifier>>,
    /// Cache of L2-normalised embeddings of the registered tools'
    /// described purposes, keyed by the (order-independent) tool-id set.
    /// Powers the tool-relevance gate (see `query_best_tool_sim`).
    /// Computed once per stable tool set — the daemon's set is fixed at
    /// startup, so this amortises to a single embed pass over the
    /// process lifetime.
    tool_embed_cache: std::sync::RwLock<Option<ToolEmbedCache>>,
}

/// Cached tool-purpose embeddings for the tool-relevance gate.
struct ToolEmbedCache {
    /// Order-independent hash of the tool-id set these embeddings cover.
    key: u64,
    /// `(tool_id, L2-normalised purpose embedding)` per registered tool.
    tools: Vec<(String, Vec<f32>)>,
}

/// Intents that retrieve-or-respond *without* invoking a tool. When the
/// embed router lands a query on one of these but the query matches a
/// registered tool's purpose more than any knowledge exemplar, the query
/// is really a tool invocation — so it is deferred to the tool-aware
/// Pass-2 rather than committed here. See the tool-relevance gate in
/// `classify`.
fn intent_is_toolless(intent: &Intent) -> bool {
    !matches!(
        intent,
        Intent::SimpleAction { .. } | Intent::ComplexTask | Intent::Continuation { .. }
    )
}

/// Order-independent hash of a tool set's ids: the embedding cache
/// survives tool-registration reordering but invalidates if the set
/// itself changes.
fn tool_set_key(tools: &[ToolDescriptor]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for id in ids {
        id.hash(&mut h);
    }
    h.finish()
}

/// L2-normalise in place (no-op on a zero vector).
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Best-matching `(tool_id, cosine)` between an L2-normalised query
/// embedding and a set of L2-normalised tool-purpose embeddings (cosine
/// = dot product on normalised vectors).
fn best_tool_cosine(q_normalized: &[f32], tools: &[(String, Vec<f32>)]) -> Option<(String, f32)> {
    tools
        .iter()
        .filter(|(_, e)| e.len() == q_normalized.len())
        .map(|(id, e)| {
            let sim: f32 = q_normalized.iter().zip(e).map(|(a, b)| a * b).sum();
            (id.clone(), sim)
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
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
            effort_classifier: None,
            current_info_classifier: None,
            tool_embed_cache: std::sync::RwLock::new(None),
        }
    }

    /// Install an `EmbedRouter` as the first pre-check. When the
    /// embed classifier returns a confident result the router skips
    /// every downstream heuristic + the LLM Pass 1/2 calls.
    pub fn with_embed_router(mut self, embed: Arc<EmbedRouter>) -> Self {
        self.embed_router = Some(embed);
        self
    }

    /// Best `(tool_id, cosine)` between the (already L2-normalised) query
    /// embedding and any registered tool's described purpose, or `None`
    /// when there are no tools / embedding is unavailable. Tool-purpose
    /// embeddings are computed once per stable tool set and cached
    /// (`tool_embed_cache`), so per-query cost is just N dot products.
    /// Tools are embedded with `embed_query` — the same space the query
    /// and the intent exemplars live in — so the cosine is directly
    /// comparable to the embed router's `top_sim`.
    async fn query_best_tool_sim(
        &self,
        q_normalized: &[f32],
        tools: &[ToolDescriptor],
    ) -> Option<(String, f32)> {
        if tools.is_empty() {
            return None;
        }
        let key = tool_set_key(tools);
        // Fast path: cached embeddings for this exact tool set.
        if let Ok(guard) = self.tool_embed_cache.read() {
            if let Some(cache) = guard.as_ref() {
                if cache.key == key {
                    return best_tool_cosine(q_normalized, &cache.tools);
                }
            }
        }
        // Miss: embed each tool's "name. description" (no lock held across
        // the await), then cache for the rest of the process.
        let mut embedded: Vec<(String, Vec<f32>)> = Vec::with_capacity(tools.len());
        for t in tools {
            let text = format!("{}. {}", t.name, t.description);
            match self.inference.embed_query(&text).await {
                Ok(mut v) => {
                    l2_normalize(&mut v);
                    embedded.push((t.id.clone(), v));
                }
                Err(e) => tracing::warn!(
                    target: "router.tool_relevance",
                    tool = %t.id,
                    error = %e,
                    "tool-purpose embed failed; tool excluded from relevance gate"
                ),
            }
        }
        if embedded.is_empty() {
            return None;
        }
        let best = best_tool_cosine(q_normalized, &embedded);
        if let Ok(mut guard) = self.tool_embed_cache.write() {
            *guard = Some(ToolEmbedCache { key, tools: embedded });
        }
        best
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

    /// Install the binary effort classifier. Runs off the same query embedding
    /// as the scope classifier; a `High` verdict escalates a referential
    /// `Answer` (KnowledgeQuery/SimpleQuery) to `DeepQuery` so exhaustive asks
    /// reach the primary slot deterministically.
    pub fn with_effort_classifier(
        mut self,
        classifier: Arc<crate::effort_classifier::EffortClassifier>,
    ) -> Self {
        self.effort_classifier = Some(classifier);
        self
    }

    /// Install a binary current-info classifier. When present it drives
    /// the `force_action` pre-check instead of the `needs_current_info`
    /// keyword heuristic — a semantic decision over time-sensitivity.
    pub fn with_current_info_classifier(
        mut self,
        classifier: Arc<crate::current_info_classifier::CurrentInfoClassifier>,
    ) -> Self {
        self.current_info_classifier = Some(classifier);
        self
    }

    /// Pass 1: Coarse classification into one of three buckets.
    /// Each is a simple yes/no-like question the small model handles well.
    ///
    /// Skill-based trigger phrases were retired alongside the
    /// skills-as-menu UI; the wisdom they encoded was migrated into
    /// the embed-exemplar bank (`sovereign/router/exemplars.toml`)
    /// where it informs classification at every turn rather than
    /// only when the matching skill happened to be activated.
    fn build_pass1_prompt(
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
        corrections: &[RoutingCorrection],
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
            format!("\n\nPrevious classification mistakes (avoid these):\n{examples}")
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

        // 2026-05-21 compression pass: condensed from ~1000 words →
        // ~520 words for ~9B router model context budget. SHAPE-level
        // signals + critical NOT-X disambiguations preserved; one
        // example per category (was three); INFERENCE header slimmed.
        // Cells_v1 + voice_routing_v1 + skills_migration_smoke gate
        // any future cuts.
        format!(
            r#"Classify this message into exactly ONE category.

═══ SITUATED CONTEXT ═══
{context_str}

Installed knowledge sources: {corpus_list}
Other available tools: {tool_list}

═══ INFERENCE ═══
Pick the intent for the user's actual MOVE, not the surface form.
"shorter please" after a long answer is CONATION (transform prior
turn), not a question. "I'm stuck" while debugging is EXPRESSIVE
with implicit help-request, not SIMPLE.

═══ Categories ═══

SIMPLE
  Pure reasoning / math / definitions / logic — one universally
  known answer, no lookup needed.
  NOT SIMPLE: contested philosophy / ethics / metaphysics (free
  will, consciousness, moral realism) → REASONING.
  Example: "If all A are B and all B are C, are all A C?"

LOOKUP
  One specific atomic fact: names, dates, statistics, records.
  When knowledge sources are installed, prefer LOOKUP over SIMPLE
  for atomic facts. When in doubt: LOOKUP.
  NOT LOOKUP: causes / effects / reasons / multi-source aggregation
  → REASONING. "Difference between X and Y" → COMPARISON.
  Example: "What year was the Eiffel Tower built?"

COMPARISON
  Two named entities contrasted on bounded axes. Fits in three
  bullets, not an essay.
  Signals: "difference between X and Y", "X vs Y", "compare X and Y".
  Example: "What's the difference between TCP and UDP?"

REASONING
  Open-ended synthesis or multi-step thinking. Aggregates evidence
  across facts WITHOUT the bounded contrast shape of COMPARISON.
  Causes, effects, "how did X relate to Y", multi-actor attributions.
  Example: "What were the main causes of the 2008 financial crisis?"

ACTION
  Needs an external-reach tool: web, email, calendar, files, shell.
  Use only when no installed knowledge source could answer. Current
  events / live data / today's news are ACTION.
  NOT ACTION: processing prompt-embedded content ("summarize this",
  "explain this passage") is REASONING, even with imperative verbs.
  Example: "What time is it in Tokyo right now?"

CONATION
  Short imperative directed at the assistant about its last reply.
  Typically 1-4 words, no question mark, no proper nouns. The user
  is asking to TRANSFORM what was just produced.
  Example: "Shorter please"

COMMISSION
  First-person future commitment ("I'll", "I'm going to",
  "remind me to"). Persists a commitment, not an immediate answer.
  NOT COMMISSION: memory-recall framings ("Remember when…",
  "You mentioned X") are EXPRESSIVE or REASONING.
  Example: "Remind me to review this Friday"

EXPRESSIVE
  Short emotive disclosure with implicit help-request ("stuck",
  "frustrated", "no idea"). Surface looks like a statement, the
  move is "help me unstick".
  Example: "I'm stuck on this"

METALINGUAL
  Asks how a term is USED by a named anchor — this system, this
  conversation, or a named external source. Meta to language, not
  the fact itself.
  Signals: "in this codebase / conversation", "we discussed",
  "according to <source>", "how does <source> define".
  Example: "According to Wikipedia, what does 'recursion' mean?"
  NOT METALINGUAL: bare definitions with no anchor are SIMPLE / LOOKUP.

User message: "{message}"{corrections_note}

Respond with JSON only:
{{"intent": "SIMPLE|LOOKUP|COMPARISON|REASONING|ACTION|CONATION|COMMISSION|EXPRESSIVE|METALINGUAL"}}"#,
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
                let facts = wm
                    .facts
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ");
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
            "compare",
            "contrast",
            "analyze",
            "analyse",
            "explain how",
            "explain why",
            "explain the difference",
            "what are the arguments",
            "what are the implications",
            "evaluate",
            "critically",
            "assess",
            "discuss",
            "debate",
            "reconcile",
            "how does",
            "why does",
            "in what ways",
            "pros and cons",
            "advantages and disadvantages",
            // "relationship between" / "difference between" removed
            // 2026-05-21: those are bounded-comparison signals, not
            // deep-reasoning. The embed router's comparison_query
            // cluster catches them faster + more accurately than
            // forcing them to deep_query here.
            "summarize the",
            "summarise the",
            "history of",
            "overview of",
            "evolution of",
            "how have",
            "how has",
            // Structural causal patterns (general — any causal-shape
            // question, not bank-specific):
            "how did",
            "why did",
            "what were the consequences",
            "what were the effects",
            "what were the causes",
            "what were the implications",
            "what led to",
            "what caused",
            "contribute to",
            "contributed to",
            "influence on",
            "influenced the",
        ];

        // Complex conceptual domains where even short questions require reasoning.
        let complex_domains = [
            "free will",
            "determinism",
            "compatibilism",
            "incompatibilism",
            "consciousness",
            "qualia",
            "hard problem",
            "epistemology",
            "ontology",
            "metaphysics",
            "phenomenology",
            "moral realism",
            "ethics",
            "morality",
            "normative",
            "political philosophy",
            "social contract",
            "justice",
            "dialectic",
            "existentialism",
            "absurdism",
            "artificial general intelligence",
            "alignment problem",
            "philosophy of mind",
            "philosophy of language",
            "emergence",
            "supervenience",
            "reduction",
        ];

        // Compatibility/tension questions — always require reasoning regardless of domain.
        let tension_markers = [
            "compatible",
            "incompatible",
            "consistent with",
            "inconsistent with",
            "reconcile",
            "tension between",
            "can both",
            "are both",
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
            think_budget: Some(0), // suppress thinking — prevents Qwen <think> consuming the 5-token budget
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
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
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
        };
        let response = self.inference.complete(&request).await?;
        eprintln!("[router] classify_json raw output: {:?}", response.text);
        Ok(response.text)
    }

    /// Schema-constrained call for Pass 2.5 tool selection. The
    /// schema's `tool` enum is the set of registered tool ids — the
    /// constraint enforcer mathematically can't emit anything else.
    async fn classify_call_tool_json(&self, prompt: String, tool_ids: &[String]) -> Result<String> {
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
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
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
        // §9.1 glassbox: the pass-2 letter decides the whole execution path
        // (action vs knowledge vs complex). Surface it so an operator can see
        // why a request was routed the way it was without re-running.
        let intent_label = match refined {
            'A' => "SimpleAction",
            'C' => "KnowledgeQuery",
            _ => "ComplexTask",
        };
        tracing::debug!(
            refined_char = %refined,
            intent = intent_label,
            "router:intent_decision — pass-2 refined classification"
        );
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
        let prompt = Self::build_pass2_tool_selection_prompt(message, context, available_tools);
        let tool_ids: Vec<String> = available_tools.iter().map(|t| t.id.clone()).collect();
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
        let after_think =
            if let (Some(start), Some(end)) = (raw.find("<think>"), raw.find("</think>")) {
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
        // Robust extraction (gated on SOVEREIGN_ROUTER_ROBUST_COARSE).
        // Small fast models (4B-IQ/Q8) frequently emit the JSON verdict and
        // then LEAK chain-of-thought after it — e.g.
        //   {"intent":"REASONING"}\n</think>\n\nThe user is asking…
        // Note the LONE `</think>` with no opener: the `<think>` strip above
        // requires both tags, so it's skipped, `serde_json::from_str` fails on
        // the trailing prose, and we `unwrap_or_default()` to an empty intent →
        // the `_ => KnowledgeQuery` arm in `classify`, silently dropping a
        // correct REASONING→DeepQuery verdict (the maximal-essay misroute,
        // 2026-06-09). Extract the first balanced `{…}` object and parse that.
        // Flag default-off so the A/B is flag-off (current) vs on; flip the
        // default once the bench confirms the recovered escalations don't
        // regress sep/wiki/marathon.
        let candidate = if Self::robust_coarse_enabled() {
            Self::extract_first_json_object(cleaned).unwrap_or(cleaned)
        } else {
            cleaned
        };
        serde_json::from_str(candidate).unwrap_or_default()
    }

    /// Gate for the robust first-JSON-object extraction in
    /// [`Self::parse_coarse`]. **Default ON** (2026-06-09): recovers the 4B's
    /// `REASONING` verdict it was silently dropping (lone `</think>` after a
    /// CoT leak). Validated by the chaos + SEP/Wiki no-regression A/B.
    /// Set `SOVEREIGN_ROUTER_ROBUST_COARSE=0` (or empty) to disable.
    fn robust_coarse_enabled() -> bool {
        std::env::var("SOVEREIGN_ROUTER_ROBUST_COARSE").map_or(true, |v| v != "0" && !v.is_empty())
    }

    /// Gate for effort-tier escalation (high-effort `Answer` → `DeepQuery` →
    /// primary slot). **Default ON** (2026-06-09): SEP/Wiki no-regression PASS
    /// — judge SEP +0.036 / Wiki flat, no question regressed, SEP source recall
    /// +0.467, the long-stubborn trolley question's judge +0.143. Escalation is
    /// monotone (only promotes; never downgrades) and conservative (6/41 flips).
    /// Set `SOVEREIGN_KQ_EFFORT_TIER=0` (or empty) to disable.
    fn effort_tier_enabled() -> bool {
        std::env::var("SOVEREIGN_KQ_EFFORT_TIER").map_or(true, |v| v != "0" && !v.is_empty())
    }

    /// Promote a referential `Answer` intent (`KnowledgeQuery`/`SimpleQuery`) to
    /// `DeepQuery` (→ primary slot) when the effort verdict is `High`. The MECE
    /// effort axis (`docs/QUERY_TAXONOMY_MECE.md`): same operation, higher
    /// effort → bigger model. Leaves `Compare`/`Enumerate`/non-referential
    /// intents untouched (Comparison stays fast — a preserved bench learning).
    /// No-op unless [`Self::effort_tier_enabled`].
    fn escalate_for_effort(intent: Intent, effort: Option<Effort>) -> Intent {
        if !Self::effort_tier_enabled() {
            return intent;
        }
        Self::apply_effort_escalation(intent, effort)
    }

    /// Pure escalation mapping (no env gate) — the unit-testable core of
    /// [`Self::escalate_for_effort`].
    fn apply_effort_escalation(intent: Intent, effort: Option<Effort>) -> Intent {
        if !matches!(effort, Some(Effort::High)) {
            return intent;
        }
        match intent {
            Intent::KnowledgeQuery | Intent::SimpleQuery => Intent::DeepQuery,
            other => other,
        }
    }

    /// Return the first balanced `{…}` JSON object substring in `s`, tolerant
    /// of leading/trailing prose and a lone `</think>` the model leaks after
    /// its verdict. String-aware (braces inside `"…"` don't count) with
    /// backslash escaping. `None` when there is no balanced object.
    fn extract_first_json_object(s: &str) -> Option<&str> {
        let start = s.find('{')?;
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        for (i, c) in s[start..].char_indices() {
            if esc {
                esc = false;
                continue;
            }
            match c {
                '\\' if in_str => esc = true,
                '"' => in_str = !in_str,
                '{' if !in_str => depth += 1,
                '}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&s[start..start + i + c.len_utf8()]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Parse a Pass 2.5 tool-selection JSON response. Returns
    /// `Some(tool_id)` only when the parsed `tool` field is one of
    /// `valid_ids`. Tolerant of `<think>` blocks and code fences for
    /// the rare case the schema constraint is bypassed (e.g. legacy
    /// providers without `JsonConstraint` wired in).
    fn parse_tool_selection(raw: &str, valid_ids: &[String]) -> Option<String> {
        let after_think =
            if let (Some(start), Some(end)) = (raw.find("<think>"), raw.find("</think>")) {
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
                    "simple" | "deep" | "knowledge" | "action" | "complex" | "continuation"
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
                "the document",
                "the paper",
                "the article",
                "the book",
                "chapter",
                "page",
                "paragraph",
                "section",
                "the author writes",
                "according to the text",
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
            "what are the",
            "what is the",
            "how does",
            "how do",
            "core differences",
            "main differences",
            "key differences",
            "compare",
            "contrast",
            "relationship between",
            "explain",
            "define",
            "describe",
        ];

        let is_general = general_knowledge_signals
            .iter()
            .any(|p| msg_lower.contains(p));

        // Pronoun-heavy short follow-ups in an established conversation
        // are likely continuations that can be answered from general knowledge.
        let pronoun_patterns = [
            "he ", "she ", "they ", "it ", "his ", "her ", "their ", "that ",
        ];
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

        // Pre-check -2: inherit prior knowledge-family intent when
        // the conversation already has an established knowledge
        // thread. Structural detector — keys off
        // `prior_assistant.metadata.intent`, no lexical pattern
        // matching on the current message. See
        // `inherits_prior_knowledge_intent` for the full rationale.
        if let Some(inherited) = inherits_prior_knowledge_intent(context) {
            let latency_ms = start.elapsed().as_millis() as i64;
            let hash = message_hash(message);
            let intent_str = format!("{inherited:?}");
            let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
            let _ = self
                .store
                .log_routing_meta(&hash, "KNOWLEDGE_THREAD_INHERIT", None)
                .await;
            eprintln!(
                "[router] \"{}\" → {:?} (knowledge thread; inherited from prior turn)",
                &message[..message.len().min(60)],
                inherited,
            );
            return Ok(RouterClassification {
                primary: IntentCandidate {
                    intent: inherited,
                    confidence: 0.9,
                },
                alternatives: Vec::new(),
                rationale: Some(
                    "knowledge thread continuation — inherited intent from prior turn".into(),
                ),
                coarse_intent: Some("KNOWLEDGE_THREAD_INHERIT".to_string()),
                self_assessment: None,
                timing: None,
                scope: None,
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
        // Effort verdict (high/low). Computed up front via the effort
        // classifier's OWN unprefixed embed (NOT the embed-router's prefixed
        // `embed_query` embedding — the retrieval prefix collapses the effort
        // signal, see effort_classifier.rs). Hoisted here so it survives to
        // BOTH the embed-confident return and the coarse-fallback return — the
        // maximal asks abstain on the embed intent and resolve via the coarse
        // path, so the escalation must apply there too. The extra embed is paid
        // only when escalation is enabled. `None` otherwise.
        let effort: Option<Effort> = if Self::effort_tier_enabled() {
            match self.effort_classifier.as_ref() {
                Some(ec) => ec.classify(message, &*self.inference).await.ok().flatten(),
                None => None,
            }
        } else {
            None
        };

        if let Some(embed) = self.embed_router.as_ref() {
            match embed
                .classify_returning_embedding(message, &*self.inference)
                .await
            {
                Ok((intent_verdict, query_embedding)) => {
                    // Scope classifier runs off the embed-router's query
                    // embedding (single embed call serves both intent + scope).
                    if let Some(scope_cls) = self.scope_classifier.as_ref() {
                        scope_hint = scope_cls.classify_from_embedding(&query_embedding);
                    }
                    if let Some(verdict) = intent_verdict {
                        // ── Tool-relevance gate (generalized) ──────────
                        // The embed router ranks the query against
                        // KNOWLEDGE/intent exemplars only; it has no view of
                        // the registered tools. A query whose wording matches
                        // a tool's described purpose MORE than any knowledge
                        // exemplar is really a tool invocation (e.g. "compute
                        // the land-value-tax rate" with a `parcel_analytics`
                        // tool present). Neither the embed router's tool-less
                        // verdict nor the LLM coarse pass — which reads such a
                        // query as "reasoning" — routes it to a tool-capable
                        // path, so the tool never runs. When a registered tool
                        // clearly out-matches the nearest exemplar, commit to
                        // the agentic ComplexTask path here: its planner +
                        // executor decide which tool(s) to run, and the
                        // cited-synthesis + numeric-audit gate apply. The
                        // relative `tool_sim > top_sim` rule keeps this
                        // targeted — a genuine knowledge question matches a
                        // knowledge exemplar best and is unaffected.
                        let tool_match = if intent_is_toolless(&verdict.intent) {
                            self.query_best_tool_sim(&query_embedding, available_tools)
                                .await
                                .filter(|(_, tool_sim)| *tool_sim > verdict.top_sim)
                        } else {
                            None
                        };
                        if let Some((tool_id, tool_sim)) = tool_match {
                            let latency_ms = start.elapsed().as_millis() as i64;
                            let hash = message_hash(message);
                            let _ = self.store.log_routing(&hash, "ComplexTask", latency_ms).await;
                            let _ = self
                                .store
                                .log_routing_meta(&hash, "TOOL_RELEVANCE", None)
                                .await;
                            eprintln!(
                                "[router] \"{}\" embed→{:?} (sim={:.3}) → ComplexTask: registered tool '{}' matches the query more closely (tool_sim={:.3}); the agentic path will plan the tool call",
                                &message[..message.len().min(50)],
                                verdict.intent,
                                verdict.top_sim,
                                tool_id,
                                tool_sim,
                            );
                            return Ok(RouterClassification {
                                primary: IntentCandidate {
                                    intent: Intent::ComplexTask,
                                    confidence: 0.9,
                                },
                                alternatives: Vec::new(),
                                rationale: Some(format!(
                                    "tool-relevance gate: tool '{}' (cosine {:.3}) out-matched nearest exemplar {:?} (cosine {:.3}) — routed to the agentic path",
                                    tool_id, tool_sim, verdict.nearest_exemplar, verdict.top_sim
                                )),
                                coarse_intent: Some("TOOL_RELEVANCE".to_string()),
                                self_assessment: None,
                                timing: None,
                                scope: scope_hint.clone(),
                            });
                        }
                        // Effort escalation: a confident embed intent that is a
                        // referential Answer (Knowledge/Simple) but high-effort
                        // is promoted to DeepQuery (primary slot). Flag-gated.
                        let routed = Self::escalate_for_effort(verdict.intent.clone(), effort);
                        let latency_ms = start.elapsed().as_millis() as i64;
                        let hash = message_hash(message);
                        let intent_str = format!("{routed:?}");
                        let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
                        let _ = self
                            .store
                            .log_routing_meta(&hash, "EMBED_ROUTER", None)
                            .await;
                        eprintln!(
                            "[router] \"{}\" → {:?} (embed: sim={:.3} margin={:.3} nearest={:?} scope={:?} effort={:?})",
                            &message[..message.len().min(50)],
                            routed,
                            verdict.top_sim,
                            verdict.margin,
                            verdict.nearest_exemplar,
                            scope_hint,
                            effort,
                        );
                        return Ok(RouterClassification {
                            primary: IntentCandidate {
                                intent: routed,
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
            let _ = self
                .store
                .log_routing_meta(&hash, "TOPIC_CONTINUITY", None)
                .await;

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

        // Pre-check 1: temporal/current-info → force ACTION (search).
        // Small models are unreliable at detecting these. When a binary
        // current-info classifier is installed, make this call
        // SEMANTICALLY (centroid cosine over time-sensitivity) instead
        // of substring-matching a keyword list. The keyword path forced
        // "…history of Lebanon from antiquity to today" to ACTION on the
        // bare word "today" → synthesis then treated its 19 retrieved
        // chunks as inadequate and refused. The embed call is paid only
        // when a search tool is present — i.e. exactly when `force_action`
        // could fire — so the common path pays nothing. Falls back to the
        // keyword heuristic on classify error or when no classifier is wired.
        let has_search = available_tools.iter().any(|t| t.name.contains("search"));
        let force_action = if has_search {
            match self.current_info_classifier.as_ref() {
                Some(cls) => match cls.classify(message, &*self.inference).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            target: "router.current_info",
                            error = %e,
                            "current-info classify failed; falling back to keyword heuristic"
                        );
                        Self::needs_current_info(message)
                    }
                },
                None => Self::needs_current_info(message),
            }
        } else {
            false
        };

        // Pre-check 3: content-processing signal → force REASONING. Catches
        // "summarize this", "explain this passage", "compare these sections"
        // etc. which the Fast model sometimes misreads as ACTION because of
        // the imperative verb. Content processing never needs external reach.
        let force_content_reasoning =
            !force_action && Self::looks_like_content_processing(message);

        // Pre-check 4: deep reasoning signal → force REASONING before the LLM sees it.
        // This catches philosophical, analytical, and compatibility questions that
        // small fast models frequently mis-classify as SimpleQuery.
        let force_deep = !force_action
            && !force_content_reasoning
            && Self::needs_deep_reasoning(message);

        // Iter6: pre-check chain done. Cap timer here — anything
        // after this is either zero-cost branch selection or the
        // LLM Pass 1.
        let precheck_ms = precheck_start.elapsed().as_millis() as u64;
        let mut llm_ms: u64 = 0;
        let mut parse_ms: u64 = 0;
        let mut used_llm = false;

        // Pass 1: Coarse classification (skipped for pre-checked cases).
        let coarse = if force_action {
            CoarseClassification {
                intent: "ACTION".to_string(),
                confidence: 1.0,
                rationale: Some("current/time-sensitive signal → external tool".to_string()),
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
        } else {
            let pass1_prompt =
                Self::build_pass1_prompt(message, context, available_tools, &corrections);
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
            "ACTION" => (
                self.pass2_refine(message, context, available_tools).await?,
                None,
            ),
            "SIMPLE" => {
                self.assess_simple_query(message, context, coarse.confidence)
                    .await?
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

        // Effort escalation on the coarse-resolved intent. This is the path the
        // maximal-essay asks take (they abstain on the embed intent), so the
        // deterministic effort verdict here is what makes their escalation to
        // the primary slot reliable — independent of the flaky 4B coarse
        // verdict. Flag-gated; only promotes referential Answer intents.
        let intent = Self::escalate_for_effort(intent, effort);

        let latency_ms = start.elapsed().as_millis() as i64;

        // Log routing decision.
        let hash = message_hash(message);
        let intent_str = format!("{intent:?}");
        let _ = self.store.log_routing(&hash, &intent_str, latency_ms).await;
        let _ = self
            .store
            .log_routing_meta(&hash, &coarse.intent, self_assessment_outcome.as_deref())
            .await;

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
    fn tool_relevance_gate_helpers() {
        // Tool-less intents (retrieve/respond) are gated; tool-capable
        // intents are not — the gate only ever fires on the former.
        assert!(intent_is_toolless(&Intent::KnowledgeQuery));
        assert!(intent_is_toolless(&Intent::DeepQuery));
        assert!(!intent_is_toolless(&Intent::ComplexTask));
        assert!(!intent_is_toolless(&Intent::SimpleAction { tool: "x".into() }));

        // l2_normalize + best_tool_cosine: a normalised query matches an
        // identical tool vector at cosine ≈ 1.0 and picks it over an
        // orthogonal one (0.0).
        let mut q = vec![3.0_f32, 4.0];
        l2_normalize(&mut q); // → (0.6, 0.8)
        let mut same = vec![3.0_f32, 4.0];
        l2_normalize(&mut same);
        let mut orth = vec![4.0_f32, -3.0];
        l2_normalize(&mut orth);
        let tools = vec![("orth".to_string(), orth), ("same".to_string(), same)];
        let (id, sim) = best_tool_cosine(&q, &tools).expect("a best match exists");
        assert_eq!(id, "same");
        assert!((sim - 1.0).abs() < 1e-6, "cosine with identical vector ≈ 1.0, got {sim}");
        // The matched tool's cosine exceeds the orthogonal alternative (0.0),
        // i.e. it would clear the relative `tool_sim > top_sim` rule.
        assert!(sim > 0.0);
    }

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
                enabled_corpora: None,
                searched_sources: None,
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
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
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
                enabled_corpora: None,
                searched_sources: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
        };

        let summary = LlmRouter::format_context_summary(&ctx);
        assert!(summary.contains("user: Hello there"));
    }

    #[test]
    fn needs_current_info_recent_year() {
        assert!(LlmRouter::needs_current_info(
            "Who won the Nobel Prize in 2025?"
        ));
        assert!(LlmRouter::needs_current_info("What happened in 2024?"));
        assert!(!LlmRouter::needs_current_info("What happened in 1969?"));
    }

    #[test]
    fn needs_current_info_temporal_keywords() {
        assert!(LlmRouter::needs_current_info("What is the latest news?"));
        assert!(LlmRouter::needs_current_info(
            "What's the current price of Bitcoin?"
        ));
        assert!(LlmRouter::needs_current_info("Who won the game today?"));
        assert!(LlmRouter::needs_current_info("What's the weather like?"));
        assert!(LlmRouter::needs_current_info("Who won the election?"));
    }

    #[test]
    fn needs_current_info_search_requests() {
        assert!(LlmRouter::needs_current_info(
            "Search for restaurants near me"
        ));
        assert!(LlmRouter::needs_current_info(
            "Can you look up flight prices?"
        ));
        assert!(LlmRouter::needs_current_info("Google the EU AI Act"));
    }

    #[test]
    fn needs_current_info_false_for_general() {
        assert!(!LlmRouter::needs_current_info("What is recursion?"));
        assert!(!LlmRouter::needs_current_info("Explain photosynthesis"));
        assert!(!LlmRouter::needs_current_info("Hello, how are you?"));
        assert!(!LlmRouter::needs_current_info(
            "Write a poem about the ocean"
        ));
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
        let c =
            LlmRouter::parse_coarse("```json\n{\"intent\":\"SIMPLE\",\"confidence\":0.95}\n```");
        assert_eq!(c.intent, "SIMPLE");
    }

    #[test]
    fn parse_coarse_garbage_returns_default() {
        let c = LlmRouter::parse_coarse("I cannot classify this message.");
        assert_eq!(c.intent, "");
        assert_eq!(c.confidence, 0.0);
    }

    #[test]
    fn extract_first_json_object_recovers_leaked_verdict() {
        // The maximal-essay misroute (2026-06-09): the 4B emits the verdict and
        // then LEAKS CoT after it, with a LONE `</think>` (no opener). The bare
        // `serde_json::from_str` on this fails → empty default → KnowledgeQuery,
        // dropping the correct REASONING→DeepQuery verdict.
        let raw = "{\"intent\": \"REASONING\"}\n</think>\n\nThe user is asking for an exhaustive account…";
        let obj = LlmRouter::extract_first_json_object(raw).expect("first JSON object");
        assert_eq!(obj, "{\"intent\": \"REASONING\"}");
        // Recovered object parses to the correct intent.
        let parsed: CoarseClassification = serde_json::from_str(obj).unwrap();
        assert_eq!(parsed.intent, "REASONING");
    }

    #[test]
    fn apply_effort_escalation_promotes_only_answer_intents_on_high() {
        use crate::types::Effort;
        // High effort promotes the referential Answer intents to DeepQuery.
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::KnowledgeQuery, Some(Effort::High)),
            Intent::DeepQuery
        );
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::SimpleQuery, Some(Effort::High)),
            Intent::DeepQuery
        );
        // Low / absent effort never escalates.
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::KnowledgeQuery, Some(Effort::Low)),
            Intent::KnowledgeQuery
        );
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::KnowledgeQuery, None),
            Intent::KnowledgeQuery
        );
        // Non-Answer intents are never escalated, even at High effort
        // (Comparison stays fast — preserved bench learning).
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::ComparisonQuery, Some(Effort::High)),
            Intent::ComparisonQuery
        );
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::DeepQuery, Some(Effort::High)),
            Intent::DeepQuery
        );
        assert_eq!(
            LlmRouter::apply_effort_escalation(Intent::MetalingualQuery, Some(Effort::High)),
            Intent::MetalingualQuery
        );
    }

    #[test]
    fn extract_first_json_object_handles_clean_prose_and_strings() {
        // Clean object unchanged.
        assert_eq!(
            LlmRouter::extract_first_json_object(r#"{"intent":"LOOKUP"}"#),
            Some(r#"{"intent":"LOOKUP"}"#)
        );
        // Leading prose before the object.
        assert_eq!(
            LlmRouter::extract_first_json_object(r#"Sure: {"intent":"SIMPLE"} done"#),
            Some(r#"{"intent":"SIMPLE"}"#)
        );
        // Braces inside a string value don't unbalance the scan.
        assert_eq!(
            LlmRouter::extract_first_json_object(r#"{"intent":"X","r":"a{b}c"}"#),
            Some(r#"{"intent":"X","r":"a{b}c"}"#)
        );
        // No JSON object present.
        assert_eq!(LlmRouter::extract_first_json_object("no json here"), None);
    }

    // ── parse_self_assessment ───────────────────────────────────

    #[test]
    fn parse_self_assessment_uncertain() {
        assert!(matches!(
            parse_self_assessment("UNCERTAIN"),
            SelfAssessment::Uncertain
        ));
        assert!(matches!(
            parse_self_assessment("uncertain"),
            SelfAssessment::Uncertain
        ));
    }

    #[test]
    fn parse_self_assessment_web() {
        assert!(matches!(
            parse_self_assessment("WEB"),
            SelfAssessment::NeedsWebSearch
        ));
    }

    #[test]
    fn parse_self_assessment_confident() {
        assert!(matches!(
            parse_self_assessment("CONFIDENT"),
            SelfAssessment::Confident
        ));
    }

    #[test]
    fn parse_self_assessment_garbage_defaults_to_uncertain() {
        // Safe fallback — prefer local search over confabulation.
        assert!(matches!(
            parse_self_assessment("???"),
            SelfAssessment::Uncertain
        ));
        assert!(matches!(
            parse_self_assessment(""),
            SelfAssessment::Uncertain
        ));
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
        let has_web = alts
            .iter()
            .any(|c| matches!(&c.intent, Intent::SimpleAction { tool } if tool == "web_search"));
        assert!(
            has_web,
            "temporal + web tool should surface SimpleAction: {alts:?}"
        );
    }

    #[test]
    fn alternatives_temporal_without_tool_skips_web() {
        let alts = suggest_alternatives(
            "what's the latest on the election?",
            &Intent::DeepQuery,
            &[],
        );
        let has_web = alts
            .iter()
            .any(|c| matches!(&c.intent, Intent::SimpleAction { .. }));
        assert!(!has_web, "no web tool → no SimpleAction alternative");
    }

    #[test]
    fn alternatives_how_does_suggests_deep() {
        let alts = suggest_alternatives("how does the scheduler work?", &Intent::SimpleQuery, &[]);
        assert!(alts.iter().any(|c| matches!(c.intent, Intent::DeepQuery)));
    }

    #[test]
    fn alternatives_excludes_primary() {
        // Primary is DeepQuery; even though the message carries a
        // "how does" signal, the DeepQuery alternative must be
        // omitted because it equals the primary.
        let alts = suggest_alternatives("how does the scheduler work?", &Intent::DeepQuery, &[]);
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
        assert!(
            alts.is_empty(),
            "nothing matched → no alternatives: {alts:?}"
        );
    }
}
