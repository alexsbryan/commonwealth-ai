//! Deterministic checks for Tier-B voice eval.
//!
//! Cheap, no inference, fully unit-tested. Each check produces a
//! pass/fail boolean plus a structured detail field; the full
//! result is rolled up into [`ScenarioResult`] for the report
//! writer.
//!
//! Four checks today, all pinned by the avoid-list and the eight
//! principles:
//!
//! * **Length cap** — principle 4 (let silence sit). Penalises
//!   over-long responses for scenarios that demand brevity.
//! * **Question density** — principle 3 (load-bearing questions).
//!   Bounds on `?` count.
//! * **Banned-phrase scan** — avoid-list (therapist register,
//!   wisdom voice, over-affirmation, no-right-answer cop-out, plus
//!   the generic-AI-disclaimer pattern from principle 1).
//! * **Required-content match** — at least one of the
//!   scenario-specified substrings must appear (the "right move
//!   was made" signal — e.g., "you mentioned Mark twice in March").
//!
//! All string matching is case-insensitive — model output
//! capitalisation is unstable and the goal is to score *content*,
//! not punctuation.

use serde::{Deserialize, Serialize};

use super::scenarios::Scenario;

/// One scenario's full check result. Goes into the JSON report
/// verbatim and is rolled up into the per-axis aggregate at write
/// time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_id: String,
    pub skill: String,
    pub probes: Vec<String>,
    pub response: String,
    pub length: LengthCheck,
    pub question_density: QuestionDensityCheck,
    pub banned_phrases: BannedPhraseCheck,
    pub required_content: RequiredContentCheck,
    /// Overall pass — every individual check that's enabled (i.e.
    /// the scenario specified a constraint) must pass.
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LengthCheck {
    pub enabled: bool,
    pub response_chars: usize,
    pub max_chars: Option<usize>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionDensityCheck {
    pub enabled: bool,
    pub question_count: usize,
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BannedPhraseCheck {
    pub enabled: bool,
    /// Phrases that were specified as forbidden but appeared in
    /// the response (case-insensitive match). Each entry is the
    /// original scenario phrasing; downstream tools format it
    /// however they like.
    pub hits: Vec<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredContentCheck {
    pub enabled: bool,
    /// First matching phrase from `must_include_one_of`. `None`
    /// when the check is enabled but no phrase matched.
    pub matched: Option<String>,
    pub passed: bool,
}

/// Run all enabled deterministic checks for a scenario against a
/// candidate response. Pure function — no I/O, no inference.
pub fn run_checks(scenario: &Scenario, response: &str) -> ScenarioResult {
    let length = length_check(scenario, response);
    let question_density = question_density_check(scenario, response);
    let banned_phrases = banned_phrase_check(scenario, response);
    let required_content = required_content_check(scenario, response);

    let passed = length.passed
        && question_density.passed
        && banned_phrases.passed
        && required_content.passed;

    ScenarioResult {
        scenario_id: scenario.scenario.id.clone(),
        skill: scenario.scenario.skill.clone(),
        probes: scenario.scenario.probes.clone(),
        response: response.to_string(),
        length,
        question_density,
        banned_phrases,
        required_content,
        passed,
    }
}

fn length_check(scenario: &Scenario, response: &str) -> LengthCheck {
    let response_chars = response.chars().count();
    match scenario.expected.max_response_chars {
        Some(max) => LengthCheck {
            enabled: true,
            response_chars,
            max_chars: Some(max),
            passed: response_chars <= max,
        },
        None => LengthCheck {
            enabled: false,
            response_chars,
            max_chars: None,
            passed: true,
        },
    }
}

fn question_density_check(scenario: &Scenario, response: &str) -> QuestionDensityCheck {
    let question_count = response.chars().filter(|c| *c == '?').count();
    let min = scenario.expected.question_count_min;
    let max = scenario.expected.question_count_max;
    if min.is_none() && max.is_none() {
        return QuestionDensityCheck {
            enabled: false,
            question_count,
            min: None,
            max: None,
            passed: true,
        };
    }
    let lo = min.unwrap_or(0);
    let hi = max.unwrap_or(usize::MAX);
    QuestionDensityCheck {
        enabled: true,
        question_count,
        min,
        max,
        passed: question_count >= lo && question_count <= hi,
    }
}

fn banned_phrase_check(scenario: &Scenario, response: &str) -> BannedPhraseCheck {
    let lower = response.to_lowercase();
    let banned = &scenario.expected.must_not_include_phrases;
    if banned.is_empty() {
        return BannedPhraseCheck {
            enabled: false,
            hits: Vec::new(),
            passed: true,
        };
    }
    let hits: Vec<String> = banned
        .iter()
        .filter(|phrase| lower.contains(&phrase.to_lowercase()))
        .cloned()
        .collect();
    let passed = hits.is_empty();
    BannedPhraseCheck {
        enabled: true,
        hits,
        passed,
    }
}

fn required_content_check(scenario: &Scenario, response: &str) -> RequiredContentCheck {
    let lower = response.to_lowercase();
    let required = &scenario.expected.must_include_one_of;
    if required.is_empty() {
        return RequiredContentCheck {
            enabled: false,
            matched: None,
            passed: true,
        };
    }
    let matched = required
        .iter()
        .find(|phrase| lower.contains(&phrase.to_lowercase()))
        .cloned();
    let passed = matched.is_some();
    RequiredContentCheck {
        enabled: true,
        matched,
        passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_eval::scenarios::{Expected, Scenario, ScenarioMeta, Turn};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixture(expected: Expected) -> Scenario {
        Scenario {
            scenario: ScenarioMeta {
                id: "t".into(),
                skill: "inner-work".into(),
                description: String::new(),
                probes: Vec::new(),
            },
            seed_memories: BTreeMap::new(),
            turn: Turn {
                user: "hello".into(),
            },
            expected,
            source_path: PathBuf::new(),
        }
    }

    #[test]
    fn length_check_disabled_when_max_unset() {
        let s = fixture(Expected::default());
        let r = run_checks(&s, "any length is fine");
        assert!(!r.length.enabled);
        assert!(r.length.passed);
    }

    #[test]
    fn length_check_passes_when_within_cap() {
        let s = fixture(Expected {
            max_response_chars: Some(50),
            ..Expected::default()
        });
        let r = run_checks(&s, "short");
        assert!(r.length.enabled);
        assert!(r.length.passed);
    }

    #[test]
    fn length_check_fails_when_over_cap() {
        let s = fixture(Expected {
            max_response_chars: Some(10),
            ..Expected::default()
        });
        let r = run_checks(&s, "this is too long, definitely too long");
        assert!(r.length.enabled);
        assert!(!r.length.passed);
        assert!(!r.passed);
    }

    #[test]
    fn question_density_disabled_when_no_bounds() {
        let s = fixture(Expected::default());
        let r = run_checks(&s, "What? How? Why?");
        assert!(!r.question_density.enabled);
        assert!(r.question_density.passed);
    }

    #[test]
    fn question_density_counts_question_marks() {
        let s = fixture(Expected {
            question_count_max: Some(1),
            ..Expected::default()
        });
        let r = run_checks(&s, "What? How? Why?");
        assert_eq!(r.question_density.question_count, 3);
        assert!(!r.question_density.passed);
    }

    #[test]
    fn question_density_passes_at_lower_bound() {
        let s = fixture(Expected {
            question_count_min: Some(1),
            question_count_max: Some(2),
            ..Expected::default()
        });
        let r = run_checks(&s, "Just one?");
        assert!(r.question_density.passed);
    }

    #[test]
    fn question_density_min_zero_means_no_floor() {
        let s = fixture(Expected {
            question_count_min: Some(0),
            question_count_max: Some(0),
            ..Expected::default()
        });
        let r = run_checks(&s, "No questions here.");
        assert!(r.question_density.passed);
    }

    #[test]
    fn banned_phrase_check_is_case_insensitive() {
        let s = fixture(Expected {
            must_not_include_phrases: vec!["As an AI".into()],
            ..Expected::default()
        });
        let r = run_checks(&s, "AS AN ai I should remind you...");
        assert!(!r.banned_phrases.passed);
        assert_eq!(r.banned_phrases.hits, vec!["As an AI".to_string()]);
    }

    #[test]
    fn banned_phrase_check_clean_when_phrases_absent() {
        let s = fixture(Expected {
            must_not_include_phrases: vec![
                "As an AI".into(),
                "great question".into(),
            ],
            ..Expected::default()
        });
        let r = run_checks(&s, "Honest, specific answer.");
        assert!(r.banned_phrases.passed);
        assert!(r.banned_phrases.hits.is_empty());
    }

    #[test]
    fn required_content_passes_on_first_match() {
        let s = fixture(Expected {
            must_include_one_of: vec![
                "you mentioned Mark".into(),
                "you described Mark".into(),
            ],
            ..Expected::default()
        });
        let r = run_checks(&s, "You described Mark differently last month.");
        assert!(r.required_content.passed);
        assert_eq!(
            r.required_content.matched.as_deref(),
            Some("you described Mark")
        );
    }

    #[test]
    fn required_content_fails_when_none_match() {
        let s = fixture(Expected {
            must_include_one_of: vec!["you mentioned Mark".into()],
            ..Expected::default()
        });
        let r = run_checks(&s, "Tell me more about that.");
        assert!(!r.required_content.passed);
        assert!(r.required_content.matched.is_none());
    }

    #[test]
    fn overall_pass_requires_all_enabled_checks_to_pass() {
        let s = fixture(Expected {
            max_response_chars: Some(100),
            must_not_include_phrases: vec!["As an AI".into()],
            must_include_one_of: vec!["you said".into()],
            ..Expected::default()
        });
        // Length OK + banned absent + required present → pass.
        let r = run_checks(&s, "you said no Saturdays — what changed?");
        assert!(r.passed);

        // Banned hit → fail even though others pass.
        let r2 = run_checks(&s, "as an ai I note you said no Saturdays.");
        assert!(!r2.passed);
        assert!(!r2.banned_phrases.passed);
    }

    #[test]
    fn empty_response_fails_required_but_passes_others() {
        let s = fixture(Expected {
            must_include_one_of: vec!["you said".into()],
            max_response_chars: Some(50),
            ..Expected::default()
        });
        let r = run_checks(&s, "");
        assert!(!r.required_content.passed);
        assert!(r.length.passed); // 0 ≤ 50
        assert!(r.banned_phrases.passed);
        assert!(!r.passed);
    }
}
