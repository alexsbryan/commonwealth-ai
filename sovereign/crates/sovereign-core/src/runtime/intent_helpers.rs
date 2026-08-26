// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure helpers around `Intent` — defaults, labels, wire-form encode/decode,
//! and clarification UI strings. No state, no I/O; safe to call from
//! anywhere in the runtime dispatch path.
//!
//! Kept separate from the `Intent` enum itself (in `crate::types`) because
//! these are runtime-presentation concerns (banner phrasing, OICP envelope
//! defaults, redirect-chip labels) rather than properties of the type.

use crate::types::*;

/// Intent-implied OICP defaults (v0.3), read off the intent table.
///
/// The classified intent carries a latency signal — `DeepQuery` wants extended
/// thinking budget, `ComplexTask` and `KnowledgeQuery` want solid normal
/// latency — which the scheduler consumes as `latency_class`. Both that and
/// the capability hint are the `oicp` column of [`Intent::row`]; the per-intent
/// reasoning lives on the rows.
///
/// `None` means the intent declares no envelope at all (`SimpleQuery`,
/// `Continuation`, `SimpleAction`): cross-network latency is not worth trading
/// for a marginal quality bump, so the local Fast slot serves without invoking
/// the scheduler.
pub(crate) fn default_oicp_for_intent(
    intent: &Intent,
) -> Option<crate::oicp::InferenceRequirements> {
    use crate::oicp::{CapabilityHint, InferenceRequirements};
    let (hint, latency_class) = intent.row().oicp?;
    Some(
        InferenceRequirements::new()
            // Every hint in the table is one of `CapabilityHint::STANDARDIZED`,
            // which `parse` accepts unconditionally — held by
            // `intent_table_hints_are_standardized`, which is what makes this
            // `expect` a statement about the test rather than a hope.
            .with_hint(CapabilityHint::parse(hint).expect(
                "intent table hint is standardized \
                 (intent_table_hints_are_standardized)",
            ))
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
    let intent_phrase = primary.row().interpretation;
    if let Some(r) = rationale {
        format!("I'm reading this as {intent_phrase} ({r}). If that's off, redirect below.")
    } else {
        format!("I'm reading this as {intent_phrase}. If that's off, redirect below.")
    }
}

/// Human label for a redirect chip on the banner — the `redirect_label`
/// column of [`Intent::row`].
pub(crate) fn label_for_intent(intent: &Intent) -> String {
    let label = intent.row().redirect_label;
    match intent {
        // The only payload-carrying label: two `SimpleAction`s naming
        // different tools want different chips, so the row holds the template
        // and the payload fills the hole.
        Intent::SimpleAction { tool } => label.replace("{tool}", &tool.to_string()),
        _ => label.to_string(),
    }
}

/// Wire-form `Intent` hint used by the desktop → runtime redirect
/// payload. Converting at this boundary keeps
/// [`InterpretationProposed`] and [`ClarificationOption`] trivially
/// serializable — the full `Intent` enum carries a `ToolId` for
/// `SimpleAction`, which is ergonomic in Rust but awkward in JSON.
pub(crate) fn intent_hint(intent: &Intent) -> String {
    let slug = intent.row().slug;
    match intent {
        // The two payload-carrying variants suffix the base slug with their
        // payload; `parse_intent_hint` splits on the same `:`.
        Intent::SimpleAction { tool } => format!("{slug}:{tool}"),
        Intent::Continuation { task_id } => format!("{slug}:{task_id}"),
        _ => slug.to_string(),
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
        "code_query" => Intent::CodeQuery,
        "comparison_query" => Intent::ComparisonQuery,
        "metalingual_query" => Intent::MetalingualQuery,
        "conation_query" => Intent::ConationQuery,
        "commissive_query" => Intent::CommissiveQuery,
        "expressive_query" => Intent::ExpressiveQuery,
        "generative_query" => Intent::GenerativeQuery,
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
    let read_as = primary.row().read_as;
    format!(
        "I could approach this a few ways — my best read is {read_as}, \
         but could you pick what you'd like most?"
    )
}

#[cfg(test)]
mod intent_wire_tests {
    use super::*;

    /// Every variant, including both payload shapes. The `slug` column is the
    /// only place the wire vocabulary is decided, but `parse_intent_hint` is
    /// its INVERSE and is still hand-written — so adding an intent and
    /// forgetting the parser is a test failure here rather than a silent
    /// `SimpleQuery` in production.
    fn every_variant() -> Vec<Intent> {
        vec![
            Intent::SimpleQuery,
            Intent::DeepQuery,
            Intent::KnowledgeQuery,
            Intent::ComparisonQuery,
            Intent::MetalingualQuery,
            Intent::ConationQuery,
            Intent::CommissiveQuery,
            Intent::ExpressiveQuery,
            Intent::GenerativeQuery,
            Intent::CodeQuery,
            Intent::SimpleAction {
                tool: ToolId::from("search".to_string()),
            },
            Intent::ComplexTask,
            Intent::Continuation {
                task_id: TaskId::from("task-1".to_string()),
            },
        ]
    }

    #[test]
    fn every_wire_hint_round_trips() {
        for intent in every_variant() {
            let hint = intent_hint(&intent);
            assert_eq!(
                parse_intent_hint(&hint),
                intent,
                "{} did not survive the wire round trip via {hint:?}",
                intent.name()
            );
        }
    }

    #[test]
    fn the_hint_is_the_table_slug_plus_any_payload() {
        // The base of every hint is the row's `slug` — one decider for the
        // wire key. The two payload variants suffix it after a `:`.
        for intent in every_variant() {
            let hint = intent_hint(&intent);
            let base = hint.split(':').next().unwrap();
            assert_eq!(base, intent.row().slug, "{}", intent.name());
        }
        assert_eq!(
            intent_hint(&Intent::SimpleAction {
                tool: ToolId::from("web_search".to_string())
            }),
            "simple_action:web_search"
        );
        assert_eq!(
            intent_hint(&Intent::Continuation {
                task_id: TaskId::from("t-9".to_string())
            }),
            "continuation:t-9"
        );
    }

    #[test]
    fn the_redirect_chip_names_the_tool_it_would_use() {
        // `redirect_label` is the one column holding a `{tool}` placeholder;
        // if the substitution ever stops firing the chip reads literally.
        let label = label_for_intent(&Intent::SimpleAction {
            tool: ToolId::from("code_search".to_string()),
        });
        assert_eq!(label, "Use the code_search tool");
        assert!(!label.contains('{'), "the placeholder leaked: {label}");
        assert_eq!(
            label_for_intent(&Intent::DeepQuery),
            "Walk me through it in depth"
        );
    }

    #[test]
    fn an_unknown_hint_is_reported_not_guessed() {
        // The fallback is deliberate and logged (`SimpleQuery` keeps the
        // continuation path from hard-failing), but it must not swallow a
        // hint that merely LOOKS like a payload form.
        assert_eq!(parse_intent_hint("not_an_intent"), Intent::SimpleQuery);
        assert_eq!(
            parse_intent_hint("continuation:"),
            Intent::Continuation {
                task_id: TaskId::from(String::new())
            }
        );
    }
}
