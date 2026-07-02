// SPDX-License-Identifier: AGPL-3.0-or-later
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

use super::normalize::Normalizer;
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

/// One LLM-extracted entity mention before coalesce. Carries the
/// declared attribute values the model harvested for this mention.
/// `name` is a surface form (coalesce merges variants); `attributes`
/// keys come from the entity_type's `attributes: [...]` declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedEntity {
    pub name: String,
    /// References a `[[enrichment.entity_types]] name`.
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// The parsed result of one chunk's extraction call: typed entities
/// (with attributes) plus typed relationships. `entities` is
/// optional in the wire schema — a model that only emits
/// relationships still parses, and the relationship endpoints
/// backfill any entities not listed explicitly.
// `Serialize`/`Deserialize` so a parsed chunk can be persisted verbatim to
// the Phase-1 resume checkpoint (see `checkpoint.rs`) and rebuilt on a
// re-run without re-calling the LLM.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtractedChunk {
    pub entities: Vec<ExtractedEntity>,
    pub relationships: Vec<ExtractedRelationship>,
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
        "\n\nReturn JSON with the shape `{\"entities\": [...], \"relationships\": [...]}` \
         per the schema. For each distinct entity you reference, emit one `entities[]` \
         row using its SPECIFIC proper name (never the bare type word) and populating as \
         many declared attributes as the chunk supports. Empty arrays if none are present.",
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
Return JSON only — no prose, no Markdown fences. Emit an `entities` array \
(each entity's specific proper name + declared attributes) AND a `relationships` \
array. Use the entity's real name, NEVER the type word as the name. Empty arrays \
mean nothing of that kind was found in this chunk.\n";

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
            "entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "type": {
                            "type": "string",
                            "enum": entity_type_names,
                        },
                        "attributes": {
                            "type": "object",
                            "additionalProperties": true,
                        },
                    },
                    "required": ["name", "type"],
                    "additionalProperties": false,
                },
            },
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
/// - **Trailing characters after the JSON object** — the 35B
///   occasionally appends a duplicate object / stray prose after a
///   complete `{...}` (observed mid-run on noisy OCR input). We read
///   the FIRST complete JSON value via a streaming deserializer and
///   ignore whatever follows, rather than rejecting the whole response.
///
/// Errors with a descriptive message on schema mismatch so the
/// caller can log + skip the chunk.
pub fn parse_extract_response(response: &str) -> Result<ExtractedChunk> {
    let cleaned = strip_think(response);
    let cleaned = strip_code_fences(&cleaned);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Ok(ExtractedChunk::default());
    }

    // Read the first complete JSON value; trailing characters after it
    // (a duplicated object, stray prose) are tolerated and discarded.
    let mut stream = serde_json::Deserializer::from_str(cleaned).into_iter::<serde_json::Value>();
    let value: serde_json::Value = match stream.next() {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            return Err(Error::Serialization(format!(
                "investigation extract response is not valid JSON: {e} (got: {})",
                preview(cleaned, 200),
            )))
        }
        None => return Ok(ExtractedChunk::default()),
    };

    // `relationships` is required (the load-bearing output); `entities`
    // is optional so a model that only emits relationships still parses
    // and the relationship endpoints backfill the entity set.
    let rel_arr = value
        .get("relationships")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::Serialization(format!(
                "investigation extract response missing `relationships` array: {}",
                preview(cleaned, 200),
            ))
        })?;

    let mut relationships = Vec::with_capacity(rel_arr.len());
    for (i, item) in rel_arr.iter().enumerate() {
        let rel: ExtractedRelationship = serde_json::from_value(item.clone()).map_err(|e| {
            Error::Serialization(format!(
                "investigation extract response relationship {i} parse failed: {e}"
            ))
        })?;
        relationships.push(rel);
    }

    let mut entities = Vec::new();
    if let Some(ent_arr) = value.get("entities").and_then(|v| v.as_array()) {
        for (i, item) in ent_arr.iter().enumerate() {
            let ent: ExtractedEntity = serde_json::from_value(item.clone()).map_err(|e| {
                Error::Serialization(format!(
                    "investigation extract response entity {i} parse failed: {e}"
                ))
            })?;
            entities.push(ent);
        }
    }

    Ok(ExtractedChunk {
        entities,
        relationships,
    })
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

