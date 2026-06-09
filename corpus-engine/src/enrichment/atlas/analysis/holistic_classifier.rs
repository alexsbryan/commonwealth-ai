// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 6 *holistic* fault-line classifier.
//!
//! Sibling of `tension_classifier`. The per-pair classifier asks
//! "is THIS pair of atoms in tension?" — a question the model can't
//! answer well from a single claim slice, because a fault line
//! between two positions is a property of both positions' overall
//! commitments, not of any one claim pair. The holistic classifier
//! asks instead "given the corpus's positions and what they hold,
//! what fault lines do you see?" — aligning the question with the
//! unit being judged.
//!
//! Used by the philosophy pipeline (where fault lines are between
//! schools/doctrines). The literary pipeline keeps the per-pair
//! classifier (literary tensions are within-character — e.g. stated
//! resolve vs enacted vacillation — which the per-pair frame does
//! handle well).
//!
//! Output: a flat list of `HolisticTension` records the runner
//! materializes as `Tension` edges with entity-id endpoints. The
//! existing eval logic chases endpoint→entity-name, so entity-id
//! endpoints score directly.

use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomsFile};
use crate::enrichment::pipeline::atlas::EntityType;
use crate::error::{Error, Result};

/// One identified between-position fault line.
///
/// Names are surface forms the model emits — the runner resolves
/// them against the atlas's entity inventory to produce edge
/// endpoints. `crux` becomes the edge's `sub_question`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HolisticTension {
    pub position_a: String,
    pub position_b: String,
    pub crux: String,
}

/// Top-level shape returned by the model. Tolerant of:
/// - the key being either `fault_lines` or `tensions` (model variance)
/// - the field being absent (returns empty list)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HolisticResponse {
    #[serde(default, alias = "tensions")]
    pub fault_lines: Vec<HolisticTension>,
}

/// Parse the holistic-classifier response. Tolerant of:
/// - leading prose (chain-of-thought) before the JSON
/// - markdown code fences around the JSON
/// - `<think>`...`</think>` blocks the model emits before its answer
/// - either `fault_lines` or `tensions` key
///
/// Strategy: walk from the end of the response back to find the
/// last balanced `{...}` block, then parse it. A model that thinks
/// aloud and concludes with structured JSON gets parsed cleanly.
pub fn parse_holistic_response(raw: &str) -> Result<Vec<HolisticTension>> {
    let stripped = strip_think_block(raw);
    let trimmed = stripped.trim();
    let json_slice = extract_trailing_json(trimmed)
        .ok_or_else(|| Error::Serialization("phase 6 holistic: no JSON object found".into()))?;
    let parsed: HolisticResponse = serde_json::from_str(json_slice).map_err(|e| {
        Error::Serialization(format!(
            "phase 6 holistic: parse failed: {e} | json[head]: {}",
            json_slice.chars().take(160).collect::<String>()
        ))
    })?;
    Ok(parsed.fault_lines)
}

