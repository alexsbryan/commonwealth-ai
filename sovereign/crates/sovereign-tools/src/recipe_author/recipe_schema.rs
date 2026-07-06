// SPDX-License-Identifier: AGPL-3.0-or-later
//! JSON Schema for the recipe TOML data model.
//!
//! Returned by `RecipeWriteStructuredTool::descriptor()` as the
//! `parameters.recipe` schema, so the daemon's LLGuidance sampler can
//! grammar-constrain the model's emitted tool-call arguments to valid recipes.
//! Two layers of correctness:
//!
//! 1. **Generation-time** — the daemon constrains the model to emit a JSON
//!    object matching this schema. Discriminator enums (`acquire.type`,
//!    `extract.type`, `chunk.type`, …) are grammar-validated at sampling time.
//! 2. **Validation-time** — the tool re-validates against this schema before
//!    converting to TOML and writing to disk.
//!
//! ## The variant catalog is GENERATED — it cannot drift
//!
//! The discriminator strings and required fields for `acquire` / `extract` /
//! `chunk` / `filter` / `enrichment.patterns` come from
//! [`sovereign_contracts::recipe::schema::RECIPE_SCHEMA_DESCRIPTOR_JSON`], a typed
//! const over the checked-in artifact
//! `sovereign-recipes/schema/recipe_schema_descriptor.json`, which
//! `corpus-engine/tests/recipe_schema.rs` regenerates (drift-gated) from
//! `corpus-engine/src/recipe.rs`'s actual `AcquirerConfig` / `ExtractorConfig`
//! / `ChunkerConfig` / `FilterConfig` / `PatternDecl` / `Comparison` types. Add
//! a new extractor to `recipe.rs`, run the SCHEMA regen, and it appears here
//! automatically — no hand-sync (this file used to hardcode 4 of 22
//! extractors). corpus-engine owns the catalog (it owns the types); this file
//! owns the schema shape + hand-authored overlays.
//!
//! The descriptor const lives in `sovereign_contracts::recipe::schema` (the
//! contract crate both corpus-engine and this authoring stack depend on), so
//! this file references a typed const, not a path, and needs no `corpus-engine`
//! dependency — which is what lets the recipe-author bundle move to its own
//! crate (plan B:P6).
//!
//! What stays hand-authored here is the *shape* (the grammar-friendly
//! tagged-union JSON Schema) plus **rich overlays** for variants worth extra
//! guidance — e.g. the `http_api` pagination/follow sub-schemas. Per-variant
//! fields are otherwise `additionalProperties: true`, so knobs flow through and
//! the on-disk `RecipeValidate` catches per-variant errors after writing.

use serde_json::{json, Map, Value};
use std::sync::LazyLock;

/// Recipe variant catalog generated from `recipe.rs` by the corpus-engine
/// `recipe_schema` test, embedded as a typed const in `sovereign-contracts`. No
/// build script, no cross-crate source-tree reach-in, and no repo-relative path
/// in this crate — the descriptor travels with the contract dependency.
static DESCRIPTOR: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(sovereign_contracts::recipe::schema::RECIPE_SCHEMA_DESCRIPTOR_JSON)
        .expect("checked-in recipe_schema_descriptor.json must parse")
});

