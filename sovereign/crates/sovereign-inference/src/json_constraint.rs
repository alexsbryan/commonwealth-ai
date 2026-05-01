//! JSON Schema-driven logit masker — bypasses llama.cpp's grammar
//! sampler entirely.
//!
//! The native `LlamaSampler::grammar` path crashes the daemon process
//! (`GGML_ASSERT(!stacks.empty())` at `llama-grammar.cpp:940`)
//! reproducibly across both Vulkan and ROCm, multi-slot AND
//! single-slot. The standalone smoke works; the long-lived daemon
//! does not. See `memory/project_grammar_alpha_blocker.md`.
//!
//! This module reimplements the part of grammar enforcement we
//! actually need (constrain output to a JSON-Schema-conforming
//! document) entirely in Rust, with no call into
//! `llama.cpp/src/llama-grammar.cpp`. The approach: maintain a byte
//! buffer of what the model has emitted, and at each sample step,
//! for every candidate token, walk a partial-JSON-with-schema
//! validator over `buffer + token_bytes`. Tokens whose bytes would
//! produce a definitively-invalid prefix are masked
//! (`logit = -INF`). The remaining sampler chain (DRY, top_k, min_p,
//! temp, dist) picks from the surviving distribution.
//!
//! Supported schema subset matches `json_grammar.rs`:
//! object/array/string (any + enum)/integer/number/boolean/null,
//! `anyOf`, `oneOf`, `$ref` to `$defs`/`definitions`, `type` unions
//! like `["string", "null"]`. `additionalProperties: false` is
//! respected; `additionalProperties: true` allows any trailing
//! pairs.
//!
//! Not supported: `pattern`, `format`, length/value bounds,
//! `if/then/else`, `not`, `allOf`, external `$ref`. Returns
//! `ConstraintError::Unsupported` at compile time so callers fail
//! loudly rather than producing a too-permissive constraint.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use rayon::iter::{IntoParallelRefMutIterator, ParallelIterator};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConstraintError {
    #[error("schema must be a JSON object at root, got {kind}")]
    NotAnObject { kind: &'static str },
    #[error("unsupported schema feature `{feature}` at `{pointer}`")]
    Unsupported { feature: String, pointer: String },
    #[error("$ref `{reference}` does not resolve in this schema")]
    UnresolvedRef { reference: String },
    #[error("malformed schema at `{pointer}`: {detail}")]
    Malformed { pointer: String, detail: String },
}

/// Compiled schema. Kept simple — recursive enum, no interning.
#[derive(Debug, Clone)]
pub enum Schema {
    Object {
        /// Properties in declaration order — first `required_count`
        /// are required, the rest optional.
        properties: Vec<(String, Schema)>,
        required_count: usize,
        /// If true, allow arbitrary additional name:value pairs
        /// after the typed ones. If false, reject anything beyond
        /// the declared properties.
        additional: bool,
    },
    Array {
        items: Box<Schema>,
    },
    StringEnum(Vec<String>),
    StringAny,
    Integer,
    Number,
    Boolean,
    Null,
    AnyOf(Vec<Schema>),
}

/// Compile a JSON Schema into our internal representation.
pub fn compile_schema(schema: &Value) -> Result<Schema, ConstraintError> {
    let root_obj = schema.as_object().ok_or(ConstraintError::NotAnObject {
        kind: kind_of(schema),
    })?;
    let defs = collect_defs(root_obj).unwrap_or_default();
    let mut ctx = CompileCtx { defs };
    ctx.compile(schema, "")
}

struct CompileCtx {
    defs: BTreeMap<String, Value>,
}

impl CompileCtx {
    fn compile(&mut self, schema: &Value, pointer: &str) -> Result<Schema, ConstraintError> {
        let obj = schema.as_object().ok_or_else(|| ConstraintError::Malformed {
            pointer: pointer.into(),
            detail: format!("expected object, got {}", kind_of(schema)),
        })?;

        if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
            return self.resolve_ref(r);
        }

