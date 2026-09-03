// SPDX-License-Identifier: AGPL-3.0-or-later
//! Narrative discourse-mode Phase 1 — system prompt, schema, parser.
//!
//! Fires when the Phase 0 classifier's vector surfaces
//! `DiscourseMode::Narrative` above `DISCOURSE_ROUTING_THRESHOLD`.
//! The atoms produced — events, entity_states, relations,
//! relation_states, participant_arcs — overlap with the literary
//! atlas's base schema; routing to a narrative-specific prompt lets
//! a hybrid section (argument-with-vignette) still surface its
//! event-arc without the argumentative extractor swallowing the
//! whole prompt budget.

use crate::enrichment::pipeline::atlas::{
    EntityStateSketch, EventSketch, NarrativeExtension, ParticipantArcSketch, RelationSketch,
    RelationStateSketch, TypeExtension,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};

pub const PHASE1_NARRATIVE_SYSTEM: &str = include_str!("narrative_phase1_system.md");

pub fn phase1_narrative_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["description"],
                    "properties": {
                        "description": { "type": "string", "minLength": 1 },
                        "participants": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "entity_states": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["entity_name", "label"],
                    "properties": {
                        "entity_name": { "type": "string", "minLength": 1 },
                        "label": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "relations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["participants", "label"],
                    "properties": {
                        "participants": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string" }
                        },
                        "label": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "relation_states": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["participants", "label"],
                    "properties": {
                        "participants": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string" }
                        },
                        "label": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "participant_arcs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["participant", "label"],
                    "properties": {
                        "participant": { "type": "string", "minLength": 1 },
                        "label": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Lenient parser for the narrative Phase 1 response. Drops atoms
/// missing required non-empty fields; reasoning tags pre-JSON are
/// stripped.
pub fn parse_phase1_narrative(response: &str) -> Result<NarrativeExtension> {
    let stripped = strip_reasoning_tags(response);
    let cleaned: String = crate::enrichment::pipeline::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "narrative typed-extension response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let events: Vec<EventSketch> = v
        .get("events")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let description = e
                        .get("description")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let participants = e
                        .get("participants")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| p.as_str().map(str::trim).filter(|s| !s.is_empty()))
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let anchor = e
                        .get("anchor")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(EventSketch {
                        attributes: Default::default(),
                        event_type: None,
                        description,
                        participants,
                        anchor,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let entity_states: Vec<EntityStateSketch> = v
        .get("entity_states")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let entity_name = e
                        .get("entity_name")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let label = e
                        .get("label")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let anchor = e
                        .get("anchor")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(EntityStateSketch {
                        entity_name,
                        label,
                        anchor,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    fn parse_relation_like(arr: &[serde_json::Value]) -> Vec<(Vec<String>, String, String)> {
        arr.iter()
            .filter_map(|e| {
                let participants = e
                    .get("participants")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::trim).filter(|s| !s.is_empty()))
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if participants.is_empty() {
                    return None;
                }
                let label = e
                    .get("label")
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())?
                    .to_string();
                let anchor = e
                    .get("anchor")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                Some((participants, label, anchor))
            })
            .collect()
    }

    let relations: Vec<RelationSketch> = v
        .get("relations")
        .and_then(|x| x.as_array())
        .map(|a| parse_relation_like(a))
        .unwrap_or_default()
        .into_iter()
        .map(|(participants, label, anchor)| RelationSketch {
            attributes: Default::default(),
            relation_type: None,
            participants,
            label,
            anchor,
        })
        .collect();

    let relation_states: Vec<RelationStateSketch> = v
        .get("relation_states")
        .and_then(|x| x.as_array())
        .map(|a| parse_relation_like(a))
        .unwrap_or_default()
        .into_iter()
        .map(|(participants, label, anchor)| RelationStateSketch {
            participants,
            label,
            anchor,
        })
        .collect();

    let participant_arcs: Vec<ParticipantArcSketch> = v
        .get("participant_arcs")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let participant = e
                        .get("participant")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let label = e
                        .get("label")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let anchor = e
                        .get("anchor")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(ParticipantArcSketch {
                        participant,
                        label,
                        anchor,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(NarrativeExtension {
        events,
        entity_states,
        relations,
        relation_states,
        participant_arcs,
    })
}

/// Wrap the parsed extension as a `TypeExtension::Narrative` variant
/// for direct attachment to `SectionExtraction.type_extensions`.
pub fn parse_phase1_narrative_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Narrative(parse_phase1_narrative(response)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_event() {
        let json = r#"{
            "events": [
                {"description": "Wheelers arrive at the homestead in late November.", "participants": ["Wheeler family"], "anchor": "late November arrival"}
            ]
        }"#;
        let e = parse_phase1_narrative(json).expect("parses");
        assert_eq!(e.events.len(), 1);
        assert_eq!(
            e.events[0].description,
            "Wheelers arrive at the homestead in late November."
        );
    }

    #[test]
    fn drops_empty_required_fields() {
        let json = r#"{
            "events": [
                {"description": "", "participants": []},
                {"description": "Real event.", "participants": []}
            ],
            "entity_states": [
                {"entity_name": "Wheeler", "label": ""}
            ]
        }"#;
        let e = parse_phase1_narrative(json).expect("parses");
        assert_eq!(e.events.len(), 1);
        assert!(e.entity_states.is_empty());
    }

    #[test]
    fn strips_reasoning_tags() {
        let json =
            "<think>scanning</think>{\"events\":[{\"description\":\"Hawthorn split open.\"}]}";
        let e = parse_phase1_narrative(json).expect("parses");
        assert_eq!(e.events.len(), 1);
    }

    #[test]
    fn empty_object_yields_empty_extension() {
        let e = parse_phase1_narrative("{}").expect("parses");
        assert_eq!(e.atom_count(), 0);
    }
}
