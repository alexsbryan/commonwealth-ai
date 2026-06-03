//! Pure helpers around `Intent` — defaults, labels, wire-form encode/decode,
//! and clarification UI strings. No state, no I/O; safe to call from
//! anywhere in the runtime dispatch path.
//!
//! Kept separate from the `Intent` enum itself (in `crate::types`) because
//! these are runtime-presentation concerns (banner phrasing, OICP envelope
//! defaults, redirect-chip labels) rather than properties of the type.

use crate::types::*;

/// Intent-implied OICP defaults (v0.3). The classified intent
/// carries a latency signal — "DeepQuery" wants extended thinking
/// budget, "ComplexTask" and "KnowledgeQuery" want solid normal
/// latency — which the scheduler consumes as `latency_class`.
/// `capability_hint` defaults to `general`; code/prose/etc. are left
/// to skill-level overrides since the intent vocabulary doesn't
/// carry a specialization distinction.
///
/// Returns `None` for small-model intents (SimpleQuery, Continuation,
/// SimpleAction) where cross-network latency wouldn't be worth
/// trading for a marginal quality bump — no OICP envelope means
/// the local Fast slot serves without invoking the scheduler.
pub(crate) fn default_oicp_for_intent(
    intent: &Intent,
) -> Option<crate::oicp::InferenceRequirements> {
    use crate::oicp::{CapabilityHint, InferenceRequirements, LatencyClass};
    let (hint, latency_class) = match intent {
        Intent::DeepQuery => {
            // Reasoning-heavy: extended class tolerates higher TTFT
            // in exchange for deeper thinking budgets.
            (CapabilityHint::general(), LatencyClass::Extended)
        }
        Intent::ComplexTask => {
            // Tool-using plans want solid normal-latency responses;
            // extended would add round-trip overhead per tool step.
            (CapabilityHint::general(), LatencyClass::Normal)
        }
        Intent::KnowledgeQuery => {
            // Retrieval-driven synthesis over a bounded chunk set.
            (CapabilityHint::general(), LatencyClass::Normal)
        }
        Intent::ComparisonQuery => {
            // Bounded two-entity contrast — Fast slot, no reasoning
            // budget. Retrieval over a small chunk set, constrained
            // synthesis prompt, sub-second TTFT target.
            (CapabilityHint::general(), LatencyClass::Fast)
        }
        Intent::MetalingualQuery => {
            // Codebase lookup + brief synthesis — same shape as
            // KnowledgeQuery's FastFocused path but against code
            // corpora. Fast slot is enough; no reasoning budget.
            (CapabilityHint::code(), LatencyClass::Fast)
        }
        Intent::ConationQuery => {
            // Operates on the prior turn — no new retrieval, no
            // reclassification. The OICP envelope of the rebound
            // classification is what actually matters; this default
            // just covers the rare case where conation is dispatched
            // without rebind context.
            (CapabilityHint::general(), LatencyClass::Fast)
        }
        Intent::CommissiveQuery => {
            // Persistence-only path — no LLM synthesis required for
            // the storage step; a brief Fast-slot acknowledgment
            // citing the situated anchor is all we need.
            (CapabilityHint::general(), LatencyClass::Fast)
        }
        Intent::ExpressiveQuery => {
            // Acknowledge + situated help-offer. Fast slot synthesis
            // grounded in working_memory + last assistant turn; no
            // retrieval against the world corpus.
            (CapabilityHint::general(), LatencyClass::Fast)
        }
        Intent::SimpleQuery | Intent::SimpleAction { .. } | Intent::Continuation { .. } => {
            return None;
        }
    };
    Some(
        InferenceRequirements::new()
            .with_hint(hint)
            .with_latency_class(latency_class),
    )
}

/// Produce a short human-readable banner for `interpretation-proposed`.
/// Runs without a model call — we'd like the banner to appear before
/// the first token, so an extra Fast-slot turn for phrasing would
/// defeat the "under 2s immediate engagement" requirement.
pub(crate) fn format_interpretation(
    _message: &str,
    primary: &Intent,
    rationale: Option<&str>,
) -> String {
    let intent_phrase = match primary {
        Intent::SimpleQuery => "a quick factual answer",
        Intent::DeepQuery => "a deeper explanation",
        Intent::KnowledgeQuery => "a look in your installed knowledge",
        Intent::ComparisonQuery => "a comparison between two things",
        Intent::MetalingualQuery => "a lookup in your codebase",
        Intent::ConationQuery => "a tweak to my last reply",
        Intent::CommissiveQuery => "a commitment to save",
        Intent::ExpressiveQuery => "an acknowledgment + help offer",
        Intent::SimpleAction { .. } => "a tool call",
        Intent::ComplexTask => "a multi-step task",
        Intent::Continuation { .. } => "a follow-up to earlier work",
    };
    if let Some(r) = rationale {
        format!("I'm reading this as {intent_phrase} ({r}). If that's off, redirect below.")
    } else {
        format!("I'm reading this as {intent_phrase}. If that's off, redirect below.")
    }
}