/// `[{key, required}]` for a tagged enum (acquire / extract).
fn desc_variants(section: &str) -> Vec<(String, Vec<String>)> {
    DESCRIPTOR[section]
        .as_array()
        .unwrap_or_else(|| panic!("descriptor.{section} must be an array"))
        .iter()
        .map(|v| {
            let key = v["key"].as_str().unwrap_or_default().to_string();
            let required = v["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (key, required)
        })
        .collect()
}

/// Wire-key list for a tagged enum (chunk / filter / pattern / comparison).
fn desc_keys(section: &str) -> Vec<Value> {
    DESCRIPTOR[section]
        .as_array()
        .unwrap_or_else(|| panic!("descriptor.{section} must be an array"))
        .iter()
        .map(|v| json!(v.as_str().unwrap_or_default()))
        .collect()
}

/// One tagged-union arm: `type` const + generated required fields + any
/// hand-authored overlay properties merged on top.
fn variant_arm(key: &str, required: &[String], overlay: Value) -> Value {
    let mut req: Vec<Value> = vec![json!("type")];
    req.extend(required.iter().map(|r| json!(r)));
    let mut props = Map::new();
    props.insert("type".into(), json!({ "const": key }));
    if let Value::Object(extra) = overlay {
        for (k, v) in extra {
            props.insert(k, v);
        }
    }
    json!({
        "type": "object",
        "required": req,
        "additionalProperties": true,
        "properties": props,
    })
}

/// Top-level JSON Schema for a recipe.
pub fn recipe_json_schema() -> Value {
    json!({
        "type": "object",
        "title": "Recipe",
        "description":
            "Sovereign corpus recipe. Mirrors the on-disk \
             `~/.sovereign/recipes/<id>/recipe.toml` shape but as a JSON object \
             the tool mechanically serializes to TOML. The authoritative field \
             reference is `sovereign-recipes/SCHEMA.md` (generated from \
             `corpus-engine/src/recipe.rs`); copy `sovereign-recipes/_templates/\
             annotated/recipe.toml` as a starting point.",
        "required": ["corpus", "acquire", "extract", "chunk"],
        "additionalProperties": false,
        "properties": {
            "corpus":      corpus_schema(),
            "parameters":  parameters_schema(),
            "acquire":     acquire_schema(),
            "extract":     extract_schema(),
            "chunk":       chunk_schema(),
            "filter":      filter_schema(),
            "filter_mode": filter_mode_schema(),
            "index":       index_schema(),
            "enrichment":  enrichment_schema(),
        }
    })
}

fn corpus_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "name"],
        "additionalProperties": true,
        "properties": {
            "id": {
                "type": "string",
                "description":
                    "Stable corpus id. Becomes the recipe's directory name \
                     under ~/.sovereign/recipes/."
            },
            "name":          { "type": "string" },
            "description":   { "type": "string" },
            "license":       { "type": "string" },
            "mesh_sharing":  { "type": "boolean" },
            "query_sharing": { "type": "boolean" },
            "parent_corpus_id":   { "type": "string" },
            "size_compressed_gb": { "type": "number" },
            "size_indexed_gb":    { "type": "number" }
        }
    })
}

fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "description":
            "Install-time parameters. Each key is a parameter name; the value \
             declares its type and (optionally) default.",
        "additionalProperties": {
            "type": "object",
            "required": ["type"],
            "additionalProperties": true,
            "properties": {
                "type":        { "enum": ["string", "int", "date", "list"] },
                "description": { "type": "string" },
                "required":    { "type": "boolean" },
                "default":     { }
            }
        }
    })
}

fn acquire_schema() -> Value {
    let arms: Vec<Value> = desc_variants("acquire")
        .iter()
        .map(|(key, required)| variant_arm(key, required, acquire_overlay(key)))
        .collect();
    json!({
        "description":
            "Acquirer config. Discriminated by `type`. Each variant has its own \
             required-fields list — match the variant's arm exactly. The on-disk \
             validator double-checks at write time.",
        "oneOf": arms,
    })
}

/// Rich, hand-authored property hints for specific acquirers. Everything else
/// flows through `additionalProperties: true`.
fn acquire_overlay(key: &str) -> Value {
    match key {
        "http_api" => json!({
            "base_url": {
                "type": "string",
                "description":
                    "Optional URL prefix referenced as `{base_url}` in \
                     `requests[].url`. Omit if request URLs are absolute."
            },
            "rate_limit_per_second": { "type": "number" },
            "user_agent":            { "type": "string" },
            "headers": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            },
            "requests": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["url"],
                    "additionalProperties": true,
                    "properties": {
                        "url": { "type": "string" },
                        "for_each": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description":
                                "Names of [parameters.<name>] entries this URL \
                                 iterates over."
                        }
                    }
                }
            },
            "pagination": {
                "description": "Pagination strategy, discriminated by `type`.",
                "oneOf": [
                    { "type": "object", "required": ["type", "page_size"], "additionalProperties": true,
                      "properties": { "type": { "const": "offset" }, "param": { "type": "string" },
                        "page_size": { "type": "integer", "minimum": 1 }, "items_path": { "type": "string" } } },
                    { "type": "object", "required": ["type", "param", "response_path"], "additionalProperties": true,
                      "properties": { "type": { "const": "cursor" }, "param": { "type": "string" }, "response_path": { "type": "string" } } },
                    { "type": "object", "required": ["type", "response_path"], "additionalProperties": true,
                      "properties": { "type": { "const": "next_url" }, "response_path": { "type": "string" } } },
                    { "type": "object", "required": ["type"], "additionalProperties": true,
                      "properties": { "type": { "const": "page_number" }, "param": { "type": "string" },
                        "start": { "type": "integer" }, "end": { "type": ["integer", "string"] } } }
                ]
            },
            "follow": {
                "type": "object",
                "required": ["document_url_path"],
                "additionalProperties": true,
                "properties": {
                    "document_url_path": { "type": "string" },
                    "document_format":   { "enum": ["html", "json", "xml", "plaintext"] }
                }
            }
        }),
        "bulk_download" => json!({
            "url":  { "type": "string", "description": "Single archive/dump URL." },
            "urls": { "type": "array", "items": { "type": "string" },
                      "description": "Multiple source URLs (use instead of `url`)." },
            "resume": { "type": "boolean" }
        }),
        "web_crawl" => json!({
            "seed_urls":    { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "link_pattern": { "type": "string", "description": "Regex; links matching it are followed." },
            "max_pages":    { "type": "integer", "minimum": 1 }
        }),
        "local_file" => json!({
            "path": { "type": "string", "description": "Folder or file already on disk (e.g. ~/data/my-corpus)." }
        }),
        "huggingface_dataset" => json!({
            "repo":    { "type": "string", "description": "HF dataset repo, e.g. `org/name`." },
            "subset":  { "type": "string" }
        }),
        _ => json!({}),
    }
}

