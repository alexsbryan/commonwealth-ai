//! JSON Schema for the recipe TOML data model.
//!
//! Returned by `RecipeWriteStructuredTool::descriptor()` as the
//! `parameters.recipe` schema, so the daemon's LLGuidance sampler
//! can grammar-constrain the model's emitted tool-call arguments to
//! valid recipes. Two layers of correctness:
//!
//! 1. **Generation-time** — the daemon constrains the model to emit
//!    a JSON object that matches this schema. Discriminator enums
//!    (`acquire.type`, `extract.type`, `chunk.type`, etc.) are
//!    grammar-validated at sampling time.
//! 2. **Validation-time** — the tool re-validates against this
//!    schema before converting to TOML and writing to disk.
//!    Belt-and-suspenders: if the daemon's grammar layer is off
//!    (model unsupported, daemon not running constrained sampler),
//!    the tool still rejects malformed input with a clear error.
//!
//! Schema design:
//!
//! - **Strict on discriminators**: `acquire.type` must be one of
//!   the variants `corpus_engine::AcquirerConfig` recognizes; same
//!   for `extract.type`, `chunk.type`, `enrichment.type`,
//!   `filter[].type`, `enrichment.patterns[].type`.
//! - **Strict on required fields**: every required field in
//!   `recipe.rs` is required here (corpus.id, corpus.name,
//!   parameters.<name>.type, etc.).
//! - **Permissive on per-variant fields**: each variant has
//!   `additionalProperties: true` so per-acquirer / per-extractor
//!   knobs (base_url, rate_limit_per_second, sections,
//!   start_pattern, etc.) flow through without us having to mirror
//!   every field. The on-disk RecipeValidate catches any
//!   per-variant errors after writing.
//!
//! Trade-off: a fully precise schema with per-variant `oneOf`
//! arms gets richer LLGuidance constraint, but the JSON Schema
//! becomes ~400 lines and harder to keep in sync with `recipe.rs`.
//! The current pragmatic shape gives the agent enough structure
//! to pattern off without the maintenance burden of a 1:1 mirror.

use serde_json::{json, Value};

/// Top-level JSON Schema for a recipe. Used as the
/// `parameters.recipe` schema in `RecipeWriteStructuredTool` and as
/// the runtime validator's input schema.
pub fn recipe_json_schema() -> Value {
    json!({
        "type": "object",
        "title": "Recipe",
        "description":
            "Sovereign corpus recipe. Mirrors the on-disk \
             `~/.sovereign/recipes/<id>/recipe.toml` shape but as a \
             JSON object that gets mechanically serialized to TOML \
             by the tool. Consult `corpus-engine/src/recipe.rs` for \
             the source-of-truth Rust types.",
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
                    "Stable corpus id. Becomes the recipe's \
                     directory name under ~/.sovereign/recipes/."
            },
            "name":          { "type": "string" },
            "description":   { "type": "string" },
            "license":       { "type": "string" },
            "mesh_sharing":  { "type": "boolean" },
            "query_sharing": { "type": "boolean" },
            "size_compressed_gb": { "type": "number" },
            "size_indexed_gb":    { "type": "number" }
        }
    })
}

fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "description":
            "Install-time parameters. Each key is a parameter name; \
             the value declares its type and (optionally) default.",
        "additionalProperties": {
            "type": "object",
            "required": ["type"],
            "additionalProperties": true,
            "properties": {
                "type":        { "enum": ["string", "int", "date", "list"] },
                "description": { "type": "string" },
                "required":    { "type": "boolean" },
                "default":     { /* any */ }
            }
        }
    })
}

