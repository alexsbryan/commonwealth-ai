// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which route a banked turn actually took, read off its persisted metadata.
//!
//! The synth runner reported `provenance.intent` as "the intent", and that
//! field is a free-form DISPLAY label — `sovereign_contracts::types::routing`
//! says so, and the values prove it: the knowledge handler writes
//! `"knowledge_query"` for both `KnowledgeQuery` and `ComparisonQuery`, the
//! attached-doc door writes `"AttachedDoc"`, which names no variant at all.
//! Reading it as a route makes two surfaces look like one.
//!
//! `routed_intent` is the field that answers the question, so read it first
//! and keep the old label as the fallback — a transcript banked before the
//! handlers stamped the key has nothing else, and dropping those rows to
//! `None` would turn "recorded under the old name" into "not measured".
//!
//! Lives beside `runner.rs` rather than in it because that file is 2,117
//! lines against an arch-gate ceiling of 2,124.

use serde_json::Value;

/// The route this turn took, preferring the stamped record over the display
/// label. `None` when the row carries neither.
///
/// Goes through `project_turn_metadata` rather than `meta.get("routed_intent")`
/// because that is the one accessor for this key (ARCH 8), and it is where the
/// rule that a `null` is an ABSENT key rather than a value is written down — a
/// hand-rolled `get` here would read `"routed_intent": null` as present.
pub fn snapshot_intent(metadata: &Option<Value>) -> Option<String> {
    sovereign_contracts::types::projection::project_turn_metadata(metadata)
        .and_then(|turn| turn.routed_intent)
        .or_else(|| {
            metadata
                .as_ref()
                .and_then(|m| m.get("provenance"))
                .and_then(|p| p.get("intent"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are `json!` literals — the PRODUCER's form. The handlers that
    /// stamp this key write it as a literal
    /// (`sovereign-core/src/runtime/handlers/knowledge_query.rs` and its
    /// siblings), so a fixture serialised out of `TurnMetadata`, the reader's
    /// own type, would move with a `#[serde(rename)]` there and keep passing
    /// while every producer was left behind (ARCH §18.1).
    ///
    /// The failing input, watched red 2026-09-20: make `snapshot_intent` read
    /// `provenance.intent` first — what the runner did before this module —
    /// and the first assertion fails `left: Some("knowledge_query")`,
    /// `right: Some("ComparisonQuery")`.
    #[test]
    fn synth_snapshot_prefers_routed_intent() {
        // The real disagreement, not a synthetic one: a ComparisonQuery turn
        // persists `routed_intent: "ComparisonQuery"` beside
        // `provenance.intent: "knowledge_query"`, so reading the display label
        // reports two surfaces as one.
        let both = serde_json::json!({
            "routed_intent": "ComparisonQuery",
            "provenance": { "intent": "knowledge_query" },
        });
        assert_eq!(
            snapshot_intent(&Some(both)).as_deref(),
            Some("ComparisonQuery"),
        );

        // Banked before the handlers stamped the key: the old label is all
        // there is, and losing it would shrink the bank rather than report it.
        let legacy = serde_json::json!({ "provenance": { "intent": "knowledge_query" } });
        assert_eq!(
            snapshot_intent(&Some(legacy)).as_deref(),
            Some("knowledge_query"),
        );

        // A `null` is an ABSENT key, not a value, so it must fall through to
        // the label rather than shadow it. Failing input: read the key with
        // `.map(|v| v.to_string())` instead of through `project_turn_metadata`
        // and this returns the string `"null"`.
        let nulled = serde_json::json!({
            "routed_intent": Value::Null,
            "provenance": { "intent": "knowledge_query" },
        });
        assert_eq!(
            snapshot_intent(&Some(nulled)).as_deref(),
            Some("knowledge_query"),
        );

        // No metadata at all is absence, reported as absence — never a
        // stand-in like `"unknown"`, which would enter the bank as a route.
        assert_eq!(snapshot_intent(&None), None);
    }
}