        for key in ["anyOf", "oneOf"] {
            if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                let alts: Vec<Schema> = arr
                    .iter()
                    .enumerate()
                    .map(|(i, sub)| self.compile(sub, &format!("{pointer}/{key}/{i}")))
                    .collect::<Result<_, _>>()?;
                return Ok(Schema::AnyOf(alts));
            }
        }

        // `type: ["string", "null"]` shorthand — expand to anyOf.
        if let Some(Value::Array(types)) = obj.get("type") {
            let alts: Vec<Schema> = types
                .iter()
                .map(|t| {
                    let mut clone = obj.clone();
                    clone.insert("type".into(), t.clone());
                    self.compile(&Value::Object(clone), pointer)
                })
                .collect::<Result<_, _>>()?;
            return Ok(Schema::AnyOf(alts));
        }

        let ty = obj
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConstraintError::Unsupported {
                feature: "schema without `type` (and no $ref/anyOf/oneOf)".into(),
                pointer: pointer.into(),
            })?;

        match ty {
            "object" => self.compile_object(obj, pointer),
            "array" => {
                let items = obj.get("items").ok_or_else(|| ConstraintError::Unsupported {
                    feature: "array without `items`".into(),
                    pointer: pointer.into(),
                })?;
                Ok(Schema::Array {
                    items: Box::new(self.compile(items, &format!("{pointer}/items"))?),
                })
            }
            "string" => {
                if let Some(en) = obj.get("enum").and_then(|v| v.as_array()) {
                    let opts: Vec<String> = en
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| ConstraintError::Malformed {
                                    pointer: format!("{pointer}/enum"),
                                    detail: "non-string enum value for string type".into(),
                                })
                                .map(|s| s.to_string())
                        })
                        .collect::<Result<_, _>>()?;
                    Ok(Schema::StringEnum(opts))
                } else {
                    Ok(Schema::StringAny)
                }
            }
            "integer" => Ok(Schema::Integer),
            "number" => Ok(Schema::Number),
            "boolean" => Ok(Schema::Boolean),
            "null" => Ok(Schema::Null),
            other => Err(ConstraintError::Unsupported {
                feature: format!("type `{other}`"),
                pointer: pointer.into(),
            }),
        }
    }

    fn compile_object(
        &mut self,
        obj: &serde_json::Map<String, Value>,
        pointer: &str,
    ) -> Result<Schema, ConstraintError> {
        let props = obj
            .get("properties")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let required: Vec<String> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        // additionalProperties: we DEFAULT TO FALSE for structured
        // output. JSON Schema spec defaults this to true, but the
        // callers wiring `response_format`/`structured_output`
        // overwhelmingly want strict — the whole point of opting into
        // structured output is to constrain, not to allow drift.
        // Explicit `additionalProperties: true` is still honored for
        // the rare schema that genuinely wants extensibility.
        let additional = obj
            .get("additionalProperties")
            .map(|v| matches!(v, Value::Bool(true)))
            .unwrap_or(false);

        // Required first (in `required` order), then optional in
        // declaration order. Matches what `json_grammar.rs` emits for
        // determinism.
        let mut properties: Vec<(String, Schema)> = Vec::new();
        for name in &required {
            let sub = props.get(name).ok_or_else(|| ConstraintError::Malformed {
                pointer: format!("{pointer}/required"),
                detail: format!("required property `{name}` not in `properties`"),
            })?;
            let s = self.compile(sub, &format!("{pointer}/properties/{name}"))?;
            properties.push((name.clone(), s));
        }
        let required_count = properties.len();
        for (name, sub) in &props {
            if required.contains(name) {
                continue;
            }
            let s = self.compile(sub, &format!("{pointer}/properties/{name}"))?;
            properties.push((name.clone(), s));
        }

        Ok(Schema::Object {
            properties,
            required_count,
            additional,
        })
    }

    fn resolve_ref(&mut self, r: &str) -> Result<Schema, ConstraintError> {
        let key = r
            .strip_prefix("#/$defs/")
            .or_else(|| r.strip_prefix("#/definitions/"))
            .ok_or_else(|| ConstraintError::Unsupported {
                feature: format!("non-local $ref `{r}`"),
                pointer: "$ref".into(),
            })?;
        let def = self
            .defs
            .get(key)
            .cloned()
            .ok_or_else(|| ConstraintError::UnresolvedRef {
                reference: r.into(),
            })?;
        self.compile(&def, &format!("/$defs/{key}"))
    }
}

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

// ─── Partial parser ────────────────────────────────────────────

/// Outcome of validating a byte prefix against a schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    /// Bytes are a complete, well-formed value satisfying the schema.
    /// EOS / trailing whitespace is now allowed.
    Complete,
    /// Bytes are a valid prefix but more is expected.
    Incomplete,
    /// Bytes are definitely invalid; no completion can fix this.
    Invalid,
}

/// Run the schema-aware partial parser over a byte buffer and return
/// whether the buffer is a valid prefix (or full match) of any
/// document conforming to `schema`.
///
/// Note: leading whitespace is NOT skipped. With `temperature=0` and
/// greedy sampling, allowing leading whitespace causes the model to
/// emit whitespace tokens forever when its first-byte logits favour
/// whitespace over the JSON open token (`{`/`[`/`"`/digit). Forcing
/// the first byte to be a value-starter avoids the loop. Trailing
/// whitespace after the root value IS allowed (it's the only thing
/// the model can emit in a valid completion before EOS).
pub fn validate(schema: &Schema, bytes: &[u8]) -> ParseStatus {
    let mut p = Cursor::new(bytes);
    let v = parse_value(&mut p, schema);
    if v == ParseStatus::Invalid {
        return ParseStatus::Invalid;
    }
    if v == ParseStatus::Incomplete {
        return ParseStatus::Incomplete;
    }
    // Value parsed to completion — only trailing whitespace allowed.
    skip_ws(&mut p);
    if p.eof() {
        ParseStatus::Complete
    } else {
        ParseStatus::Invalid
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn advance(&mut self) {
        self.pos += 1;
    }
    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }
}

fn skip_ws(p: &mut Cursor) {
    while let Some(b) = p.peek() {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            p.advance();
        } else {
            break;
        }
    }
}

fn parse_value(p: &mut Cursor, schema: &Schema) -> ParseStatus {
    if p.eof() {
        return ParseStatus::Incomplete;
    }
    match schema {
        Schema::Object {
            properties,
            required_count,
            additional,
        } => parse_object(p, properties, *required_count, *additional),
        Schema::Array { items } => parse_array(p, items),
        Schema::StringEnum(opts) => parse_string_enum(p, opts),
        Schema::StringAny => parse_string_any(p),
        Schema::Integer => parse_number(p, false),
        Schema::Number => parse_number(p, true),
        Schema::Boolean => parse_keyword_alt(p, &["true", "false"]),
        Schema::Null => parse_keyword(p, "null"),
        Schema::AnyOf(alts) => parse_anyof(p, alts),
    }
}

