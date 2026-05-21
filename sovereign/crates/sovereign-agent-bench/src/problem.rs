//! Problem TOML schema + loader.
//!
//! On-disk shape (per `~/.claude/plans/i-want-to-pickup-sorted-eagle.md`):
//!
//! ```toml
//! [meta]
//! id = "3.2-lights-out"
//! title = "Light's Out — find a clicking sequence over GF(2)"
//! category = "Algorithmic"
//! version = "v0"
//! notes = ""
//!
//! [prompt]
//! file = "prompt.md"
//!
//! [witness]
//! kind = "AutoTestPass"
//! language = "Rust"
//! fixture_subdir = "fixtures"
//! verify_cmd = "cargo test --test integration -- --nocapture"
//! score_buckets = [[0.0, 0.25, 0], [0.25, 0.6, 1], [0.6, 0.85, 2], [0.85, 1.001, 3]]
//!
//! [budget]
//! token_cap = 16000
//! wall_seconds_cap = 600
//!
//! [scoring.dim_a]
//! name = "Correctness"
//! mode = "AutoTestPassFraction"
//!
//! [scoring.dim_b]
//! name = "Algorithmic insight"
//! mode = "JudgeRubric"
//! rubric_id = "dim_b"
//!
//! [scoring.dim_c]
//! name = "Code quality / efficiency"
//! mode = "HybridAutoFloor"
//! rubric_id = "dim_c"
//! ```
//!
//! Closed enums per ARCH §2.1. Data lives alongside the problem dir
//! (rubric.md, prompt.md, fixtures/) per ARCH §6.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Category {
    Algorithmic,
    SystemDesign,
    CodeTest,
}

impl Category {
    pub fn id(&self) -> &'static str {
        match self {
            Category::Algorithmic => "Algorithmic",
            Category::SystemDesign => "SystemDesign",
            Category::CodeTest => "CodeTest",
        }
    }
}

/// Difficulty tier — separates "did the agent implement an algorithm
/// given a clean scaffold" from "did the agent scaffold a Rust project
/// from scratch." Two distinct signals; collapsing them obscures both.
///
/// `Scaffolded` (Level 1) — the harness pre-copies a `scaffold/`
/// directory into the workdir before the agent runs. Agent's job is
/// strictly the algorithmic work.
///
/// `FromScratch` (Level 2) — the harness gives the agent an empty
/// workdir and the agent has to scaffold the cargo project itself.
/// This exercises the full tool-call + project-scaffolding surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tier {
    Scaffolded,
    FromScratch,
}

impl Tier {
    pub fn id(&self) -> &'static str {
        match self {
            Tier::Scaffolded => "Scaffolded",
            Tier::FromScratch => "FromScratch",
        }
    }
}

