//! Question-bank parsing.
//!
//! A bank is a TOML file with one `[bank]` block + N `[[questions]]` rows.
//! See `sovereign-recipes/wikipedia/eval/wikipedia_questions.toml` for the
//! reference shape. The schema is intentionally narrow — every field is
//! either a bare string or a list of strings — so an analyst can hand-edit
//! the file without learning a new format.
//!
//! Validation we enforce up-front (so a typo doesn't surface mid-run):
//!   - `bank.name` and `bank.corpus` non-empty
//!   - every question has a non-empty `id` and `question`
//!   - ids are unique within the bank (we key per-question results on them)
//!   - every question carries at least one `expected_fact` OR `expected_source`
//!     (a question with neither is unscoreable; the bank author meant to
//!     fill it in).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalBank {
    pub bank: BankMeta,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankMeta {
    pub name: String,
    pub corpus: String,
    #[serde(default)]
    pub description: String,
    /// Latency budget for synth-mode wall time, by category. Optional —
    /// banks without a budget skip the over-budget surfacing in the
    /// rollup. Categories not listed here have no budget. Calibrate
    /// once per hardware/model pair: a target that's tight on a Strix
    /// Halo Vulkan + 4B-Q4 setup is loose on a server-class GPU + 27B,
    /// and meaningful comparisons require holding the bench setup
    /// fixed. See `wikipedia_questions.toml` for a reference shape.
    #[serde(default)]
    pub latency_budget: Option<LatencyBudget>,
}

/// Wall-time targets in milliseconds. Values are *budgets*, not hard
/// failures: a row that exceeds is flagged in the report and counted
/// against the over-budget percentage, but the run still completes
/// and per-question scoring is unchanged. Budgets serve regression
/// detection and capacity-planning, not pass/fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyBudget {
    /// Per-category overrides keyed by category string (matches
    /// `Question.category`). Categories absent from the map fall back
    /// to `default_p95_ms` (or no budget if that's also unset).
    #[serde(default)]
    pub by_category: std::collections::BTreeMap<String, CategoryBudget>,
    /// Default p95 budget applied to any category not in
    /// `by_category`. `None` means "no default" — only categories
    /// with explicit budgets get evaluated.
    #[serde(default)]
    pub default_p95_ms: Option<u64>,
    /// Hard wall-time ceiling. Any individual question over this
    /// reads as "stuck" regardless of category. `None` disables.
    #[serde(default)]
    pub max_per_question_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryBudget {
    pub p50_ms: u64,
    pub p95_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub question: String,
    #[serde(default)]
    pub expected_facts: Vec<String>,
    #[serde(default)]
    pub expected_sources: Vec<String>,
    #[serde(default)]
    pub notes: String,
    /// Routing-eval target. Optional: when present, `--routing-only`
    /// scores the classifier against this. When absent, derived from
    /// `category` via `Question::default_expected_intent`. Wire form
    /// is the lowercase Intent variant: `simple_query`,
    /// `knowledge_query`, `deep_query`, `complex_task`, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_intent: Option<String>,
}

impl Question {
    /// Map bank category → expected intent for routing-eval scoring
    /// when no per-question `expected_intent` override is set.
    ///
    /// The mapping reflects the synthesis pipeline's design: short
    /// factual lookups belong on `KnowledgeQuery` (FastFocused or
    /// PrimarySynthesis depending on evidence shape), while
    /// multi-article / causal / comparative / contested questions
    /// require the `DeepQuery` reasoning path with its larger merge
    /// limit and multi-source expansion. `boundary_coverage` is left
    /// permissive — these probe the corpus-vs-training boundary and
    /// either route can be defensible — so it accepts both.
    pub fn default_expected_intent(&self) -> ExpectedIntent {
        match self.category.as_str() {
            "factual_recall" => ExpectedIntent::Exact("knowledge_query"),
            "multi_article_synthesis"
            | "causal_reasoning"
            | "contested" => ExpectedIntent::Exact("deep_query"),
            // `comparative` is the bounded two-entity contrast shape
            // — split off from DeepQuery in the comparison-pre-check
            // landed in the v20 routing pass. Per-question override
            // still lets a recipe pin a different intent when the
            // shape is genuinely open-ended (e.g. "how do X and Y
            // relate" rather than "how do X and Y differ").
            "comparative" => ExpectedIntent::Exact("comparison_query"),
            "boundary_coverage" => {
                ExpectedIntent::AnyOf(&["knowledge_query", "deep_query"])
            }
            _ => ExpectedIntent::AnyOf(&["knowledge_query", "deep_query"]),
        }
    }
}