fn parse_object(
    p: &mut Cursor,
    properties: &[(String, Schema)],
    required_count: usize,
    additional: bool,
) -> ParseStatus {
    if p.peek() != Some(b'{') {
        return ParseStatus::Invalid;
    }
    p.advance();
    skip_ws(p);
    if p.eof() {
        return ParseStatus::Incomplete;
    }

    // Track which property indices we've already seen, in declaration
    // order. Required props must appear in declaration order; optional
    // props can be any subset of the remainder; additional pairs only
    // after typed ones, when `additional=true`.
    let mut next_idx = 0usize;
    let mut pairs_consumed = 0usize;

    loop {
        skip_ws(p);
        if p.eof() {
            return ParseStatus::Incomplete;
        }
        // What's allowed at this position?
        // - `}`: only if all required are satisfied.
        // - `,`: only if a pair was already consumed AND there's room
        //   for another (an unfilled property OR additional=true).
        // - `"` (start of next key): only if no pairs yet, OR after a
        //   comma was consumed below.
        let required_satisfied = next_idx >= required_count;
        let more_pairs_possible = next_idx < properties.len() || additional;

        match p.peek() {
            Some(b'}') => {
                if !required_satisfied {
                    return ParseStatus::Invalid;
                }
                p.advance();
                return ParseStatus::Complete;
            }
            Some(b',') => {
                if pairs_consumed == 0 || !more_pairs_possible {
                    return ParseStatus::Invalid;
                }
                p.advance();
                skip_ws(p);
                if p.eof() {
                    return ParseStatus::Incomplete;
                }
            }
            Some(b'"') if pairs_consumed == 0 => {
                // First pair — fall through to parse_object_key.
            }
            _ => return ParseStatus::Invalid,
        }
        // Parse the property name as a JSON string. We need to know
        // WHICH key it is to pick the right value schema, which means
        // capturing the key's bytes during the string parse.
        let key_status = parse_object_key(p, properties, required_count, next_idx, additional);
        let chosen_key = match key_status {
            KeyParse::Incomplete => return ParseStatus::Incomplete,
            KeyParse::Invalid => return ParseStatus::Invalid,
            KeyParse::Picked(k) => k,
        };
        skip_ws(p);
        if p.eof() {
            return ParseStatus::Incomplete;
        }
        if p.peek() != Some(b':') {
            return ParseStatus::Invalid;
        }
        p.advance();
        skip_ws(p);
        if p.eof() {
            return ParseStatus::Incomplete;
        }
        // Pick the value schema. If `Picked::Typed(idx)`, use that
        // property's schema and bump next_idx. If `Picked::Additional`,
        // accept any value (use a wildcard StringAny-like wildcard).
        let value_status = match chosen_key {
            ChosenKey::Typed(idx) => {
                let s = parse_value(p, &properties[idx].1);
                if s == ParseStatus::Complete {
                    next_idx = idx + 1;
                }
                s
            }
            ChosenKey::Additional => parse_value_any(p),
        };
        match value_status {
            ParseStatus::Incomplete => return ParseStatus::Incomplete,
            ParseStatus::Invalid => return ParseStatus::Invalid,
            ParseStatus::Complete => {}
        }
        pairs_consumed += 1;
    }
}

/// Result of parsing an object key — either we picked a typed property
/// by index, or we're in additional-pair territory.
#[derive(Debug, Clone, Copy)]
enum ChosenKey {
    Typed(usize),
    Additional,
}

#[derive(Debug)]
enum KeyParse {
    Incomplete,
    Invalid,
    Picked(ChosenKey),
}

/// Parse a JSON string that names an object property. We track which
/// of the remaining property names the bytes we've seen so far are a
/// valid prefix of. If exactly one matches by the closing `"`, that's
/// the picked property. If zero match and `additional=true` we fall
/// through to additional-pair handling. If zero match and
/// `additional=false`, the prefix is invalid.
fn parse_object_key(
    p: &mut Cursor,
    properties: &[(String, Schema)],
    required_count: usize,
    next_idx: usize,
    additional: bool,
) -> KeyParse {
    if p.peek() != Some(b'"') {
        return KeyParse::Invalid;
    }
    p.advance();
    let mut accumulated: Vec<u8> = Vec::new();
    // Property names in supported schemas are simple alphanumeric/
    // underscore identifiers. JSON allows escape sequences in keys
    // (`\"`, `\u00XX`, etc.), but no real schema uses them — and
    // permitting them is exploitable: under temp=0 + greedy a model
    // can fall into a degenerate `\u\u\u…` loop in the key (observed
    // 2026-04-30 on the judge's `present`/`evidence` schema, fact=
    // "poverty"). Each `\u` token kept the parse `Incomplete` while
    // the prefix check on the regular-byte branch never fired
    // (escapes bypassed it). Treating any backslash in a key as
    // Invalid breaks the loop and matches what real schemas use.
    loop {
        match p.peek() {
            None => return KeyParse::Incomplete,
            Some(b'"') => {
                p.advance();
                let key_str = match std::str::from_utf8(&accumulated) {
                    Ok(s) => s.to_string(),
                    Err(_) => return KeyParse::Invalid,
                };
                // Pick: exact match against any allowed property name.
                // Required props must come in declaration order — only
                // properties at index >= next_idx within
                // [0..required_count) are reachable; for optional
                // props any unconsumed index is fine.
                match match_property(properties, required_count, next_idx, &key_str) {
                    KeyMatch::Picked(idx) => return KeyParse::Picked(ChosenKey::Typed(idx)),
                    KeyMatch::Forbidden => return KeyParse::Invalid,
                    KeyMatch::NotDeclared => {
                        if additional {
                            return KeyParse::Picked(ChosenKey::Additional);
                        }
                        return KeyParse::Invalid;
                    }
                }
            }
            Some(b'\\') => return KeyParse::Invalid,
            Some(b) if b < 0x20 => return KeyParse::Invalid,
            Some(b) => {
                accumulated.push(b);
                p.advance();
                // Fast-fail: if accumulated bytes are not a prefix of
                // any reachable property name AND additional=false,
                // we're invalid even before the closing quote.
                if !additional
                    && !any_property_starts_with(
                        properties,
                        required_count,
                        next_idx,
                        &accumulated,
                    )
                {
                    return KeyParse::Invalid;
                }
            }
        }
    }
}

