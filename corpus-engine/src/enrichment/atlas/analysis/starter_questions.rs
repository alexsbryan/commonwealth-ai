// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starter questions mined from a corpus's atlas — the chat empty state and
//! the onboarding celebration screen.
//!
//! Lived in `sovereign-desktop`'s `enrich_commands.rs` until 2026-09-11
//! (thin-desktop order) as a fold the desktop ran over every atom it pulled
//! from `GET /internal/corpus/{corpus}/atoms`. It is a projection over
//! Question atoms, so it belongs with the other atom folds here, and the
//! daemon serves it at `GET /internal/corpus/{corpus}/starter-questions`
//! (`sovereign_mesh::enrich_http`). The answer type is the wire's
//! (`daemon_wire::StarterQuestion`) so a thin client parses it without
//! naming this crate.
//!
//! Heuristic (shipped question atoms lack a salience or `addressed_by`
//! field — verified against three live corpora). Ranking:
//!
//!   1. Length window 25..=220 chars (drops too-terse fragments and
//!      run-on multi-clause questions).
//!   2. Question-type preference, in order: Thematic, Interpretive,
//!      Open, Factual, Rhetorical, Other.
//!   3. Diversify by first `raised_at.chunk_id`: at most one question
//!      per section in the returned set, as far as `limit` and corpus
//!      size permit.

use std::collections::HashSet;

use sovereign_contracts::daemon_wire::StarterQuestion;

use crate::enrichment::atlas::AtomEnvelope;

/// Core ranker. Separated from the Tauri command so unit tests can
/// feed it synthetic atom slices without touching the filesystem.
pub fn rank_starter_questions(atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion> {
    if limit == 0 {
        return Vec::new();
    }
    // Tier score — lower is better.
    fn tier(q_type: &str) -> u8 {
        match q_type {
            "thematic" => 0,
            "interpretive" => 1,
            "open" => 2,
            "factual" => 3,
            "rhetorical" => 4,
            _ => 5,
        }
    }
    // Collect candidates that pass the length + shape filters.
    let mut candidates: Vec<StarterQuestion> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Question(q) => {
                let text = q.content.trim();
                let char_count = text.chars().count();
                if !(25..=220).contains(&char_count) {
                    return None;
                }
                // Normalise trailing punctuation to a question mark.
                let cleaned = if text.ends_with('?') {
                    text.to_string()
                } else {
                    let stripped = text.trim_end_matches(['.', '!', ',', ';', ':']);
                    format!("{stripped}?")
                };
                let source_section = q
                    .raised_at
                    .first()
                    .map(|r| r.chunk_id.clone())
                    .filter(|s| !s.is_empty());
                Some(StarterQuestion {
                    text: cleaned,
                    atom_id: q.id.as_str().to_string(),
                    source_section,
                    question_type: q.question_type.as_str_repr().to_string(),
                })
            }
            _ => None,
        })
        .collect();
    // Stable sort by (tier, then atom_id) so ties resolve deterministically.
    candidates.sort_by(|a, b| {
        tier(&a.question_type)
            .cmp(&tier(&b.question_type))
            .then_with(|| a.atom_id.cmp(&b.atom_id))
    });
    // Round-robin diversify by source_section. First pass: pick one
    // per section in tier order. Second pass: fill remaining slots
    // from the leftover pool.
    let mut picked: Vec<StarterQuestion> = Vec::with_capacity(limit);
    let mut used_sections: HashSet<String> = HashSet::new();
    let mut leftovers: Vec<StarterQuestion> = Vec::new();
    for q in candidates {
        if picked.len() >= limit {
            leftovers.push(q);
            continue;
        }
        match &q.source_section {
            Some(section) if !used_sections.contains(section) => {
                used_sections.insert(section.clone());
                picked.push(q);
            }
            _ => leftovers.push(q),
        }
    }
    for q in leftovers {
        if picked.len() >= limit {
            break;
        }
        picked.push(q);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::{AtomId, ChunkRef, Question, ResolutionStatus};
    use crate::enrichment::pipeline::{EnrichmentDepth, QuestionType};

    #[test]
    fn starter_question_ranker_prefers_thematic_then_interpretive() {
        let mk = |id: usize, text: &str, qtype: QuestionType, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: qtype,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(
                1,
                "What is the factual date of the encounter between the brothers?",
                QuestionType::Factual,
                "sec_0001",
            ),
            mk(
                2,
                "How does faith change when grief meets doubt across chapters?",
                QuestionType::Thematic,
                "sec_0002",
            ),
            mk(
                3,
                "Does the ending dissolve or resolve the central question posed here?",
                QuestionType::Interpretive,
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        assert_eq!(picks.len(), 3, "all three should pass length gate");
        assert_eq!(picks[0].question_type, "thematic", "thematic wins tier 0");
        assert_eq!(
            picks[1].question_type, "interpretive",
            "interpretive wins tier 1"
        );
        assert_eq!(picks[2].question_type, "factual", "factual in tier 3");
    }

    #[test]
    fn starter_question_ranker_diversifies_by_section() {
        let mk = |id: usize, text: &str, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        // Three questions from the same section and two from different
        // sections. Limit=3 should pull at most one from sec_0001
        // before falling back to leftovers.
        let atoms = vec![
            mk(
                1,
                "A first long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                2,
                "A second long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                3,
                "A third long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                4,
                "A long enough thematic question from section two probing meaning?",
                "sec_0002",
            ),
            mk(
                5,
                "A long enough thematic question from section three probing nuance?",
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        let sections: Vec<Option<String>> =
            picks.iter().map(|p| p.source_section.clone()).collect();
        let distinct_sections: HashSet<_> = picks
            .iter()
            .filter_map(|p| p.source_section.clone())
            .collect();
        assert_eq!(picks.len(), 3);
        assert_eq!(
            distinct_sections.len(),
            3,
            "should cover three distinct sections before revisiting one; got {:?}",
            sections
        );
    }

    #[test]
    fn starter_question_ranker_rejects_too_short_and_too_long() {
        let mk = |id: usize, text: String| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text,
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new("sec_0001".to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(1, "Why?".into()),   // too short
            mk(2, "a".repeat(300)), // too long
            mk(
                3,
                "What actually grounds a claim like this in the shipped corpus?".into(),
            ),
        ];
        let picks = rank_starter_questions(&atoms, 5);
        assert_eq!(picks.len(), 1, "only the middle-length question survives");
        assert!(picks[0].text.ends_with('?'));
    }

    #[test]
    fn starter_question_ranker_limit_zero_returns_empty() {
        let picks = rank_starter_questions(&[], 0);
        assert!(picks.is_empty());
    }
}
