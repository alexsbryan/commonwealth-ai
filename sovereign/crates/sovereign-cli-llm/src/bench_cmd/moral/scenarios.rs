// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moral-reasoning scenario loader.
//!
//! Reads `bench/moral/scenarios/<id>.toml` files converted from the
//! MoReBench public split (see `bench/moral/README.md` for provenance
//! and the scoring contract). Each scenario is one dilemma plus a
//! list of weighted rubric criteria, each tagged with the dimension
//! of moral reasoning it probes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The five MoReBench rubric dimensions plus the small `other`
/// bucket present in the upstream data. Closed set — new upstream
/// tags should fail loading loudly rather than silently aggregate
/// under a wrong key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dimension {
    #[serde(rename = "identifying")]
    Identifying,
    #[serde(rename = "logical process")]
    LogicalProcess,
    #[serde(rename = "clear process")]
    ClearProcess,
    #[serde(rename = "helpful outcome")]
    HelpfulOutcome,
    #[serde(rename = "harmless outcome")]
    HarmlessOutcome,
    #[serde(rename = "other")]
    Other,
}

impl Dimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            Dimension::Identifying => "identifying",
            Dimension::LogicalProcess => "logical process",
            Dimension::ClearProcess => "clear process",
            Dimension::HelpfulOutcome => "helpful outcome",
            Dimension::HarmlessOutcome => "harmless outcome",
            Dimension::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub scenario: ScenarioMeta,
    pub dilemma: Dilemma,
    /// Default-empty so a criteria-less file reaches `validate`,
    /// which names the actual problem ("no criteria — nothing to
    /// judge") instead of serde's "missing field".
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Path of the loaded file, for diagnostics. Not part of the TOML.
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    /// Provenance tag, e.g. `morebench_public`.
    pub source: String,
    #[serde(default)]
    pub dilemma_source: String,
    #[serde(default)]
    pub dilemma_type: String,
    /// `ai_advisor` (model advises a human) or `ai_agent` (model
    /// decides autonomously). Reported as a slice axis.
    #[serde(default)]
    pub role_domain: String,
    #[serde(default)]
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dilemma {
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub id: String,
    pub text: String,
    pub dimension: Dimension,
    /// Signed weight. Positive = good reasoning includes this;
    /// negative = good reasoning avoids this. Never zero.
    pub weight: i32,
}

/// Load every `*.toml` under `dir` (non-recursive). Individual file
/// failures are reported and skipped so a malformed sibling doesn't
/// hide the rest of the bank; the caller sees both counts.
pub fn load_all(dir: &Path) -> Result<Vec<Scenario>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("read scenarios dir {}: {e}", dir.display()))?;

    let mut scenarios = Vec::new();
    let mut errors = Vec::new();
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
    for e in &errors {
        eprintln!("bench moral: scenario load error — {e}");
    }
    // Stable order by id keeps reports deterministic across runs.
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
    if s.dilemma.prompt.trim().is_empty() {
        return Err("[dilemma].prompt must be non-empty".into());
    }
    if s.criteria.is_empty() {
        return Err("scenario has no criteria — nothing to judge".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &s.criteria {
        if c.id.is_empty() {
            return Err("criterion with empty id".into());
        }
        if !seen.insert(c.id.as_str()) {
            return Err(format!("duplicate criterion id `{}`", c.id));
        }
        if c.text.trim().is_empty() {
            return Err(format!("criterion `{}` has empty text", c.id));
        }
        if c.weight == 0 {
            return Err(format!(
                "criterion `{}` has weight 0 — a zero-weight criterion can never move the score \
                 and would silently pad the denominator",
                c.id
            ));
        }
        if c.weight.abs() > 10 {
            return Err(format!(
                "criterion `{}` weight {} outside sane range (upstream uses -3..=3)",
                c.id, c.weight
            ));
        }
    }
    Ok(())
}

/// Default scenarios dir resolved by walking up from CWD, mirroring
/// `voice_eval::resolve_scenarios_dir`.
pub const DEFAULT_SCENARIOS_DIR: &str = "bench/moral/scenarios";

pub fn resolve_scenarios_dir(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        let p = PathBuf::from(p);
        return if p.is_dir() {
            Ok(p)
        } else {
            Err(format!(
                "--scenarios-dir `{}` is not a directory",
                p.display()
            ))
        };
    }
    let mut here =
        std::env::current_dir().map_err(|e| format!("cannot resolve current dir: {e}"))?;
    loop {
        for prefix in ["", "sovereign"] {
            let candidate = if prefix.is_empty() {
                here.join(DEFAULT_SCENARIOS_DIR)
            } else {
                here.join(prefix).join(DEFAULT_SCENARIOS_DIR)
            };
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }
        if !here.pop() {
            break;
        }
    }
    Err(format!(
        "could not find `{DEFAULT_SCENARIOS_DIR}` walking up from CWD. Pass --scenarios-dir."
    ))
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

    const MINIMAL: &str = r#"
[scenario]
id = "mb-test"
source = "morebench_public"
role_domain = "ai_advisor"

[dilemma]
prompt = "Should I tell my friend the truth?"

[[criteria]]
id = "c1"
text = "Identifies the honesty vs. loyalty tension"
dimension = "identifying"
weight = 2

[[criteria]]
id = "c2"
text = "Asserts there is one objectively correct answer"
dimension = "logical process"
weight = -3
"#;

    #[test]
    fn loads_minimal_scenario_with_signed_weights() {
        let (_d, p) = write_tmp(MINIMAL);
        let s = load_one(&p).unwrap();
        assert_eq!(s.scenario.id, "mb-test");
        assert_eq!(s.criteria.len(), 2);
        assert_eq!(s.criteria[0].dimension, Dimension::Identifying);
        assert_eq!(s.criteria[1].weight, -3);
    }

    #[test]
    fn rejects_zero_weight() {
        let toml = MINIMAL.replace("weight = 2", "weight = 0");
        let (_d, p) = write_tmp(&toml);
        let err = load_one(&p).unwrap_err();
        assert!(err.contains("weight 0"));
    }

    #[test]
    fn rejects_unknown_dimension() {
        let toml = MINIMAL.replace("\"identifying\"", "\"vibes\"");
        let (_d, p) = write_tmp(&toml);
        assert!(load_one(&p).is_err());
    }

    #[test]
    fn rejects_duplicate_criterion_ids() {
        let toml = MINIMAL.replace("id = \"c2\"", "id = \"c1\"");
        let (_d, p) = write_tmp(&toml);
        let err = load_one(&p).unwrap_err();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn rejects_empty_criteria_list() {
        let toml = r#"
[scenario]
id = "x"
source = "s"

[dilemma]
prompt = "p"
"#;
        let (_d, p) = write_tmp(toml);
        let err = load_one(&p).unwrap_err();
        assert!(err.contains("no criteria"));
    }

    #[test]
    fn checked_in_bank_loads_cleanly() {
        // The real bank ships in-repo; loading it here keeps the
        // converter's output contract and this loader from drifting
        // apart. Skipped silently only if the bank dir is absent
        // (e.g. a filtered source checkout).
        let dir = match resolve_scenarios_dir(None) {
            Ok(d) => d,
            Err(_) => return,
        };
        let scenarios = load_all(&dir).unwrap();
        assert!(
            scenarios.len() >= 20,
            "expected the checked-in bank (24 scenarios), got {}",
            scenarios.len()
        );
        let criteria: usize = scenarios.iter().map(|s| s.criteria.len()).sum();
        assert!(criteria > 400, "expected ~554 criteria, got {criteria}");
    }
}
