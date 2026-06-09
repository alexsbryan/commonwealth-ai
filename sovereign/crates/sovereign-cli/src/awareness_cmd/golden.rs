// SPDX-License-Identifier: AGPL-3.0-or-later
//! Golden-set comparison primitives shared by `eval` and
//! `scenario`. Two input shapes:
//!
//! 1. **Template metadata** — the `expected_entities` and
//!    `expected_suggestions` arrays declared in a built-in or
//!    file-loaded scenario template (see `templates/`). Cheapest
//!    path; the template is the source of truth.
//!
//! 2. **JSONL golden file** — one record per line, each carrying
//!    a `conversation_id` plus per-turn expectations. Per the
//!    spec; lets the developer score against human-labeled custom
//!    data when the templates are insufficient.
//!
//! Both shapes flow through [`GoldenSet`] before scoring, so the
//! comparison engine is shape-agnostic.

use serde::{Deserialize, Serialize};

use super::templates::Template;

/// Combined golden-set view that the comparison engine reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct GoldenSet {
    pub expected_entities: Vec<ExpectedEntity>,
    pub expected_suggestions: Vec<ExpectedSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExpectedEntity {
    pub name: String,
    pub kind: String, // "person" | "organization" | "initiative"
    #[serde(default)]
    pub affiliation: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExpectedSuggestion {
    pub conversation_id: String,
    pub turn: u32,
    pub kind: String,
    #[serde(default)]
    pub content_contains: Option<String>,
    #[serde(default)]
    pub related_entity: Option<String>,
}

impl GoldenSet {
    /// Build from a template's metadata. The template loader
    /// already validated the kind enum so we just project.
    pub(super) fn from_template(t: &Template) -> Self {
        let expected_entities = t
            .expected_entities
            .iter()
            .map(|e| ExpectedEntity {
                name: e.name.clone(),
                kind: e.kind.clone(),
                affiliation: e.affiliation.clone(),
                role: e.role.clone(),
                participants: e.participants.clone(),
            })
            .collect();
        let expected_suggestions = t
            .expected_suggestions
            .iter()
            .map(|s| ExpectedSuggestion {
                conversation_id: s.conversation_id.clone(),
                turn: s.turn,
                kind: s.kind.clone(),
                content_contains: s.content_contains.clone(),
                related_entity: s.related_entity.clone(),
            })
            .collect();
        Self {
            expected_entities,
            expected_suggestions,
        }
    }

    /// Parse JSONL — one record per line, each a `GoldenLine`.
    /// Records are merged into a single GoldenSet (entity-level
    /// expectations are flattened across all conversations).
    pub(super) fn from_jsonl(body: &str) -> Result<Self, String> {
        let mut out = GoldenSet::default();
        for (lineno, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }
            let record: GoldenLine = serde_json::from_str(trimmed)
                .map_err(|e| format!("line {}: invalid JSONL record: {e}", lineno + 1))?;
            out.expected_entities.extend(record.expected_entities);
            for s in record.expected_suggestions {
                out.expected_suggestions.push(ExpectedSuggestion {
                    conversation_id: record.conversation_id.clone(),
                    turn: s.turn,
                    kind: s.kind,
                    content_contains: s.content_contains,
                    related_entity: s.related_entity,
                });
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenLine {
    #[serde(default)]
    conversation_id: String,
    #[serde(default)]
    expected_entities: Vec<ExpectedEntity>,
    #[serde(default)]
    expected_suggestions: Vec<GoldenSuggestionLine>,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenSuggestionLine {
    turn: u32,
    kind: String,
    #[serde(default)]
    content_contains: Option<String>,
    #[serde(default)]
    related_entity: Option<String>,
}

// ── Scoring primitives ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub(super) struct EntityScore {
    pub matched: usize,
    pub expected: usize,
    pub extracted: usize,
    pub false_positives: Vec<String>,
    pub false_negatives: Vec<String>,
}

impl EntityScore {
    pub(super) fn precision(&self) -> f64 {
        if self.extracted == 0 {
            0.0
        } else {
            self.matched as f64 / self.extracted as f64
        }
    }
    pub(super) fn recall(&self) -> f64 {
        if self.expected == 0 {
            1.0
        } else {
            self.matched as f64 / self.expected as f64
        }
    }
    pub(super) fn f1(&self) -> f64 {
        let p = self.precision();
        let r = self.recall();
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

/// Compare extracted entity names against expected. Folds names
/// case-insensitive + whole-trim. Names matched once each; an
/// extracted name not in `expected` becomes a false positive,
/// and an expected name not in `extracted` becomes a false negative.
pub(super) fn score_entities(
    expected: &[ExpectedEntity],
    extracted_names: &[String],
) -> EntityScore {
    let exp_set: std::collections::HashMap<String, &ExpectedEntity> =
        expected.iter().map(|e| (fold(&e.name), e)).collect();
    let ext_set: std::collections::HashSet<String> =
        extracted_names.iter().map(|n| fold(n)).collect();

    let mut matched = 0;
    let mut false_negatives = Vec::new();
    for (k, e) in &exp_set {
        if ext_set.contains(k) {
            matched += 1;
        } else {
            false_negatives.push(e.name.clone());
        }
    }
    let mut false_positives = Vec::new();
    for n in extracted_names {
        if !exp_set.contains_key(&fold(n)) {
            false_positives.push(n.clone());
        }
    }

    EntityScore {
        matched,
        expected: expected.len(),
        extracted: extracted_names.len(),
        false_positives,
        false_negatives,
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SuggestionScore {
    pub matched: usize,
    pub expected: usize,
    pub fired: usize,
    pub missed: Vec<MissedSuggestion>,
    pub false_fires: Vec<FalseFireSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MissedSuggestion {
    pub conversation_id: String,
    pub turn: u32,
    pub kind: String,
    pub content_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct FalseFireSuggestion {
    pub conversation_id: String,
    pub turn: u32,
    pub kind: String,
    pub content: String,
}

/// One actually-detected suggestion produced by the suggest
/// pipeline (or its mock).
#[derive(Debug, Clone)]
pub(super) struct DetectedSuggestion {
    pub conversation_id: String,
    pub turn: u32,
    pub kind: String,
    pub content: String,
    pub related_entity: Option<String>,
}

impl SuggestionScore {
    pub(super) fn precision(&self) -> f64 {
        if self.fired == 0 {
            0.0
        } else {
            self.matched as f64 / self.fired as f64
        }
    }
    pub(super) fn recall(&self) -> f64 {
        if self.expected == 0 {
            1.0
        } else {
            self.matched as f64 / self.expected as f64
        }
    }
    pub(super) fn f1(&self) -> f64 {
        let p = self.precision();
        let r = self.recall();
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

/// Score detected suggestions against expected. A detected
/// suggestion matches an expected when the conversation_id and kind
/// match, the turn is within ±1 (the heuristic mock may detect on
/// the user turn vs the expected turn shifted), and the
/// `content_contains` substring appears in the detected content.
pub(super) fn score_suggestions(
    expected: &[ExpectedSuggestion],
    detected: &[DetectedSuggestion],
) -> SuggestionScore {
    let mut matched = 0;
    let mut missed: Vec<MissedSuggestion> = Vec::new();
    let mut consumed: Vec<bool> = vec![false; detected.len()];

    for exp in expected {
        let mut hit = false;
        for (i, det) in detected.iter().enumerate() {
            if consumed[i] {
                continue;
            }
            if det.conversation_id != exp.conversation_id {
                continue;
            }
            if !kind_matches(&det.kind, &exp.kind) {
                continue;
            }
            if (det.turn as i64 - exp.turn as i64).abs() > 1 {
                continue;
            }
            if let Some(needle) = &exp.content_contains {
                if !det.content.to_lowercase().contains(&needle.to_lowercase()) {
                    continue;
                }
            }
            consumed[i] = true;
            hit = true;
            matched += 1;
            break;
        }
        if !hit {
            missed.push(MissedSuggestion {
                conversation_id: exp.conversation_id.clone(),
                turn: exp.turn,
                kind: exp.kind.clone(),
                content_contains: exp.content_contains.clone(),
            });
        }
    }

    let mut false_fires: Vec<FalseFireSuggestion> = Vec::new();
    for (i, det) in detected.iter().enumerate() {
        if !consumed[i] {
            false_fires.push(FalseFireSuggestion {
                conversation_id: det.conversation_id.clone(),
                turn: det.turn,
                kind: det.kind.clone(),
                content: det.content.clone(),
            });
        }
    }

    SuggestionScore {
        matched,
        expected: expected.len(),
        fired: detected.len(),
        missed,
        false_fires,
    }
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

fn kind_matches(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase().replace('-', "_");
    let b = b.trim().to_lowercase().replace('-', "_");
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(name: &str, kind: &str) -> ExpectedEntity {
        ExpectedEntity {
            name: name.into(),
            kind: kind.into(),
            affiliation: None,
            role: None,
            participants: Vec::new(),
        }
    }

    #[test]
    fn score_entities_counts_matches_and_diffs() {
        let expected = vec![
            ex("Sarah Chen", "person"),
            ex("Acme Corp", "organization"),
            ex("API migration", "initiative"),
        ];
        let extracted: Vec<String> = vec![
            "Sarah Chen".into(),
            "Mike Torres".into(),
            "API migration".into(),
        ];
        let s = score_entities(&expected, &extracted);
        assert_eq!(s.matched, 2); // Sarah Chen + API migration
        assert_eq!(s.expected, 3);
        assert_eq!(s.extracted, 3);
        assert!(s.false_positives.contains(&"Mike Torres".to_string()));
        assert!(s.false_negatives.contains(&"Acme Corp".to_string()));
    }

    #[test]
    fn score_entities_precision_recall_f1() {
        let expected = vec![ex("a", "person"), ex("b", "person")];
        let extracted: Vec<String> = vec!["a".into(), "c".into()];
        let s = score_entities(&expected, &extracted);
        assert!((s.precision() - 0.5).abs() < 1e-9);
        assert!((s.recall() - 0.5).abs() < 1e-9);
        assert!((s.f1() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn score_entities_handles_case_and_whitespace() {
        let expected = vec![ex("Sarah Chen", "person")];
        let extracted = vec!["  sarah chen  ".to_string()];
        let s = score_entities(&expected, &extracted);
        assert_eq!(s.matched, 1);
    }

    #[test]
    fn from_jsonl_parses_multiple_records() {
        let body = r#"
        {"conversation_id":"c1","expected_entities":[{"name":"Sarah","kind":"person"}],"expected_suggestions":[{"turn":3,"kind":"commitment"}]}
        {"conversation_id":"c2","expected_entities":[{"name":"Acme","kind":"organization"}]}
        "#;
        let g = GoldenSet::from_jsonl(body).unwrap();
        assert_eq!(g.expected_entities.len(), 2);
        assert_eq!(g.expected_suggestions.len(), 1);
        assert_eq!(g.expected_suggestions[0].conversation_id, "c1");
    }

    #[test]
    fn from_jsonl_skips_blank_and_comment_lines() {
        let body = "\n# header\n{\"conversation_id\":\"c1\"}\n";
        let g = GoldenSet::from_jsonl(body).unwrap();
        assert!(g.expected_entities.is_empty());
    }

    #[test]
    fn score_suggestions_matches_within_turn_window() {
        let expected = vec![ExpectedSuggestion {
            conversation_id: "c1".into(),
            turn: 3,
            kind: "commitment".into(),
            content_contains: Some("pricing".into()),
            related_entity: None,
        }];
        let detected = vec![DetectedSuggestion {
            conversation_id: "c1".into(),
            turn: 4, // off by 1, still within window
            kind: "commitment".into(),
            content: "send revised pricing to Sarah".into(),
            related_entity: None,
        }];
        let s = score_suggestions(&expected, &detected);
        assert_eq!(s.matched, 1);
        assert!(s.missed.is_empty());
        assert!(s.false_fires.is_empty());
    }

    #[test]
    fn score_suggestions_records_missed_and_false_fires() {
        let expected = vec![
            ExpectedSuggestion {
                conversation_id: "c1".into(),
                turn: 1,
                kind: "goal".into(),
                content_contains: Some("Q3".into()),
                related_entity: None,
            },
            ExpectedSuggestion {
                conversation_id: "c2".into(),
                turn: 1,
                kind: "follow_up".into(),
                content_contains: None,
                related_entity: None,
            },
        ];
        let detected = vec![
            DetectedSuggestion {
                conversation_id: "c2".into(),
                turn: 1,
                kind: "follow_up".into(),
                content: "circle back".into(),
                related_entity: None,
            },
            DetectedSuggestion {
                conversation_id: "c3".into(),
                turn: 1,
                kind: "commitment".into(),
                content: "ship Tuesday".into(),
                related_entity: None,
            },
        ];
        let s = score_suggestions(&expected, &detected);
        assert_eq!(s.matched, 1); // c2 follow_up
        assert_eq!(s.missed.len(), 1); // c1 goal
        assert_eq!(s.false_fires.len(), 1); // c3 commitment
    }

    #[test]
    fn kind_matches_normalises_dash_and_case() {
        assert!(kind_matches("follow-up", "follow_up"));
        assert!(kind_matches("FOLLOW_UP", "follow_up"));
        assert!(!kind_matches("commitment", "goal"));
    }
}