/// Routing-eval acceptance shape: a single intent or any of a set.
#[derive(Debug, Clone)]
pub enum ExpectedIntent {
    Exact(&'static str),
    AnyOf(&'static [&'static str]),
}

impl ExpectedIntent {
    pub fn matches(&self, actual: &str) -> bool {
        match self {
            ExpectedIntent::Exact(s) => *s == actual,
            ExpectedIntent::AnyOf(set) => set.contains(&actual),
        }
    }

    pub fn label(&self) -> String {
        match self {
            ExpectedIntent::Exact(s) => (*s).into(),
            ExpectedIntent::AnyOf(set) => set.join("|"),
        }
    }
}

pub fn load_bank(path: &Path) -> Result<EvalBank, String> {
    let bytes =
        fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bank: EvalBank =
        toml::from_str(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    validate(&bank)?;
    Ok(bank)
}

fn validate(bank: &EvalBank) -> Result<(), String> {
    if bank.bank.name.trim().is_empty() {
        return Err("bank.name is empty".into());
    }
    if bank.bank.corpus.trim().is_empty() {
        return Err("bank.corpus is empty".into());
    }
    if bank.questions.is_empty() {
        return Err("bank has zero questions".into());
    }

    let mut seen: HashSet<&str> = HashSet::with_capacity(bank.questions.len());
    for q in &bank.questions {
        if q.id.trim().is_empty() {
            return Err("question with empty id".into());
        }
        if !seen.insert(q.id.as_str()) {
            return Err(format!("duplicate question id `{}`", q.id));
        }
        if q.question.trim().is_empty() {
            return Err(format!("question `{}` has empty `question`", q.id));
        }
        if q.expected_facts.is_empty() && q.expected_sources.is_empty() {
            return Err(format!(
                "question `{}` has no expected_facts and no expected_sources \
                 (would be unscoreable)",
                q.id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(toml_str: &str) -> Result<EvalBank, String> {
        let bank: EvalBank = toml::from_str(toml_str).map_err(|e| e.to_string())?;
        validate(&bank)?;
        Ok(bank)
    }

    #[test]
    fn parses_minimal_bank() {
        let src = r#"
[bank]
name = "demo"
corpus = "wikipedia"

[[questions]]
id = "q1"
category = "factual"
question = "What is the capital of France?"
expected_facts = ["Paris"]
"#;
        let b = round_trip(src).unwrap();
        assert_eq!(b.questions.len(), 1);
        assert_eq!(b.questions[0].expected_facts, vec!["Paris".to_string()]);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let src = r#"
[bank]
name = "demo"
corpus = "wikipedia"

[[questions]]
id = "q1"
category = "factual"
question = "A?"
expected_facts = ["a"]

[[questions]]
id = "q1"
category = "factual"
question = "B?"
expected_facts = ["b"]
"#;
        assert!(round_trip(src).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn rejects_unscoreable_question() {
        let src = r#"
[bank]
name = "demo"
corpus = "wikipedia"

[[questions]]
id = "q1"
category = "factual"
question = "?"
"#;
        assert!(round_trip(src).unwrap_err().contains("unscoreable"));
    }

    #[test]
    fn rejects_empty_corpus() {
        let src = r#"
[bank]
name = "demo"
corpus = ""

[[questions]]
id = "q1"
category = "factual"
question = "A?"
expected_facts = ["a"]
"#;
        assert!(round_trip(src).unwrap_err().contains("corpus"));
    }
}