impl Default for Tier {
    fn default() -> Self {
        // Default to from-scratch so problems that don't declare a
        // tier explicitly behave like the pre-tier MVS — no implicit
        // scaffold copy.
        Tier::FromScratch
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WitnessKind {
    AutoTestPass,
    JudgeOnly,
    Hybrid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WitnessLanguage {
    Rust,
    Go,
    TypeScript,
    Python,
}

impl WitnessLanguage {
    pub fn id(&self) -> &'static str {
        match self {
            WitnessLanguage::Rust => "Rust",
            WitnessLanguage::Go => "Go",
            WitnessLanguage::TypeScript => "TypeScript",
            WitnessLanguage::Python => "Python",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode")]
pub enum ScoringMode {
    AutoTestPassFraction,
    JudgeRubric { rubric_id: String },
    HybridAutoFloor { rubric_id: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProblemMeta {
    pub id: String,
    pub title: String,
    pub category: Category,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub notes: String,
    /// Difficulty tier. Defaults to `FromScratch` for back-compat
    /// with the MVS problem shape.
    #[serde(default)]
    pub tier: Tier,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptCfg {
    pub file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WitnessCfg {
    pub kind: WitnessKind,
    pub language: WitnessLanguage,
    pub fixture_subdir: String,
    pub verify_cmd: String,
    /// Optional per-language build command (cargo build, go build,
    /// tsc --noEmit, etc.). The canonical `build` primitive runs
    /// this. When `None`, the executor returns a no-op success
    /// (interpreted languages: Python).
    #[serde(default)]
    pub build_cmd: Option<String>,
    /// Optional subdirectory copied into the agent's workdir BEFORE
    /// the agent runs. Used to pre-supply scaffolding (Cargo.toml +
    /// src/lib.rs stub) so the bench measures algorithmic work, not
    /// project-scaffolding fluency. When `None`, the agent starts
    /// with an empty workdir (FromScratch tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaffold_subdir: Option<String>,
    /// Bucket rows are `[low_inclusive, high_exclusive, score]`. Score
    /// is `0..=3`. Buckets are sorted ascending by `low`.
    #[serde(default)]
    pub score_buckets: Vec<[f64; 3]>,
}

impl WitnessCfg {
    /// Resolved build command — falls back to a per-language default
    /// when problem.toml didn't set one explicitly. Python's default
    /// is empty (no-op build).
    pub fn resolved_build_cmd(&self) -> String {
        if let Some(cmd) = self.build_cmd.as_deref() {
            return cmd.to_string();
        }
        match self.language {
            WitnessLanguage::Rust => "cargo build 2>&1".to_string(),
            WitnessLanguage::Go => "go build ./... 2>&1".to_string(),
            WitnessLanguage::TypeScript => "tsc --noEmit 2>&1".to_string(),
            WitnessLanguage::Python => String::new(), // interpreted
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BudgetCfg {
    pub token_cap: u64,
    pub wall_seconds_cap: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoringDimCfg {
    pub name: String,
    #[serde(flatten)]
    pub mode: ScoringMode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoringCfg {
    pub dim_a: ScoringDimCfg,
    pub dim_b: ScoringDimCfg,
    pub dim_c: ScoringDimCfg,
}

#[derive(Debug, Clone, Deserialize)]
struct RawProblem {
    meta: ProblemMeta,
    prompt: PromptCfg,
    witness: WitnessCfg,
    budget: BudgetCfg,
    scoring: ScoringCfg,
}

/// Loaded problem — TOML config + materialised prompt + rubric anchors.
#[derive(Debug, Clone)]
pub struct Problem {
    pub meta: ProblemMeta,
    pub prompt: PromptCfg,
    pub witness: WitnessCfg,
    pub budget: BudgetCfg,
    pub scoring: ScoringCfg,
    /// Contents of `prompt.md` — handed to the agent verbatim.
    pub prompt_text: String,
    /// Rubric anchor prose, keyed by `rubric_id` then a 0..=3 index.
    /// `["dim_b"][2]` is the anchor text for "score = 2" on dim_b.
    pub rubric_anchors: HashMap<String, [String; 4]>,
    /// Absolute path to the problem directory (used by witness to
    /// resolve `fixture_subdir`).
    pub problem_dir: PathBuf,
}

impl Problem {
    pub fn fixture_path(&self) -> PathBuf {
        self.problem_dir.join(&self.witness.fixture_subdir)
    }

    /// Absolute path to the scaffold directory, if the problem
    /// declared one. Caller is responsible for handling `None`.
    pub fn scaffold_path(&self) -> Option<PathBuf> {
        self.witness
            .scaffold_subdir
            .as_ref()
            .map(|s| self.problem_dir.join(s))
    }

    pub fn rubric_for(&self, rubric_id: &str) -> Option<&[String; 4]> {
        self.rubric_anchors.get(rubric_id)
    }
}

#[derive(Debug, Error)]
pub enum ProblemLoadError {
    #[error("problem directory not found: {0}")]
    NotFound(PathBuf),
    #[error("problem.toml missing at {0}")]
    TomlMissing(PathBuf),
    #[error("problem.toml parse error: {0}")]
    TomlParse(String),
    #[error("prompt file missing at {0}")]
    PromptMissing(PathBuf),
    #[error("rubric file missing at {0} (required by dim_b/dim_c mode)")]
    RubricMissing(PathBuf),
    #[error("rubric.md missing anchors for `{rubric_id}` (need ## {rubric_id} with ### 0..3 subheads)")]
    RubricIncomplete { rubric_id: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Load a problem from `<bench-root>/problems/<id>/`.
pub fn load_problem(problem_dir: &Path) -> Result<Problem, ProblemLoadError> {
    if !problem_dir.is_dir() {
        return Err(ProblemLoadError::NotFound(problem_dir.to_path_buf()));
    }
    let toml_path = problem_dir.join("problem.toml");
    let toml_str = std::fs::read_to_string(&toml_path)
        .map_err(|_| ProblemLoadError::TomlMissing(toml_path.clone()))?;
    let raw: RawProblem =
        toml::from_str(&toml_str).map_err(|e| ProblemLoadError::TomlParse(e.to_string()))?;

    let prompt_path = problem_dir.join(&raw.prompt.file);
    let prompt_text = std::fs::read_to_string(&prompt_path)
        .map_err(|_| ProblemLoadError::PromptMissing(prompt_path.clone()))?;

    // Rubric is required iff any judge-mode dimension references it.
    let mut needed_rubric_ids: Vec<String> = Vec::new();
    for dim in [&raw.scoring.dim_a, &raw.scoring.dim_b, &raw.scoring.dim_c] {
        match &dim.mode {
            ScoringMode::JudgeRubric { rubric_id }
            | ScoringMode::HybridAutoFloor { rubric_id } => {
                needed_rubric_ids.push(rubric_id.clone());
            }
            ScoringMode::AutoTestPassFraction => {}
        }
    }

    let rubric_anchors = if needed_rubric_ids.is_empty() {
        HashMap::new()
    } else {
        let rubric_path = problem_dir.join("rubric.md");
        let rubric_str = std::fs::read_to_string(&rubric_path)
            .map_err(|_| ProblemLoadError::RubricMissing(rubric_path.clone()))?;
        let parsed = parse_rubric_markdown(&rubric_str);
        for id in &needed_rubric_ids {
            let entry = parsed.get(id);
            let complete = entry
                .map(|anchors| anchors.iter().all(|a| !a.is_empty()))
                .unwrap_or(false);
            if !complete {
                return Err(ProblemLoadError::RubricIncomplete {
                    rubric_id: id.clone(),
                });
            }
        }
        parsed
    };

    Ok(Problem {
        meta: raw.meta,
        prompt: raw.prompt,
        witness: raw.witness,
        budget: raw.budget,
        scoring: raw.scoring,
        prompt_text,
        rubric_anchors,
        problem_dir: problem_dir.to_path_buf(),
    })
}

/// Parse a `rubric.md` of the form:
///
/// ```text
/// ## dim_b
/// ### 0
/// anchor prose…
/// ### 1
/// anchor prose…
/// ### 2
/// anchor prose…
/// ### 3
/// anchor prose…
///
/// ## dim_c
/// ### 0
/// …
/// ```
///
/// Anchors that aren't found are left as empty strings; the loader's
/// completeness check above turns missing anchors into a hard error.
pub fn parse_rubric_markdown(src: &str) -> HashMap<String, [String; 4]> {
    let mut out: HashMap<String, [String; 4]> = HashMap::new();
    let mut current_dim: Option<String> = None;
    let mut current_anchor: Option<usize> = None;
    let mut acc: Vec<String> = Vec::new();

    let flush = |out: &mut HashMap<String, [String; 4]>,
                 current_dim: &Option<String>,
                 current_anchor: Option<usize>,
                 acc: &mut Vec<String>| {
        if let (Some(dim), Some(idx)) = (current_dim, current_anchor) {
            let text = acc.join("\n").trim().to_string();
            let entry = out.entry(dim.clone()).or_insert_with(Default::default);
            if idx < 4 {
                entry[idx] = text;
            }
        }
        acc.clear();
    };

    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // New dim. Flush prior anchor.
            flush(&mut out, &current_dim, current_anchor, &mut acc);
            current_dim = Some(rest.trim().to_string());
            current_anchor = None;
        } else if let Some(rest) = line.strip_prefix("### ") {
            flush(&mut out, &current_dim, current_anchor, &mut acc);
            let n: Option<usize> = rest.trim().parse().ok();
            current_anchor = n.filter(|n| *n < 4);
        } else {
            acc.push(line.to_string());
        }
    }
    flush(&mut out, &current_dim, current_anchor, &mut acc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let target = dir.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(target, body).unwrap();
        }
    }

    fn minimal_problem_toml() -> &'static str {
        r#"
[meta]
id = "test-1"
title = "T"
category = "Algorithmic"
version = "v0"

[prompt]
file = "prompt.md"

[witness]
kind = "AutoTestPass"
language = "Rust"
fixture_subdir = "fixtures"
verify_cmd = "cargo test --test integration -- --nocapture"
score_buckets = [[0.0, 0.5, 0], [0.5, 1.001, 3]]

[budget]
token_cap = 1000
wall_seconds_cap = 60

[scoring.dim_a]
name = "Correctness"
mode = "AutoTestPassFraction"

[scoring.dim_b]
name = "Approach"
mode = "JudgeRubric"
rubric_id = "dim_b"

[scoring.dim_c]
name = "Efficiency"
mode = "HybridAutoFloor"
rubric_id = "dim_c"
"#
    }

    fn full_rubric_md() -> &'static str {
        "## dim_b\n### 0\nwrong family\n### 1\nright family suboptimal\n### 2\nright family good\n### 3\noptimal\n\n## dim_c\n### 0\nwasteful\n### 1\nokay\n### 2\nefficient\n### 3\nminimal\n"
    }

    #[test]
    fn load_problem_happy_path() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            &[
                ("problem.toml", minimal_problem_toml()),
                ("prompt.md", "solve it"),
                ("rubric.md", full_rubric_md()),
                ("fixtures/.keep", ""),
            ],
        );
        let p = load_problem(tmp.path()).unwrap();
        assert_eq!(p.meta.id, "test-1");
        assert_eq!(p.meta.category, Category::Algorithmic);
        assert_eq!(p.witness.language, WitnessLanguage::Rust);
        let dim_b = p.rubric_for("dim_b").unwrap();
        assert_eq!(dim_b[3], "optimal");
        assert_eq!(p.fixture_path(), tmp.path().join("fixtures"));
    }

    #[test]
    fn load_problem_rejects_missing_dir() {
        let err = load_problem(Path::new("/this/is/definitely/not/there"));
        assert!(matches!(err, Err(ProblemLoadError::NotFound(_))));
    }

    #[test]
    fn load_problem_rejects_missing_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_problem(tmp.path());
        assert!(matches!(err, Err(ProblemLoadError::TomlMissing(_))));
    }

    #[test]
    fn load_problem_rejects_incomplete_rubric() {
        let tmp = tempfile::tempdir().unwrap();
        // Only dim_b is defined; dim_c is needed by HybridAutoFloor.
        let partial = "## dim_b\n### 0\nwrong\n### 1\nok\n### 2\ngood\n### 3\nbest\n";
        write_fixture(
            tmp.path(),
            &[
                ("problem.toml", minimal_problem_toml()),
                ("prompt.md", "do it"),
                ("rubric.md", partial),
            ],
        );
        let err = load_problem(tmp.path()).unwrap_err();
        match err {
            ProblemLoadError::RubricIncomplete { rubric_id } => {
                assert_eq!(rubric_id, "dim_c");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_rubric_markdown_extracts_four_anchors_per_dim() {
        let map = parse_rubric_markdown(full_rubric_md());
        let a = map.get("dim_b").unwrap();
        assert_eq!(a[0], "wrong family");
        assert_eq!(a[3], "optimal");
        let c = map.get("dim_c").unwrap();
        assert_eq!(c[2], "efficient");
    }
}