/// Coalesce extracted entity mentions + relationship endpoints into
/// canonical [`Entity`] records, keyed by a type-scoped, fold-normalized name
/// produced by the [`Normalizer`] (whose vocabulary comes from the recipe).
/// Surface-form variants of one entity (e.g. `"Wright-Patterson AFB"` /
/// `"Wright-Patterson Air Force Base"` / `"Wright-Patterson"`, when the recipe
/// declares the facility fold) collapse into a single record; the longest
/// surface form becomes `canonical_name`, the rest become `aliases`. Declared
/// attributes from `entities[]` rows are merged onto the record (first-non-null
/// wins). Relationship endpoints backfill any entity only mentioned in an edge.
///
/// Entity rows are processed before relationship endpoints so the attribute
/// merge sees the richest data first.
pub fn group_extracted_entities(
    normalizer: &Normalizer,
    entities: &[(String /* chunk_id */, ExtractedEntity)],
    relationships: &[(String /* chunk_id */, ExtractedRelationship)],
) -> BTreeMap<String, super::graph::Entity> {
    let mut by_key: BTreeMap<String, super::graph::Entity> = BTreeMap::new();

    for (_chunk_id, ent) in entities {
        upsert_entity(
            normalizer,
            &mut by_key,
            &ent.entity_type,
            &ent.name,
            Some(&ent.attributes),
        );
    }
    for (_chunk_id, rel) in relationships {
        upsert_entity(
            normalizer,
            &mut by_key,
            &rel.from_type,
            &rel.from_entity,
            None,
        );
        upsert_entity(normalizer, &mut by_key, &rel.to_type, &rel.to_entity, None);
    }
    by_key
}

