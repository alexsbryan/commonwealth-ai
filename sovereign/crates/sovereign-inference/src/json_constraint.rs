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

/// Compiled schema. Children wrapped in `Arc` so the incremental
/// validator's per-byte stack frames can hold cheap references
/// without cloning entire subtrees.
#[derive(Debug, Clone)]
pub enum Schema {
    Object {
        /// Properties in declaration order — first `required_count`
        /// are required, the rest optional.
        properties: Arc<Vec<(String, Schema)>>,
        required_count: usize,
        /// If true, allow arbitrary additional name:value pairs
        /// after the typed ones. If false, reject anything beyond
        /// the declared properties.
        additional: bool,
    },
    Array {
        items: Arc<Schema>,
        /// Inclusive upper bound on number of elements. `None` means
        /// unbounded. When set, the parser tracks the running count
        /// and rejects `,` after the cap is reached, leaving `]` as
        /// the only valid continuation — the mask sampler then
        /// forces the model to close the array.
        max_items: Option<usize>,
    },
    StringEnum(Arc<Vec<String>>),
    /// Free-form string. `max_length` is the JSON-Schema `maxLength`
    /// (in unicode code points, not bytes — counted on UTF-8 start
    /// bytes during validation). When set and the running count
    /// reaches it, the parser/validator treats every non-`"` next
    /// byte as Invalid — the mask sampler then forces the close-
    /// quote, just like the array-cap path uses `]`.
    ///
    /// Without this cap, an unbounded string field is the prime
    /// runaway path on schema-constrained generation: nothing in
    /// the mask makes the model ever close the quote, so a single
    /// `description` or `content` field can swallow the entire
    /// token budget. (Concrete repro: Phase 1 extraction on a
    /// 78-word Wikipedia lead burned 11337 tokens before deadline.)
    StringAny {
        max_length: Option<usize>,
    },
    Integer,
    Number,
    Boolean,
    Null,
    AnyOf(Arc<Vec<Schema>>),
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
                return Ok(Schema::AnyOf(Arc::new(alts)));
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
            return Ok(Schema::AnyOf(Arc::new(alts)));
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
                let max_items = obj
                    .get("maxItems")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                Ok(Schema::Array {
                    items: Arc::new(self.compile(items, &format!("{pointer}/items"))?),
                    max_items,
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
                    Ok(Schema::StringEnum(Arc::new(opts)))
                } else {
                    let max_length = obj
                        .get("maxLength")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);
                    Ok(Schema::StringAny { max_length })
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
            properties: Arc::new(properties),
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
        Schema::Array { items, max_items } => parse_array(p, items, *max_items),
        Schema::StringEnum(opts) => parse_string_enum(p, opts),
        Schema::StringAny { max_length } => parse_string_any(p, *max_length),
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
        // property's schema and ratchet next_idx forward (never
        // backward — picking an EARLIER optional after a later one
        // would otherwise revert next_idx and re-open already-
        // consumed indices for re-emission, which is the duplicate
        // loop the rejection logic in `match_property` exists to
        // prevent). If `Picked::Additional`, accept any value.
        let value_status = match chosen_key {
            ChosenKey::Typed(idx) => {
                let s = parse_value(p, &properties[idx].1);
                if s == ParseStatus::Complete {
                    next_idx = next_idx.max(idx + 1);
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
    // Optional block: only properties at index >= next_idx are still
    // available. Iterating from `required_count` (the original bug)
    // re-matched already-consumed optionals — under temp=0 + greedy
    // the model would emit the same `description` field over and
    // over, blowing the token budget on a runaway loop. Iterating
    // from `next_idx` skips consumed indices; duplicates fall through
    // to the Forbidden check below.
    let optional_start = next_idx.max(required_count);
    for (i, (name, _)) in properties
        .iter()
        .enumerate()
        .skip(optional_start)
    {
        if name == key {
            return KeyMatch::Picked(i);
        }
    }
    // A declared property name reappearing at an index we've already
    // passed (required OR optional) is a duplicate → forbidden.
    if properties
        .iter()
        .take(optional_start)
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
    // Match `match_property`: only properties at index >= next_idx
    // are still reachable. Skipping by `required_count` (the bug
    // partner of `match_property`) would let the prefix check pass
    // on already-consumed property names.
    let optional_start = next_idx.max(required_count);
    properties
        .iter()
        .skip(optional_start)
        .any(|(name, _)| name.as_bytes().starts_with(prefix))
}

fn parse_array(p: &mut Cursor, items: &Schema, max_items: Option<usize>) -> ParseStatus {
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
    // If max_items is Some(0) and the array isn't already closed, the
    // input is invalid — there's no way to satisfy "0 items" with the
    // bracket already consumed and content following.
    if matches!(max_items, Some(0)) {
        return ParseStatus::Invalid;
    }
    let mut count = 0usize;
    let mut first = true;
    loop {
        if !first {
            skip_ws(p);
            if p.eof() {
                return ParseStatus::Incomplete;
            }
            match p.peek() {
                Some(b',') => {
                    // Cap reached: reject `,`, only `]` is valid.
                    // The mask sampler will see this as "comma is
                    // not a valid next byte" and force the close.
                    if let Some(max) = max_items {
                        if count >= max {
                            return ParseStatus::Invalid;
                        }
                    }
                    p.advance();
                }
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
        count += 1;
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
fn parse_string_any(p: &mut Cursor, max_length: Option<usize>) -> ParseStatus {
    if p.peek() != Some(b'"') {
        return ParseStatus::Invalid;
    }
    p.advance();
    let mut consecutive_escapes = 0usize;
    let mut char_count = 0usize;
    loop {
        let at_cap = matches!(max_length, Some(m) if char_count >= m);
        match p.peek() {
            None => return ParseStatus::Incomplete,
            Some(b'"') => {
                p.advance();
                return ParseStatus::Complete;
            }
            Some(b'\\') => {
                if at_cap {
                    return ParseStatus::Invalid;
                }
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
                        char_count = char_count.saturating_add(1);
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
                        char_count = char_count.saturating_add(1);
                    }
                    _ => return ParseStatus::Invalid,
                }
            }
            Some(b) if b < 0x20 => return ParseStatus::Invalid,
            Some(b) => {
                let is_continuation = (b & 0xC0) == 0x80;
                if !is_continuation && at_cap {
                    return ParseStatus::Invalid;
                }
                p.advance();
                if !is_continuation {
                    char_count = char_count.saturating_add(1);
                }
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
        Some(b'[') => parse_array(p, &Schema::AnyOf(Arc::new(any_value_alts())), None),
        Some(b'"') => parse_string_any(p, None),
        Some(b't') | Some(b'f') => parse_keyword_alt(p, &["true", "false"]),
        Some(b'n') => parse_keyword(p, "null"),
        Some(b'-') | Some(b'0'..=b'9') => parse_number(p, true),
        _ => ParseStatus::Invalid,
    }
}

fn any_value_alts() -> Vec<Schema> {
    vec![
        Schema::Object {
            properties: Arc::new(vec![]),
            required_count: 0,
            additional: true,
        },
        Schema::StringAny { max_length: None },
        Schema::Number,
        Schema::Boolean,
        Schema::Null,
    ]
}

// ─── Incremental validator (explicit-stack state machine) ──────
//
// The recursive parser above re-walks `emitted` from byte 0 each call.
// Per-token mask cost is O(V × N) where V≈32k vocab and N=emitted bytes
// — quadratic in N when a chapter generates long output. This module
// rebuilds the same validation as a byte-driven stack machine: each
// `Frame` records progress through one schema element; `advance(byte)`
// updates the top frame in O(stack_depth). The state is cheaply
// cloneable, so `mask()` forks one snapshot per candidate token and
// runs only the candidate's own bytes (~5) through advance — total
// per-token cost becomes O(V × stack_depth × bytes_per_token), i.e.
// constant in N.

const MAX_CONSEC_ESCAPES_INCR: u8 = MAX_CONSECUTIVE_STRING_ESCAPES as u8;

#[derive(Clone, Debug)]
enum Frame {
    /// Awaiting the first non-whitespace byte of a value matching this
    /// schema. On the first non-ws byte we replace ourselves with the
    /// concrete frame for the chosen value type.
    AwaitValue(Schema),

    /// Inside an object after the opening `{`. Sub-state tracks what
    /// byte we expect next; key parsing is inlined (no separate frame).
    Object {
        properties: Arc<Vec<(String, Schema)>>,
        required_count: usize,
        additional: bool,
        next_idx: usize,
        pairs_consumed: usize,
        sub: ObjectSub,
    },

    /// Inside an array after the opening `[`.
    Array {
        items: Arc<Schema>,
        max_items: Option<usize>,
        count: usize,
        sub: ArraySub,
    },

    /// Inside an enum-string. We track accumulated bytes and verify
    /// they remain a prefix of at least one enum option.
    StringEnum {
        opts: Arc<Vec<String>>,
        accumulated: Vec<u8>,
    },

    /// Inside a free-form JSON string (escapes allowed).
    ///
    /// `char_count` is the running count of code points emitted into
    /// the string body so far (counted on UTF-8 start bytes; each
    /// `\X` and `\uXXXX` escape counts as 1). When `max_length` is
    /// `Some(n)` and `char_count >= n`, the only valid next byte is
    /// `"` — every other body byte is rejected as Invalid, which
    /// the mask sampler reads as "force the close-quote." Without
    /// the cap, an unbounded string is the prime token-budget
    /// runaway path under schema-constrained generation.
    StringAny {
        consecutive_escapes: u8,
        sub: StringSub,
        char_count: usize,
        max_length: Option<usize>,
    },

    /// Inside a JSON number. `allow_fraction` distinguishes Number from
    /// Integer (matches `parse_number`).
    Number {
        allow_fraction: bool,
        sub: NumberSub,
    },

    /// Matching a fixed keyword (`true`/`false`/`null`). `pos` indexes
    /// into `word` for the next expected byte.
    Keyword {
        word: &'static [u8],
        pos: u8,
    },

    /// Awaiting the disambiguating byte of an anyOf — once we see a
    /// non-ws byte we narrow to the matching alternative and replace
    /// ourselves with its `AwaitValue`.
    AnyOf(Arc<Vec<Schema>>),

    /// The root value has completed; only trailing whitespace is
    /// permitted before EOS.
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ObjectSub {
    /// Just consumed `{`; expecting `"` (key open) or `}` (close) or
    /// whitespace.
    AwaitFirstKeyOrClose,
    /// After `,`; expecting `"` (next key open) or whitespace.
    AwaitNextKey,
    /// Inside the key string between the opening and closing quotes.
    /// Bytes go into `accumulated`. No escapes allowed in keys (matches
    /// the recursive parser's anti-loop guard).
    InKey { accumulated: Vec<u8> },
    /// After the closing `"` of the key; expecting `:` or whitespace.
    /// `chosen` records which property the key matched.
    AwaitColon { chosen: ChosenKeyKind },
    /// After `:`; the next non-ws byte should start the value, so we
    /// push a child `AwaitValue` frame (the byte is NOT consumed by us;
    /// the child handles it).
    AfterColon { chosen: ChosenKeyKind },
    /// Child value frame is on top; we stay parked here until it pops.
    /// On the next byte we see (i.e. the byte that triggered the child
    /// to pop, or anything after if the child completed cleanly), we
    /// transition to AwaitCommaOrClose without consuming.
    InValue { chosen: ChosenKeyKind },
    /// After a value: expecting `,` or `}` or whitespace.
    AwaitCommaOrClose,
}

/// Mirrors `ChosenKey` from the recursive parser but stored inside
/// `ObjectSub` so the Object frame can transition cleanly without a
/// separate child frame.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ChosenKeyKind {
    /// Declared property at this index (use `properties[i].1` as the
    /// value schema).
    Typed(usize),
    /// `additionalProperties: true` wildcard — value can be any JSON.
    Additional,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArraySub {
    /// Just consumed `[`; expecting value or `]` or whitespace.
    AwaitFirstItemOrClose,
    /// After an item: expecting `,` or `]` or whitespace.
    AwaitCommaOrClose,
    /// After `,`: expecting value or whitespace.
    AwaitNextItem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StringSub {
    /// Inside the string body; reading literal bytes.
    InBody,
    /// Just saw `\`; expecting one of the escape chars.
    AfterBackslash,
    /// Inside `\uXXXX`; counting hex digits remaining (0..4).
    InUnicode { remaining: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NumberSub {
    /// At the start; sign byte has not been consumed yet.
    Start,
    /// Just consumed `-`; expecting first digit.
    AfterSign,
    /// Already emitted leading `0`; further digit means malformed
    /// (JSON forbids `01`). `.`/`e`/`E` continue, otherwise complete.
    AtLeadingZero,
    /// In integer digits ([1-9][0-9]*).
    InInt,
    /// Just consumed `.`; need at least one fractional digit.
    AfterDot,
    /// In fractional digits.
    InFrac,
    /// Just consumed `e`/`E`; need optional sign + ≥1 exp digit.
    AfterExpChar,
    /// Just consumed `+`/`-` after `e`/`E`; need ≥1 exp digit.
    AfterExpSign,
    /// In exponent digits.
    InExp,
}

#[derive(Clone, Debug)]
struct ValidatorState {
    stack: Vec<Frame>,
    /// Latched once the root value completes — subsequent advance()
    /// calls only accept whitespace (Complete) or reject (Invalid).
    root_complete: bool,
}

/// One-byte step result. A frame's `step` returns a `StepResult` that
/// the driver loop interprets to update the stack and decide whether
/// to consume the byte.
#[derive(Debug)]
enum StepResult {
    /// Byte handled; frame stays.
    Consumed,
    /// Frame done; byte was NOT consumed (re-process at parent).
    Pop,
    /// Frame done; byte WAS consumed.
    PopConsumed,
    /// Replace the top frame with this one; byte was NOT consumed.
    Replace(Frame),
    /// Replace the top frame with this one; byte WAS consumed.
    ReplaceConsumed(Frame),
    /// Push a child frame; byte was NOT consumed (child will see it).
    Push(Frame),
    /// Push a child frame; byte WAS consumed by the parent
    /// (e.g. `{` consumed before pushing the Object frame's body).
    PushConsumed(Frame),
    /// Definitively invalid.
    Invalid,
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

impl ValidatorState {
    fn new(schema: Schema) -> Self {
        Self {
            stack: vec![Frame::AwaitValue(schema)],
            root_complete: false,
        }
    }

    fn current_status(&self) -> ParseStatus {
        if self.root_complete {
            ParseStatus::Complete
        } else if self.stack.is_empty() {
            // Stack drained without root_complete latched (shouldn't
            // happen via advance), treat as Complete.
            ParseStatus::Complete
        } else {
            ParseStatus::Incomplete
        }
    }

    /// Feed one byte. Returns the resulting status.
    fn advance(&mut self, byte: u8) -> ParseStatus {
        // Trailing-whitespace handling once the root is closed.
        if self.root_complete {
            return if is_ws(byte) {
                ParseStatus::Complete
            } else {
                ParseStatus::Invalid
            };
        }

        let mut byte_consumed = false;
        // Loop until the byte is consumed (or we decide the parse is
        // done / invalid). Each iteration acts on the top frame.
        // Bound by stack_depth × small constant — `Pop` chains are
        // short because the schema tree is shallow.
        let mut safety = 0usize;
        loop {
            safety += 1;
            if safety > 256 {
                // Schema-loop guard — should be unreachable under our
                // supported schema subset; bail rather than spin.
                return ParseStatus::Invalid;
            }
            if byte_consumed {
                break;
            }
            let Some(top) = self.stack.last_mut() else {
                // Stack drained while still expecting bytes → only
                // trailing whitespace is permitted (matches the
                // recursive parser's post-root behaviour).
                self.root_complete = true;
                return if is_ws(byte) {
                    ParseStatus::Complete
                } else {
                    ParseStatus::Invalid
                };
            };
            let result = top.step(byte);
            match result {
                StepResult::Consumed => {
                    byte_consumed = true;
                }
                StepResult::Pop => {
                    self.stack.pop();
                    // Loop again — byte still in flight.
                }
                StepResult::PopConsumed => {
                    self.stack.pop();
                    byte_consumed = true;
                }
                StepResult::Replace(new) => {
                    *self.stack.last_mut().unwrap() = new;
                    // Loop again — byte still in flight.
                }
                StepResult::ReplaceConsumed(new) => {
                    *self.stack.last_mut().unwrap() = new;
                    byte_consumed = true;
                }
                StepResult::Push(child) => {
                    self.stack.push(child);
                    // Loop again — child will see this byte.
                }
                StepResult::PushConsumed(child) => {
                    self.stack.push(child);
                    byte_consumed = true;
                }
                StepResult::Invalid => return ParseStatus::Invalid,
            }
        }

        // After consuming, if the stack drained, the root value is
        // complete — only trailing ws is permitted from here on.
        if self.stack.is_empty() {
            self.root_complete = true;
            return ParseStatus::Complete;
        }
        ParseStatus::Incomplete
    }

    /// Feed a slice of bytes. Returns the status with EOF-finalization
    /// applied — a raw number like `0` reads Complete even though no
    /// terminator byte arrived (matches the recursive parser's
    /// `parse_number` behaviour at end-of-buffer). Short-circuits on
    /// Invalid.
    fn advance_bytes(&mut self, bytes: &[u8]) -> ParseStatus {
        for &b in bytes {
            let s = self.advance(b);
            if matches!(s, ParseStatus::Invalid) {
                return s;
            }
        }
        self.eof_status()
    }

    /// Return Complete if the current state would be valid at EOF
    /// (root completed, OR stack is entirely Numbers in
    /// at-least-one-digit-consumed states). Pure inspection — does not
    /// mutate state, so subsequent advance() calls still work
    /// correctly if more bytes do arrive.
    fn eof_status(&self) -> ParseStatus {
        if self.root_complete || self.stack.is_empty() {
            return ParseStatus::Complete;
        }
        if self.stack.iter().all(frame_can_eof_complete) {
            ParseStatus::Complete
        } else {
            ParseStatus::Incomplete
        }
    }
}

fn frame_can_eof_complete(frame: &Frame) -> bool {
    match frame {
        Frame::Number { sub, .. } => matches!(
            sub,
            NumberSub::AtLeadingZero
                | NumberSub::InInt
                | NumberSub::InFrac
                | NumberSub::InExp
        ),
        // All structural frames (object/array/string/keyword/anyOf/
        // await-value) require an explicit terminator byte.
        _ => false,
    }
}

impl Frame {
    fn step(&mut self, byte: u8) -> StepResult {
        match self {
            Frame::AwaitValue(schema) => Self::step_await_value(schema, byte),
            Frame::Object {
                properties,
                required_count,
                additional,
                next_idx,
                pairs_consumed,
                sub,
            } => Self::step_object(
                properties,
                *required_count,
                *additional,
                next_idx,
                pairs_consumed,
                sub,
                byte,
            ),
            Frame::Array {
                items,
                max_items,
                count,
                sub,
            } => Self::step_array(items, *max_items, count, sub, byte),
            Frame::StringEnum { opts, accumulated } => {
                Self::step_string_enum(opts, accumulated, byte)
            }
            Frame::StringAny {
                consecutive_escapes,
                sub,
                char_count,
                max_length,
            } => Self::step_string_any(consecutive_escapes, sub, char_count, *max_length, byte),
            Frame::Number {
                allow_fraction,
                sub,
            } => Self::step_number(*allow_fraction, sub, byte),
            Frame::Keyword { word, pos } => Self::step_keyword(word, pos, byte),
            Frame::AnyOf(alts) => Self::step_anyof(alts, byte),
            Frame::Finished => StepResult::Invalid, // unreachable in driver
        }
    }

    fn step_await_value(schema: &Schema, byte: u8) -> StepResult {
        if is_ws(byte) {
            return StepResult::Consumed;
        }
        // Dispatch on the value's first byte. The byte is consumed
        // when it's the structural opener (`{`/`[`) since the new
        // frame represents the post-opener state.
        match schema {
            Schema::Object {
                properties,
                required_count,
                additional,
            } => {
                if byte != b'{' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::Object {
                    properties: Arc::clone(properties),
                    required_count: *required_count,
                    additional: *additional,
                    next_idx: 0,
                    pairs_consumed: 0,
                    sub: ObjectSub::AwaitFirstKeyOrClose,
                })
            }
            Schema::Array { items, max_items } => {
                if byte != b'[' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::Array {
                    items: Arc::clone(items),
                    max_items: *max_items,
                    count: 0,
                    sub: ArraySub::AwaitFirstItemOrClose,
                })
            }
            Schema::StringEnum(opts) => {
                if byte != b'"' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::StringEnum {
                    opts: Arc::clone(opts),
                    accumulated: Vec::new(),
                })
            }
            Schema::StringAny { max_length } => {
                if byte != b'"' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::StringAny {
                    consecutive_escapes: 0,
                    sub: StringSub::InBody,
                    char_count: 0,
                    max_length: *max_length,
                })
            }
            Schema::Integer => {
                // The Number frame consumes the first byte itself
                // (sign or digit) — pass through unconsumed.
                StepResult::Replace(Frame::Number {
                    allow_fraction: false,
                    sub: NumberSub::Start,
                })
            }
            Schema::Number => StepResult::Replace(Frame::Number {
                allow_fraction: true,
                sub: NumberSub::Start,
            }),
            Schema::Boolean => {
                let word: &'static [u8] = match byte {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => return StepResult::Invalid,
                };
                StepResult::ReplaceConsumed(Frame::Keyword { word, pos: 1 })
            }
            Schema::Null => {
                if byte != b'n' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::Keyword {
                    word: b"null",
                    pos: 1,
                })
            }
            Schema::AnyOf(alts) => StepResult::Replace(Frame::AnyOf(Arc::clone(alts))),
        }
    }

    fn step_object(
        properties: &Arc<Vec<(String, Schema)>>,
        required_count: usize,
        additional: bool,
        next_idx: &mut usize,
        pairs_consumed: &mut usize,
        sub: &mut ObjectSub,
        byte: u8,
    ) -> StepResult {
        match sub {
            ObjectSub::AwaitFirstKeyOrClose => {
                if is_ws(byte) {
                    return StepResult::Consumed;
                }
                match byte {
                    b'}' => {
                        if *next_idx < required_count {
                            StepResult::Invalid
                        } else {
                            StepResult::PopConsumed
                        }
                    }
                    b'"' => {
                        *sub = ObjectSub::InKey {
                            accumulated: Vec::new(),
                        };
                        StepResult::Consumed
                    }
                    _ => StepResult::Invalid,
                }
            }
            ObjectSub::AwaitNextKey => {
                if is_ws(byte) {
                    return StepResult::Consumed;
                }
                if byte == b'"' {
                    *sub = ObjectSub::InKey {
                        accumulated: Vec::new(),
                    };
                    StepResult::Consumed
                } else {
                    StepResult::Invalid
                }
            }
            ObjectSub::InKey { accumulated } => {
                match byte {
                    b'"' => {
                        // Resolve the key.
                        let key_str = match std::str::from_utf8(accumulated) {
                            Ok(s) => s.to_string(),
                            Err(_) => return StepResult::Invalid,
                        };
                        let chosen = match match_property(
                            properties,
                            required_count,
                            *next_idx,
                            &key_str,
                        ) {
                            KeyMatch::Picked(idx) => ChosenKeyKind::Typed(idx),
                            KeyMatch::Forbidden => return StepResult::Invalid,
                            KeyMatch::NotDeclared => {
                                if additional {
                                    ChosenKeyKind::Additional
                                } else {
                                    return StepResult::Invalid;
                                }
                            }
                        };
                        *sub = ObjectSub::AwaitColon { chosen };
                        StepResult::Consumed
                    }
                    b'\\' => StepResult::Invalid,
                    b if b < 0x20 => StepResult::Invalid,
                    _ => {
                        accumulated.push(byte);
                        // Fast-fail prefix check (skip when additional
                        // is allowed — any string is then valid).
                        if !additional
                            && !any_property_starts_with(
                                properties,
                                required_count,
                                *next_idx,
                                accumulated,
                            )
                        {
                            return StepResult::Invalid;
                        }
                        StepResult::Consumed
                    }
                }
            }
            ObjectSub::AwaitColon { chosen } => {
                if is_ws(byte) {
                    return StepResult::Consumed;
                }
                if byte == b':' {
                    *sub = ObjectSub::AfterColon {
                        chosen: chosen.clone(),
                    };
                    StepResult::Consumed
                } else {
                    StepResult::Invalid
                }
            }
            ObjectSub::AfterColon { chosen } => {
                // Push the value frame; the byte is forwarded to the
                // child (which will skip its own leading whitespace).
                let chosen = chosen.clone();
                let value_schema = match &chosen {
                    ChosenKeyKind::Typed(i) => properties[*i].1.clone(),
                    ChosenKeyKind::Additional => Schema::AnyOf(Arc::new(any_value_alts())),
                };
                *sub = ObjectSub::InValue { chosen };
                StepResult::Push(Frame::AwaitValue(value_schema))
            }
            ObjectSub::InValue { chosen } => {
                // Reached only after the child value frame has popped.
                // Bump bookkeeping and re-process the byte at the new
                // sub-state. `next_idx` ratchets forward only — see
                // the `parse_object` recursive parser for why.
                if let ChosenKeyKind::Typed(i) = chosen {
                    *next_idx = (*next_idx).max(*i + 1);
                }
                *pairs_consumed = pairs_consumed.saturating_add(1);
                *sub = ObjectSub::AwaitCommaOrClose;
                // Don't consume — the byte is `,` or `}` (or ws), which
                // AwaitCommaOrClose handles.
                // We're already the top frame; loop again.
                // Re-enter via Replace? We're modifying ourselves
                // in-place, so just signal "loop, don't consume":
                // emulate via a no-op step that returns the same byte.
                Self::step_object(
                    properties,
                    required_count,
                    additional,
                    next_idx,
                    pairs_consumed,
                    sub,
                    byte,
                )
            }
            ObjectSub::AwaitCommaOrClose => {
                if is_ws(byte) {
                    return StepResult::Consumed;
                }
                match byte {
                    b'}' => {
                        if *next_idx < required_count {
                            StepResult::Invalid
                        } else {
                            StepResult::PopConsumed
                        }
                    }
                    b',' => {
                        *sub = ObjectSub::AwaitNextKey;
                        StepResult::Consumed
                    }
                    _ => StepResult::Invalid,
                }
            }
        }
    }

    fn step_array(
        items: &Arc<Schema>,
        max_items: Option<usize>,
        count: &mut usize,
        sub: &mut ArraySub,
        byte: u8,
    ) -> StepResult {
        if is_ws(byte) {
            return StepResult::Consumed;
        }
        match sub {
            ArraySub::AwaitFirstItemOrClose => {
                if byte == b']' {
                    return StepResult::PopConsumed;
                }
                if matches!(max_items, Some(0)) {
                    return StepResult::Invalid;
                }
                // We've committed to a value: bump count now so the
                // post-pop comma/close check sees the right tally.
                *count += 1;
                *sub = ArraySub::AwaitCommaOrClose;
                StepResult::Push(Frame::AwaitValue((**items).clone()))
            }
            ArraySub::AwaitCommaOrClose => match byte {
                b']' => StepResult::PopConsumed,
                b',' => {
                    if let Some(max) = max_items {
                        if *count >= max {
                            return StepResult::Invalid;
                        }
                    }
                    *sub = ArraySub::AwaitNextItem;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            ArraySub::AwaitNextItem => {
                *count += 1;
                *sub = ArraySub::AwaitCommaOrClose;
                StepResult::Push(Frame::AwaitValue((**items).clone()))
            }
        }
    }

    fn step_string_enum(
        opts: &Arc<Vec<String>>,
        accumulated: &mut Vec<u8>,
        byte: u8,
    ) -> StepResult {
        match byte {
            b'"' => {
                let s = match std::str::from_utf8(accumulated) {
                    Ok(s) => s,
                    Err(_) => return StepResult::Invalid,
                };
                if opts.iter().any(|o| o == s) {
                    StepResult::PopConsumed
                } else {
                    StepResult::Invalid
                }
            }
            b'\\' => StepResult::Invalid,
            _ => {
                accumulated.push(byte);
                if !opts.iter().any(|o| o.as_bytes().starts_with(accumulated)) {
                    return StepResult::Invalid;
                }
                StepResult::Consumed
            }
        }
    }

    fn step_string_any(
        consecutive_escapes: &mut u8,
        sub: &mut StringSub,
        char_count: &mut usize,
        max_length: Option<usize>,
        byte: u8,
    ) -> StepResult {
        // Hard cap: once we've emitted `max_length` code points, the
        // only valid next byte at a code-point boundary is `"`. UTF-8
        // continuation bytes (10xxxxxx) finish the in-progress code
        // point and pass even at-cap; new code-point starts and
        // escape openers are rejected. Mask sampler then forces the
        // close-quote (same pattern the array-cap uses for `]` once
        // `maxItems` is hit).
        let at_cap = matches!(max_length, Some(m) if *char_count >= m);
        match sub {
            StringSub::InBody => match byte {
                b'"' => StepResult::PopConsumed,
                b'\\' => {
                    if at_cap {
                        return StepResult::Invalid;
                    }
                    if *consecutive_escapes >= MAX_CONSEC_ESCAPES_INCR {
                        return StepResult::Invalid;
                    }
                    *sub = StringSub::AfterBackslash;
                    StepResult::Consumed
                }
                b if b < 0x20 => StepResult::Invalid,
                b => {
                    let is_continuation = (b & 0xC0) == 0x80;
                    if !is_continuation && at_cap {
                        // A new code-point start would overrun the cap.
                        return StepResult::Invalid;
                    }
                    if !is_continuation {
                        *char_count = char_count.saturating_add(1);
                    }
                    *consecutive_escapes = 0;
                    StepResult::Consumed
                }
            },
            StringSub::AfterBackslash => match byte {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                    *sub = StringSub::InBody;
                    *consecutive_escapes = consecutive_escapes.saturating_add(1);
                    *char_count = char_count.saturating_add(1);
                    StepResult::Consumed
                }
                b'u' => {
                    *sub = StringSub::InUnicode { remaining: 4 };
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            StringSub::InUnicode { remaining } => {
                if !byte.is_ascii_hexdigit() {
                    return StepResult::Invalid;
                }
                *remaining -= 1;
                if *remaining == 0 {
                    *sub = StringSub::InBody;
                    *consecutive_escapes = consecutive_escapes.saturating_add(1);
                    *char_count = char_count.saturating_add(1);
                }
                StepResult::Consumed
            }
        }
    }

    fn step_number(allow_fraction: bool, sub: &mut NumberSub, byte: u8) -> StepResult {
        match sub {
            NumberSub::Start => match byte {
                b'-' => {
                    *sub = NumberSub::AfterSign;
                    StepResult::Consumed
                }
                b'0' => {
                    *sub = NumberSub::AtLeadingZero;
                    StepResult::Consumed
                }
                b'1'..=b'9' => {
                    *sub = NumberSub::InInt;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            NumberSub::AfterSign => match byte {
                b'0' => {
                    *sub = NumberSub::AtLeadingZero;
                    StepResult::Consumed
                }
                b'1'..=b'9' => {
                    *sub = NumberSub::InInt;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            NumberSub::AtLeadingZero => match byte {
                b'.' if allow_fraction => {
                    *sub = NumberSub::AfterDot;
                    StepResult::Consumed
                }
                b'e' | b'E' if allow_fraction => {
                    *sub = NumberSub::AfterExpChar;
                    StepResult::Consumed
                }
                _ => StepResult::Pop,
            },
            NumberSub::InInt => match byte {
                b'0'..=b'9' => StepResult::Consumed,
                b'.' if allow_fraction => {
                    *sub = NumberSub::AfterDot;
                    StepResult::Consumed
                }
                b'e' | b'E' if allow_fraction => {
                    *sub = NumberSub::AfterExpChar;
                    StepResult::Consumed
                }
                _ => StepResult::Pop,
            },
            NumberSub::AfterDot => match byte {
                b'0'..=b'9' => {
                    *sub = NumberSub::InFrac;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            NumberSub::InFrac => match byte {
                b'0'..=b'9' => StepResult::Consumed,
                b'e' | b'E' => {
                    *sub = NumberSub::AfterExpChar;
                    StepResult::Consumed
                }
                _ => StepResult::Pop,
            },
            NumberSub::AfterExpChar => match byte {
                b'+' | b'-' => {
                    *sub = NumberSub::AfterExpSign;
                    StepResult::Consumed
                }
                b'0'..=b'9' => {
                    *sub = NumberSub::InExp;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            NumberSub::AfterExpSign => match byte {
                b'0'..=b'9' => {
                    *sub = NumberSub::InExp;
                    StepResult::Consumed
                }
                _ => StepResult::Invalid,
            },
            NumberSub::InExp => match byte {
                b'0'..=b'9' => StepResult::Consumed,
                _ => StepResult::Pop,
            },
        }
    }

    fn step_keyword(word: &'static [u8], pos: &mut u8, byte: u8) -> StepResult {
        let i = *pos as usize;
        if i >= word.len() {
            // Already complete — should not happen via driver.
            return StepResult::Pop;
        }
        if word[i] != byte {
            return StepResult::Invalid;
        }
        *pos += 1;
        if (*pos as usize) == word.len() {
            StepResult::PopConsumed
        } else {
            StepResult::Consumed
        }
    }

    fn step_anyof(alts: &Arc<Vec<Schema>>, byte: u8) -> StepResult {
        // Whitespace before disambiguation is allowed.
        if is_ws(byte) {
            return StepResult::Consumed;
        }
        // Pick the alternative that matches the leading byte. JSON's
        // first-byte determinism means at most one alt can start with
        // any given non-ws byte (`{`/`[`/`"`/digit/`-`/`t`/`f`/`n`).
        for alt in alts.iter() {
            if matches_first_byte(alt, byte) {
                return StepResult::Replace(Frame::AwaitValue(alt.clone()));
            }
        }
        StepResult::Invalid
    }
}

fn matches_first_byte(schema: &Schema, b: u8) -> bool {
    match schema {
        Schema::Object { .. } => b == b'{',
        Schema::Array { .. } => b == b'[',
        Schema::StringEnum(_) | Schema::StringAny { .. } => b == b'"',
        Schema::Integer | Schema::Number => matches!(b, b'-' | b'0'..=b'9'),
        Schema::Boolean => matches!(b, b't' | b'f'),
        Schema::Null => b == b'n',
        Schema::AnyOf(alts) => alts.iter().any(|a| matches_first_byte(a, b)),
    }
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

/// State carried across sample steps: the cached parser state at the
/// end of `emitted`, the emitted buffer itself (kept for diagnostics
/// + post-accept validation), and a lazily-cached vocab byte map.
pub struct JsonConstraint {
    schema: Schema,
    emitted: Vec<u8>,
    /// Incremental parser state at `emitted_len`. Cloned per candidate
    /// inside `mask()` so each candidate just runs its own ~5 token
    /// bytes through `advance` instead of re-parsing the whole buffer.
    /// Updated in lock-step with `emitted` by `accept()`.
    state: ValidatorState,
    /// byte sequence per token id (indexed by token id, sparse holes
    /// for unknown-type tokens are empty Vec). Shared across requests
    /// against the same model via `vocab_cache`.
    vocab_bytes: Arc<Vec<Vec<u8>>>,
    eos_token: i32,
    /// Latched once `accept()` sees the cumulative buffer go Invalid by
    /// a fresh `validate()` re-parse. The masker's incremental
    /// `advance_bytes` and the recursive `validate()` can disagree on
    /// edge cases; without a latch the model would otherwise tail-loop
    /// for thousands of tokens against an unrecoverable prefix until
    /// the inference deadline fires. With it, the very next `mask()`
    /// call clamps every non-EOS token to NEG_INFINITY so the slot
    /// returns whatever truncated bytes it has and Phase-3 falls into
    /// `parse_drift` (a fast-failure outcome — seconds, not 5 min).
    emitted_invalid: bool,
}

impl JsonConstraint {
    /// Build a constraint from a JSON Schema and the model's vocab.
    pub fn new(schema: &Value, model: &LlamaModel) -> Result<Self, ConstraintError> {
        let compiled = compile_schema(schema)?;
        let vocab_bytes = vocab_bytes_for(model);
        let eos_token = model.token_eos().0;
        let state = ValidatorState::new(compiled.clone());
        Ok(Self {
            schema: compiled,
            emitted: Vec::new(),
            state,
            vocab_bytes,
            eos_token,
            emitted_invalid: false,
        })
    }

    /// Mask logits: set NEG_INFINITY for any token whose bytes would
    /// produce a definitively-invalid prefix when appended to the
    /// emitted buffer.
    ///
    /// Uses the incremental parser: each candidate clones the cached
    /// `ValidatorState` (Vec<Frame> shallow copy + Arc clones, not a
    /// deep traversal) and runs only the candidate's own bytes through
    /// `advance` — per-candidate cost is O(bytes_per_token × stack_depth)
    /// rather than O(emitted_len × stack_depth) for the recursive
    /// re-parse it replaced.
    ///
    /// Parallelised across rayon's global pool — for Gemma-3-E4B
    /// (n_vocab ≈ 262K) the per-candidate validator is the dominant
    /// cost of a generation step.
    pub fn mask(&self, data: &mut LlamaTokenDataArray) {
        let buffer_is_complete = matches!(self.state.eof_status(), ParseStatus::Complete);
        let vocab_bytes = &*self.vocab_bytes;
        let eos_token = self.eos_token;
        let state = &self.state;
        // Buffer has already drifted Invalid (validate() vs incremental
        // advance_bytes disagreed in a prior accept()). Mute every
        // non-EOS token so the slot exits the generation loop on the
        // next sample step instead of running to deadline.
        if self.emitted_invalid {
            data.data.par_iter_mut().for_each(|entry| {
                if entry.id().0 != eos_token {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            });
            return;
        }

        data.data.par_iter_mut().for_each_init(
            // Each rayon worker reuses one scratch state across all
            // its candidates. Pre-clone the cached state once; per
            // candidate we re-clone (cheap — bounded stack depth) so
            // sibling candidates don't see each other's mutations.
            || state.clone(),
            |worker_state, entry| {
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
                // Fork worker_state for this candidate (the mutation
                // would otherwise leak into sibling candidates).
                let mut candidate_state = worker_state.clone();
                if matches!(
                    candidate_state.advance_bytes(bytes),
                    ParseStatus::Invalid
                ) {
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
        // Advance the cached parser state by the new bytes. mask()
        // forks this state per candidate — keeping it in lock-step
        // with `emitted` is the whole point of the incremental
        // validator.
        let _ = self.state.advance_bytes(&bytes);
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
            // Latch so the next mask() call only allows EOS. Every
            // extension of an Invalid prefix is itself Invalid by
            // definition, but the incremental walker disagreed once
            // and would keep disagreeing until the deadline fires.
            self.emitted_invalid = true;
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
                "JsonConstraint::accept: post-accept buffer is Invalid — masker did not catch this token; latching to EOS-only on next mask()"
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
        matches!(self.state.eof_status(), ParseStatus::Complete)
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

    #[test]
    fn validate_array_max_items_caps_count() {
        // maxItems=3: arrays with up to 3 elements are Complete; a 4th
        // element after the cap is Invalid (the parser must see `]`).
        let s = compile_schema(&json!({
            "type": "array",
            "items": {"type": "string"},
            "maxItems": 3
        }))
        .unwrap();
        assert_eq!(validate(&s, br#"[]"#), ParseStatus::Complete);
        assert_eq!(validate(&s, br#"["a"]"#), ParseStatus::Complete);
        assert_eq!(validate(&s, br#"["a","b","c"]"#), ParseStatus::Complete);
        // Cap reached, but bytes after the 3rd item haven't decided
        // between `,` and `]` yet → Incomplete (mask still narrowing).
        assert_eq!(validate(&s, br#"["a","b","c""#), ParseStatus::Incomplete);
        // Comma after cap is Invalid — the mask sampler will reject
        // any token that emits one, forcing the close.
        assert_eq!(validate(&s, br#"["a","b","c","#), ParseStatus::Invalid);
        // Already-emitted 4th element is also Invalid.
        assert_eq!(validate(&s, br#"["a","b","c","d"]"#), ParseStatus::Invalid);
    }

    #[test]
    fn validate_array_max_items_unbounded_when_absent() {
        // No `maxItems` → behaves as before, no cap.
        let s = compile_schema(&json!({
            "type": "array",
            "items": {"type": "integer"}
        }))
        .unwrap();
        assert_eq!(validate(&s, br#"[1,2,3,4,5,6,7,8,9,10]"#), ParseStatus::Complete);
    }

    #[test]
    fn validate_string_max_length_caps_count() {
        // maxLength=5 — five-char strings are Complete; six-char or
        // more is Invalid because the only valid byte after char 5 is
        // the closing `"`. This is the cap that prevents an unbounded
        // string field from swallowing the entire token budget under
        // schema-constrained generation.
        let s = compile_schema(&json!({
            "type": "string",
            "maxLength": 5
        }))
        .unwrap();
        assert_eq!(validate(&s, br#""""#), ParseStatus::Complete);
        assert_eq!(validate(&s, br#""hi""#), ParseStatus::Complete);
        assert_eq!(validate(&s, br#""abcde""#), ParseStatus::Complete);
        // 5 chars in, no `"` yet — partial.
        assert_eq!(validate(&s, br#""abcde"#), ParseStatus::Incomplete);
        // 6th body byte rejected — only `"` would be valid.
        assert_eq!(validate(&s, br#""abcdef"#), ParseStatus::Invalid);
        assert_eq!(validate(&s, br#""abcdef""#), ParseStatus::Invalid);
    }

    #[test]
    fn validate_string_max_length_unbounded_when_absent() {
        // No `maxLength` → behaves as before, no cap.
        let s = compile_schema(&json!({"type": "string"})).unwrap();
        let long = format!("\"{}\"", "x".repeat(2000));
        assert_eq!(validate(&s, long.as_bytes()), ParseStatus::Complete);
    }

    #[test]
    fn validate_string_max_length_counts_unicode_code_points_not_bytes() {
        // "café" is 4 code points but 5 UTF-8 bytes. With maxLength=4
        // the string completes cleanly; with maxLength=3 it overruns.
        let four = compile_schema(&json!({"type": "string", "maxLength": 4})).unwrap();
        assert_eq!(validate(&four, "\"café\"".as_bytes()), ParseStatus::Complete);

        let three = compile_schema(&json!({"type": "string", "maxLength": 3})).unwrap();
        // After "caf" (3 chars) the 'é' start byte (0xC3) is rejected.
        assert_eq!(validate(&three, "\"café\"".as_bytes()), ParseStatus::Invalid);
    }

    #[test]
    fn validate_string_max_length_counts_escapes_as_one() {
        // `\n` is 2 bytes on the wire but 1 character. With maxLength=3
        // we should accept exactly three escape pairs and then close.
        let s = compile_schema(&json!({"type": "string", "maxLength": 3})).unwrap();
        assert_eq!(validate(&s, br#""\n\n\n""#), ParseStatus::Complete);
        // 4 escapes overruns.
        assert_eq!(validate(&s, br#""\n\n\n\n""#), ParseStatus::Invalid);
    }

    #[test]
    fn validate_rejects_duplicate_optional_property() {
        // Schema with one required + two optional. Once `description`
        // is consumed, a second `description` is a duplicate and
        // must be Invalid — otherwise greedy temp=0 sampling under
        // an optional unbounded-string field can loop forever
        // re-emitting the same key. (Concrete repro 2026-05-04: a
        // Phase-3 question naming response burned 11681 tokens
        // emitting `description` field over and over.)
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "alias": {"type": "string"}
            },
            "required": ["name"]
        }))
        .unwrap();
        // Single-instance optional is fine.
        assert_eq!(
            validate(&s, br#"{"name":"x","description":"hi"}"#),
            ParseStatus::Complete
        );
        // Duplicate optional → Invalid.
        assert_eq!(
            validate(
                &s,
                br#"{"name":"x","description":"a","description":"b"}"#
            ),
            ParseStatus::Invalid
        );
    }

    #[test]
    fn validate_rejects_duplicate_required_property() {
        // Same problem on the required side — must also reject.
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": {"type": "string"}
            },
            "required": ["id"]
        }))
        .unwrap();
        assert_eq!(validate(&s, br#"{"id":"x"}"#), ParseStatus::Complete);
        assert_eq!(
            validate(&s, br#"{"id":"x","id":"y"}"#),
            ParseStatus::Invalid
        );
    }

    #[test]
    fn validate_optional_properties_in_declaration_order() {
        // Policy: optionals must appear in declaration order. This is
        // stricter than JSON Schema spec (which doesn't enforce order)
        // but it's the closure of "no duplicates allowed" + "track
        // progress with a single high-water-mark cursor." Tracking a
        // bitmask of consumed indices instead would relax this — that
        // would be a larger change with its own state-management cost,
        // and in practice schema-aware models like Qwen3 emit fields
        // in schema order at temp=0. Schema authors should declare
        // optional properties in the order they want them generated.
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string"},
                "a": {"type": "string"},
                "b": {"type": "string"}
            },
            "required": ["name"]
        }))
        .unwrap();
        // In-order: accepted.
        assert_eq!(
            validate(&s, br#"{"name":"x","a":"1","b":"2"}"#),
            ParseStatus::Complete
        );
        // Out-of-order: rejected. Picking `b` first ratchets next_idx
        // past `a`'s declaration index, so a subsequent `a` falls
        // into the "declared at a passed index" → Forbidden branch.
        assert_eq!(
            validate(&s, br#"{"name":"x","b":"2","a":"1"}"#),
            ParseStatus::Invalid
        );
        // Skipping an optional is fine — only `b` is OK.
        assert_eq!(
            validate(&s, br#"{"name":"x","b":"2"}"#),
            ParseStatus::Complete
        );
    }

    #[test]
    fn validate_string_max_length_in_nested_object_property() {
        // Schema-constrained generation pathology lives at the leaf
        // level: an object with one unbounded string field is the
        // shape that ate 11k tokens of LATIN's Phase-1 budget. With
        // a property-level maxLength, the runaway is bounded.
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string", "maxLength": 8}
            },
            "required": ["summary"]
        }))
        .unwrap();
        assert_eq!(validate(&s, br#"{"summary":"hi"}"#), ParseStatus::Complete);
        assert_eq!(validate(&s, br#"{"summary":"12345678"}"#), ParseStatus::Complete);
        assert_eq!(
            validate(&s, br#"{"summary":"123456789"}"#),
            ParseStatus::Invalid
        );
    }

    /// Drive the incremental state machine over a byte buffer, return
    /// final ParseStatus (with EOF-finalization). Used by the parity
    /// tests below.
    fn validate_incremental(schema: &Schema, bytes: &[u8]) -> ParseStatus {
        let mut state = ValidatorState::new(schema.clone());
        for &b in bytes {
            let s = state.advance(b);
            if matches!(s, ParseStatus::Invalid) {
                return s;
            }
        }
        state.eof_status()
    }

    /// Cover every supported schema construct against the new validator.
    /// Mirrors the recursive-parser asserts so we catch regressions
    /// either way.
    #[test]
    fn incremental_parity_against_recursive() {
        // Simple object
        let s = compile_schema(&json!({
            "type": "object",
            "properties": {
                "intent": {"type": "string", "enum": ["A", "B"]},
                "confidence": {"type": "number"}
            },
            "required": ["intent", "confidence"]
        }))
        .unwrap();
        let cases: &[(&[u8], ParseStatus)] = &[
            (br#"{"intent":"A","confidence":0.9}"#, ParseStatus::Complete),
            (br#"{"intent":"A"#, ParseStatus::Incomplete),
            (br#"{"intent":"Z"#, ParseStatus::Invalid),
            (br#"{"intent":"A","confidence":0.9}   "#, ParseStatus::Complete),
        ];
        for (bytes, expected) in cases {
            let r = validate_incremental(&s, bytes);
            let v = validate(&s, bytes);
            assert_eq!(r, *expected, "incremental: {:?}", std::str::from_utf8(bytes));
            assert_eq!(
                v, *expected,
                "recursive (parity): {:?}",
                std::str::from_utf8(bytes)
            );
        }

        // Array with maxItems
        let s2 = compile_schema(&json!({
            "type": "array",
            "items": {"type": "string"},
            "maxItems": 3
        }))
        .unwrap();
        for (bytes, expected) in &[
            (b"[]" as &[u8], ParseStatus::Complete),
            (b"[\"a\"]", ParseStatus::Complete),
            (b"[\"a\",\"b\",\"c\"]", ParseStatus::Complete),
            (b"[\"a\",\"b\",\"c\",", ParseStatus::Invalid),
            (b"[\"a\",", ParseStatus::Incomplete),
        ] {
            assert_eq!(
                validate_incremental(&s2, bytes),
                *expected,
                "incremental array: {:?}",
                std::str::from_utf8(bytes)
            );
        }

        // anyOf string/null
        let s3 = compile_schema(&json!({"type": ["string", "null"]})).unwrap();
        for (bytes, expected) in &[
            (b"\"hi\"" as &[u8], ParseStatus::Complete),
            (b"null", ParseStatus::Complete),
            (b"nul", ParseStatus::Incomplete),
            (b"42", ParseStatus::Invalid),
        ] {
            assert_eq!(
                validate_incremental(&s3, bytes),
                *expected,
                "incremental anyOf: {:?}",
                std::str::from_utf8(bytes)
            );
        }

        // Nested: object containing array of objects (mirrors phase1
        // shape). This exercises object→array→object→string traversal.
        let s4 = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "section_id": {"type": "string"},
                "questions_raised": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"content": {"type": "string"}},
                        "required": ["content"]
                    }
                }
            },
            "required": ["section_id", "questions_raised"]
        }))
        .unwrap();
        let nested = br#"{"section_id":"x","questions_raised":[{"content":"q1"},{"content":"q2"}]}"#;
        assert_eq!(validate_incremental(&s4, nested), ParseStatus::Complete);
        let over_cap =
            br#"{"section_id":"x","questions_raised":[{"content":"q1"},{"content":"q2"},"#;
        assert_eq!(validate_incremental(&s4, over_cap), ParseStatus::Invalid);

        // Number / integer / negatives / fractions / exponent
        let num = compile_schema(&json!({"type": "number"})).unwrap();
        for (bytes, expected) in &[
            (b"0" as &[u8], ParseStatus::Complete),
            (b"-1", ParseStatus::Complete),
            (b"1.5", ParseStatus::Complete),
            (b"-3.14e10", ParseStatus::Complete),
            (b"-3.14e-10", ParseStatus::Complete),
            (b"01", ParseStatus::Invalid), // leading-zero rule
            (b"1.", ParseStatus::Incomplete),
            (b"1e", ParseStatus::Incomplete),
        ] {
            assert_eq!(
                validate_incremental(&num, bytes),
                *expected,
                "incremental number: {:?}",
                std::str::from_utf8(bytes)
            );
        }
    }

    /// Confirm the state-machine path catches the same bad-prefix that
    /// `mask_shaped_validate_catches_mid_token_backtick_corruption`
    /// catches via the recursive path (the gemma backtick bug).
    #[test]
    fn incremental_rejects_backtick_in_string_position() {
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "a": {"type": "string"}
            },
            "required": ["a"]
        }))
        .unwrap();
        // After '"a":', the next non-ws byte must be `"`. A backtick
        // opens nothing valid.
        let bad = br#"{"a":`"#;
        assert_eq!(validate_incremental(&s, bad), ParseStatus::Invalid);
    }

    #[test]
    fn validate_nested_array_max_items_in_object_property() {
        // Schema mirrors the Phase 1 shape: an object with a capped
        // questions_raised array. Verifies the cap is honoured when
        // the array is nested inside object property dispatch.
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "section_id": {"type": "string"},
                "questions_raised": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"content": {"type": "string"}},
                        "required": ["content"]
                    }
                }
            },
            "required": ["section_id", "questions_raised"]
        }))
        .unwrap();
        assert_eq!(
            validate(&s, br#"{"section_id":"x","questions_raised":[{"content":"q1"},{"content":"q2"}]}"#),
            ParseStatus::Complete
        );
        // Third item attempted after the cap → comma Invalid.
        assert_eq!(
            validate(&s, br#"{"section_id":"x","questions_raised":[{"content":"q1"},{"content":"q2"},"#),
            ParseStatus::Invalid
        );
    }
}