/// Three-way result so callers can distinguish "declared but
/// invalid here" from "not declared at all" — the former should
/// reject regardless of `additionalProperties`, the latter falls
/// through to the additional-pair handler.
enum KeyMatch {
    Picked(usize),
    Forbidden,
    NotDeclared,
}

fn match_property(
    properties: &[(String, Schema)],
    required_count: usize,
    next_idx: usize,
    key: &str,
) -> KeyMatch {
    if next_idx < required_count {
        if properties[next_idx].0 == key {
            return KeyMatch::Picked(next_idx);
        }
        // Key in the declared list but in an invalid position
        // (already-consumed, or skipping a required prop) → forbidden.
        if properties.iter().any(|(name, _)| name == key) {
            return KeyMatch::Forbidden;
        }
        return KeyMatch::NotDeclared;
    }
    // Optional block: any unconsumed optional property is valid.
    for (i, (name, _)) in properties.iter().enumerate().skip(required_count) {
        if name == key {
            return KeyMatch::Picked(i);
        }
    }
    // A required property name appearing again in optional position
    // is a duplicate → forbidden.
    if properties
        .iter()
        .take(required_count)
        .any(|(name, _)| name == key)
    {
        return KeyMatch::Forbidden;
    }
    KeyMatch::NotDeclared
}

fn any_property_starts_with(
    properties: &[(String, Schema)],
    required_count: usize,
    next_idx: usize,
    prefix: &[u8],
) -> bool {
    if next_idx < required_count {
        return properties[next_idx].0.as_bytes().starts_with(prefix);
    }
    properties
        .iter()
        .skip(required_count)
        .any(|(name, _)| name.as_bytes().starts_with(prefix))
}

fn parse_array(p: &mut Cursor, items: &Schema) -> ParseStatus {
    if p.peek() != Some(b'[') {
        return ParseStatus::Invalid;
    }
    p.advance();
    skip_ws(p);
    if p.eof() {
        return ParseStatus::Incomplete;
    }
    if p.peek() == Some(b']') {
        p.advance();
        return ParseStatus::Complete;
    }
    let mut first = true;
    loop {
        if !first {
            skip_ws(p);
            if p.eof() {
                return ParseStatus::Incomplete;
            }
            match p.peek() {
                Some(b',') => p.advance(),
                Some(b']') => {
                    p.advance();
                    return ParseStatus::Complete;
                }
                _ => return ParseStatus::Invalid,
            }
        }
        skip_ws(p);
        if p.eof() {
            return ParseStatus::Incomplete;
        }
        match parse_value(p, items) {
            ParseStatus::Incomplete => return ParseStatus::Incomplete,
            ParseStatus::Invalid => return ParseStatus::Invalid,
            ParseStatus::Complete => {}
        }
        first = false;
    }
}

fn parse_string_enum(p: &mut Cursor, opts: &[String]) -> ParseStatus {
    if p.peek() != Some(b'"') {
        return ParseStatus::Invalid;
    }
    p.advance();
    let mut acc: Vec<u8> = Vec::new();
    loop {
        match p.peek() {
            None => {
                if opts.iter().any(|o| o.as_bytes().starts_with(&acc)) {
                    return ParseStatus::Incomplete;
                }
                return ParseStatus::Invalid;
            }
            Some(b'"') => {
                let s = match std::str::from_utf8(&acc) {
                    Ok(s) => s,
                    Err(_) => return ParseStatus::Invalid,
                };
                if opts.iter().any(|o| o == s) {
                    p.advance();
                    return ParseStatus::Complete;
                }
                return ParseStatus::Invalid;
            }
            Some(b'\\') => {
                // Enum strings shouldn't need escapes for our use cases.
                return ParseStatus::Invalid;
            }
            Some(b) => {
                acc.push(b);
                p.advance();
                if !opts.iter().any(|o| o.as_bytes().starts_with(&acc)) {
                    return ParseStatus::Invalid;
                }
            }
        }
    }
}

