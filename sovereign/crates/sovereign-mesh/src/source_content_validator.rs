//! Source-content validation framework for tool-call arguments.
//!
//! Why this exists. The tool-envelope grammar (see
//! `inference_adapter::tool_envelope_schema_for`) constrains the
//! *shape* of `{"name":..., "arguments": {...}}` but is opaque to
//! the *contents* of any string field inside `arguments`. When
//! opencode/Aider/etc. ask the model to write a file, the source
//! lives inside an `arguments.content` (or similar) string — to
//! the JSON-Schema sampler, that's a string value, full stop.
//! Mid-string corruption (`LatENCYClass`, `Lat encyClass`,
//! `Inference Requirements`) sails past the grammar.
//!
//! The cargo-check gate downstream catches these eventually, but
//! the model has already executed the tool against disk by then —
//! several seconds of wasted wall-clock per corrupted iter, with
//! no daemon-side log of WHEN or WHERE the corruption appeared.
//!
//! This module sits between `parse_tool_envelope_direct` and the
//! conversion to `ToolCall` structs. It walks each tool's parameter
//! schema for the `x-source-content` extension keyword, extracts
//! the corresponding string value from the parsed `arguments`, and
//! runs a pluggable validator over the contents. Findings are
//! logged at `tracing::warn` so the operator sees corruption at the
//! moment it's emitted, not after a downstream verify run.
//!
//! ## Schema marker convention
//!
//! In the tool's `parameters` JSON Schema, mark a string field by
//! adding the extension keyword `x-source-content` whose value is
//! the language tag the validator registry will look up:
//!
//! ```json
//! {
//!   "type": "object",
//!   "properties": {
//!     "filePath": { "type": "string" },
//!     "content":  { "type": "string", "x-source-content": "rust" }
//!   },
//!   "required": ["filePath", "content"]
//! }
//! ```
//!
//! The keyword is unknown to standard JSON Schema validators (so
//! existing pipelines ignore it cleanly) but is recognised here.
//! Tags are arbitrary strings; the registry decides which ones it
//! has a validator for. Unknown tags log a `debug` line and skip.
//!
//! ## Architecture (per ARCH_PRINCIPLES § 4)
//!
//! `SourceContentValidator` is a trait. `ValidatorRegistry` is a
//! string→Box<dyn Validator> map. Wiring code calls
//! `validate_tool_calls` with both; the function knows nothing
//! about specific languages. Future per-language validators slot
//! into the registry via `register("rust", RustValidator::new())`
//! without touching the call site.
//!
//! ## What this is NOT (yet)
//!
//! - It does not (yet) reject the tool call when corruption is
//!   detected. The tool_calls field in the response is unchanged.
//!   First job is observability: surface where corruption appears
//!   so we have data for designing the right intervention
//!   (sampler-time grammar over the field, retry-with-error,
//!   hard reject).
//! - It does not ship a Rust validator today. The framework is
//!   wired so a future PR can drop one in. Right now the registry
//!   is empty by default; calls with non-empty registries log
//!   findings, and the end-to-end path is pinned by tests.

use std::collections::HashMap;

use commonwealth_api::openai_types::ToolDefinition;
use sovereign_inference::embedded::ParsedToolCall;

/// One validation finding. Field names are flat to simplify log
/// ingestion; nested fields would only matter once the response
/// shape evolves to surface findings to the client.
#[derive(Debug, Clone)]
pub struct Finding {
    pub tool_name: String,
    pub field_path: String,
    pub language: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Source did not lex/parse cleanly — corruption suspected.
    Warn,
    /// Validator could not run (missing language, malformed
    /// arguments). Surfaces operational issues separately from
    /// content issues.
    Error,
}

/// Per-language validator. The contract is intentionally minimal:
/// take a string of source, return either Ok (clean) or Err with a
/// human-readable error message. The validator MUST NOT panic on
/// arbitrary input — fuzzy-emitted source can be anything.
pub trait SourceContentValidator: Send + Sync {
    fn validate(&self, source: &str) -> Result<(), String>;
}

/// String-keyed registry. Empty by default; callers add validators
/// for the languages they expect to see. Lookup is by exact match
/// against the `x-source-content` tag; unknown tags are silently
/// ignored at validation time.
#[derive(Default)]
pub struct ValidatorRegistry {
    by_language: HashMap<String, Box<dyn SourceContentValidator>>,
}

impl ValidatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        language: impl Into<String>,
        validator: Box<dyn SourceContentValidator>,
    ) {
        self.by_language.insert(language.into(), validator);
    }

    pub fn get(&self, language: &str) -> Option<&dyn SourceContentValidator> {
        self.by_language.get(language).map(|b| b.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.by_language.is_empty()
    }
}

