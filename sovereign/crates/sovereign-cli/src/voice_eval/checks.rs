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
//! not punctuation. Curly quotes (`'`, `'`, `"`, `"`) and dashes
//! (em/en) are normalised to ASCII before matching, so a witness
//! reply that uses typographic quotes ("I don't have a record" with
//! a curly apostrophe) doesn't fail a `must_include` like
//! "I don't have a record" written in straight ASCII.

use serde::{Deserialize, Serialize};

use super::scenarios::Scenario;

/// Normalise typographic punctuation to ASCII equivalents. Used by
/// `required_content_check` and `banned_phrase_check` so curly
/// quotes in model output don't defeat substring matching against
/// scenario phrases written in straight ASCII.
///
/// Iter2 H02 was the canonical reproduction: the model wrote
/// "I don't have any record" (curly apostrophe), the must-include
/// list had "I don't have" (straight), and the substring match
/// failed despite the witness move being executed correctly.
fn normalise_quotes(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201F}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            other => other,
        })
        .collect()
}

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
    pub code_identifier: CodeIdentifierCheck,
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

/// Snake_case / SCREAMING_SNAKE identifier scan. Witness responses
/// are prose; identifiers like `make_sep_like_parquet` or
/// `MIN_CLAIM_LENGTH` are a tell that the planner invoked a
/// corpus-retrieval path and the model echoed chunk titles into
/// its output. The 2026-05-04 inner-work incident — heartfelt
/// journal routed through the citation-grounded knowledge prompt —
/// is the canonical reproduction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeIdentifierCheck {
    pub enabled: bool,
    /// Distinct identifiers found in the response, sorted by
    /// first-seen order. Capped at 32 in the report so a runaway
    /// regression doesn't bloat the JSON file.
    pub matches: Vec<String>,
    pub count: usize,
    pub max: Option<usize>,
    pub passed: bool,
}

