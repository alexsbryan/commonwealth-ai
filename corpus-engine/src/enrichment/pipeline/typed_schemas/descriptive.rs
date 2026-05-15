//! Descriptive discourse-mode Phase 1 — system prompt, schema, parser.
//!
//! Fires when the Phase 0 classifier's vector surfaces
//! `DiscourseMode::Descriptive` above `DISCOURSE_ROUTING_THRESHOLD`.
//! Atoms: definitions, property_claims, relationships, examples,
//! provenance.
//!
//! The descriptive extractor exists so a section that's primarily
//! structural (zettel card, institutional anatomy, glossary) doesn't
//! get squashed into the literary base schema's narrative-shaped
//! sketches. It's also a load-bearing secondary for argumentative
//! essays whose primary mode misses the institutional structure the
//! argument turns on (see Pharmacy Benefit recovery in the plan).

use crate::enrichment::pipeline::atlas::{
    DefinitionSketch, DescriptiveExtension, ExampleSketch, PropertyClaimSketch,
    ProvenanceSketch, RelationSketch, TypeExtension,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};

pub const PHASE1_DESCRIPTIVE_SYSTEM: &str =
    include_str!("descriptive_phase1_system.md");

pub fn phase1_descriptive_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "definitions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["term", "content"],
                    "properties": {
                        "term": { "type": "string", "minLength": 1 },
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "property_claims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["subject", "property", "value"],
                    "properties": {
                        "subject": { "type": "string", "minLength": 1 },
                        "property": { "type": "string", "minLength": 1 },
                        "value": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "relationships": {
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
            "examples": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label", "content"],
                    "properties": {
                        "label": { "type": "string", "minLength": 1 },
                        "content": { "type": "string", "minLength": 1 },
                        "illustrates": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "provenance": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label"],
                    "properties": {
                        "label": { "type": "string", "minLength": 1 },
                        "context": { "type": "string" },
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

pub fn parse_phase1_descriptive(response: &str) -> Result<DescriptiveExtension> {
    let stripped = strip_reasoning_tags(response);
    let cleaned: String = crate::enrichment::pipeline::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "descriptive typed-extension response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let definitions: Vec<DefinitionSketch> = v
        .get("definitions")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(DefinitionSketch {
                        term: required_str(e, "term")?,
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let property_claims: Vec<PropertyClaimSketch> = v
        .get("property_claims")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(PropertyClaimSketch {
                        subject: required_str(e, "subject")?,
                        property: required_str(e, "property")?,
                        value: required_str(e, "value")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let relationships: Vec<RelationSketch> = v
        .get("relationships")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let participants = e
                        .get("participants")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| {
                                    p.as_str().map(str::trim).filter(|s| !s.is_empty())
                                })
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if participants.is_empty() {
                        return None;
                    }
                    Some(RelationSketch {
                        participants,
                        label: required_str(e, "label")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let examples: Vec<ExampleSketch> = v
        .get("examples")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(ExampleSketch {
                        label: required_str(e, "label")?,
                        content: required_str(e, "content")?,
                        illustrates: str_field(e, "illustrates"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let provenance: Vec<ProvenanceSketch> = v
        .get("provenance")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(ProvenanceSketch {
                        label: required_str(e, "label")?,
                        context: str_field(e, "context"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DescriptiveExtension {
        definitions,
        property_claims,
        relationships,
        examples,
        provenance,
    })
}

pub fn parse_phase1_descriptive_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Descriptive(parse_phase1_descriptive(response)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_definition() {
        let json = r#"{"definitions":[{"term":"PBM","content":"A pharmacy benefit manager intermediates drug claims between insurers and pharmacies.","anchor":"intermediates drug claims"}]}"#;
        let e = parse_phase1_descriptive(json).expect("parses");
        assert_eq!(e.definitions.len(), 1);
        assert_eq!(e.definitions[0].term, "PBM");
    }

    #[test]
    fn empty_object_yields_empty_extension() {
        let e = parse_phase1_descriptive("{}").expect("parses");
        assert_eq!(e.atom_count(), 0);
    }

    #[test]
    fn drops_incomplete_property_claims() {
        let json = r#"{
            "property_claims": [
                {"subject":"A","property":"","value":"x"},
                {"subject":"B","property":"size","value":"large"}
            ]
        }"#;
        let e = parse_phase1_descriptive(json).expect("parses");
        assert_eq!(e.property_claims.len(), 1);
        assert_eq!(e.property_claims[0].subject, "B");
    }
}
