//! Tier-B scenario loader.
//!
//! Reads `bench/voice/<id>.toml` files into a typed [`Scenario`].
//! The TOML shape is calibrated for the eight glass-box-voice
//! principles plus the four avoid-list patterns — every scenario
//! probes one or more of them with assertions tight enough to
//! score a real response automatically.
//!
//! ## TOML shape
//!
//! ```toml
//! [scenario]
//! id = "boyfriend-pattern-v-moment"
//! skill = "inner-work"
//! description = "User mentions a partner positively in March, ambivalently in April."
//! probes = ["specific-uncertainty", "contradiction-across-time"]
//!
//! [seed_memories.m1]
//! content = "Mark and I had a really nice weekend."
//! confidence = 0.91
//! created_at = "2026-03-04"
//! source_conversation_id = "c-mar"
//!
//! [turn]
//! user = "I don't know, things with Mark feel off lately."
//!
//! [expected]
//! must_include_one_of = [
//!   "you mentioned Mark",
//!   "you described Mark",
//! ]
//! must_not_include_phrases = [
//!   "As an AI",
//!   "It sounds like you're feeling",
//! ]
//! max_response_chars = 800
//! question_count_min = 0
//! question_count_max = 1
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub scenario: ScenarioMeta,
    #[serde(default)]
    pub seed_memories: BTreeMap<String, SeedMemory>,
    pub turn: Turn,
    pub expected: Expected,
    /// Local path of the loaded file — set by [`load_all`] /
    /// [`load_one`] for diagnostic output. Not part of the TOML.
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    /// Skill the scenario expects to be active. Tier-B harness
    /// activates this skill on the in-process Runtime before the
    /// turn. Today must be `"inner-work"` or `"personal-assistant"`
    /// — the two skills with the relational voice contract.
    pub skill: String,
    #[serde(default)]
    pub description: String,
    /// Human-readable principle / avoid-list tags this scenario
    /// targets. Reported alongside the score so a refinement run
    /// can show "specific-uncertainty: 9/10 scenarios passing".
    #[serde(default)]
    pub probes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedMemory {
    pub content: String,
    pub confidence: f64,
    /// Date string `YYYY-MM-DD` (UTC). Parsed into a Unix
    /// timestamp at scenario-load time so the runner can build a
    /// `Memory` directly. Optional — when absent the memory is
    /// stored without a date prefix.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Optional source-conversation id. Setting it makes the
    /// three-register memory format render the `[YYYY-MM-DD]`
    /// date prefix; leaving it out renders the bullet without a
    /// date even if `created_at` is set.
    #[serde(default)]
    pub source_conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub user: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expected {
    /// At least one of these substrings must appear in the
    /// response (case-insensitive). Empty = no requirement.
    #[serde(default)]
    pub must_include_one_of: Vec<String>,
    /// None of these substrings may appear in the response
    /// (case-insensitive). Used to flag the avoid-list patterns.
    #[serde(default)]
    pub must_not_include_phrases: Vec<String>,
    /// Hard cap on response length in characters.
    #[serde(default)]
    pub max_response_chars: Option<usize>,
    /// Inclusive minimum number of `?` characters in the response.
    /// Pairs with `question_count_max` to encode "exactly one
    /// question" or "no filler questions".
    #[serde(default)]
    pub question_count_min: Option<usize>,
    /// Inclusive maximum number of `?` characters in the response.
    #[serde(default)]
    pub question_count_max: Option<usize>,
    /// Maximum number of distinct snake_case / SCREAMING_SNAKE
    /// identifiers permitted in the response. Set to 0 for "the
    /// witness must not surface any code-shaped tokens" — the
    /// 2026-05-04 incident leaked names like `make_sep_like_parquet`,
    /// `PersonalDomain`, `QueryPlan`, `MIN_CLAIM_LENGTH` into a
    /// heartfelt journal entry, a clear sign that the planner had
    /// invoked a corpus-retrieval path it should never have. This
    /// check is the cheap regression gate against that recurrence.
    /// `None` disables the check.
    #[serde(default)]
    pub max_snake_case_identifier_count: Option<usize>,
}

/// Load every `*.toml` under `dir` (non-recursive) as a scenario.
/// Failures on individual files are reported but don't abort the
/// load — the harness reports both "loaded N scenarios" and "failed
/// to load M files" so an author iterating on a single scenario
/// gets a useful signal even when a sibling file is malformed.
pub fn load_all(dir: &Path) -> Result<Vec<Scenario>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("read scenarios dir {}: {e}", dir.display()))?;

    let mut scenarios: Vec<Scenario> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_one(&path) {
            Ok(s) => scenarios.push(s),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }

    if !errors.is_empty() {
        for e in &errors {
            eprintln!("voice eval: scenario load error — {e}");
        }
    }

    // Stable order by id makes the report deterministic across runs.
    scenarios.sort_by(|a, b| a.scenario.id.cmp(&b.scenario.id));
    Ok(scenarios)
}

pub fn load_one(path: &Path) -> Result<Scenario, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut scenario: Scenario = toml::from_str(&content).map_err(|e| format!("parse: {e}"))?;
    scenario.source_path = path.to_path_buf();
    validate(&scenario)?;
    Ok(scenario)
}

