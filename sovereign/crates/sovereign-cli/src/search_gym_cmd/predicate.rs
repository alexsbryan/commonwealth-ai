//! Pass-predicate vocabulary for `sovereign search-gym`.
//!
//! Each fixture's `pass.toml` parses into a `Predicate`. The struct is
//! intentionally flat: every key is optional, present-keys form an
//! AND'd conjunction. The scorer in `score.rs` walks the keys and
//! returns the first failure reason it finds.
//!
//! Why TOML and not YAML (the code gym's choice)? The code gym was
//! Bash + jq, where YAML-subset was the path of least resistance. In
//! Rust we get `toml = { workspace = true }` for free and the rest of
//! the recipe corpus (sep-core-v1.toml, sep_questions.toml, voice
//! routing banks) is already TOML — predicate files match.
//!
//! Vocabulary documented in `sovereign-recipes/search-gym/PASS_SCHEMA.md`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    // ─── Decision axis (judiciousness) ──────────────────────────
    /// Did the model invoke `search` at all?
    pub should_call_search: Option<bool>,
    /// Tools the model must never call in this fixture.
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
    /// Multi-tool fixtures: the first tool the model invokes must be
    /// this one. (`None` = no ordering constraint.)
    pub expected_first_tool: Option<String>,
    /// Cap on search invocations across the conversation. `None` =
    /// uncapped (rarely what you want).
    pub max_search_calls: Option<usize>,

    // ─── Query shape (anti-verbatim, anti-leak) ─────────────────
    /// Every listed substring must appear in the search query the
    /// model emitted. Case-insensitive.
    #[serde(default)]
    pub expected_query_contains: Vec<String>,
    /// Every listed substring must NOT appear in the search query.
    /// Used to guard against verbatim echoing of the user prompt —
    /// the model should reformulate, not parrot.
    #[serde(default)]
    pub expected_query_not_contains: Vec<String>,
    /// Whitespace-token count cap on the search query. Discourages
    /// paragraph-as-query.
    pub expected_query_max_tokens: Option<usize>,

    // ─── Result handling (when should_call_search: true) ────────
    /// Minimum number of distinct URLs from the mock response that
    /// must appear in the final assistant message.
    pub must_cite_url_from_mock: Option<usize>,
    /// If true, any URL cited in the final assistant message that
    /// is NOT in the mock response is a failure (fabricated cite).
    #[serde(default)]
    pub must_not_cite_url_outside_mock: bool,
    /// Phrasing required when results contradict each other. Used
    /// for the "two-top-results-disagree" fixture. Case-insensitive
    /// substring match.
    #[serde(default)]
    pub contradiction_phrases: Vec<String>,
    /// If true, when the mock returns zero results, the model's final
    /// message must acknowledge that — empty/zero/no-results phrasing.
    pub zero_results_must_be_acknowledged: Option<bool>,

    // ─── Refusal path ───────────────────────────────────────────
    /// If true, the model is expected to decline to search and say
    /// so (e.g. no API key configured fixture). Implies
    /// `should_call_search: false`.
    pub must_decline_gracefully: Option<bool>,

    // ─── Final-message content checks (string-match — to be retired) ─
    /// Substrings (case-insensitive) that must appear in the final
    /// assistant message. Used to pin content correctness for
    /// skip-search fixtures where the answer is in context or
    /// well-known — `should_call_search = false` alone doesn't
    /// catch a model that skips search but then hallucinates.
    ///
    /// **Sunset notice:** retiring in Phase 2c in favor of
    /// `final_message_satisfies` (judge-evaluated). Kept here so
    /// Phase 1 fixtures keep working through the transition.
    #[serde(default)]
    pub final_message_contains: Vec<String>,
    /// Substrings (case-insensitive) that must NOT appear.
    /// **Sunset notice:** retiring in Phase 2c (see above).
    #[serde(default)]
    pub final_message_not_contains: Vec<String>,

    // ─── Semantic predicates (judge-evaluated) ──────────────────
    /// Natural-language assertions about the final assistant
    /// message. Each is evaluated by the Judge (see judge.rs).
    /// Replaces the string-list anti-pattern in
    /// `zero_results_must_be_acknowledged` and
    /// `must_decline_gracefully`, which are scheduled for removal
    /// in Phase 2c.
    ///
    /// Each assertion runs as its own judge call — multi-criteria
    /// evaluation = multiple calls, keeping verdicts auditable.
    ///
    /// Example:
    /// ```toml
    /// final_message_satisfies = [
    ///   "The response acknowledges that no search results were found.",
    ///   "The response does not fabricate any details about the event.",
    /// ]
    /// ```
    #[serde(default)]
    pub final_message_satisfies: Vec<String>,

    /// Natural-language assertions about the search query the model
    /// emitted. Evaluated by the Judge against the first search
    /// call's `query` argument. Replaces
    /// `expected_query_contains` / `_not_contains` (string-match
    /// anti-pattern) once Phase 2c lands.
    #[serde(default)]
    pub query_satisfies: Vec<String>,
}