fn extract_schema() -> Value {
    let arms: Vec<Value> = desc_variants("extract")
        .iter()
        .map(|(key, required)| variant_arm(key, required, extract_overlay(key)))
        .collect();
    json!({
        "description":
            "Extractor config. Discriminated by `type` — match the format of the \
             bytes the acquirer produced. The on-disk validator surfaces \
             per-variant errors at write time.",
        "oneOf": arms,
    })
}

/// Hand-authored hints for the higher-traffic extractors.
fn extract_overlay(key: &str) -> Value {
    match key {
        "jsonl" => json!({
            "content_field": { "type": "string", "description": "JSON field holding the body text." },
            "title_field":   { "type": "string" }
        }),
        "json" => json!({
            "document_path": { "type": "string", "description": "JSONPath into the response, e.g. `$.results[*]`." },
            "content_field": { "type": "string", "description": "Field on each matched object holding the body." },
            "title_field":   { "type": "string" },
            "url_field":     { "type": "string" },
            "id_field":      { "type": "string" }
        }),
        "csv" => json!({
            "content_column": { "type": "string" },
            "title_column":   { "type": "string" }
        }),
        "parquet" => json!({
            "content_column": { "type": "string" },
            "label_column":   { "type": "string" }
        }),
        "html" => json!({
            "title_selector":   { "type": "string" },
            "content_selector": { "type": "string" }
        }),
        "html_sections" => json!({
            "sections": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["name", "start_pattern"],
                    "additionalProperties": true,
                    "properties": {
                        "name":          { "type": "string" },
                        "description":   { "type": "string" },
                        "start_pattern": { "type": "string" },
                        "end_pattern":   { "type": "string" }
                    }
                }
            }
        }),
        _ => json!({}),
    }
}

fn chunk_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type"],
        "additionalProperties": true,
        "properties": {
            "type":          { "enum": desc_keys("chunk") },
            "max_chars":     { "type": "integer", "minimum": 1 },
            "overlap_chars": { "type": "integer", "minimum": 0 }
        }
    })
}

fn filter_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "required": ["type"],
            "additionalProperties": true,
            "properties": {
                "type": { "enum": desc_keys("filter") }
            }
        }
    })
}

fn filter_mode_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "mode": { "enum": ["any", "all"] }
        }
    })
}

fn index_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": true,
        "properties": {
            "fts":             { "type": "boolean" },
            "vector":          { "type": "boolean" },
            "embedding_model": { "type": "string" }
        }
    })
}

fn enrichment_schema() -> Value {
    json!({
        "type": "object",
        "required": ["enabled", "type"],
        "additionalProperties": true,
        "properties": {
            // `enrichment.type` is a free string field in recipe.rs (not a
            // typed enum), so these are the conventional pipeline names rather
            // than a generated list.
            "enabled": { "type": "boolean" },
            "type": { "enum": ["field_model", "atlas", "investigation", "multi"] },
            "domain": { "type": "string" },
            // Explicit atlas pipeline pin (genre `*_atlas`); usually omitted —
            // a custom `ontology` (below) or `domain` selects the pipeline.
            "pipeline": { "type": "string" },
            // CUSTOM ATLAS ONTOLOGY — the headline "build the ontology for this
            // domain" surface. Declared here so the grammar GUIDES the agent to
            // emit it (not just permits it): `guidance` is prose describing what
            // entities/relations/claims/events matter, in the domain's language;
            // a generic atlas pipeline extracts to it → atoms.json that feeds chat.
            "ontology": ontology_schema(),
            "prompt_version": { "type": "string" },
            "entity_types": { "type": "array", "items": entity_type_schema() },
            "relationship_types": { "type": "array", "items": relationship_type_schema() },
            "patterns": { "type": "array", "items": pattern_schema() }
        }
    })
}

