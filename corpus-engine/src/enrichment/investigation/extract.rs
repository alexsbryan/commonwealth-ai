//! Investigation extraction phase: ask the LLM for typed
//! relationships per chunk.
//!
//! The recipe author declares the entity / relationship schema in
//! TOML; this module turns that schema into:
//!
//! 1. A system prompt that lists the typed shapes the model is
//!    expected to extract.
//! 2. A JSON Schema for grammar-constrained generation, so the
//!    model can't drift the response shape (uses the same
//!    LLGuidance pattern that Phase 1 atlas extraction relies on
//!    — see memory `project_grammar_constrained_phase1`).
//! 3. A `parse_extract_response` helper that pulls the typed
//!    relationships out of the model's JSON.
//!
//! Pure, no I/O — the orchestrator (mod.rs) wires this to the
//! injected `ChatCompletionFn`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enrichment::pipeline::types::ChatPrompt;
use crate::error::{Error, Result};
use crate::recipe::{EntityTypeDecl, RelationshipTypeDecl};

/// One chunk handed to the extractor. Lightweight on purpose so
/// callers can shovel chunks from a `CorpusIndex` iteration into
/// the pipeline without cloning more than necessary.
#[derive(Debug, Clone)]
pub struct ChunkInput<'a> {
    pub chunk_id: &'a str,
    pub source_title: Option<&'a str>,
    pub content: &'a str,
}

/// One LLM-extracted relationship before coalesce. Names are
/// surface forms (not canonical-resolved yet) — the coalesce
/// phase merges them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedRelationship {
    pub from_entity: String,
    pub to_entity: String,
    pub from_type: String,
    pub to_type: String,
    /// References a `[[enrichment.relationship_types]] name`.
    #[serde(rename = "type")]
    pub relationship_type: String,
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Verbatim excerpt from the chunk that justifies the
    /// extraction. Required field — without evidence, an
    /// extracted relationship can't be cited.
    pub verbatim_excerpt: String,
    #[serde(default = "default_one")]
    pub confidence: f32,
}

fn default_one() -> f32 {
    1.0
}

/// Build the schema-driven extraction prompt for one chunk.
/// Embeds the entity_types and relationship_types in the system
/// preamble; the user message carries the chunk content. Pairs
/// with a JSON Schema that constrains the response to a
/// `{relationships: [...]}` shape.
pub fn compose_extract_prompt(
    chunk: &ChunkInput,
    entity_types: &[EntityTypeDecl],
    relationship_types: &[RelationshipTypeDecl],
) -> ChatPrompt {
    let mut system = String::new();
    system.push_str(EXTRACT_SYSTEM_PREAMBLE);
    system.push('\n');
    system.push_str("\n## Entity types you should canonicalize mentions to:\n");
    for et in entity_types {
        system.push_str(&format!(
            "- **{}**: {}\n  Attributes (keys to populate when present): [{}]\n",
            et.name,
            if et.description.is_empty() {
                "(no description)"
            } else {
                &et.description
            },
            et.attributes.join(", "),
        ));
    }
    system.push_str("\n## Relationship types to extract:\n");
    for rt in relationship_types {
        system.push_str(&format!(
            "- **{}**{}: {}\n  Attributes: [{}]\n",
            rt.name,
            if rt.directional {
                " (directional A→B)"
            } else {
                " (symmetric)"
            },
            if rt.description.is_empty() {
                "(no description)"
            } else {
                &rt.description
            },
            rt.attributes.join(", "),
        ));
    }
    system.push_str(EXTRACT_OUTPUT_INSTRUCTIONS);

    let mut user = String::new();
    if let Some(title) = chunk.source_title {
        user.push_str(&format!("Source: {title}\n\n"));
    }
    user.push_str("Chunk content:\n\n");
    user.push_str(chunk.content);
    user.push_str(
        "\n\nReturn JSON with the shape `{\"relationships\": [...]}` per the schema. \
         Empty array if no relationships are present in this chunk.",
    );

    let schema = response_schema(entity_types, relationship_types);
    ChatPrompt::new(system, user)
        .with_phase_id("investigation_extract")
        .with_response_schema("investigation_extract", schema)
}

const EXTRACT_SYSTEM_PREAMBLE: &str = "\
You are an expert investigative analyst extracting typed relationships \
from a single chunk of text. You only return structured JSON; you do \
not narrate.

Rules:
- Each relationship must reference TWO distinct entities by their \
  canonical names as they appear in the chunk.
- Only extract relationships clearly grounded in the chunk's text. \
  Do NOT infer relationships not directly supported by the excerpt.
- Populate as many declared attributes as the text supports; omit \
  the rest. Do NOT invent values.
- `verbatim_excerpt` MUST be a contiguous substring of the chunk \
  (or close to it — minor trimming is OK).