impl Predicate {
    /// Parse a `pass.toml` body into a `Predicate`. Errors carry the
    /// path of the offending file in the message so the operator can
    /// jump to it without grep.
    pub fn from_toml(body: &str, path_for_errors: &std::path::Path) -> Result<Self, String> {
        toml::from_str(body).map_err(|e| {
            format!(
                "predicate parse error in {}: {e}",
                path_for_errors.display()
            )
        })
    }

    /// Number of distinct constraints this predicate carries — useful
    /// in the report to surface "this fixture asserts nothing" as a
    /// fixture-author smell.
    pub fn constraint_count(&self) -> usize {
        let mut n = 0;
        if self.should_call_search.is_some() { n += 1; }
        if !self.forbidden_tools.is_empty() { n += 1; }
        if self.expected_first_tool.is_some() { n += 1; }
        if self.max_search_calls.is_some() { n += 1; }
        if !self.expected_query_contains.is_empty() { n += 1; }
        if !self.expected_query_not_contains.is_empty() { n += 1; }
        if self.expected_query_max_tokens.is_some() { n += 1; }
        if self.must_cite_url_from_mock.is_some() { n += 1; }
        if self.must_not_cite_url_outside_mock { n += 1; }
        if !self.contradiction_phrases.is_empty() { n += 1; }
        if self.zero_results_must_be_acknowledged.is_some() { n += 1; }
        if self.must_decline_gracefully.is_some() { n += 1; }
        if !self.final_message_contains.is_empty() { n += 1; }
        if !self.final_message_not_contains.is_empty() { n += 1; }
        if !self.final_message_satisfies.is_empty() { n += 1; }
        if !self.query_satisfies.is_empty() { n += 1; }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_full_predicate() {
        let body = r#"
            should_call_search = true
            forbidden_tools = ["calendar"]
            expected_first_tool = "search"
            max_search_calls = 1
            expected_query_contains = ["spacex", "starship"]
            expected_query_not_contains = ["what happened with"]
            expected_query_max_tokens = 8
            must_cite_url_from_mock = 2
            must_not_cite_url_outside_mock = true
        "#;
        let p = Predicate::from_toml(body, Path::new("test")).unwrap();
        assert_eq!(p.should_call_search, Some(true));
        assert_eq!(p.forbidden_tools, vec!["calendar"]);
        assert_eq!(p.expected_query_contains.len(), 2);
        assert_eq!(p.must_cite_url_from_mock, Some(2));
        assert!(p.must_not_cite_url_outside_mock);
    }

    #[test]
    fn empty_predicate_constraint_count_is_zero() {
        let p = Predicate::from_toml("", Path::new("test")).unwrap();
        assert_eq!(p.constraint_count(), 0);
    }

    #[test]
    fn unknown_keys_error_loudly() {
        // §4.3 unknown-id handling: a typo should fail loudly so
        // fixture authors find it immediately.
        let body = "shoud_call_search = true";  // note typo
        let err = Predicate::from_toml(body, Path::new("test/pass.toml")).unwrap_err();
        assert!(err.contains("test/pass.toml"), "err={err}");
    }

    #[test]
    fn parse_error_path_is_surfaced() {
        let body = "should_call_search = \"not-a-bool\"";
        let err = Predicate::from_toml(body, Path::new("fixtures/01/pass.toml")).unwrap_err();
        assert!(err.contains("fixtures/01/pass.toml"), "err={err}");
    }
}
