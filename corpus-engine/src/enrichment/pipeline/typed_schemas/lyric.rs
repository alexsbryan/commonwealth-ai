//! Lyric discourse-mode Phase 1 — system prompt, schema, parser.
//!
//! Fires when the Phase 0 classifier surfaces `DiscourseMode::Lyric`
//! above the routing threshold. Atoms: images, motifs, formal_devices,
//! voice_shifts, tonal_movements.

use crate::enrichment::pipeline::atlas::{
    FormalDeviceSketch, ImageSketch, LyricExtension, MotifSketch, TonalMovementSketch,
    TypeExtension, VoiceShiftSketch,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};

pub const PHASE1_LYRIC_SYSTEM: &str = include_str!("lyric_phase1_system.md");

pub fn phase1_lyric_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "images": {
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
            "motifs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "description": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "formal_devices": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "example": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "voice_shifts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string", "minLength": 1 },
                        "to": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "tonal_movements": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string", "minLength": 1 },
                        "to": { "type": "string", "minLength": 1 },
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

pub fn parse_phase1_lyric(response: &str) -> Result<LyricExtension> {
    let stripped = strip_reasoning_tags(response);
    let cleaned: String = crate::enrichment::pipeline::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "lyric typed-extension response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let images: Vec<ImageSketch> = v
        .get("images")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(ImageSketch {
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let motifs: Vec<MotifSketch> = v
        .get("motifs")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(MotifSketch {
                        name: required_str(e, "name")?,
                        description: str_field(e, "description"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let formal_devices: Vec<FormalDeviceSketch> = v
        .get("formal_devices")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(FormalDeviceSketch {
                        name: required_str(e, "name")?,
                        example: str_field(e, "example"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let voice_shifts: Vec<VoiceShiftSketch> = v
        .get("voice_shifts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(VoiceShiftSketch {
                        from: required_str(e, "from")?,
                        to: required_str(e, "to")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let tonal_movements: Vec<TonalMovementSketch> = v
        .get("tonal_movements")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(TonalMovementSketch {
                        from: required_str(e, "from")?,
                        to: required_str(e, "to")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(LyricExtension {
        images,
        motifs,
        formal_devices,
        voice_shifts,
        tonal_movements,
    })
}

pub fn parse_phase1_lyric_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Lyric(parse_phase1_lyric(response)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_and_motif() {
        let json = r#"{
            "images":[{"content":"the bruised plum"}],
            "motifs":[{"name":"threshold","description":"Recurring image of doorways and entrances."}]
        }"#;
        let e = parse_phase1_lyric(json).expect("parses");
        assert_eq!(e.images.len(), 1);
        assert_eq!(e.motifs.len(), 1);
    }

    #[test]
    fn empty_object_yields_empty_extension() {
        let e = parse_phase1_lyric("{}").expect("parses");
        assert_eq!(e.atom_count(), 0);
    }
}
