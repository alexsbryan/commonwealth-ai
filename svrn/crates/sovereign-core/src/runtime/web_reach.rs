// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether this turn can reach the web, read from the registry that was
//! actually built, and the prompt clauses that only a web-capable turn may
//! carry (phase-c pc-onprem-followups).
//!
//! A sealed daemon (the on-prem distribution) registers `search` with no web
//! fallback and no `web_fetch`, yet the synthesis prompt told the model to
//! "offer to search the web", and it did: a false capability claim to the
//! user. The prompts stay one text; a turn whose registry cannot reach the
//! web gets them with these clauses removed, so the offer follows the
//! registry rather than a posture flag (principle 8).

use std::borrow::Cow;

use crate::types::{Scope, ToolDescriptor};

/// The tool ids that search the web when their descriptor's scope is
/// `External`. `search` is `Persistent` when built corpus-only.
const WEB_SEARCH_TOOL_IDS: &[&str] = &["search", "web_search"];

/// Clauses that name web search, each in exactly one prompt constant
/// (pinned by `every_web_clause_is_in_its_prompt_once`). Removing one leaves
/// a grammatical sentence.
pub(crate) const WEB_ONLY_CLAUSES: &[&str] = &[
    // KNOWLEDGE_SYNTHESIS_SYSTEM, "NEVER END ON A DEAD END".
    ", or offer to search the web for the missing piece",
    // PRIMARY_BASE_SYSTEM_PROMPT, "On tool results".
    " or \"the web search returned nothing relevant\"",
];

/// True when some registered tool searches the web.
pub(crate) fn web_search_in_reach(tools: &[ToolDescriptor]) -> bool {
    tools
        .iter()
        .any(|t| t.scope == Scope::External && WEB_SEARCH_TOOL_IDS.contains(&t.id.as_str()))
}

/// `system` as this registry can honour it: unchanged when web search is in
/// reach, else with every [`WEB_ONLY_CLAUSES`] entry removed.
pub(crate) fn offers_only_what_is_registered<'a>(
    system: &'a str,
    tools: &[ToolDescriptor],
) -> Cow<'a, str> {
    if web_search_in_reach(tools) || !WEB_ONLY_CLAUSES.iter().any(|c| system.contains(c)) {
        return Cow::Borrowed(system);
    }
    tracing::debug!(
        "system_message: no registered tool searches the web — the prompt's web offers removed"
    );
    let mut out = system.to_string();
    for clause in WEB_ONLY_CLAUSES {
        out = out.replace(clause, "");
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::super::voice_prompts::PRIMARY_BASE_SYSTEM_PROMPT;
    use super::*;
    use crate::runtime::KNOWLEDGE_SYNTHESIS_SYSTEM;
    use crate::types::{Effect, Idempotency, Latency};

    fn search(scope: Scope) -> ToolDescriptor {
        ToolDescriptor {
            id: "search".into(),
            name: "Search".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope,
            output_schema: None,
        }
    }

    /// A clause that drifted out of its prompt would make the removal a
    /// silent no-op; one that appeared twice would half-apply.
    #[test]
    fn every_web_clause_is_in_its_prompt_once() {
        let both = format!("{PRIMARY_BASE_SYSTEM_PROMPT}\n\n{KNOWLEDGE_SYNTHESIS_SYSTEM}");
        for clause in WEB_ONLY_CLAUSES {
            assert_eq!(both.matches(clause).count(), 1, "{clause}");
        }
    }

    /// A sealed turn's system context (the primary contract over the
    /// synthesis prompt, as `build_primary_system_message` joins them)
    /// names no web search when the registry's `search` is corpus-only, and
    /// is untouched when it reaches the web. Failing input: drop the
    /// `web_search_in_reach` check and the open turn loses its offer; drop
    /// the removal and the sealed one keeps it.
    #[test]
    fn a_sealed_turn_names_no_web_search() {
        let system = format!("{PRIMARY_BASE_SYSTEM_PROMPT}\n\n{KNOWLEDGE_SYNTHESIS_SYSTEM}");
        let sealed = offers_only_what_is_registered(&system, &[search(Scope::Persistent)]);
        assert!(!sealed.to_lowercase().contains("web"), "{sealed}");
        assert!(sealed.contains("name the nearest thing these sources CAN answer. One"));
        let none = offers_only_what_is_registered(&system, &[]);
        assert!(!none.to_lowercase().contains("web"));
        let open = offers_only_what_is_registered(&system, &[search(Scope::External)]);
        assert_eq!(open, system);
    }
}
