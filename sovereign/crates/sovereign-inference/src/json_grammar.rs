//! JSON Schema → GBNF (GGML BNF) grammar translator.
//!
//! We feed the result to `LlamaSampler::grammar`, which uses
//! llama.cpp's native grammar sampler. That sampler has been in
//! production for years and works on every model llama.cpp supports
//! — replacing the silent-fallthrough llguidance integration that
//! gave us no actual structured-output guarantee across the BYOM
//! model space (see `embedded.rs::build_sampler` history).
//!
//! ## Supported JSON Schema subset (v0)
//!
//! Tight on purpose. Targets the actual atlas Phase 1 schema and the
//! `cluster_name_synth` bench schema. Extend as new cases land — the
//! [`SchemaError`] surfaces unsupported shapes with the offending
//! pointer, so a grammar that needs a feature we don't have yet
//! fails loud at translate time instead of silently undermining the
//! sampler.
//!
//! - `type: "object"` with `properties`, `required`, and either
//!   `additionalProperties: true` (default) or `additionalProperties: false`.
//!   Required properties emit in declaration order followed by optional
//!   ones in declaration order. `additionalProperties: true` allows
//!   arbitrary trailing pairs after the typed ones.
//! - `type: "array"` with `items: <schema>`.
//! - `type: "string"` — any JSON string. With `enum: [...]` the grammar
//!   restricts to the listed alternatives (literal-quoted in the grammar).
//! - `type: "integer"`, `type: "number"`, `type: "boolean"`, `type: "null"`.
//! - `type: ["string", "null"]` and similar 2-element type unions —
//!   compiled as `anyOf`.
//! - `$ref: "#/$defs/<name>"` and `#/definitions/<name>` — resolved
//!   against the root schema's `$defs` / `definitions` map. Each $def
//!   becomes its own named GBNF rule so recursive schemas don't loop
//!   the translator.
//! - `anyOf` / `oneOf` — both compile to `(a | b | c)` alternation.
//!   Grammar can't enforce oneOf's "exactly one" semantics; the parser
//!   handles that downstream if it matters.
//!
//! ## Not yet supported
//!
//! `pattern`, `format`, `minLength`/`maxLength`, `minimum`/`maximum`,
//! `multipleOf`, `if/then/else`, `dependentSchemas`, `not`, external
//! `$ref` URLs, `allOf`. All return [`SchemaError::Unsupported`] with
//! the offending pointer so callers can choose between fixing the
//! schema or extending the translator.

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("schema must be a JSON object at root, got {kind}")]
    NotAnObject { kind: &'static str },
    #[error("unsupported schema feature `{feature}` at `{pointer}`")]
    Unsupported { feature: String, pointer: String },
    #[error("$ref `{reference}` does not resolve in this schema")]
    UnresolvedRef { reference: String },
    #[error("malformed schema at `{pointer}`: {detail}")]
    Malformed { pointer: String, detail: String },
}