/// Custom atlas ontology (`[enrichment.ontology]`). `guidance` is the
/// load-bearing field — prose, in the domain's own language, naming what
/// entities / relations / claims / events the extractor should lift. Optional
/// `vocabulary` renames the CLI/label terms per domain.
fn ontology_schema() -> Value {
    json!({
        "type": "object",
        "required": ["guidance"],
        "additionalProperties": true,
        "properties": {
            "guidance": { "type": "string" },
            "vocabulary": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "concern_term":  { "type": "string" },
                    "position_term": { "type": "string" },
                    "tension_term":  { "type": "string" },
                    "absence_term":  { "type": "string" },
                    "evidence_term": { "type": "string" }
                }
            }
        }
    })
}

fn entity_type_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name"],
        "additionalProperties": true,
        "properties": {
            "name":        { "type": "string" },
            "description": { "type": "string" },
            "attributes":  { "type": "array", "items": { "type": "string" } }
        }
    })
}

fn relationship_type_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name"],
        "additionalProperties": true,
        "properties": {
            "name":        { "type": "string" },
            "description": { "type": "string" },
            "directional": { "type": "boolean" },
            "attributes":  { "type": "array", "items": { "type": "string" } }
        }
    })
}

fn pattern_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type"],
        "additionalProperties": true,
        "properties": {
            "type":        { "enum": desc_keys("pattern") },
            "name":        { "type": "string" },
            "description": { "type": "string" },
            // Comparison variants are generated too, so the grammar rejects
            // `gt`/`lt` abbreviations the model is otherwise tempted to emit.
            "comparison":  { "enum": desc_keys("comparison") }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant_consts(section_oneof: &Value) -> Vec<String> {
        section_oneof
            .as_array()
            .expect("oneOf array")
            .iter()
            .filter_map(|arm| {
                arm["properties"]["type"]["const"]
                    .as_str()
                    .map(String::from)
            })
            .collect()
    }

    #[test]
    fn schema_is_valid_json_object() {
        let s = recipe_json_schema();
        assert_eq!(s["type"], "object");
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for k in ["corpus", "acquire", "extract", "chunk"] {
            assert!(required.contains(&k), "root must require {k}");
        }
    }

    #[test]
    fn extract_covers_all_generated_variants() {
        // The whole point of the generated descriptor: every ExtractorConfig
        // variant in recipe.rs must surface as an arm. Pre-generation this was
        // 4 of 22 (email/jsonl/csv/markdown were unauthorable).
        let s = recipe_json_schema();
        let arms = variant_consts(&s["properties"]["extract"]["oneOf"]);
        for v in [
            "jsonl",
            "csv",
            "email",
            "markdown",
            "code",
            "parquet",
            "html",
            "html_sections",
        ] {
            assert!(
                arms.iter().any(|x| x == v),
                "missing extractor arm `{v}`; got {arms:?}"
            );
        }
        assert!(
            arms.len() >= 20,
            "expected the full extractor catalog, got {}",
            arms.len()
        );
    }

    #[test]
    fn acquire_covers_all_generated_variants() {
        let s = recipe_json_schema();
        let arms = variant_consts(&s["properties"]["acquire"]["oneOf"]);
        for v in [
            "bulk_download",
            "http_api",
            "huggingface_dataset",
            "local_file",
            "web_crawl",
        ] {
            assert!(
                arms.iter().any(|x| x == v),
                "missing acquire arm `{v}`; got {arms:?}"
            );
        }
    }

    #[test]
    fn http_api_arm_requires_requests_not_base_url() {
        // base_url is `#[serde(default)]` in recipe.rs, so it is NOT required
        // (the old hand-schema wrongly required it); `requests` IS required.
        let s = recipe_json_schema();
        let arms = s["properties"]["acquire"]["oneOf"].as_array().unwrap();
        let http_api = arms
            .iter()
            .find(|a| a["properties"]["type"]["const"] == "http_api")
            .expect("http_api arm");
        let required: Vec<&str> = http_api["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            required.contains(&"requests"),
            "http_api must require requests"
        );
        assert!(
            !required.contains(&"base_url"),
            "base_url is defaulted, must not be required"
        );
    }

    #[test]
    fn chunk_and_filter_and_pattern_enums_are_generated() {
        let s = recipe_json_schema();
        let chunk: Vec<&str> = s["properties"]["chunk"]["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            chunk.contains(&"threaded_turns"),
            "chunk enum should include threaded_turns; got {chunk:?}"
        );
        let pats: Vec<&str> = s["properties"]["enrichment"]["properties"]["patterns"]["items"]
            ["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(pats.contains(&"role_overlap"));
    }
}