/// Run all enabled deterministic checks for a scenario against a
/// candidate response. Pure function — no I/O, no inference.
pub fn run_checks(scenario: &Scenario, response: &str) -> ScenarioResult {
    let length = length_check(scenario, response);
    let question_density = question_density_check(scenario, response);
    let banned_phrases = banned_phrase_check(scenario, response);
    let required_content = required_content_check(scenario, response);
    let code_identifier = code_identifier_check(scenario, response);

    let passed = length.passed
        && question_density.passed
        && banned_phrases.passed
        && required_content.passed
        && code_identifier.passed;

    ScenarioResult {
        scenario_id: scenario.scenario.id.clone(),
        skill: scenario.scenario.skill.clone(),
        probes: scenario.scenario.probes.clone(),
        response: response.to_string(),
        length,
        question_density,
        banned_phrases,
        required_content,
        code_identifier,
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
    let lower = normalise_quotes(&response.to_lowercase());
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
        .filter(|phrase| lower.contains(&normalise_quotes(&phrase.to_lowercase())))
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
    let lower = normalise_quotes(&response.to_lowercase());
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
        .find(|phrase| lower.contains(&normalise_quotes(&phrase.to_lowercase())))
        .cloned();
    let passed = matched.is_some();
    RequiredContentCheck {
        enabled: true,
        matched,
        passed,
    }
}

/// Maximum number of distinct snake_case identifiers we'll keep in
/// the report. A clean witness response has zero; a polluted one
/// might have dozens. Capping the report list keeps the JSON small
/// without losing the regression signal — the `count` field still
/// reflects the true total even when `matches` is truncated.
const CODE_IDENTIFIER_REPORT_CAP: usize = 32;

/// Returns true when `word` is a snake_case / SCREAMING_SNAKE
/// identifier of the shape `<seg>(_<seg>)+` where each segment is
/// `[A-Za-z][A-Za-z0-9]*` and at least one underscore separates two
/// non-empty alphanumeric segments.
///
/// Conservative on purpose: a single underscore between two letters
/// is enough to flag the token. False positives (e.g. user prose
/// that legitimately contains `well_being` or `co_worker`) are
/// possible but rare in witness register; the cost of a false
/// positive is "bench fails, author tightens the prompt or adds an
/// allow-list" — far cheaper than missing a corpus-pollution
/// regression. If the false-positive rate becomes a problem,
/// consider an `allow_snake_case` field on `Expected`.
fn is_codeish_identifier(word: &str) -> bool {
    if !word.contains('_') {
        return false;
    }
    let segments: Vec<&str> = word.split('_').collect();
    if segments.len() < 2 {
        return false;
    }
    for seg in &segments {
        if seg.is_empty() {
            return false; // leading/trailing/double underscore
        }
        let mut chars = seg.chars();
        let first = chars.next().expect("non-empty");
        if !first.is_ascii_alphabetic() {
            return false;
        }
        for c in chars {
            if !c.is_ascii_alphanumeric() {
                return false;
            }
        }
    }
    true
}

fn code_identifier_check(scenario: &Scenario, response: &str) -> CodeIdentifierCheck {
    let max = scenario.expected.max_snake_case_identifier_count;
    let Some(max) = max else {
        return CodeIdentifierCheck {
            enabled: false,
            matches: Vec::new(),
            count: 0,
            max: None,
            passed: true,
        };
    };

    // Tokenise on anything that's not [A-Za-z0-9_]. Track first-seen
    // order via a Vec<String> and a HashSet for dedup; preserves the
    // most useful debugging signal (which identifier appeared first)
    // when truncating to the report cap.
    let mut current = String::new();
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut total: usize = 0;
    for c in response.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            current.push(c);
        } else {
            if !current.is_empty() {
                if is_codeish_identifier(&current) && seen.insert(current.clone()) {
                    total += 1;
                    if ordered.len() < CODE_IDENTIFIER_REPORT_CAP {
                        ordered.push(current.clone());
                    }
                }
                current.clear();
            }
        }
    }
    if !current.is_empty()
        && is_codeish_identifier(&current)
        && seen.insert(current.clone())
    {
        total += 1;
        if ordered.len() < CODE_IDENTIFIER_REPORT_CAP {
            ordered.push(current);
        }
    }

    CodeIdentifierCheck {
        enabled: true,
        matches: ordered,
        count: total,
        max: Some(max),
        passed: total <= max,
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
    fn code_identifier_disabled_when_max_unset() {
        let s = fixture(Expected::default());
        let r = run_checks(&s, "the words make_sep_like_parquet appear");
        assert!(!r.code_identifier.enabled);
        assert!(r.code_identifier.passed);
    }

    #[test]
    fn code_identifier_passes_on_pure_prose() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(0),
            ..Expected::default()
        });
        let r = run_checks(&s, "I notice you're sitting with something difficult.");
        assert!(r.code_identifier.enabled);
        assert!(r.code_identifier.passed);
        assert_eq!(r.code_identifier.count, 0);
        assert!(r.code_identifier.matches.is_empty());
    }

    #[test]
    fn code_identifier_flags_snake_case() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(0),
            ..Expected::default()
        });
        let r = run_checks(
            &s,
            "Looking at retrieved sources: make_sep_like_parquet, \
             skeleton_extraction_prompt, and ingest were considered.",
        );
        assert!(!r.code_identifier.passed);
        assert_eq!(r.code_identifier.count, 2);
        assert!(r.code_identifier.matches.contains(&"make_sep_like_parquet".to_string()));
        assert!(r.code_identifier.matches.contains(&"skeleton_extraction_prompt".to_string()));
    }

    #[test]
    fn code_identifier_flags_screaming_snake() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(0),
            ..Expected::default()
        });
        let r = run_checks(&s, "MIN_CLAIM_LENGTH must be at least 20 chars.");
        assert!(!r.code_identifier.passed);
        assert_eq!(r.code_identifier.count, 1);
        assert_eq!(r.code_identifier.matches, vec!["MIN_CLAIM_LENGTH"]);
    }

    #[test]
    fn code_identifier_dedupes_repeats() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(5),
            ..Expected::default()
        });
        let r = run_checks(&s, "ingest_chunk and ingest_chunk and ingest_chunk again");
        assert_eq!(r.code_identifier.count, 1);
        assert_eq!(r.code_identifier.matches, vec!["ingest_chunk"]);
        assert!(r.code_identifier.passed); // 1 ≤ 5
    }

    #[test]
    fn code_identifier_ignores_single_word_or_lone_underscore() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(0),
            ..Expected::default()
        });
        // Pure prose with no underscore — must not flag.
        let r = run_checks(&s, "ingest is fine, simple words pass");
        assert!(r.code_identifier.passed);
        // Leading/trailing/double underscore are not identifiers.
        let r2 = run_checks(&s, "_leading and trailing_ and __double__");
        assert!(r2.code_identifier.passed);
    }

    #[test]
    fn code_identifier_threshold_allows_some() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(2),
            ..Expected::default()
        });
        let r = run_checks(&s, "saw query_plan and corpus_id");
        assert_eq!(r.code_identifier.count, 2);
        assert!(r.code_identifier.passed); // 2 ≤ 2
        let r2 = run_checks(&s, "saw query_plan and corpus_id and chunk_id");
        assert_eq!(r2.code_identifier.count, 3);
        assert!(!r2.code_identifier.passed); // 3 > 2
    }

    /// Smoke-test the actual leaked output from the 2026-05-04
    /// inner-work incident. The bench is only as honest as the
    /// canary; if the canary doesn't trip, the bench isn't tight.
    #[test]
    fn code_identifier_catches_2026_05_04_leak() {
        let s = fixture(Expected {
            max_snake_case_identifier_count: Some(0),
            ..Expected::default()
        });
        let leaked = "Looking at my retrieved sources: \
                      - make_sep_like_parquet \
                      - long_passage \
                      - skeleton_extraction_prompt \
                      - PersonalDomain (no underscore — not a hit) \
                      - MIN_CLAIM_LENGTH \
                      - recommendation_for \
                      - ExemplarKind (no underscore) \
                      - QueryPlan (no underscore) \
                      None of these passages contain the phrases.";
        let r = run_checks(&s, leaked);
        assert!(!r.code_identifier.passed);
        // Five snake_case / SCREAMING_SNAKE: make_sep_like_parquet,
        // long_passage, skeleton_extraction_prompt, MIN_CLAIM_LENGTH,
        // recommendation_for. The CamelCase ones are deliberately
        // not matched — that's a separate signal class.
        assert_eq!(r.code_identifier.count, 5);
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
