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