/// Parse any JSON string. Supports basic escape sequences
/// (`\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, `\uXXXX`).
///
/// **Anti-loop guard.** With temperature=0 + greedy sampling, a model
/// can fall into a degenerate mode where it emits `\"` over and over
/// without ever consuming a non-escape byte (observed on the judge's
/// `evidence` field for `contested_quantum_determinism`'s "free will"
/// fact: output was `{"\"\"\"\"…` repeating). Each `\"` is a valid
/// 2-byte escape and the parser would never reject it on its own.
/// Capping consecutive escapes at `MAX_CONSECUTIVE_STRING_ESCAPES`
/// (3) breaks the loop without forbidding legitimate uses (escaped
/// runs that long are vanishingly rare in real JSON content).
fn parse_string_any(p: &mut Cursor) -> ParseStatus {
    if p.peek() != Some(b'"') {
        return ParseStatus::Invalid;
    }
    p.advance();
    let mut consecutive_escapes = 0usize;
    loop {
        match p.peek() {
            None => return ParseStatus::Incomplete,
            Some(b'"') => {
                p.advance();
                return ParseStatus::Complete;
            }
            Some(b'\\') => {
                if consecutive_escapes >= MAX_CONSECUTIVE_STRING_ESCAPES {
                    return ParseStatus::Invalid;
                }
                p.advance();
                match p.peek() {
                    None => return ParseStatus::Incomplete,
                    Some(b'"') | Some(b'\\') | Some(b'/') | Some(b'b') | Some(b'f')
                    | Some(b'n') | Some(b'r') | Some(b't') => {
                        p.advance();
                        consecutive_escapes += 1;
                    }
                    Some(b'u') => {
                        p.advance();
                        for _ in 0..4 {
                            match p.peek() {
                                None => return ParseStatus::Incomplete,
                                Some(b) if b.is_ascii_hexdigit() => p.advance(),
                                _ => return ParseStatus::Invalid,
                            }
                        }
                        consecutive_escapes += 1;
                    }
                    _ => return ParseStatus::Invalid,
                }
            }
            Some(b) if b < 0x20 => return ParseStatus::Invalid,
            Some(_) => {
                p.advance();
                consecutive_escapes = 0;
            }
        }
    }
}

const MAX_CONSECUTIVE_STRING_ESCAPES: usize = 3;

/// Parse a JSON number. `allow_fraction` distinguishes Number from
/// Integer.
fn parse_number(p: &mut Cursor, allow_fraction: bool) -> ParseStatus {
    let start = p.pos;
    // Optional sign
    if p.peek() == Some(b'-') {
        p.advance();
        if p.eof() {
            return ParseStatus::Incomplete;
        }
    }
    // Integer part: 0 | [1-9][0-9]*
    match p.peek() {
        None => return ParseStatus::Incomplete,
        Some(b'0') => p.advance(),
        Some(b) if b.is_ascii_digit() && b != b'0' => {
            p.advance();
            while let Some(b) = p.peek() {
                if b.is_ascii_digit() {
                    p.advance();
                } else {
                    break;
                }
            }
        }
        _ => return ParseStatus::Invalid,
    }
    if p.pos == start {
        return ParseStatus::Invalid;
    }
    // Fraction
    if allow_fraction && p.peek() == Some(b'.') {
        p.advance();
        let frac_start = p.pos;
        while let Some(b) = p.peek() {
            if b.is_ascii_digit() {
                p.advance();
            } else {
                break;
            }
        }
        if p.pos == frac_start {
            return if p.eof() {
                ParseStatus::Incomplete
            } else {
                ParseStatus::Invalid
            };
        }
    }
    // Exponent
    if allow_fraction && matches!(p.peek(), Some(b'e') | Some(b'E')) {
        p.advance();
        if matches!(p.peek(), Some(b'+') | Some(b'-')) {
            p.advance();
        }
        let exp_start = p.pos;
        while let Some(b) = p.peek() {
            if b.is_ascii_digit() {
                p.advance();
            } else {
                break;
            }
        }
        if p.pos == exp_start {
            return if p.eof() {
                ParseStatus::Incomplete
            } else {
                ParseStatus::Invalid
            };
        }
    }
    ParseStatus::Complete
}

fn parse_keyword(p: &mut Cursor, kw: &str) -> ParseStatus {
    let bytes = kw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match p.peek() {
            None => return ParseStatus::Incomplete,
            Some(b) if b == bytes[i] => {
                p.advance();
                i += 1;
            }
            _ => return ParseStatus::Invalid,
        }
    }
    ParseStatus::Complete
}

fn parse_keyword_alt(p: &mut Cursor, kws: &[&str]) -> ParseStatus {
    // Pick the keyword by first character.
    let first = match p.peek() {
        None => return ParseStatus::Incomplete,
        Some(b) => b,
    };
    for kw in kws {
        if kw.as_bytes()[0] == first {
            return parse_keyword(p, kw);
        }
    }
    ParseStatus::Invalid
}

fn parse_anyof(p: &mut Cursor, alts: &[Schema]) -> ParseStatus {
    // Try each alternative and pick the most-completed result. If any
    // complete, return Complete. Else if any incomplete, return
    // Incomplete. Else Invalid. Note: we restore cursor between tries.
    let saved = p.pos;
    let mut best = ParseStatus::Invalid;
    let mut best_pos = saved;
    for alt in alts {
        p.pos = saved;
        let status = parse_value(p, alt);
        match (status, best) {
            (ParseStatus::Complete, _) => {
                // Complete wins immediately; pick the longest-consuming one.
                if status == ParseStatus::Complete && p.pos > best_pos {
                    best = status;
                    best_pos = p.pos;
                }
            }
            (ParseStatus::Incomplete, ParseStatus::Invalid) => {
                best = status;
                best_pos = p.pos;
            }
            _ => {}
        }
    }
    p.pos = best_pos;
    best
}