fn validate(s: &Scenario) -> Result<(), String> {
    if s.scenario.id.is_empty() {
        return Err("[scenario].id must be non-empty".into());
    }
    if s.scenario.skill.is_empty() {
        return Err("[scenario].skill must be non-empty".into());
    }
    if s.turn.user.is_empty() {
        return Err("[turn].user must be non-empty".into());
    }
    for (k, m) in &s.seed_memories {
        if m.content.is_empty() {
            return Err(format!("[seed_memories.{k}].content must be non-empty"));
        }
        if !(0.0..=1.0).contains(&m.confidence) {
            return Err(format!(
                "[seed_memories.{k}].confidence must be in [0.0, 1.0], got {}",
                m.confidence
            ));
        }
        if let Some(d) = &m.created_at {
            if parse_date(d).is_none() {
                return Err(format!(
                    "[seed_memories.{k}].created_at must be YYYY-MM-DD, got `{d}`"
                ));
            }
        }
    }
    if let (Some(lo), Some(hi)) = (s.expected.question_count_min, s.expected.question_count_max) {
        if lo > hi {
            return Err(format!(
                "question_count_min ({lo}) > question_count_max ({hi})"
            ));
        }
    }
    Ok(())
}

/// Parse a `YYYY-MM-DD` date as a UTC Unix timestamp at midnight.
/// Returns `None` for malformed input — callers up-stack treat
/// `None` as a validation failure.
pub fn parse_date(s: &str) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let dt = date.and_hms_opt(0, 0, 0)?.and_utc();
    Some(dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn parse_date_round_trips_iso_8601() {
        let ts = parse_date("2026-03-12").unwrap();
        // 2026-03-12 00:00:00 UTC.
        assert_eq!(ts, 1_773_273_600);
    }

    #[test]
    fn parse_date_rejects_garbage() {
        assert!(parse_date("not-a-date").is_none());
        assert!(parse_date("2026-13-01").is_none()); // month out of range
        assert!(parse_date("2026-03").is_none()); // missing day
    }

    #[test]
    fn loads_minimal_valid_scenario() {
        let toml = r#"
[scenario]
id = "test-minimal"
skill = "inner-work"

[turn]
user = "hello"

[expected]
"#;
        let (_dir, path) = write_tmp(toml);
        let s = load_one(&path).unwrap();
        assert_eq!(s.scenario.id, "test-minimal");
        assert_eq!(s.scenario.skill, "inner-work");
        assert_eq!(s.turn.user, "hello");
        assert!(s.seed_memories.is_empty());
    }

    #[test]
    fn loads_full_scenario_with_seed_memories_and_expectations() {
        let toml = r#"
[scenario]
id = "test-full"
skill = "personal-assistant"
description = "covers the full shape"
probes = ["specific-uncertainty", "contradiction-across-time"]

[seed_memories.m1]
content = "I told you no Saturdays"
confidence = 0.92
created_at = "2026-03-01"
source_conversation_id = "c1"

[turn]
user = "schedule for saturday please"

[expected]
must_include_one_of = ["you said no Saturdays"]
must_not_include_phrases = ["As an AI", "great question"]
max_response_chars = 600
question_count_min = 0
question_count_max = 1
"#;
        let (_dir, path) = write_tmp(toml);
        let s = load_one(&path).unwrap();
        assert_eq!(s.scenario.probes.len(), 2);
        assert_eq!(s.seed_memories["m1"].confidence, 0.92);
        assert_eq!(s.expected.max_response_chars, Some(600));
        assert_eq!(s.expected.question_count_max, Some(1));
        assert_eq!(s.expected.must_include_one_of.len(), 1);
        assert_eq!(s.expected.must_not_include_phrases.len(), 2);
    }

    #[test]
    fn rejects_blank_id() {
        let toml = r#"
[scenario]
id = ""
skill = "inner-work"

[turn]
user = "hello"

[expected]
"#;
        let (_dir, path) = write_tmp(toml);
        assert!(load_one(&path).is_err());
    }

    #[test]
    fn rejects_blank_skill() {
        let toml = r#"
[scenario]
id = "x"
skill = ""

[turn]
user = "hello"

[expected]
"#;
        let (_dir, path) = write_tmp(toml);
        assert!(load_one(&path).is_err());
    }

    #[test]
    fn rejects_out_of_range_confidence() {
        let toml = r#"
[scenario]
id = "x"
skill = "inner-work"

[seed_memories.m1]
content = "x"
confidence = 1.5

[turn]
user = "hello"

[expected]
"#;
        let (_dir, path) = write_tmp(toml);
        assert!(load_one(&path).is_err());
    }

    #[test]
    fn rejects_inverted_question_count_range() {
        let toml = r#"
[scenario]
id = "x"
skill = "inner-work"

[turn]
user = "hello"

[expected]
question_count_min = 3
question_count_max = 1
"#;
        let (_dir, path) = write_tmp(toml);
        assert!(load_one(&path).is_err());
    }

    #[test]
    fn rejects_malformed_date() {
        let toml = r#"
[scenario]
id = "x"
skill = "inner-work"

[seed_memories.m1]
content = "x"
confidence = 0.9
created_at = "yesterday"

[turn]
user = "hello"

[expected]
"#;
        let (_dir, path) = write_tmp(toml);
        assert!(load_one(&path).is_err());
    }

    #[test]
    fn load_all_collects_all_toml_files_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            (
                "alpha.toml",
                r#"[scenario]
id = "alpha"
skill = "inner-work"
[turn]
user = "x"
[expected]
"#,
            ),
            (
                "beta.toml",
                r#"[scenario]
id = "beta"
skill = "personal-assistant"
[turn]
user = "y"
[expected]
"#,
            ),
            ("readme.md", "ignored"),
        ] {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let scenarios = load_all(dir.path()).unwrap();
        assert_eq!(scenarios.len(), 2);
        // Sorted by id.
        assert_eq!(scenarios[0].scenario.id, "alpha");
        assert_eq!(scenarios[1].scenario.id, "beta");
    }
}