/// Render the corpus inventory as the user message body the
/// holistic classifier sees. Three sections:
///
/// 1. Schools / doctrines — concept-typed entities (canonical name +
///    description). Listed first because the prompt tells the model
///    to prefer school labels over proponent names.
/// 2. Proponents — person-typed entities. Used only when no school
///    label fits.
/// 3. Claims grouped by `attributed_to`. Concept attributions render
///    first, then person, then unattributed; within each kind the
///    bucket with more claims comes first (a proxy for which
///    positions the corpus develops most).
///
/// Stable ordering: same atoms in → same body out, suitable for
/// caching and replay.
pub fn render_holistic_user_body(atoms: &AtomsFile) -> String {
    use std::collections::BTreeMap;

    let mut concepts: Vec<&crate::enrichment::atlas::atoms::Entity> = Vec::new();
    let mut persons: Vec<&crate::enrichment::atlas::atoms::Entity> = Vec::new();
    let mut claims: Vec<&crate::enrichment::atlas::atoms::Claim> = Vec::new();
    for env in &atoms.atoms {
        match env {
            AtomEnvelope::Entity(e) => match e.entity_type {
                EntityType::Concept => concepts.push(e),
                EntityType::Person => persons.push(e),
                _ => {}
            },
            AtomEnvelope::Claim(c) => claims.push(c),
            _ => {}
        }
    }

    let mut by_attr: BTreeMap<Option<String>, Vec<&crate::enrichment::atlas::atoms::Claim>> =
        BTreeMap::new();
    for c in &claims {
        let key = c.attributed_to.as_ref().map(|id| id.as_str().to_string());
        by_attr.entry(key).or_default().push(c);
    }

    let mut lines = Vec::new();
    lines.push("# Lexicon — schools / doctrines (use these names verbatim)".to_string());
    if concepts.is_empty() {
        lines.push("(none)".to_string());
    }
    for c in &concepts {
        lines.push(format!("- {}: {}", c.canonical_name, c.description.trim()));
    }
    lines.push(String::new());
    lines.push("# Lexicon — proponents (use only if no school label fits)".to_string());
    if persons.is_empty() {
        lines.push("(none)".to_string());
    }
    for p in &persons {
        lines.push(format!("- {}: {}", p.canonical_name, p.description.trim()));
    }
    lines.push(String::new());
    lines.push("# Claims grouped by who they're attributed to".to_string());

    // Stable kind-then-claim-count ordering. We need a name lookup
    // for each attribution id.
    let entity_lookup: BTreeMap<&str, &crate::enrichment::atlas::atoms::Entity> = concepts
        .iter()
        .chain(persons.iter())
        .map(|e| (e.id.as_str(), *e))
        .collect();

    fn kind_rank(
        attr: &Option<String>,
        lookup: &BTreeMap<&str, &crate::enrichment::atlas::atoms::Entity>,
    ) -> u8 {
        match attr {
            None => 2,
            Some(id) => match lookup.get(id.as_str()).map(|e| &e.entity_type) {
                Some(EntityType::Concept) => 0,
                Some(EntityType::Person) => 1,
                _ => 3,
            },
        }
    }

    let mut keys: Vec<&Option<String>> = by_attr.keys().collect();
    keys.sort_by(|a, b| {
        let rank_a = kind_rank(a, &entity_lookup);
        let rank_b = kind_rank(b, &entity_lookup);
        rank_a
            .cmp(&rank_b)
            .then_with(|| {
                // Within a kind: larger bucket first.
                by_attr
                    .get(*b)
                    .map(Vec::len)
                    .unwrap_or(0)
                    .cmp(&by_attr.get(*a).map(Vec::len).unwrap_or(0))
            })
            .then_with(|| a.cmp(b)) // tie-breaker: lex on id for stability
    });

    for k in keys {
        let bucket = match by_attr.get(k) {
            Some(b) => b,
            None => continue,
        };
        let header = match k {
            None => "(unattributed claims)".to_string(),
            Some(id) => entity_lookup
                .get(id.as_str())
                .map(|e| e.canonical_name.clone())
                .unwrap_or_else(|| id.clone()),
        };
        lines.push(String::new());
        lines.push(format!("## {header}"));
        for c in bucket {
            lines.push(format!("- {}", c.content));
        }
    }

    lines.join("\n")
}

fn strip_think_block(raw: &str) -> String {
    // Some BYOM chat models emit `<think>...</think>` before the
    // answer. The classifier shouldn't choke on it.
    if let Some(end_idx) = raw.find("</think>") {
        return raw[end_idx + "</think>".len()..].to_string();
    }
    raw.to_string()
}

/// Find the last balanced `{...}` block by scanning from the end.
/// Returns the slice including both braces, or None if no balanced
/// block exists.
fn extract_trailing_json(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b'}' {
            end = Some(i);
            break;
        }
    }
    let end = end?;
    let mut depth: i32 = 0;
    let mut start: Option<usize> = None;
    for i in (0..=end).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    start = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let start = start?;
    Some(&text[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"fault_lines":[{"position_a":"A","position_b":"B","crux":"C?"}]}"#;
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].position_a, "A");
        assert_eq!(r[0].position_b, "B");
        assert_eq!(r[0].crux, "C?");
    }

    #[test]
    fn parses_chain_of_thought_preamble() {
        let raw = "Looking at the corpus I notice that A and B disagree on X.\n\nAlso C aligns with D so they are not a fault line.\n\n{\"fault_lines\":[{\"position_a\":\"A\",\"position_b\":\"B\",\"crux\":\"X?\"}]}";
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].position_a, "A");
    }

    #[test]
    fn parses_tensions_alias_key() {
        // Model variance: sometimes emits `tensions` instead of `fault_lines`.
        let raw = r#"{"tensions":[{"position_a":"X","position_b":"Y","crux":"Z?"}]}"#;
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].position_a, "X");
    }

    #[test]
    fn parses_empty_list() {
        let r = parse_holistic_response(r#"{"fault_lines":[]}"#).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn parses_through_code_fence() {
        let raw = "Here are the tensions:\n```json\n{\"fault_lines\":[{\"position_a\":\"A\",\"position_b\":\"B\",\"crux\":\"?\"}]}\n```";
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn parses_through_think_block() {
        let raw = "<think>let me consider</think>\n{\"fault_lines\":[{\"position_a\":\"A\",\"position_b\":\"B\",\"crux\":\"?\"}]}";
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn errors_when_no_json() {
        assert!(parse_holistic_response("just prose, no JSON").is_err());
    }

    #[test]
    fn picks_last_json_when_multiple() {
        // Model included a JSON example in its preamble but the
        // canonical answer is the trailing one.
        let raw = "Example shape: {\"fault_lines\":[]}\n\nMy answer:\n{\"fault_lines\":[{\"position_a\":\"A\",\"position_b\":\"B\",\"crux\":\"?\"}]}";
        let r = parse_holistic_response(raw).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].position_a, "A");
    }
}