/// Parse any JSON value (used for additionalProperties: true). Walks
/// through a generic value recursively.
fn parse_value_any(p: &mut Cursor) -> ParseStatus {
    match p.peek() {
        None => ParseStatus::Incomplete,
        Some(b'{') => parse_object(p, &[], 0, true),
        Some(b'[') => parse_array(p, &Schema::AnyOf(any_value_alts())),
        Some(b'"') => parse_string_any(p),
        Some(b't') | Some(b'f') => parse_keyword_alt(p, &["true", "false"]),
        Some(b'n') => parse_keyword(p, "null"),
        Some(b'-') | Some(b'0'..=b'9') => parse_number(p, true),
        _ => ParseStatus::Invalid,
    }
}

fn any_value_alts() -> Vec<Schema> {
    vec![
        Schema::Object {
            properties: vec![],
            required_count: 0,
            additional: true,
        },
        Schema::StringAny,
        Schema::Number,
        Schema::Boolean,
        Schema::Null,
    ]
}

// ─── JsonConstraint ───────────────────────────────────────────

/// Per-process cache of `vocab_bytes` keyed by `LlamaModel` pointer.
///
/// Building `vocab_bytes` calls `token_to_piece_bytes` once per token
/// id (~262K calls for Gemma-3-E4B); doing that on every request was
/// the dominant `JsonConstraint::new` cost. Slots stay loaded for
/// the daemon's lifetime, so a pointer key is stable enough — and
/// when a slot drops we just leave the entry; the next constraint
/// against the same model address may rebuild (cheap miss). For
/// long-lived daemons this is effectively a one-time cost per loaded
/// model.
fn vocab_cache() -> &'static Mutex<HashMap<usize, Arc<Vec<Vec<u8>>>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Vec<Vec<u8>>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn vocab_bytes_for(model: &LlamaModel) -> Arc<Vec<Vec<u8>>> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = vocab_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    let n_vocab = model.n_vocab();
    let mut vocab_bytes = Vec::with_capacity(n_vocab as usize);
    for id in 0..n_vocab {
        // CRITICAL: must mirror the args the streaming generation loop
        // uses for `token_to_piece` in `embedded.rs`. That call uses
        // `special=true` (renders user-defined / control tokens as
        // their text), so any divergence here means the constraint
        // tracks a different `emitted` buffer than what the response
        // body actually contains. Observed 2026-04-30 with gemma-4-E4B
        // Phase 1: response had `entities_introduced` followed by a
        // literal backtick (0x60) where the closing quote should be,
        // because the chosen token's `special=false` cache view (empty
        // for a user-defined-attr token) made the mask believe the
        // candidate was a no-op while the response decoder rendered it
        // as text.
        //
        // Buffer size: the streaming loop uses `token_to_piece` which
        // retries on InsufficientBufferSpace; we replicate that
        // explicitly. 32 is a generous starting size for the typical
        // BPE token (≤16 bytes); the retry handles the long tail.
        let bytes = match model.token_to_piece_bytes(LlamaToken(id), 32, true, None) {
            Ok(b) => b,
            Err(llama_cpp_2::TokenToStringError::InsufficientBufferSpace(neg)) => {
                let needed = (-neg).try_into().unwrap_or(1024_usize).max(32);
                model
                    .token_to_piece_bytes(LlamaToken(id), needed, true, None)
                    .unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        vocab_bytes.push(bytes);
    }
    let arc = Arc::new(vocab_bytes);
    let mut guard = vocab_cache().lock().unwrap_or_else(|e| e.into_inner());
    // Race: another caller may have populated between our miss and
    // re-lock. `entry().or_insert_with` would re-walk the vocab —
    // just check once more and reuse.
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, arc.clone());
    arc
}

/// State carried across sample steps: the byte buffer of what's been
/// emitted, and a lazily-cached vocab byte map.
pub struct JsonConstraint {
    schema: Schema,
    emitted: Vec<u8>,
    /// byte sequence per token id (indexed by token id, sparse holes
    /// for unknown-type tokens are empty Vec). Shared across requests
    /// against the same model via `vocab_cache`.
    vocab_bytes: Arc<Vec<Vec<u8>>>,
    eos_token: i32,
}

impl JsonConstraint {
    /// Build a constraint from a JSON Schema and the model's vocab.
    pub fn new(schema: &Value, model: &LlamaModel) -> Result<Self, ConstraintError> {
        let compiled = compile_schema(schema)?;
        let vocab_bytes = vocab_bytes_for(model);
        let eos_token = model.token_eos().0;
        Ok(Self {
            schema: compiled,
            emitted: Vec::new(),
            vocab_bytes,
            eos_token,
        })
    }

