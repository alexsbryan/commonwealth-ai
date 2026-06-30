// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pass-predicate vocabulary for `svrn search-gym`.
//!
//! Each fixture's `pass.toml` parses into a `Predicate`. The struct is
//! intentionally flat: every key is optional, present-keys form an
//! AND'd conjunction. The scorer in `score.rs` walks the keys and
//! returns every reason that failed.
//!
//! Why TOML and not YAML (the code gym's choice)? The code gym was
//! Bash + jq, where YAML-subset was the path of least resistance. In
//! Rust we get `toml = { workspace = true }` for free and the rest of
//! the recipe corpus (sep-core-v1.toml, sep_questions.toml, voice
//! routing banks) is already TOML — predicate files match.
//!
//! ## Predicate categories
//!
//! - **Structural** — `should_call_search`, `forbidden_tools`,
//!   `expected_first_tool`, `max_search_calls`,
//!   `expected_query_max_tokens`, `must_cite_url_from_mock`,
//!   `must_not_cite_url_outside_mock`. Pure data — counts, URL set
//!   membership, tool-call observation. Cheap, deterministic, no
//!   model required.
//! - **Semantic** — `query_satisfies`, `final_message_satisfies`.
//!   Each entry is a natural-language assertion evaluated by the
//!   `Judge` (see `judge.rs`). Replaces the string-list matching
//!   that lived here through Phase 1 (predicates like
//!   `final_message_contains`, `zero_results_must_be_acknowledged`,
//!   `must_decline_gracefully`) which proved brittle and gameable
//!   by phrasing.
//!
//! Vocabulary documented in `sovereign/bench/search-gym/PASS_SCHEMA.md`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    // ─── Structural: decision axis (judiciousness) ──────────────
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

    // ─── Structural: query shape ────────────────────────────────
    /// Whitespace-token count cap on the search query. Discourages
    /// paragraph-as-query.
    pub expected_query_max_tokens: Option<usize>,

    // ─── Structural: result handling (URL set membership) ───────
    /// Minimum number of distinct URLs from the mock response that
    /// must appear in the final assistant message.
    pub must_cite_url_from_mock: Option<usize>,
    /// If true, any URL cited in the final assistant message that
    /// is NOT in the mock response is a failure (fabricated cite).
    #[serde(default)]
    pub must_not_cite_url_outside_mock: bool,

    // ─── Semantic (judge-evaluated) ─────────────────────────────
    /// Natural-language assertions about the search query the model
    /// emitted. Evaluated by the Judge against the first search
    /// call's `query` argument. Use these for phrasing checks
    /// (anti-verbatim, entity extraction) that string matching
    /// gets wrong.
    #[serde(default)]
    pub query_satisfies: Vec<String>,
    /// Natural-language assertions about the final assistant
    /// message. Each is evaluated by the Judge (see judge.rs).
    /// Use for content correctness, refusal phrasing, zero-result
    /// acknowledgement, contradiction flagging — anything that a
    /// string-list match would get wrong on a paraphrase.
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
        if self.should_call_search.is_some() {
            n += 1;
        }
        if !self.forbidden_tools.is_empty() {
            n += 1;
        }
        if self.expected_first_tool.is_some() {
            n += 1;
        }
        if self.max_search_calls.is_some() {
            n += 1;
        }
        if self.expected_query_max_tokens.is_some() {
            n += 1;
        }
        if self.must_cite_url_from_mock.is_some() {
            n += 1;
        }
        if self.must_not_cite_url_outside_mock {
            n += 1;
        }
        if !self.query_satisfies.is_empty() {
            n += 1;
        }
        if !self.final_message_satisfies.is_empty() {
            n += 1;
        }
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
            expected_query_max_tokens = 8
            must_cite_url_from_mock = 2
            must_not_cite_url_outside_mock = true
            query_satisfies = ["The query is concise and entity-focused."]
            final_message_satisfies = ["The response cites at least one source."]
        "#;
        let p = Predicate::from_toml(body, Path::new("test")).unwrap();
        assert_eq!(p.should_call_search, Some(true));
        assert_eq!(p.forbidden_tools, vec!["calendar"]);
        assert_eq!(p.must_cite_url_from_mock, Some(2));
        assert!(p.must_not_cite_url_outside_mock);
        assert_eq!(p.query_satisfies.len(), 1);
        assert_eq!(p.final_message_satisfies.len(), 1);
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
        let body = "shoud_call_search = true"; // note typo
        let err = Predicate::from_toml(body, Path::new("test/pass.toml")).unwrap_err();
        assert!(err.contains("test/pass.toml"), "err={err}");
    }

    #[test]
    fn retired_predicates_now_error_loudly() {
        // String-list predicates were retired in Phase 2c. If an old
        // fixture still uses them, the load must fail loud so the
        // author migrates to query_satisfies / final_message_satisfies
        // rather than getting silent no-op behaviour.
        for retired in [
            "final_message_contains = [\"x\"]",
            "final_message_not_contains = [\"x\"]",
            "expected_query_contains = [\"x\"]",
            "expected_query_not_contains = [\"x\"]",
            "zero_results_must_be_acknowledged = true",
            "must_decline_gracefully = true",
            "contradiction_phrases = [\"x\"]",
        ] {
            let err = Predicate::from_toml(retired, Path::new("p")).unwrap_err();
            assert!(
                err.contains("unknown field") || err.contains("missing field"),
                "expected unknown-field error for {retired:?}, got: {err}"
            );
        }
    }

    #[test]
    fn parse_error_path_is_surfaced() {
        let body = "should_call_search = \"not-a-bool\"";
        let err = Predicate::from_toml(body, Path::new("fixtures/01/pass.toml")).unwrap_err();
        assert!(err.contains("fixtures/01/pass.toml"), "err={err}");
    }
}