/// Upsert one entity mention into the coalesce map. Promotes the
/// longest surface form to `canonical_name`, demotes the rest to
/// `aliases`, and merges any supplied attributes (first-non-null wins;
/// never overwrites a present value with a later one).
fn upsert_entity(
    normalizer: &Normalizer,
    by_key: &mut BTreeMap<String, super::graph::Entity>,
    entity_type: &str,
    name: &str,
    attributes: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    let key = normalizer.coalesce_key(entity_type, name);
    let entry = by_key.entry(key).or_insert_with(|| super::graph::Entity {
        id: normalizer.entity_id(entity_type, name),
        canonical_name: name.to_string(),
        entity_type: entity_type.to_string(),
        attributes: Default::default(),
        aliases: Vec::new(),
    });
    if name != entry.canonical_name {
        if name.len() > entry.canonical_name.len() {
            // Longer surface form is the better canonical; demote the old.
            let old = std::mem::replace(&mut entry.canonical_name, name.to_string());
            if !entry.aliases.contains(&old) {
                entry.aliases.push(old);
            }
        } else if !entry.aliases.iter().any(|a| a == name) {
            entry.aliases.push(name.to_string());
        }
    }
    if let Some(attrs) = attributes {
        for (k, v) in attrs {
            if v.is_null() {
                continue;
            }
            entry
                .attributes
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
    }
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
        assert_eq!(parsed.relationships.len(), 1);
        assert_eq!(parsed.relationships[0].from_entity, "NVIDIA");
        assert_eq!(parsed.relationships[0].relationship_type, "revenue");
        assert!((parsed.relationships[0].confidence - 0.92).abs() < 1e-6);
    }

    #[test]
    fn parses_response_with_think_block_and_code_fence() {
        let response =
            "<think>Let me find the relationships.</think>\n```json\n{\"relationships\": []}\n```";
        let parsed = parse_extract_response(response).unwrap();
        assert!(parsed.relationships.is_empty());
        assert!(parsed.entities.is_empty());
    }

    #[test]
    fn empty_string_yields_empty_list() {
        let parsed = parse_extract_response("").unwrap();
        assert!(parsed.relationships.is_empty());
        assert!(parsed.entities.is_empty());
    }

    #[test]
    fn parses_entities_array_with_attributes() {
        let response = r#"
        {
            "entities": [
                {"name": "Wright-Patterson AFB", "type": "installation",
                 "attributes": {"branch": "USAF", "type": "AIRBASE"}}
            ],
            "relationships": []
        }
        "#;
        let parsed = parse_extract_response(response).unwrap();
        assert_eq!(parsed.entities.len(), 1);
        assert_eq!(parsed.entities[0].name, "Wright-Patterson AFB");
        assert_eq!(parsed.entities[0].entity_type, "installation");
        assert_eq!(
            parsed.entities[0].attributes.get("branch"),
            Some(&serde_json::json!("USAF"))
        );
    }

    #[test]
    fn parses_response_without_entities_is_tolerant() {
        // Backward-compat: a model that only emits relationships parses.
        let response = r#"{"relationships": []}"#;
        let parsed = parse_extract_response(response).unwrap();
        assert!(parsed.entities.is_empty());
        assert!(parsed.relationships.is_empty());
    }

    #[test]
    fn parses_response_with_trailing_characters() {
        // Regression: the 35B occasionally appends a duplicate object /
        // stray text after a complete JSON object (observed mid-run on
        // noisy OCR). We read the FIRST value and ignore the trailing
        // junk rather than aborting the whole run.
        let response = "{\"relationships\": [{\"from_entity\":\"a\",\"to_entity\":\"b\",\
            \"from_type\":\"t\",\"to_type\":\"t\",\"type\":\"r\",\"verbatim_excerpt\":\"x\"}]}\n\n\
            {\"relationships\": []}\nstray trailing prose";
        let parsed = parse_extract_response(response).unwrap();
        assert_eq!(parsed.relationships.len(), 1);
        assert_eq!(parsed.relationships[0].from_entity, "a");
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

    fn rel(from: &str, fty: &str, to: &str, tty: &str, rtype: &str) -> ExtractedRelationship {
        ExtractedRelationship {
            from_entity: from.into(),
            to_entity: to.into(),
            from_type: fty.into(),
            to_type: tty.into(),
            relationship_type: rtype.into(),
            attributes: Default::default(),
            verbatim_excerpt: "x".into(),
            confidence: 1.0,
        }
    }

    #[test]
    fn group_extracted_entities_dedupes_by_type_and_name() {
        let rels = vec![
            (
                "c1".to_string(),
                rel("NVIDIA", "company", "Microsoft", "company", "revenue"),
            ),
            // different case, same entity
            (
                "c2".to_string(),
                rel("Nvidia", "company", "Google", "company", "revenue"),
            ),
        ];
        let grouped = group_extracted_entities(&Normalizer::default(), &[], &rels);
        // 3 unique entities: NVIDIA (canonical), Microsoft, Google
        assert_eq!(grouped.len(), 3);
        let nvidia = grouped
            .get("company|nvidia")
            .expect("NVIDIA grouped under normalized key");
        assert!(nvidia.aliases.contains(&"Nvidia".to_string()));
    }

    /// A Normalizer with a facility fold rule, so the coalesce test exercises
    /// the recipe-shaped folding. (The fold mechanism itself is unit-tested in
    /// `normalize.rs`; here we only check coalescing wiring.)
    fn facility_norm() -> Normalizer {
        use crate::recipe::{FoldRule, NormalizationConfig};
        Normalizer::new(NormalizationConfig {
            identity_attribute: Default::default(),
            fold: vec![FoldRule {
                types: vec!["installation".into()],
                aliases: vec![],
                leading_prefixes: vec![],
                trailing_qualifiers: vec![],
                trailing_suffixes: ["air", "force", "base", "afb"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }],
        })
    }

    #[test]
    fn coalesce_promotes_longest_canonical_and_merges_attrs() {
        let entities = vec![
            (
                "c1".to_string(),
                ExtractedEntity {
                    name: "Wright-Patterson".into(),
                    entity_type: "installation".into(),
                    attributes: serde_json::Map::from_iter([(
                        "branch".to_string(),
                        serde_json::json!("USAF"),
                    )]),
                },
            ),
            (
                "c2".to_string(),
                ExtractedEntity {
                    name: "Wright-Patterson Air Force Base".into(),
                    entity_type: "installation".into(),
                    attributes: serde_json::Map::from_iter([(
                        "type".to_string(),
                        serde_json::json!("AIRBASE"),
                    )]),
                },
            ),
        ];
        let grouped = group_extracted_entities(&facility_norm(), &entities, &[]);
        assert_eq!(grouped.len(), 1);
        let inst = grouped.get("installation|wright patterson").unwrap();
        // Longest surface form wins as canonical; the other is an alias.
        assert_eq!(inst.canonical_name, "Wright-Patterson Air Force Base");
        assert!(inst.aliases.contains(&"Wright-Patterson".to_string()));
        // Attributes from both mentions merge.
        assert_eq!(
            inst.attributes.get("branch"),
            Some(&serde_json::json!("USAF"))
        );
        assert_eq!(
            inst.attributes.get("type"),
            Some(&serde_json::json!("AIRBASE"))
        );
    }
}
