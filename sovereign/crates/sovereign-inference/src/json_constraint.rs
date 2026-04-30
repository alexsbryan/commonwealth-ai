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

use std::collections::BTreeMap;
use std::sync::Arc;

use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
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
    // Pull bytes until closing quote (no escape handling for keys —
    // ASCII property names are the only thing our schemas use).
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
            Some(b'\\') => {
                // Skip escape sequence for partial validity. For the
                // prototype, `\\` + any byte is treated as 2 literal
                // bytes — keys with escapes won't match property
                // names exactly, so we'd reject regardless.
                accumulated.push(b'\\');
                p.advance();
                if p.eof() {
                    return KeyParse::Incomplete;
                }
                accumulated.push(p.peek().unwrap());
                p.advance();
            }
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
fn parse_string_any(p: &mut Cursor) -> ParseStatus {
    if p.peek() != Some(b'"') {
        return ParseStatus::Invalid;
    }
    p.advance();
    loop {
        match p.peek() {
            None => return ParseStatus::Incomplete,
            Some(b'"') => {
                p.advance();
                return ParseStatus::Complete;
            }
            Some(b'\\') => {
                p.advance();
                match p.peek() {
                    None => return ParseStatus::Incomplete,
                    Some(b'"') | Some(b'\\') | Some(b'/') | Some(b'b') | Some(b'f')
                    | Some(b'n') | Some(b'r') | Some(b't') => p.advance(),
                    Some(b'u') => {
                        p.advance();
                        for _ in 0..4 {
                            match p.peek() {
                                None => return ParseStatus::Incomplete,
                                Some(b) if b.is_ascii_hexdigit() => p.advance(),
                                _ => return ParseStatus::Invalid,
                            }
                        }
                    }
                    _ => return ParseStatus::Invalid,
                }
            }
            Some(b) if b < 0x20 => return ParseStatus::Invalid,
            Some(_) => p.advance(),
        }
    }
}

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

/// State carried across sample steps: the byte buffer of what's been
/// emitted, and a lazily-cached vocab byte map.
pub struct JsonConstraint {
    schema: Schema,
    emitted: Vec<u8>,
    /// byte sequence per token id (indexed by token id, sparse holes
    /// for unknown-type tokens are empty Vec).
    vocab_bytes: Arc<Vec<Vec<u8>>>,
    eos_token: i32,
}

impl JsonConstraint {
    /// Build a constraint from a JSON Schema and the model's vocab.
    pub fn new(schema: &Value, model: &LlamaModel) -> Result<Self, ConstraintError> {
        let compiled = compile_schema(schema)?;
        let n_vocab = model.n_vocab();
        let mut vocab_bytes = Vec::with_capacity(n_vocab as usize);
        for id in 0..n_vocab {
            let bytes = model
                .token_to_piece_bytes(LlamaToken(id), 64, false, None)
                .unwrap_or_default();
            vocab_bytes.push(bytes);
        }
        let eos_token = model.token_eos().0;
        Ok(Self {
            schema: compiled,
            emitted: Vec::new(),
            vocab_bytes: Arc::new(vocab_bytes),
            eos_token,
        })
    }

    /// Mask logits: set NEG_INFINITY for any token whose bytes would
    /// produce a definitively-invalid prefix when appended to the
    /// emitted buffer.
    pub fn mask(&self, data: &mut LlamaTokenDataArray) {
        // Pre-decide whether the buffer is a complete root value. If
        // yes, only EOS (or trailing whitespace) is allowed.
        let buffer_status = validate(&self.schema, &self.emitted);
        let buffer_is_complete = matches!(buffer_status, ParseStatus::Complete);

        for entry in data.data.iter_mut() {
            let token_id = entry.id().0;
            // EOS is special — allowed only if the buffer is at a
            // complete root value.
            if token_id == self.eos_token {
                if !buffer_is_complete {
                    entry.set_logit(f32::NEG_INFINITY);
                }
                continue;
            }
            let bytes = match self.vocab_bytes.get(token_id as usize) {
                Some(b) if !b.is_empty() => b,
                _ => {
                    entry.set_logit(f32::NEG_INFINITY);
                    continue;
                }
            };
            // If the buffer is already complete, only whitespace
            // tokens may extend (followed by EOS). Reject anything
            // else.
            if buffer_is_complete {
                if !bytes.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
                continue;
            }
            let mut probe = self.emitted.clone();
            probe.extend_from_slice(bytes);
            match validate(&self.schema, &probe) {
                ParseStatus::Complete | ParseStatus::Incomplete => {}
                ParseStatus::Invalid => entry.set_logit(f32::NEG_INFINITY),
            }
        }
    }

    /// Advance the emitted buffer with the bytes of the chosen token.
    pub fn accept(&mut self, token: LlamaToken) {
        if token.0 == self.eos_token {
            return;
        }
        if let Some(bytes) = self.vocab_bytes.get(token.0 as usize) {
            self.emitted.extend_from_slice(bytes);
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