/// Validate every tool call against the schemas declared in `tools`.
/// Returns one Finding per (call × marked-field) where the validator
/// reported an issue. Calls whose names don't match any tool, or
/// whose marked fields have validators not in the registry, are
/// skipped silently.
///
/// Side-effect: logs each Finding at `tracing::warn` (Severity::Warn)
/// or `tracing::error` (Severity::Error) with structured fields so
/// the daemon log is greppable for `source_content_validation:`.
pub fn validate_tool_calls(
    parsed_calls: &[ParsedToolCall],
    tools: &[ToolDefinition],
    registry: &ValidatorRegistry,
) -> Vec<Finding> {
    if registry.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    let by_name: HashMap<&str, &ToolDefinition> = tools
        .iter()
        .map(|t| (t.function.name.as_str(), t))
        .collect();
    for call in parsed_calls {
        let Some(tool) = by_name.get(call.name.as_str()).copied() else {
            continue;
        };
        let arg_obj: serde_json::Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                let f = Finding {
                    tool_name: call.name.clone(),
                    field_path: String::from("<arguments>"),
                    language: String::new(),
                    severity: Severity::Error,
                    message: format!("arguments JSON parse failed: {e}"),
                };
                emit(&f);
                findings.push(f);
                continue;
            }
        };
        let markers = walk_schema_for_markers(&tool.function.parameters, "");
        for marker in markers {
            let Some(value) = lookup_value(&arg_obj, &marker.path) else {
                continue;
            };
            let Some(source) = value.as_str() else {
                continue;
            };
            let Some(validator) = registry.get(&marker.language) else {
                tracing::debug!(
                    tool = %call.name,
                    field = %marker.path,
                    language = %marker.language,
                    "source_content_validation: no validator registered for language; skipping"
                );
                continue;
            };
            if let Err(msg) = validator.validate(source) {
                let f = Finding {
                    tool_name: call.name.clone(),
                    field_path: marker.path.clone(),
                    language: marker.language.clone(),
                    severity: Severity::Warn,
                    message: msg,
                };
                emit(&f);
                findings.push(f);
            }
        }
    }
    findings
}

fn emit(f: &Finding) {
    match f.severity {
        Severity::Warn => tracing::warn!(
            tool = %f.tool_name,
            field = %f.field_path,
            language = %f.language,
            error = %f.message,
            "source_content_validation: emitted source did not validate"
        ),
        Severity::Error => tracing::error!(
            tool = %f.tool_name,
            field = %f.field_path,
            language = %f.language,
            error = %f.message,
            "source_content_validation: validator could not run"
        ),
    }
}

/// One marked field in a schema: where it lives (`path`, dotted)
/// and what language it carries.
#[derive(Debug, Clone)]
struct Marker {
    path: String,
    language: String,
}

/// Recursively walk a JSON Schema looking for the `x-source-content`
/// extension keyword. Builds a dotted path under each `properties`
/// hop. Schemas without `properties` (top-level enums, primitives)
/// produce zero markers.
///
/// Today supports `properties` only — `items` and `oneOf` extensions
/// are intentionally not walked; today's tools don't use them in
/// source-content positions, and walking them blindly would multiply
/// false positives across the union variants. Add a branch when a
/// real tool needs it.
fn walk_schema_for_markers(schema: &serde_json::Value, prefix: &str) -> Vec<Marker> {
    let mut out = Vec::new();
    let Some(obj) = schema.as_object() else {
        return out;
    };
    if let Some(lang) = obj.get("x-source-content").and_then(|v| v.as_str()) {
        out.push(Marker {
            path: prefix.to_string(),
            language: lang.to_string(),
        });
    }
    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        for (key, child) in props {
            let next = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            out.extend(walk_schema_for_markers(child, &next));
        }
    }
    out
}