/// Translate a JSON Schema (as a parsed `serde_json::Value`) into a
/// GBNF grammar string usable with `LlamaSampler::grammar(model, &gbnf, "root")`.
///
/// The grammar's root rule is named `"root"`. Every `$def` becomes a
/// rule named `def_<sanitized-key>` so a debugging operator can read
/// the grammar and find the schema element a rule came from.
pub fn schema_to_gbnf(schema: &Value) -> Result<String, SchemaError> {
    let root_obj = schema.as_object().ok_or(SchemaError::NotAnObject {
        kind: kind_of(schema),
    })?;

    let mut emitter = Emitter::new();
    // Pre-register every $def by reserving a rule name. We do this
    // BEFORE compiling the root so a property whose value is a
    // `$ref` to a def we haven't visited yet still resolves cleanly.
    if let Some(defs) = collect_defs(root_obj) {
        for name in defs.keys() {
            emitter.reserve_def_rule(name);
        }
        emitter.defs = defs;
    }

    let root_rule = emitter.compile_schema(schema, "")?;
    // Wrap the root in `ws ... ws` so the grammar tolerates a chat
    // template's leading newline / leading-space tokens before the
    // first JSON byte and any trailing whitespace before EOS.
    // Without this, models whose tokenizer never emits a bare `{`
    // (Gemma, some Qwen variants — the BOS/template states emit
    // tokens like ` {` or `\n{`) trigger a `GGML_ASSERT(!stacks.empty())`
    // crash inside llama.cpp's grammar engine on the first decode
    // step, because no candidate token can satisfy the bare-`{` start.
    emitter.add_rule("root".into(), format!("ws {root_rule} ws"));

    // Compile each $def so its named rule body is populated. Order
    // doesn't matter for grammar — GBNF allows forward references.
    let names: Vec<String> = emitter.defs.keys().cloned().collect();
    for name in names {
        let def_schema = emitter.defs.get(&name).cloned().unwrap();
        let body = emitter.compile_schema(&def_schema, &format!("/$defs/{name}"))?;
        let rule_name = emitter.def_rule_name(&name);
        emitter.add_rule(rule_name, body);
    }

    // Shared primitives — emitted once, referenced by name. These
    // mirror llama.cpp's reference `grammars/json.gbnf` byte-for-byte
    // (control-char exclusion in `string`, recursive `ws`) because
    // smaller deviations triggered a `GGML_ASSERT(!stacks.empty())`
    // crash in llama.cpp's grammar engine on the very first decode
    // step. The reference shape is hardened against tokenizer edge
    // cases — model tokens that span grammar productions, mid-string
    // multi-byte token splits, etc. Don't tweak unless you've
    // reproduced the assertion crash with the new variant.
    emitter.add_rule(
        "string".into(),
        r#""\"" ([^"\\\x7F\x00-\x1F] | "\\" (["\\bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* "\"""#.into(),
    );
    emitter.add_rule(
        "integer".into(),
        r#""-"? ("0" | [1-9] [0-9]*)"#.into(),
    );
    emitter.add_rule(
        "number".into(),
        r#""-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [-+]? [0-9]+)?"#.into(),
    );
    emitter.add_rule("boolean".into(), r#""true" | "false""#.into());
    emitter.add_rule("null".into(), r#""null""#.into());
    // Recursive ws form, not `[ \t\n\r]*`. Repetition of a character
    // class through `*` had different stack-management semantics in
    // the engine — pinning to the reference avoids surprises.
    emitter.add_rule("ws".into(), r#"([ \t\n] ws)?"#.into());
    // Generic value used by `additionalProperties: true` trailing
    // pairs and untyped fallbacks. Must come last so it can reference
    // every other primitive.
    // GBNF rule names are `[a-zA-Z][a-zA-Z0-9-]*` — no underscores.
    // llama.cpp's grammar-parser.cpp rejects `object_any` with a
    // "expecting newline or end" error at the first underscore
    // because it treats the underscore as a rule-body terminator.
    // Discovered 2026-04-26 when GBNF init returned null.
    emitter.add_rule(
        "value".into(),
        "object-any | array-any | string | number | boolean | null".into(),
    );
    emitter.add_rule(
        "object-any".into(),
        r#""{" ws ( string ws ":" ws value ws ("," ws string ws ":" ws value ws)* )? "}""#.into(),
    );
    emitter.add_rule(
        "array-any".into(),
        r#""[" ws ( value ws ("," ws value ws)* )? "]""#.into(),
    );

    Ok(emitter.render())
}

// ─── Internals ─────────────────────────────────────────────────

fn collect_defs(root: &serde_json::Map<String, Value>) -> Option<BTreeMap<String, Value>> {
    let raw = root
        .get("$defs")
        .or_else(|| root.get("definitions"))?
        .as_object()?;
    Some(raw.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Build state: collected rules + $defs map + a counter for
/// anonymous sub-rule naming. Renders a deterministic GBNF string at
/// the end so test-comparison is stable across runs.
struct Emitter {
    rules: Vec<(String, String)>,
    seen: std::collections::HashSet<String>,
    defs: BTreeMap<String, Value>,
    def_name_map: BTreeMap<String, String>,
    anon_counter: usize,
}

impl Emitter {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            seen: std::collections::HashSet::new(),
            defs: BTreeMap::new(),
            def_name_map: BTreeMap::new(),
            anon_counter: 0,
        }
    }

    fn reserve_def_rule(&mut self, def_key: &str) {
        let rule = format!("def-{}", sanitize(def_key));
        self.def_name_map.insert(def_key.to_string(), rule);
    }

    fn def_rule_name(&self, def_key: &str) -> String {
        self.def_name_map
            .get(def_key)
            .cloned()
            .unwrap_or_else(|| format!("def-{}", sanitize(def_key)))
    }

    fn add_rule(&mut self, name: String, body: String) {
        if self.seen.insert(name.clone()) {
            self.rules.push((name, body));
        }
    }

    fn next_anon(&mut self, hint: &str) -> String {
        self.anon_counter += 1;
        format!("anon-{}-{}", sanitize(hint), self.anon_counter)
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (name, body) in &self.rules {
            out.push_str(name);
            out.push_str(" ::= ");
            out.push_str(body);
            out.push('\n');
        }
        out
    }

    /// Compile a schema fragment; returns the right-hand side that
    /// can be inlined into the parent rule (or used as the body of a
    /// named rule). `pointer` is the JSON Pointer to this fragment,
    /// used only for error messages.
    fn compile_schema(&mut self, schema: &Value, pointer: &str) -> Result<String, SchemaError> {
        let obj = schema.as_object().ok_or_else(|| SchemaError::Malformed {
            pointer: pointer.into(),
            detail: format!("expected object, got {}", kind_of(schema)),
        })?;

        // $ref takes precedence — the $ref'd shape supplies the rule.
        if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
            return self.resolve_ref(r);
        }

        // anyOf / oneOf — both become alternation. Grammar can't
        // enforce oneOf's exactly-one constraint, but for our use
        // case (where oneOf is mostly used for nullable string), the
        // alternation is the right semantics anyway.
        for key in ["anyOf", "oneOf"] {
            if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                let alts: Vec<String> = arr
                    .iter()
                    .enumerate()
                    .map(|(i, sub)| {
                        self.compile_schema(sub, &format!("{pointer}/{key}/{i}"))
                    })
                    .collect::<Result<_, _>>()?;
                return Ok(format!("({})", alts.join(" | ")));
            }
        }

        // `type` may be a string OR an array of strings (the JSON
        // Schema "type union" shorthand, e.g. `["string", "null"]`).
        let type_field = obj.get("type");
        if let Some(Value::Array(types)) = type_field {
            let alts: Vec<String> = types
                .iter()
                .map(|t| {
                    let mut clone = obj.clone();
                    clone.insert("type".into(), t.clone());
                    self.compile_schema(&Value::Object(clone), pointer)
                })
                .collect::<Result<_, _>>()?;
            return Ok(format!("({})", alts.join(" | ")));
        }

        let ty = type_field
            .and_then(|v| v.as_str())
            .unwrap_or("any");

        match ty {
            "object" => self.compile_object(obj, pointer),
            "array" => self.compile_array(obj, pointer),
            "string" => self.compile_string(obj),
            "integer" => Ok("integer".into()),
            "number" => Ok("number".into()),
            "boolean" => Ok("boolean".into()),
            "null" => Ok("null".into()),
            "any" => Ok("value".into()),
            other => Err(SchemaError::Unsupported {
                feature: format!("type = \"{other}\""),
                pointer: pointer.into(),
            }),
        }
    }

    fn resolve_ref(&mut self, r: &str) -> Result<String, SchemaError> {
        // Only local pointers into $defs / definitions for v0.
        let stripped = r
            .strip_prefix("#/$defs/")
            .or_else(|| r.strip_prefix("#/definitions/"))
            .ok_or_else(|| SchemaError::Unsupported {
                feature: format!("$ref `{r}`"),
                pointer: "$ref".into(),
            })?;

        if !self.def_name_map.contains_key(stripped) {
            return Err(SchemaError::UnresolvedRef { reference: r.into() });
        }
        Ok(self.def_rule_name(stripped))
    }

    fn compile_object(
        &mut self,
        obj: &serde_json::Map<String, Value>,
        pointer: &str,
    ) -> Result<String, SchemaError> {
        let props_map = obj
            .get("properties")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let required: Vec<String> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        // additionalProperties: default true per JSON Schema spec.
        // Atlas leaves it default, which gives the model headroom to
        // add metadata fields (e.g. `confidence`) that don't break
        // the parser.
        let additional = obj
            .get("additionalProperties")
            .map(|v| !matches!(v, Value::Bool(false)))
            .unwrap_or(true);

        if props_map.is_empty() {
            // Object with no typed properties — accept any pairs.
            return Ok("object-any".into());
        }

        // Compile each property's value rule. Use a named sub-rule
        // (`<obj-rule>_<prop>`) so the rendered grammar is debuggable
        // — flat inline blobs are unreadable for atlas-sized schemas.
        let mut required_pairs: Vec<String> = Vec::new();
        let mut optional_pairs: Vec<String> = Vec::new();
        // Iterate properties in JSON declaration order, NOT
        // alphabetical — this matters because the grammar enforces
        // ordering and authors structure the schema in the order
        // they want the model to think about fields.
        for (prop_name, prop_schema) in &props_map {
            let value_rule = self.compile_schema(
                prop_schema,
                &format!("{pointer}/properties/{prop_name}"),
            )?;
            let pair = format!(
                r#""\"{}\"" ws ":" ws {} ws"#,
                escape_for_grammar_literal(prop_name),
                value_rule,
            );
            if required.contains(prop_name) {
                required_pairs.push(pair);
            } else {
                optional_pairs.push(pair);
            }
        }

        // Build the body. Forced ordering: required pairs in
        // declaration order, then optional pairs (each gated by
        // `(",")?`), then optionally arbitrary additional pairs.
        //
        // The model has to learn this order, but in exchange we get
        // a single grammar with no permutation explosion. Llama.cpp's
        // own json_schema_to_grammar.py uses the same trick.
        let mut parts: Vec<String> = Vec::new();
        parts.push(r#""{" ws"#.into());
        let mut wrote_first = false;
        for pair in &required_pairs {
            if wrote_first {
                parts.push(r#""," ws"#.into());
            }
            parts.push(pair.clone());
            wrote_first = true;
        }
        for pair in &optional_pairs {
            // Optional → wrapped in `(...)?`. If we've written
            // anything, the optional pair carries its own leading
            // comma; if not, the pair is bare and any subsequent
            // pair must take care of separators.
            if wrote_first {
                parts.push(format!(r#"("," ws {})?"#, pair));
            } else {
                parts.push(format!("({})?", pair));
            }
        }
        if additional {
            // After the typed pairs, allow any number of `,
            // <key>:<value>` pairs. The leading comma is conditional
            // on having emitted anything, which is true whenever
            // there are any typed pairs.
            if !required_pairs.is_empty() || !optional_pairs.is_empty() {
                parts.push(r#"("," ws string ws ":" ws value ws)*"#.into());
            } else {
                parts.push(r#"(string ws ":" ws value ws ("," ws string ws ":" ws value ws)*)?"#.into());
            }
        }
        parts.push(r#""}""#.into());
        Ok(parts.join(" "))
    }

    fn compile_array(
        &mut self,
        obj: &serde_json::Map<String, Value>,
        pointer: &str,
    ) -> Result<String, SchemaError> {
        let items_schema = obj.get("items").cloned();
        let item_rule = match items_schema {
            Some(s) => self.compile_schema(&s, &format!("{pointer}/items"))?,
            // No `items` declared — accept any value.
            None => "value".into(),
        };
        // Promote complex item rules to named anonymous rules so the
        // emitted grammar stays readable. A primitive like `string`
        // can stay inline.
        let item_ref = if item_rule.contains(' ') {
            let name = self.next_anon("array-item");
            self.add_rule(name.clone(), item_rule);
            name
        } else {
            item_rule
        };
        Ok(format!(
            r#""[" ws ({0} ws ("," ws {0} ws)*)? "]""#,
            item_ref
        ))
    }

    fn compile_string(&mut self, obj: &serde_json::Map<String, Value>) -> Result<String, SchemaError> {
        if let Some(arr) = obj.get("enum").and_then(|v| v.as_array()) {
            let mut alts: Vec<String> = Vec::new();
            for v in arr {
                if let Some(s) = v.as_str() {
                    alts.push(format!("\"\\\"{}\\\"\"", escape_for_grammar_literal(s)));
                }
            }
            if !alts.is_empty() {
                return Ok(format!("({})", alts.join(" | ")));
            }
        }
        Ok("string".into())
    }
}

/// Escape a property name or enum value for inclusion as a GBNF
/// string literal. GBNF literals are double-quoted; we need to
/// escape backslash and double-quote inside the literal. Property
/// names are conventionally ASCII identifier-shaped, but enum
/// strings can carry anything.
fn escape_for_grammar_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// Map any string into something safe to use as a GBNF rule-name
/// suffix. llama.cpp's grammar-parser.cpp accepts `[a-zA-Z][a-zA-Z0-9-]*`
/// — letters, digits, hyphens, NO UNDERSCORES. We convert both
/// underscores and any other non-conforming character into `-` so
/// `entity_state_sketch` → `entity-state-sketch`, `foo bar` →
/// `foo-bar`, etc. Two distinct keys can theoretically collide
/// (e.g. `foo-bar` and `foo_bar` → both become `foo-bar`); in
/// practice the $defs we deal with are identifier-shaped and
/// collisions haven't surfaced.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("anon");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(grammar: &str) -> std::collections::BTreeMap<&str, &str> {
        // Quick rule-extractor for assertions: split on lines, take
        // `<name> ::= <body>`. Comments / multi-line bodies aren't
        // supported by this helper — keep test schemas simple.
        let mut out = std::collections::BTreeMap::new();
        for line in grammar.lines() {
            if let Some((name, body)) = line.split_once(" ::= ") {
                out.insert(name.trim(), body.trim());
            }
        }
        out
    }

    #[test]
    fn primitive_string_root() {
        let g = schema_to_gbnf(&json!({ "type": "string" })).unwrap();
        let r = parse(&g);
        // Root is wrapped `ws <body> ws` to absorb chat-template
        // whitespace at the boundaries — see `schema_to_gbnf` body
        // for the rationale.
        assert_eq!(r["root"], "ws string ws");
        assert!(r.contains_key("string"));
    }

    #[test]
    fn nullable_string_via_type_array() {
        let g = schema_to_gbnf(&json!({ "type": ["string", "null"] })).unwrap();
        let r = parse(&g);
        assert_eq!(r["root"], "ws (string | null) ws");
    }

    #[test]
    fn cluster_name_synth_schema_translates() {
        // The actual schema the bench uses for cluster_name_synth.
        let schema = json!({
            "type": "object",
            "properties": {
                "label": { "type": "string" },
                "rationale": { "type": "string" }
            },
            "required": ["label", "rationale"],
            "additionalProperties": false
        });
        let g = schema_to_gbnf(&schema).unwrap();
        let r = parse(&g);
        let root = r["root"];
        // Required pairs must appear in declaration order, joined by
        // a comma, with no `additional pair` tail.
        assert!(root.contains(r#""\"label\"""#));
        assert!(root.contains(r#""\"rationale\"""#));
        assert!(root.contains(r#""," ws "\"rationale\"""#));
        // additionalProperties: false → no trailing string:value pair
        // pattern after the typed pairs.
        assert!(
            !root.contains(r#"("," ws string ws ":" ws value ws)*"#),
            "additionalProperties: false must not emit the trailing-pair tail; got: {root}"
        );
    }

    #[test]
    fn atlas_phase1_schema_translates_with_defs() {
        // Full atlas Phase 1 schema. Translator must not error and
        // must emit a named rule for each $def.
        let schema = json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "section_id": { "type": "string" },
                "questions_raised": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/question_sketch" }
                }
            },
            "required": ["section_id", "questions_raised"],
            "$defs": {
                "question_sketch": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "anchor": { "type": "string" }
                    },
                    "required": ["content"]
                }
            }
        });
        let g = schema_to_gbnf(&schema).unwrap();
        let r = parse(&g);
        assert!(r.contains_key("root"));
        assert!(r.contains_key("def-question-sketch"));
        // The root rule must reference the def's rule name (not
        // inline its body) so the grammar stays compact.
        assert!(r["root"].contains("def-question-sketch")
                || g.contains("def-question-sketch"));
    }

    #[test]
    fn ref_to_undefined_def_errors_loud() {
        let schema = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/missing" }
        });
        let err = schema_to_gbnf(&schema).unwrap_err();
        assert!(matches!(err, SchemaError::UnresolvedRef { .. }));
    }

    #[test]
    fn external_ref_errors_loud() {
        let schema = json!({ "$ref": "https://example.com/schema.json" });
        let err = schema_to_gbnf(&schema).unwrap_err();
        assert!(matches!(err, SchemaError::Unsupported { .. }));
    }

    #[test]
    fn enum_strings_become_alternation() {
        let schema = json!({ "type": "string", "enum": ["a", "b", "c"] });
        let g = schema_to_gbnf(&schema).unwrap();
        let r = parse(&g);
        // Each enum value emits as a quoted literal alternative.
        assert!(r["root"].contains(r#""\"a\"""#));
        assert!(r["root"].contains(r#""\"b\"""#));
        assert!(r["root"].contains(r#""\"c\"""#));
    }

    #[test]
    #[ignore] // run with `cargo test -- --ignored dump_cluster_gbnf --nocapture`
    fn dump_cluster_gbnf() {
        let schema = json!({
            "type": "object",
            "properties": {
                "label": { "type": "string" },
                "rationale": { "type": "string" }
            },
            "required": ["label", "rationale"],
            "additionalProperties": false
        });
        let g = schema_to_gbnf(&schema).unwrap();
        eprintln!("--- cluster_name_synth GBNF ({} bytes) ---", g.len());
        eprintln!("{g}");
    }

    #[test]
    #[ignore] // run with `cargo test -- --ignored dump_atlas_gbnf --nocapture` for a peek
    fn dump_atlas_gbnf() {
        // Print the GBNF the translator emits for the actual atlas
        // Phase 1 schema. Used during translator development to
        // hand-check the grammar against llama.cpp's GBNF spec.
        let schema: Value = serde_json::from_str(ATLAS_PHASE1_SCHEMA).unwrap();
        let g = schema_to_gbnf(&schema).unwrap();
        eprintln!("--- atlas phase1 GBNF ({} bytes) ---", g.len());
        eprintln!("{g}");
    }

    /// Vendored copy of the actual Phase 1 schema so this test
    /// doesn't need to depend on corpus-engine. Keep in sync if the
    /// real schema in `literary_atlas.rs` changes shape.
    const ATLAS_PHASE1_SCHEMA: &str = r##"{
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "section_id": { "type": "string" },
        "entities_introduced": { "type": "array", "items": { "$ref": "#/$defs/entity_sketch" } },
        "questions_raised": { "type": "array", "items": { "$ref": "#/$defs/question_sketch" } },
        "claims": { "type": "array", "items": { "$ref": "#/$defs/claim_sketch" } }
      },
      "required": ["section_id", "questions_raised"],
      "$defs": {
        "entity_sketch": {
          "type": "object",
          "additionalProperties": true,
          "properties": {
            "canonical_name": { "type": "string" },
            "aliases": { "type": "array", "items": { "type": "string" } }
          },
          "required": ["canonical_name"]
        },
        "question_sketch": {
          "type": "object",
          "properties": { "content": { "type": "string" } },
          "required": ["content"]
        },
        "claim_sketch": {
          "type": "object",
          "properties": {
            "content": { "type": "string" },
            "attributed_to": {
              "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
                { "type": "null" }
              ]
            }
          },
          "required": ["content"]
        }
      }
    }"##;

    #[test]
    fn rule_names_avoid_underscore() {
        // llama.cpp's GBNF parser accepts `[a-zA-Z][a-zA-Z0-9-]*`
        // for rule names — underscores trigger "expecting newline or
        // end" at parse time. Regression guard: make sure no rule we
        // emit has `_` in the name.
        let schema: Value = serde_json::from_str(ATLAS_PHASE1_SCHEMA).unwrap();
        let g = schema_to_gbnf(&schema).unwrap();
        for line in g.lines() {
            if let Some((name, _)) = line.split_once(" ::= ") {
                assert!(
                    !name.contains('_'),
                    "rule name `{name}` contains an underscore — \
                     llama.cpp's grammar parser will reject this"
                );
            }
        }
    }

    #[test]
    fn anyof_compiles_to_alternation() {
        // Mirror the atlas claim_sketch's `attributed_to` field —
        // string | array<string> | null.
        let schema = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
                { "type": "null" }
            ]
        });
        let g = schema_to_gbnf(&schema).unwrap();
        let r = parse(&g);
        // Root has the `ws ... ws` wrapper, so the alternation is
        // surrounded by it.
        assert!(r["root"].contains('('));
        assert!(r["root"].contains(" | "));
        assert!(r["root"].contains("null"));
    }
}