";

const EXTRACT_OUTPUT_INSTRUCTIONS: &str = "\n\
Return JSON only — no prose, no Markdown fences. \
Empty `relationships` array means no relationships were found in this chunk.\n";

/// JSON Schema for the response — used to drive
/// LLGuidance grammar-constrained generation.
fn response_schema(
    entity_types: &[EntityTypeDecl],
    relationship_types: &[RelationshipTypeDecl],
) -> serde_json::Value {
    let entity_type_names: Vec<&str> = entity_types.iter().map(|e| e.name.as_str()).collect();
    let rel_type_names: Vec<&str> = relationship_types.iter().map(|r| r.name.as_str()).collect();

    serde_json::json!({
        "type": "object",
        "properties": {
            "relationships": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "from_entity": { "type": "string", "minLength": 1 },
                        "to_entity": { "type": "string", "minLength": 1 },
                        "from_type": {
                            "type": "string",
                            "enum": entity_type_names,
                        },
                        "to_type": {
                            "type": "string",
                            "enum": entity_type_names,
                        },
                        "type": {
                            "type": "string",
                            "enum": rel_type_names,
                        },
                        "attributes": {
                            "type": "object",
                            "additionalProperties": true,
                        },
                        "verbatim_excerpt": { "type": "string", "minLength": 1 },
                        "confidence": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0,
                        },
                    },
                    "required": [
                        "from_entity",
                        "to_entity",
                        "from_type",
                        "to_type",
                        "type",
                        "verbatim_excerpt",
                    ],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["relationships"],
        "additionalProperties": false,
    })
}

/// Parse one extraction response. Tolerates a few common LLM
/// format drifts:
/// - `<think>...</think>` reasoning preambles (stripped)
/// - Code-fenced JSON blocks (`json ... ```)
/// - Trailing whitespace
///
/// Errors with a descriptive message on schema mismatch so the
/// caller can log + retry.
pub fn parse_extract_response(response: &str) -> Result<Vec<ExtractedRelationship>> {
    let cleaned = strip_think(response);
    let cleaned = strip_code_fences(&cleaned);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }

    let value: serde_json::Value = serde_json::from_str(cleaned).map_err(|e| {
        Error::Serialization(format!(
            "investigation extract response is not valid JSON: {e} (got: {})",
            preview(cleaned, 200),
        ))
    })?;

    let arr = value
        .get("relationships")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::Serialization(format!(
                "investigation extract response missing `relationships` array: {}",
                preview(cleaned, 200),
            ))
        })?;

    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let rel: ExtractedRelationship = serde_json::from_value(item.clone()).map_err(|e| {
            Error::Serialization(format!(
                "investigation extract response item {i} parse failed: {e}"
            ))
        })?;
        out.push(rel);
    }
    Ok(out)
}

fn strip_think(s: &str) -> String {
    // Most "<think>...</think>" blocks are at the start; the close
    // tag may be on its own line. Rather than parse XML, find the
    // last `</think>` and take everything after it.
    if let Some(idx) = s.rfind("</think>") {
        let after = &s[idx + "</think>".len()..];
        return after.to_string();
    }
    s.to_string()
}

fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json") {
        let body = stripped.trim_start_matches('\n');
        if let Some(end) = body.rfind("```") {
            return body[..end].to_string();
        }
    }
    if let Some(stripped) = trimmed.strip_prefix("```") {
        let body = stripped.trim_start_matches('\n');
        if let Some(end) = body.rfind("```") {
            return body[..end].to_string();
        }
    }
    trimmed.to_string()
}