fn acquire_schema() -> Value {
    json!({
        "description":
            "Acquirer config. Discriminated by `type`. Each variant \
             has its own required-fields list — match the variant's \
             arm exactly. The on-disk validator double-checks at \
             write time.",
        "oneOf": [
            // http_api: requires base_url + at least one [[acquire.requests]].
            // Pagination is optional; follow is optional; auth is optional.
            {
                "type": "object",
                "required": ["type", "base_url", "requests"],
                "additionalProperties": true,
                "properties": {
                    "type":     { "const": "http_api" },
                    "base_url": { "type": "string" },
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
                                        "Names of [parameters.<name>] entries \
                                         this URL iterates over. Every \
                                         placeholder in `url` must either \
                                         appear in `for_each` or be a declared \
                                         non-list parameter."
                                }
                            }
                        }
                    },
                    "pagination": {
                        "description":
                            "Pagination strategy. Discriminated by \
                             `type`. Each strategy has its own \
                             required fields — match the right arm.",
                        "oneOf": [
                            // offset: page_size REQUIRED.
                            {
                                "type": "object",
                                "required": ["type", "page_size"],
                                "additionalProperties": true,
                                "properties": {
                                    "type":      { "const": "offset" },
                                    "param":     { "type": "string" },
                                    "page_size": { "type": "integer", "minimum": 1 },
                                    "items_path": { "type": "string" }
                                }
                            },
                            // cursor: param + response_path REQUIRED.
                            {
                                "type": "object",
                                "required": ["type", "param", "response_path"],
                                "additionalProperties": true,
                                "properties": {
                                    "type":          { "const": "cursor" },
                                    "param":         { "type": "string" },
                                    "response_path": { "type": "string" }
                                }
                            },
                            // next_url: response_path REQUIRED.
                            {
                                "type": "object",
                                "required": ["type", "response_path"],
                                "additionalProperties": true,
                                "properties": {
                                    "type":          { "const": "next_url" },
                                    "response_path": { "type": "string" }
                                }
                            },
                            // page_number: param REQUIRED.
                            {
                                "type": "object",
                                "required": ["type"],
                                "additionalProperties": true,
                                "properties": {
                                    "type":  { "const": "page_number" },
                                    "param": { "type": "string" },
                                    "start": { "type": "integer" },
                                    "end":   { "type": ["integer", "string"] }
                                }
                            }
                        ]
                    },
                    "follow": {
                        "type": "object",
                        "required": ["document_url_path"],
                        "additionalProperties": true,
                        "properties": {
                            "document_url_path": { "type": "string" },
                            "document_format": {
                                "enum": ["plaintext", "html", "json", "parquet"]
                            }
                        }
                    }
                }
            },
            // bulk_download: requires url. Single-shot zip / tarball.
            {
                "type": "object",
                "required": ["type", "url"],
                "additionalProperties": true,
                "properties": {
                    "type": { "const": "bulk_download" },
                    "url":  { "type": "string" }
                }
            },
            // web_crawl: requires start_urls.
            {
                "type": "object",
                "required": ["type", "start_urls"],
                "additionalProperties": true,
                "properties": {
                    "type":       { "const": "web_crawl" },
                    "start_urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1
                    }
                }
            },
            // local_file: requires path.
            {
                "type": "object",
                "required": ["type", "path"],
                "additionalProperties": true,
                "properties": {
                    "type": { "const": "local_file" },
                    "path": { "type": "string" }
                }
            },
            // huggingface_dataset: requires dataset.
            {
                "type": "object",
                "required": ["type", "dataset"],
                "additionalProperties": true,
                "properties": {
                    "type":    { "const": "huggingface_dataset" },
                    "dataset": { "type": "string" }
                }
            }
        ]
    })
}

fn extract_schema() -> Value {
    json!({
        "description":
            "Extractor config. Discriminated by `type`. Each variant \
             has its own required-fields list. The on-disk validator \
             surfaces any per-variant errors at write time.",
        "oneOf": [
            // plaintext: no extra fields needed.
            {
                "type": "object",
                "required": ["type"],
                "additionalProperties": true,
                "properties": {
                    "type": { "const": "plaintext" }
                }
            },
            // html: optional title_selector / content_selector.
            {
                "type": "object",
                "required": ["type"],
                "additionalProperties": true,
                "properties": {
                    "type":             { "const": "html" },
                    "title_selector":   { "type": "string" },
                    "content_selector": { "type": "string" }
                }
            },
            // html_sections: requires non-empty sections array.
            {
                "type": "object",
                "required": ["type", "sections"],
                "additionalProperties": true,
                "properties": {
                    "type": { "const": "html_sections" },
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
                    },
                    "fallback": {
                        "type": "object",
                        "additionalProperties": true,
                        "properties": {
                            "type": {
                                "enum": ["full_document", "skip"]
                            }
                        }
                    }
                }
            },
            // json: per-page response from an http_api acquirer.
            // document_path + content_field REQUIRED.
            {
                "type": "object",
                "required": ["type", "document_path", "content_field"],
                "additionalProperties": true,
                "properties": {
                    "type":          { "const": "json" },
                    "document_path": {
                        "type": "string",
                        "description":
                            "JSONPath into the response (e.g. \
                             `$.results[*]`)."
                    },
                    "content_field": {
                        "type": "string",
                        "description":
                            "Field on each matched object holding the \
                             document body text."
                    },
                    "title_field":   { "type": "string" },
                    "url_field":     { "type": "string" },
                    "id_field":      { "type": "string" }
                }
            },
            // parquet: requires content_column.
            {
                "type": "object",
                "required": ["type", "content_column"],
                "additionalProperties": true,
                "properties": {
                    "type":           { "const": "parquet" },
                    "content_column": { "type": "string" }
                }
            }
        ]
    })
}