/// Human label for a redirect chip on the banner.
pub(crate) fn label_for_intent(intent: &Intent) -> String {
    match intent {
        Intent::SimpleQuery => "Give me a quick answer".into(),
        Intent::DeepQuery => "Walk me through it in depth".into(),
        Intent::KnowledgeQuery => "Check my knowledge base".into(),
        Intent::ComparisonQuery => "Compare them side by side".into(),
        Intent::MetalingualQuery => "Look it up in this codebase".into(),
        Intent::ConationQuery => "Adjust the last reply".into(),
        Intent::CommissiveQuery => "Save this as a commitment".into(),
        Intent::ExpressiveQuery => "Hear me out and help".into(),
        Intent::SimpleAction { tool } => format!("Use the {tool} tool"),
        Intent::ComplexTask => "Plan a multi-step task".into(),
        Intent::Continuation { .. } => "Continue prior task".into(),
    }
}

/// Wire-form `Intent` hint used by the desktop → runtime redirect
/// payload. Converting at this boundary keeps
/// [`InterpretationProposed`] and [`ClarificationOption`] trivially
/// serializable — the full `Intent` enum carries a `ToolId` for
/// `SimpleAction`, which is ergonomic in Rust but awkward in JSON.
pub(crate) fn intent_hint(intent: &Intent) -> String {
    match intent {
        Intent::SimpleQuery => "simple_query".into(),
        Intent::DeepQuery => "deep_query".into(),
        Intent::KnowledgeQuery => "knowledge_query".into(),
        Intent::ComparisonQuery => "comparison_query".into(),
        Intent::MetalingualQuery => "metalingual_query".into(),
        Intent::ConationQuery => "conation_query".into(),
        Intent::CommissiveQuery => "commissive_query".into(),
        Intent::ExpressiveQuery => "expressive_query".into(),
        Intent::SimpleAction { tool } => format!("simple_action:{tool}"),
        Intent::ComplexTask => "complex_task".into(),
        Intent::Continuation { task_id } => format!("continuation:{task_id}"),
    }
}

/// Inverse of [`intent_hint`] — decode a wire-form hint back into
/// an `Intent`. Unknown variants fall back to `SimpleQuery` so the
/// continuation path never hard-fails; the caller logs the case.
pub(crate) fn parse_intent_hint(hint: &str) -> Intent {
    match hint {
        "simple_query" => Intent::SimpleQuery,
        "deep_query" => Intent::DeepQuery,
        "knowledge_query" => Intent::KnowledgeQuery,
        "comparison_query" => Intent::ComparisonQuery,
        "metalingual_query" => Intent::MetalingualQuery,
        "conation_query" => Intent::ConationQuery,
        "commissive_query" => Intent::CommissiveQuery,
        "expressive_query" => Intent::ExpressiveQuery,
        "complex_task" => Intent::ComplexTask,
        _ if hint.starts_with("simple_action:") => {
            let tool = hint.trim_start_matches("simple_action:").to_string();
            Intent::SimpleAction {
                tool: ToolId::from(tool),
            }
        }
        _ if hint.starts_with("continuation:") => {
            let task_id = hint.trim_start_matches("continuation:").to_string();
            Intent::Continuation {
                task_id: TaskId::from(task_id),
            }
        }
        _ => {
            tracing::warn!(
                hint,
                "parse_intent_hint: unknown hint, falling back to SimpleQuery"
            );
            Intent::SimpleQuery
        }
    }
}

/// Build a one-sentence clarifying question for the `Ask` move.
/// Kept short and neutral — the alternatives themselves do most of
/// the disambiguation work; the question just frames the choice.
pub(crate) fn build_clarification_question(_message: &str, primary: &Intent) -> String {
    let read_as = match primary {
        Intent::SimpleQuery => "a quick factual answer",
        Intent::DeepQuery => "a deeper explanation",
        Intent::KnowledgeQuery => "a corpus lookup",
        Intent::ComparisonQuery => "a side-by-side comparison",
        Intent::MetalingualQuery => "a vocabulary lookup in our system",
        Intent::ConationQuery => "an adjustment to my last reply",
        Intent::CommissiveQuery => "a commitment to save",
        Intent::ExpressiveQuery => "an acknowledgment + targeted help",
        Intent::SimpleAction { .. } => "an action",
        Intent::ComplexTask => "a multi-step task",
        Intent::Continuation { .. } => "a continuation",
    };
    format!(
        "I could approach this a few ways — my best read is {read_as}, \
         but could you pick what you'd like most?"
    )
}