fn preview(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Guess at canonical surface forms from extracted relationships.
/// Used by the coalesce phase to dedup mentions of the same
/// entity. Keys by `(entity_type, lowercased canonical name)`;
/// keeps every observed surface form as an alias.
pub fn group_extracted_entities(
    extractions: &[(String /* chunk_id */, ExtractedRelationship)],
) -> BTreeMap<(String, String), super::graph::Entity> {
    let mut by_key: BTreeMap<(String, String), super::graph::Entity> = BTreeMap::new();

    for (_chunk_id, rel) in extractions {
        for (name, ty) in [
            (&rel.from_entity, &rel.from_type),
            (&rel.to_entity, &rel.to_type),
        ] {
            let key = (ty.clone(), name.to_lowercase());
            let entry = by_key
                .entry(key.clone())
                .or_insert_with(|| super::graph::Entity {
                    id: entity_id_for(ty, name),
                    canonical_name: name.clone(),
                    entity_type: ty.clone(),
                    attributes: Default::default(),
                    aliases: Vec::new(),
                });
            if !entry.aliases.iter().any(|a| a == name) && entry.canonical_name != *name {
                entry.aliases.push(name.clone());
            }
        }
    }
    by_key
}

/// Stable entity id: `e-<type>-<slugged-name>`. Stays the same
/// across reruns of identical extractions so the graph is
/// reproducible.
pub fn entity_id_for(ty: &str, name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("e-{ty}-{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn et(name: &str, attrs: &[&str]) -> EntityTypeDecl {
        EntityTypeDecl {
            name: name.into(),
            description: format!("description of {name}"),
            attributes: attrs.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn rt(name: &str, attrs: &[&str]) -> RelationshipTypeDecl {
        RelationshipTypeDecl {
            name: name.into(),
            description: format!("description of {name}"),
            attributes: attrs.iter().map(|s| s.to_string()).collect(),
            directional: true,
        }
    }

    #[test]
    fn prompt_lists_declared_types_and_attributes() {
        let chunk = ChunkInput {
            chunk_id: "chunk-1",
            source_title: Some("NVDA 10-K"),
            content: "Some financial text.",
        };
        let entity_types = vec![
            et("company", &["name", "ticker", "cik"]),
            et("fund", &["name"]),
        ];
        let rel_types = vec![rt("revenue", &["amount_usd", "period"])];
        let prompt = compose_extract_prompt(&chunk, &entity_types, &rel_types);

        assert!(prompt.system.contains("**company**"));
        assert!(prompt.system.contains("**revenue**"));
        assert!(prompt.system.contains("ticker"));
        assert!(prompt.system.contains("amount_usd"));
        assert!(prompt.user.contains("NVDA 10-K"));
        assert!(prompt.user.contains("Some financial text."));
        assert_eq!(prompt.phase_id.as_deref(), Some("investigation_extract"));
        assert!(prompt.response_schema.is_some());
    }

    #[test]
    fn parses_clean_relationships_response() {
        let response = r#"
        {
            "relationships": [
                {
                    "from_entity": "NVIDIA",
                    "to_entity": "Microsoft",
                    "from_type": "company",
                    "to_type": "company",
                    "type": "revenue",
                    "attributes": { "amount_usd": 1000000000 },
                    "verbatim_excerpt": "Microsoft committed to a multi-year cloud GPU contract.",
                    "confidence": 0.92
                }
            ]
        }
        "#;
        let parsed = parse_extract_response(response).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].from_entity, "NVIDIA");
        assert_eq!(parsed[0].relationship_type, "revenue");
        assert!((parsed[0].confidence - 0.92).abs() < 1e-6);
    }

    #[test]
    fn parses_response_with_think_block_and_code_fence() {
        let response =
            "<think>Let me find the relationships.</think>\n```json\n{\"relationships\": []}\n```";
        let parsed = parse_extract_response(response).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn empty_string_yields_empty_list() {
        let parsed = parse_extract_response("").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_json_errors_with_preview() {
        let response = "{relationships: [bad}";
        let err = parse_extract_response(response).unwrap_err();
        assert!(format!("{err}").contains("not valid JSON"));
    }

    #[test]
    fn missing_relationships_field_errors() {
        let response = r#"{"items": []}"#;
        let err = parse_extract_response(response).unwrap_err();
        assert!(format!("{err}").contains("missing `relationships`"));
    }

    #[test]
    fn group_extracted_entities_dedupes_by_type_and_name() {
        let extractions = vec![
            (
                "chunk-1".to_string(),
                ExtractedRelationship {
                    from_entity: "NVIDIA".into(),
                    to_entity: "Microsoft".into(),
                    from_type: "company".into(),
                    to_type: "company".into(),
                    relationship_type: "revenue".into(),
                    attributes: Default::default(),
                    verbatim_excerpt: "x".into(),
                    confidence: 1.0,
                },
            ),
            (
                "chunk-2".to_string(),
                ExtractedRelationship {
                    from_entity: "Nvidia".into(), // different case, same entity
                    to_entity: "Google".into(),
                    from_type: "company".into(),
                    to_type: "company".into(),
                    relationship_type: "revenue".into(),
                    attributes: Default::default(),
                    verbatim_excerpt: "y".into(),
                    confidence: 1.0,
                },
            ),
        ];
        let grouped = group_extracted_entities(&extractions);
        // 3 unique entities: NVIDIA (canonical), Microsoft, Google
        assert_eq!(grouped.len(), 3);
        let nvidia = grouped
            .get(&("company".into(), "nvidia".into()))
            .expect("NVIDIA grouped under lowercased key");
        assert!(nvidia.aliases.contains(&"Nvidia".to_string()));
    }

    #[test]
    fn entity_id_for_is_stable() {
        let id1 = entity_id_for("company", "NVIDIA Corporation");
        let id2 = entity_id_for("company", "NVIDIA Corporation");
        assert_eq!(id1, id2);
        assert_eq!(id1, "e-company-nvidia-corporation");
    }
}