    /// Mask logits: set NEG_INFINITY for any token whose bytes would
    /// produce a definitively-invalid prefix when appended to the
    /// emitted buffer.
    ///
    /// Parallelised across rayon's global pool — for Gemma-3-E4B
    /// (n_vocab ≈ 262K) the per-candidate validator is the dominant
    /// cost of a generation step. `for_each_init` gives each rayon
    /// worker its own scratch buffer pre-loaded with `emitted`, so we
    /// pay one `Vec::clone` per worker per call instead of one per
    /// candidate. Net effect on a 16-core box: ~16× fewer wall-time
    /// seconds per token.
    pub fn mask(&self, data: &mut LlamaTokenDataArray) {
        // Pre-decide whether the buffer is a complete root value. If
        // yes, only EOS (or trailing whitespace) is allowed.
        let buffer_status = validate(&self.schema, &self.emitted);
        let buffer_is_complete = matches!(buffer_status, ParseStatus::Complete);
        let emitted_len = self.emitted.len();

        let schema = &self.schema;
        let emitted = &self.emitted;
        let vocab_bytes = &*self.vocab_bytes;
        let eos_token = self.eos_token;

        data.data.par_iter_mut().for_each_init(
            // Each rayon worker reuses one scratch buffer across all
            // its candidates. Pre-load it with `emitted` once; per
            // candidate we truncate to `emitted_len` and append the
            // token bytes.
            || {
                let mut scratch = Vec::with_capacity(emitted_len + 64);
                scratch.extend_from_slice(emitted);
                scratch
            },
            |scratch, entry| {
                let token_id = entry.id().0;
                if token_id == eos_token {
                    if !buffer_is_complete {
                        entry.set_logit(f32::NEG_INFINITY);
                    }
                    return;
                }
                let bytes = match vocab_bytes.get(token_id as usize) {
                    Some(b) if !b.is_empty() => b,
                    _ => {
                        entry.set_logit(f32::NEG_INFINITY);
                        return;
                    }
                };
                if buffer_is_complete {
                    if !bytes.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
                        entry.set_logit(f32::NEG_INFINITY);
                    }
                    return;
                }
                scratch.truncate(emitted_len);
                scratch.extend_from_slice(bytes);
                if let ParseStatus::Invalid = validate(schema, scratch) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            },
        );
    }

    /// Advance the emitted buffer with the bytes of the chosen token.
    ///
    /// Diagnostic invariant: the prefix `emitted + chosen_bytes` should
    /// be Complete or Incomplete after every accept — the masker is
    /// supposed to have rejected anything that produces Invalid. When
    /// the post-accept validate trips Invalid, log a `warn` with the
    /// token id and a head excerpt so operators can correlate against
    /// the produced response. Set `SOVEREIGN_CONSTRAINT_TRACE=1` to
    /// upgrade to `info`-level traces of every accept (verbose; only
    /// for triage).
    pub fn accept(&mut self, token: LlamaToken) {
        if token.0 == self.eos_token {
            return;
        }
        let Some(bytes) = self.vocab_bytes.get(token.0 as usize).cloned() else {
            tracing::warn!(
                token_id = token.0,
                "JsonConstraint::accept: chosen token is out of vocab range — emitted buffer will desync from response"
            );
            return;
        };
        self.emitted.extend_from_slice(&bytes);
        // Per-token dump (env-gated). Set
        // `SOVEREIGN_CONSTRAINT_DUMP=/path/to/file` to record one line
        // per accepted token: token_id, bytes (escaped), running
        // emitted_len. After a run, the file gives ground truth on
        // what the constraint thinks was emitted; diff against the
        // response body to spot cache-vs-decoder divergence.
        if let Ok(path) = std::env::var("SOVEREIGN_CONSTRAINT_DUMP") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let escaped: String = bytes
                    .iter()
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                let _ = writeln!(
                    f,
                    "tok={} len={} bytes={}",
                    token.0,
                    self.emitted.len(),
                    escaped
                );
            }
        }
        // Post-accept validation. If we see Invalid here, the masker
        // failed to reject this token and we now have a corrupted
        // prefix — every subsequent mask call validates against
        // garbage, which is exactly the gemma Phase-1 failure mode
        // (`"entities_introduced` 0x60). Surface it loudly.
        let status = validate(&self.schema, &self.emitted);
        if matches!(status, ParseStatus::Invalid) {
            let head: String = self
                .emitted
                .iter()
                .rev()
                .take(40)
                .copied()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|b| if (0x20..0x7f).contains(&b) { b as char } else { '·' })
                .collect();
            let token_bytes_repr: String = bytes
                .iter()
                .map(|b| if (0x20..0x7f).contains(b) { (*b as char).to_string() } else { format!("\\x{b:02x}") })
                .collect();
            tracing::warn!(
                token_id = token.0,
                token_bytes = %token_bytes_repr,
                emitted_tail = %head,
                emitted_len = self.emitted.len(),
                "JsonConstraint::accept: post-accept buffer is Invalid — masker did not catch this token"
            );
        } else if std::env::var("SOVEREIGN_CONSTRAINT_TRACE").as_deref() == Ok("1") {
            tracing::info!(
                token_id = token.0,
                token_bytes_len = bytes.len(),
                emitted_len = self.emitted.len(),
                ?status,
                "JsonConstraint::accept"
            );
        }
    }

    /// True once the emitted bytes form a complete schema-conforming
    /// document (only trailing whitespace / EOS would follow).
    pub fn is_root_complete(&self) -> bool {
        matches!(validate(&self.schema, &self.emitted), ParseStatus::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_simple_object_complete() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "intent": {"type": "string", "enum": ["A", "B"]},
                "confidence": {"type": "number"}
            },
            "required": ["intent", "confidence"]
        }))
        .unwrap();
        let bytes = br#"{"intent":"A","confidence":0.9}"#;
        assert_eq!(validate(&s, bytes), ParseStatus::Complete);
    }

    #[test]
    fn validate_partial_string_enum_incomplete() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {"intent": {"type": "string", "enum": ["LOOKUP", "REASONING"]}},
            "required": ["intent"]
        }))
        .unwrap();
        // "LO" is a valid prefix of LOOKUP but the value isn't done.
        let bytes = br#"{"intent":"LO"#;
        assert_eq!(validate(&s, bytes), ParseStatus::Incomplete);
    }

    #[test]
    fn validate_partial_string_enum_invalid() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {"intent": {"type": "string", "enum": ["LOOKUP", "REASONING"]}},
            "required": ["intent"]
        }))
        .unwrap();
        // "Z" is not a valid prefix of any enum option.
        let bytes = br#"{"intent":"Z"#;
        assert_eq!(validate(&s, bytes), ParseStatus::Invalid);
    }

    #[test]
    fn validate_required_property_order() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "a": {"type": "boolean"},
                "b": {"type": "boolean"}
            },
            "required": ["a", "b"]
        }))
        .unwrap();
        // Wrong order — `b` first, `a` later — should be invalid.
        let bytes = br#"{"b":true"#;
        assert_eq!(validate(&s, bytes), ParseStatus::Invalid);
    }

    #[test]
    fn validate_number_integer_phases() {
        let s = compile_schema(&json!({"type": "number"})).unwrap();
        assert_eq!(validate(&s, b"-1.5e10"), ParseStatus::Complete);
        assert_eq!(validate(&s, b"-"), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b"1."), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b"1.a"), ParseStatus::Invalid);
        let i = compile_schema(&json!({"type": "integer"})).unwrap();
        // Integer schema rejects "1.5" — the trailing ".5" isn't whitespace.
        assert_eq!(validate(&i, b"1.5"), ParseStatus::Invalid);
    }

    /// Reproduce the gemma Phase-1 failure pattern observed in
    /// 2026-04-30 wiki-test sec_00001 run: the model emitted
    /// `"entities_introduced` followed by literal byte 0x60 (backtick)
    /// instead of the closing quote 0x22. The validator MUST mark
    /// any bytes after `entities_introduced` that aren't a continuation
    /// of an allowed property name as Invalid, so the mask can flip
    /// the corresponding token's logit to NEG_INFINITY.
    ///
    /// This test exercises validate() directly — no rayon, no
    /// LlamaModel — so it isolates the constraint logic from any
    /// concurrency or model-state concerns.
    #[test]
    fn validate_rejects_backtick_in_property_name() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "section_id": {"type": "string"},
                "entities_introduced": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"], "additionalProperties": false}
                }
            },
            "required": ["section_id", "entities_introduced"],
            "additionalProperties": false
        }))
        .unwrap();
        // The exact byte sequence from sec_00001's failed run:
        // `{"section_id":"x","entities_introduced` then 0x60 (backtick).
        let mut bytes: Vec<u8> = br#"{"section_id":"x","entities_introduced"#.to_vec();
        bytes.push(0x60); // ` instead of "
        assert_eq!(
            validate(&s, &bytes),
            ParseStatus::Invalid,
            "backtick after a complete property name should be Invalid"
        );
    }

    /// `mask()`-shaped reproduction: simulate what the masker does
    /// per candidate. The candidate token's bytes — starting from a
    /// state where the buffer ends mid-quote — must produce Invalid
    /// when concatenated as a single multi-byte slice. This is the
    /// granularity at which `JsonConstraint::mask` validates: the
    /// whole token piece appended at once, not byte-by-byte.
    #[test]
    fn mask_shaped_validate_catches_mid_token_backtick_corruption() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "section_id": {"type": "string"},
                "entities_introduced": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"], "additionalProperties": false}
                }
            },
            "required": ["section_id", "entities_introduced"],
            "additionalProperties": false
        }))
        .unwrap();
        // emitted state: just past the comma, just-opened the next key.
        let emitted: &[u8] = br#"{"section_id":"x","#;
        // candidate token's bytes (simulating gemma fusing closing
        // bracket + structural noise into a single piece). The mid-
        // token byte 0x60 must make the whole appended slice Invalid.
        let mut candidate: Vec<u8> = b"\"entities_introduced".to_vec();
        candidate.push(0x60);
        candidate.extend_from_slice(b": [ {");
        let mut probe = emitted.to_vec();
        probe.extend_from_slice(&candidate);
        assert_eq!(
            validate(&s, &probe),
            ParseStatus::Invalid,
            "appending a token containing a mid-key backtick to a valid \
             prefix must be Invalid so the masker rejects it"
        );
    }

    /// Companion to the above: the second observed failure mode is an
    /// empty / whitespace-only property key. After `{ "`, the model
    /// emitted whitespace bytes (spaces + tabs) before the closing
    /// quote. With `additionalProperties: false`, no property in the
    /// schema starts with whitespace, so this prefix must be Invalid
    /// at the first whitespace byte.
    #[test]
    fn validate_rejects_whitespace_only_property_key() {
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "section_id": {"type": "string"},
                "entities_introduced": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"], "additionalProperties": false}
                }
            },
            "required": ["section_id", "entities_introduced"],
            "additionalProperties": false
        }))
        .unwrap();
        // Inside the entities_introduced array, the model opened an
        // object then a quote then a space — `{ "` then 0x20.
        let bytes = br#"{"section_id":"x","entities_introduced":[{ " "#;
        assert_eq!(
            validate(&s, bytes),
            ParseStatus::Invalid,
            "whitespace-only property key should be Invalid"
        );
    }

    #[test]
    fn validate_anyof_string_or_null() {
        let s = compile_schema(&json!({"type": ["string", "null"]})).unwrap();
        assert_eq!(validate(&s, br#""hello""#), ParseStatus::Complete);
        assert_eq!(validate(&s, b"null"), ParseStatus::Complete);
        assert_eq!(validate(&s, b"nul"), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b"\"hi"), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b"42"), ParseStatus::Invalid);
    }
}