fn chunk_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type"],
        "additionalProperties": true,
        "properties": {
            "type": {
                "enum": [
                    "paragraph",
                    "sentence",
                    "fixed",
                    "semantic",
                    "passthrough"
                ]
            },
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
                "type": {
                    "enum": [
                        "title_list",
                        "pageview_rank",
                        "knowledge_density"
                    ]
                }
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
            "fts":              { "type": "boolean" },
            "vector":           { "type": "boolean" },
            "embedding_model":  { "type": "string" }
        }
    })
}

fn enrichment_schema() -> Value {
    json!({
        "type": "object",
        "required": ["enabled", "type"],
        "additionalProperties": true,
        "properties": {
            "enabled": { "type": "boolean" },
            "type": {
                "enum": [
                    "field_model",
                    "atlas",
                    "investigation",
                    "multi"
                ]
            },
            "domain": { "type": "string" },
            "prompt_version": { "type": "string" },
            "entity_types": {
                "type": "array",
                "items": entity_type_schema()
            },
            "relationship_types": {
                "type": "array",
                "items": relationship_type_schema()
            },
            "patterns": {
                "type": "array",
                "items": pattern_schema()
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
            "attributes": {
                "type": "array",
                "items": { "type": "string" }
            }
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
            "attributes": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

fn pattern_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type"],
        "additionalProperties": true,
        "properties": {
            "type": {
                "enum": [
                    "circular_flow",
                    "role_overlap",
                    "threshold",
                    "custom_sql"
                ]
            },
            "name":        { "type": "string" },
            "description": { "type": "string" },
            // Threshold-specific. Mirroring corpus_engine's
            // `recipe::Comparison` snake_case variants exactly so the
            // grammar layer rejects `gt`/`lt` style abbreviations the
            // model is otherwise tempted to emit.
            "comparison": {
                "enum": [
                    "greater_than",
                    "greater_or_equal",
                    "less_than",
                    "less_or_equal",
                    "equal"
                ]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_valid_json_object() {
        let s = recipe_json_schema();
        assert!(s.is_object());
        assert_eq!(s["type"], "object");
        let required = s["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"corpus"));
        assert!(names.contains(&"acquire"));
        assert!(names.contains(&"extract"));
        assert!(names.contains(&"chunk"));
    }

    #[test]
    fn acquire_oneof_covers_all_real_variants() {
        let s = recipe_json_schema();
        let arms = s["properties"]["acquire"]["oneOf"]
            .as_array()
            .expect("acquire must be a oneOf");
        // Pull each arm's `type.const` out and confirm we see every
        // variant. This is what the daemon's grammar layer walks at
        // sampling time to constrain the model's output.
        let mut variants: Vec<String> = arms
            .iter()
            .filter_map(|arm| {
                arm.get("properties")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.get("const"))
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
            .collect();
        variants.sort();
        for v in [
            "bulk_download",
            "http_api",
            "huggingface_dataset",
            "local_file",
            "web_crawl",
        ] {
            assert!(
                variants.iter().any(|x| x == v),
                "missing acquire variant {v}; got {variants:?}"
            );
        }
    }

    #[test]
    fn http_api_arm_requires_base_url_and_requests() {
        let s = recipe_json_schema();
        let arms = s["properties"]["acquire"]["oneOf"]
            .as_array()
            .expect("acquire must be a oneOf");
        let http_api = arms
            .iter()
            .find(|arm| arm["properties"]["type"]["const"] == "http_api")
            .expect("http_api arm");
        let required: Vec<&str> = http_api["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"base_url"));
        assert!(required.contains(&"requests"));
    }

    #[test]
    fn extract_html_sections_arm_requires_sections() {
        let s = recipe_json_schema();
        let arms = s["properties"]["extract"]["oneOf"]
            .as_array()
            .expect("extract must be a oneOf");
        let hs = arms
            .iter()
            .find(|arm| arm["properties"]["type"]["const"] == "html_sections")
            .expect("html_sections arm");
        let required: Vec<&str> = hs["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"sections"));
    }

    #[test]
    fn enrichment_pattern_types_include_role_overlap() {
        let s = recipe_json_schema();
        let pat_types = s["properties"]["enrichment"]["properties"]["patterns"]["items"]
            ["properties"]["type"]["enum"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = pat_types.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"role_overlap"));
        assert!(names.contains(&"circular_flow"));
        assert!(names.contains(&"threshold"));
    }
}