/// Look up a value in a parsed-arguments object by dotted path.
/// Empty path returns the root. Missing path elements return None
/// (caller logs and skips).
fn lookup_value<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for segment in path.split('.') {
        cur = cur.as_object()?.get(segment)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_api::openai_types::{ToolDefinition, ToolFunction};
    use serde_json::json;

    fn tool(name: &str, params: serde_json::Value) -> ToolDefinition {
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: None,
                parameters: params,
            },
        }
    }

    fn call(name: &str, args: serde_json::Value) -> ParsedToolCall {
        ParsedToolCall {
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    /// Test validator that always fails — exercises the warn path.
    struct AlwaysFails;
    impl SourceContentValidator for AlwaysFails {
        fn validate(&self, _source: &str) -> Result<(), String> {
            Err("always fails".into())
        }
    }

    /// Test validator that fails when source contains "BAD".
    struct RejectsBad;
    impl SourceContentValidator for RejectsBad {
        fn validate(&self, source: &str) -> Result<(), String> {
            if source.contains("BAD") {
                Err("source contains BAD".into())
            } else {
                Ok(())
            }
        }
    }

    // ---- walk_schema_for_markers ----

    #[test]
    fn walk_finds_top_level_property_marker() {
        let schema = json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string"},
                "content":  {"type": "string", "x-source-content": "rust"}
            }
        });
        let markers = walk_schema_for_markers(&schema, "");
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].path, "content");
        assert_eq!(markers[0].language, "rust");
    }

    #[test]
    fn walk_finds_nested_property_marker() {
        let schema = json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "object",
                    "properties": {
                        "newSource": {"type": "string", "x-source-content": "python"}
                    }
                }
            }
        });
        let markers = walk_schema_for_markers(&schema, "");
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].path, "edits.newSource");
        assert_eq!(markers[0].language, "python");
    }

    #[test]
    fn walk_finds_zero_markers_when_absent() {
        let schema = json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string"},
                "content":  {"type": "string"}
            }
        });
        assert_eq!(walk_schema_for_markers(&schema, "").len(), 0);
    }

    #[test]
    fn walk_handles_non_object_schema() {
        assert_eq!(walk_schema_for_markers(&json!("primitive"), "").len(), 0);
        assert_eq!(walk_schema_for_markers(&json!(42), "").len(), 0);
    }

    // ---- lookup_value ----

    #[test]
    fn lookup_returns_root_on_empty_path() {
        let root = json!({"a": 1});
        assert_eq!(lookup_value(&root, "").unwrap(), &json!({"a": 1}));
    }

    #[test]
    fn lookup_walks_dotted_path() {
        let root = json!({"a": {"b": {"c": "deep"}}});
        assert_eq!(lookup_value(&root, "a.b.c").unwrap(), &json!("deep"));
    }

    #[test]
    fn lookup_returns_none_on_missing_segment() {
        let root = json!({"a": 1});
        assert!(lookup_value(&root, "a.b").is_none());
        assert!(lookup_value(&root, "missing").is_none());
    }

    // ---- validate_tool_calls ----

    #[test]
    fn validate_returns_empty_when_registry_empty() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "rust"}}
            }),
        )];
        let calls = vec![call("write", json!({"content": "fn main() {}"}))];
        let registry = ValidatorRegistry::new();
        assert!(validate_tool_calls(&calls, &tools, &registry).is_empty());
    }

    #[test]
    fn validate_skips_calls_without_matching_tool() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "rust"}}
            }),
        )];
        let calls = vec![call("unknown_tool", json!({"content": "anything"}))];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(AlwaysFails));
        assert!(validate_tool_calls(&calls, &tools, &registry).is_empty());
    }

    #[test]
    fn validate_skips_unmarked_fields() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"filePath": {"type": "string"}}
            }),
        )];
        let calls = vec![call("write", json!({"filePath": "/tmp/x"}))];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(AlwaysFails));
        assert!(validate_tool_calls(&calls, &tools, &registry).is_empty());
    }

    #[test]
    fn validate_emits_warning_on_marked_field_failure() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {
                    "filePath": {"type": "string"},
                    "content": {"type": "string", "x-source-content": "rust"}
                }
            }),
        )];
        let calls = vec![call(
            "write",
            json!({
                "filePath": "/tmp/lib.rs",
                "content": "fn main() { BAD }"
            }),
        )];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(RejectsBad));
        let findings = validate_tool_calls(&calls, &tools, &registry);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].tool_name, "write");
        assert_eq!(findings[0].field_path, "content");
        assert_eq!(findings[0].language, "rust");
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("BAD"));
    }

    #[test]
    fn validate_skips_when_validator_for_language_missing() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "ocaml"}}
            }),
        )];
        let calls = vec![call("write", json!({"content": "let x = 1"}))];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(AlwaysFails));
        // Only "rust" registered; "ocaml" lookup misses → skipped.
        assert!(validate_tool_calls(&calls, &tools, &registry).is_empty());
    }

    #[test]
    fn validate_emits_error_on_malformed_arguments_json() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "rust"}}
            }),
        )];
        let calls = vec![ParsedToolCall {
            name: "write".into(),
            arguments: "{not-json".into(),
        }];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(AlwaysFails));
        let findings = validate_tool_calls(&calls, &tools, &registry);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].field_path, "<arguments>");
    }

    #[test]
    fn validate_passes_clean_source_through() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "rust"}}
            }),
        )];
        let calls = vec![call("write", json!({"content": "fn main() {}"}))];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(RejectsBad));
        assert!(validate_tool_calls(&calls, &tools, &registry).is_empty());
    }

    #[test]
    fn validate_handles_two_calls_one_corrupt() {
        let tools = vec![tool(
            "write",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "x-source-content": "rust"}}
            }),
        )];
        let calls = vec![
            call("write", json!({"content": "fn ok() {}"})),
            call("write", json!({"content": "fn BAD() {}"})),
        ];
        let mut registry = ValidatorRegistry::new();
        registry.register("rust", Box::new(RejectsBad));
        let findings = validate_tool_calls(&calls, &tools, &registry);
        assert_eq!(findings.len(), 1);
    }
}
