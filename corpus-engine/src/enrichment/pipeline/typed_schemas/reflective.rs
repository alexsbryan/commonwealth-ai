//! Reflective discourse-mode Phase 1 — system prompt, schema, parser.
//!
//! Fires when the Phase 0 classifier surfaces `DiscourseMode::Reflective`
//! above the routing threshold. Journal entries, day-end notes,
//! post-mortems. Atoms: interactions, observations, open_threads,
//! mood_shifts, realisations.

use crate::enrichment::pipeline::atlas::{
    InteractionSketch, MoodShiftSketch, ObservationSketch, OpenThreadSketch, RealisationSketch,
    ReflectiveExtension, TypeExtension,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};

pub const PHASE1_REFLECTIVE_SYSTEM: &str = include_str!("reflective_phase1_system.md");

pub fn phase1_reflective_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "interactions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "with": { "type": "string" },
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "observations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "open_threads": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "mood_shifts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string", "minLength": 1 },
                        "to": { "type": "string", "minLength": 1 },
                        "catalyst": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "realisations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn required_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn parse_phase1_reflective(response: &str) -> Result<ReflectiveExtension> {
    let stripped = strip_reasoning_tags(response);
    let cleaned: String = crate::enrichment::pipeline::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "reflective typed-extension response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let interactions: Vec<InteractionSketch> = v
        .get("interactions")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(InteractionSketch {
                        with: str_field(e, "with"),
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let observations: Vec<ObservationSketch> = v
        .get("observations")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(ObservationSketch {
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let open_threads: Vec<OpenThreadSketch> = v
        .get("open_threads")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(OpenThreadSketch {
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let mood_shifts: Vec<MoodShiftSketch> = v
        .get("mood_shifts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(MoodShiftSketch {
                        from: required_str(e, "from")?,
                        to: required_str(e, "to")?,
                        catalyst: str_field(e, "catalyst"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let realisations: Vec<RealisationSketch> = v
        .get("realisations")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(RealisationSketch {
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ReflectiveExtension {
        interactions,
        observations,
        open_threads,
        mood_shifts,
        realisations,
    })
}

pub fn parse_phase1_reflective_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Reflective(parse_phase1_reflective(
        response,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_thread_and_observation() {
        let json = r#"{
            "observations":[{"content":"Tempo is off this week."}],
            "open_threads":[{"content":"Find out why X breaks under load."}]
        }"#;
        let e = parse_phase1_reflective(json).expect("parses");
        assert_eq!(e.observations.len(), 1);
        assert_eq!(e.open_threads.len(), 1);
    }

    #[test]
    fn empty_object_yields_empty_extension() {
        let e = parse_phase1_reflective("{}").expect("parses");
        assert_eq!(e.atom_count(), 0);
    }

    #[test]
    fn mood_shift_requires_both_endpoints() {
        let json =
            r#"{"mood_shifts":[{"from":"anxious","to":""},{"from":"unsure","to":"settled"}]}"#;
        let e = parse_phase1_reflective(json).expect("parses");
        assert_eq!(e.mood_shifts.len(), 1);
        assert_eq!(e.mood_shifts[0].to, "settled");
    }
}
