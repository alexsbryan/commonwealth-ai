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

use crate::llama::cpp::model::LlamaModel;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::cpp::token::data_array::LlamaTokenDataArray;
// Shim-restored 0.1.x method names: `token_to_piece_bytes` lives on
// the trait. JsonConstraint validates UTF-8 across token boundaries,
// so it needs the lossless `Vec<u8>` form.
use crate::llama::LlamaModelExt;
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
        /// When `true`, reject UTF-8 leading bytes `>= 0xE0` inside the
        /// string body — i.e. permit only ASCII + 2-byte UTF-8 (codepoints
        /// U+0000–U+07FF: Latin Extended, Greek, Cyrillic, Arabic, Hebrew).
        /// Blocks CJK, Devanagari, Hangul, and other 3+ byte scripts.
        ///
        /// Wired via the custom JSON Schema keyword `x-asciiExtended: true`.
        /// Default is `false` (no behaviour change). Enable on schemas
        /// where the model occasionally drifts into non-Latin tokens
        /// (e.g. Chinese characters in English atom extraction).
        ascii_extended: bool,
        /// Optional literal prefix the string body MUST start with.
        /// While `prefix_pos < prefix.len()`, the only accepted body
        /// byte is `prefix[prefix_pos]` — every other byte is
        /// `Invalid`, so the mask sampler forces the prefix into the
        /// emission one byte at a time. Once `prefix_pos == prefix.len()`,
        /// the field behaves like a normal `StringAny`.
        ///
        /// Sourced from JSON Schema `pattern` when the pattern is the
        /// literal-prefix subset `^<literal>` (no regex metacharacters
        /// other than the `^` anchor — compile_schema rejects richer
        /// patterns rather than silently misinterpreting them).
        ///
        /// Wire path: `CompletionRequest.cmd_prefix` →
        /// `tool_envelope_schema_for_with_env_and_cmd_prefix` →
        /// `pattern: "^<literal>"` on the `cmd` field → here.
        prefix: Option<Arc<Vec<u8>>>,
    },
    Integer,
    Number,
    Boolean,
    Null,
    AnyOf(Arc<Vec<Schema>>),
}

/// Parse a JSON Schema `pattern` keyword as a literal prefix.
///
/// Accepted subset: the pattern must start with `^` and the remainder
/// must contain only literal characters or backslash-escaped regex
/// metacharacters (`\\`, `\^`, `\$`, `\.`, `\|`, `\?`, `\*`, `\+`,
/// `\(`, `\)`, `\[`, `\]`, `\{`, `\}`). Anything richer (`.`, `*`,
/// alternation, classes) is rejected loudly — silently misinterpreting
/// a regex as a literal would let the model sample bytes the schema
/// author intended to forbid.
///
/// Returns the literal prefix as bytes, ready to be matched
/// position-by-position by the string-body walker.
fn parse_literal_prefix_pattern(p: &str, pointer: &str) -> Result<Arc<Vec<u8>>, ConstraintError> {
    let body = p.strip_prefix('^').ok_or_else(|| ConstraintError::Unsupported {
        feature: format!("pattern `{p}` (only literal-prefix subset `^<literal>` is supported)"),
        pointer: pointer.into(),
    })?;
    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let escaped = chars.next().ok_or_else(|| ConstraintError::Malformed {
                    pointer: pointer.into(),
                    detail: "trailing backslash in pattern".into(),
                })?;
                if !matches!(
                    escaped,
                    '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
                ) {
                    return Err(ConstraintError::Unsupported {
                        feature: format!("escape `\\{escaped}` in pattern (not a regex metacharacter)"),
                        pointer: pointer.into(),
                    });
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(escaped.encode_utf8(&mut buf).as_bytes());
            }
            c if matches!(c, '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$') => {
                return Err(ConstraintError::Unsupported {
                    feature: format!(
                        "regex metacharacter `{c}` in pattern (only literal-prefix subset \
                         supported; escape it as `\\{c}` if literal)"
                    ),
                    pointer: pointer.into(),
                });
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Ok(Arc::new(out))
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
                    let ascii_extended = obj
                        .get("x-asciiExtended")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // `pattern` — accept ONLY the literal-prefix
                    // subset: must start with `^` and contain no
                    // regex metacharacters past the anchor. Anything
                    // richer is rejected to avoid silently
                    // misinterpreting a regex as a literal.
                    let prefix = match obj.get("pattern").and_then(|v| v.as_str()) {
                        Some(p) => Some(parse_literal_prefix_pattern(p, pointer)?),
                        None => None,
                    };
                    Ok(Schema::StringAny {
                        max_length,
                        ascii_extended,
                        prefix,
                    })
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
    // Skip leading whitespace to match the incremental
    // `step_await_value` behavior. Validator and incremental parser
    // must agree or the masker's `emitted_invalid` latch fires
    // spuriously.
    skip_ws(&mut p);
    if p.eof() {
        return ParseStatus::Incomplete;
    }
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
        Schema::StringAny {
            max_length,
            ascii_extended,
            prefix,
        } => parse_string_any(
            p,
            *max_length,
            *ascii_extended,
            prefix.as_ref().map(|p| p.as_slice()),
        ),
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
fn parse_string_any(
    p: &mut Cursor,
    max_length: Option<usize>,
    ascii_extended: bool,
    prefix: Option<&[u8]>,
) -> ParseStatus {
    if p.peek() != Some(b'"') {
        return ParseStatus::Invalid;
    }
    p.advance();
    let mut consecutive_escapes = 0usize;
    let mut char_count = 0usize;
    let prefix_bytes: &[u8] = prefix.unwrap_or(&[]);
    let mut prefix_pos = 0usize;
    loop {
        let at_cap = matches!(max_length, Some(m) if char_count >= m);
        let in_prefix = prefix_pos < prefix_bytes.len();
        match p.peek() {
            None => return ParseStatus::Incomplete,
            Some(b'"') => {
                if in_prefix {
                    // Closing quote inside the literal prefix is a
                    // structural error — the prefix must be fully
                    // emitted before the string can close.
                    return ParseStatus::Invalid;
                }
                p.advance();
                return ParseStatus::Complete;
            }
            Some(b) if in_prefix => {
                // Inside the literal-prefix segment: the only legal
                // next byte is the next prefix byte. Reject anything
                // else (`\\` escapes, control chars, divergent bytes).
                if b != prefix_bytes[prefix_pos] {
                    return ParseStatus::Invalid;
                }
                p.advance();
                prefix_pos += 1;
                // Update char_count for UTF-8 start bytes only, to
                // stay in sync with the cap accounting.
                if (b & 0xC0) != 0x80 {
                    char_count = char_count.saturating_add(1);
                }
                consecutive_escapes = 0;
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
                        let mut hex = [0u8; 4];
                        for slot in hex.iter_mut() {
                            match p.peek() {
                                None => return ParseStatus::Incomplete,
                                Some(b) if b.is_ascii_hexdigit() => {
                                    *slot = b;
                                    p.advance();
                                }
                                _ => return ParseStatus::Invalid,
                            }
                        }
                        if ascii_extended && hex_codepoint_exceeds_2byte(&hex) {
                            return ParseStatus::Invalid;
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
                // `ascii_extended` blocks 3+ byte UTF-8 leading bytes
                // (0xE0..=0xF7). Continuation bytes (0x80..=0xBF) are
                // accepted; they belong to an already-validated 2-byte
                // start. 2-byte starts (0xC2..=0xDF) and ASCII pass.
                if ascii_extended && !is_continuation && b >= 0xE0 {
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

/// `hex` is the 4 ASCII hex digits of a `\uXXXX` escape. Returns true
/// iff the encoded codepoint is U+0800 or higher (3-byte UTF-8). Used
/// to enforce `ascii_extended` against escape-encoded CJK etc.
fn hex_codepoint_exceeds_2byte(hex: &[u8; 4]) -> bool {
    fn nib(b: u8) -> u32 {
        match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => 0,
        }
    }
    let cp = (nib(hex[0]) << 12) | (nib(hex[1]) << 8) | (nib(hex[2]) << 4) | nib(hex[3]);
    cp >= 0x0800
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
        Some(b'"') => parse_string_any(p, None, false, None),
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
        Schema::StringAny {
            max_length: None,
            ascii_extended: false,
            prefix: None,
        },
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
        /// Mirrors `Schema::StringAny.ascii_extended`. When `true`, the
        /// validator rejects UTF-8 leading bytes `>= 0xE0` (3+ byte
        /// sequences) and `\uXXXX` escapes with `XXXX >= 0800`.
        ascii_extended: bool,
        /// Literal-prefix constraint inherited from the schema. While
        /// `prefix_pos < prefix.len()`, the step function masks any
        /// byte that doesn't extend the prefix exactly.
        prefix: Option<Arc<Vec<u8>>>,
        prefix_pos: usize,
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
    /// Count of consecutive whitespace bytes accepted in the
    /// current run. JSON permits whitespace at many state
    /// boundaries (before the root, between value and comma,
    /// between key and colon, inside arrays, …). At each of these
    /// the constraint walker historically permitted unbounded
    /// whitespace, which lets a greedy sampler at T=0.0 stall by
    /// picking high-prob whitespace tokens indefinitely.
    ///
    /// Observed 2026-05-19:
    /// - Gemma 4 E4B-it on a calibration schema emitted 1024
    ///   whitespace tokens at the **root** before the opening `{`
    ///   (the original cause for adding this counter).
    /// - Gemma 4 26B-A4B-it after emitting `{"choice": "A"` then
    ///   emitted 1024 whitespace tokens **inside the object** while
    ///   awaiting the next key.
    ///
    /// Both classes are the same shape: any state that accepts ws
    /// is a stall vector. The cap therefore applies globally —
    /// reset to 0 the moment a non-ws byte is accepted. 16 bytes
    /// of ws-tolerance covers pretty-printed JSON (`\n` + up to
    /// 15-space indent = 16 bytes between tokens) without allowing
    /// the runaway.
    consecutive_ws_count: usize,
}

/// Upper bound on consecutive whitespace bytes accepted at any
/// state boundary in the constraint walk. Sized to cover
/// pretty-printed JSON indents (a `\n` plus generous indent fits)
/// while rejecting indefinite stalls. See
/// `ValidatorState::consecutive_ws_count`.
const MAX_CONSECUTIVE_WS: usize = 16;

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
            consecutive_ws_count: 0,
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

        // Whitespace-run cap. Applies at every state boundary —
        // root AwaitValue, between value+comma, awaiting next key,
        // etc. The constraint walker permits whitespace generously
        // per JSON spec, which lets a greedy sampler at T=0.0 stall
        // by emitting whitespace tokens indefinitely. Cap forces
        // structural progress once the model has had reasonable
        // room for indentation. See `ValidatorState::consecutive_ws_count`.
        if is_ws(byte) {
            if self.consecutive_ws_count >= MAX_CONSECUTIVE_WS {
                return ParseStatus::Invalid;
            }
            self.consecutive_ws_count += 1;
        } else {
            self.consecutive_ws_count = 0;
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

    /// Returns `Some(byte)` iff exactly one byte is legal in the
    /// current FSM state — the byte-level building block for Tier 2
    /// jump-forward decoding. None on ambiguity (≥2 legal bytes),
    /// degenerate-no-legal-bytes, or root-complete.
    ///
    /// Mechanically: for each candidate byte 0..256, clone `self` and
    /// call `advance`. A non-Invalid result means the byte was
    /// accepted. Short-circuits as soon as a second legal byte is
    /// found — so ambiguous states (the common case during string
    /// bodies, key prefixes) usually terminate after the first 2-3
    /// trials rather than walking the full 256.
    ///
    /// Cheap by construction: `ValidatorState` clones at Arc-pointer
    /// granularity (every heavy field on `Frame` is Arc-wrapped),
    /// so 256 clones cost ~stack-depth × atomic increments — well
    /// under the cost of one full-vocab parser walk (which Tier 1's
    /// mask path performs).
    pub(crate) fn forced_next_byte(&self) -> Option<u8> {
        if self.root_complete {
            return None;
        }
        let mut found: Option<u8> = None;
        for b in 0u16..=255u16 {
            let b = b as u8;
            let mut probe = self.clone();
            if !matches!(probe.advance(b), ParseStatus::Invalid) {
                if found.is_some() {
                    return None;
                }
                found = Some(b);
            }
        }
        found
    }

    /// Walks `forced_next_byte` forward, advancing an internal probe
    /// state by each forced byte, and returns the deterministic byte
    /// sequence the FSM emits from this state. Stops on ambiguity,
    /// when the probe reaches `Complete`, or when `max_bytes` is
    /// reached. Empty when the very first state is ambiguous.
    ///
    /// Pure of side effects on `self` — the walk runs entirely on
    /// clones. The caller advances `self` separately via `accept`
    /// after the vocab longest-match resolves the byte run into
    /// tokens (see `JsonConstraint::forced_next_run`).
    pub(crate) fn forced_byte_run(&self, max_bytes: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(max_bytes.min(16));
        let mut probe = self.clone();
        while out.len() < max_bytes {
            let Some(b) = probe.forced_next_byte() else {
                break;
            };
            // Advance probe by the forced byte. `forced_next_byte`
            // already verified b is non-Invalid from this exact state,
            // so this advance cannot return Invalid — defensive check
            // bails if it somehow does.
            if matches!(probe.advance(b), ParseStatus::Invalid) {
                break;
            }
            out.push(b);
        }
        out
    }
}

/// Content-based fingerprint of a Schema enum value. Hashes the
/// discriminant + `Arc::as_ptr` of each interned child collection
/// — stable across `Clone` (Arc pointers stay identical) but
/// distinguishes different schemas. Address-based hashing
/// (`as *const Schema`) fails because Clone deep-copies the enum
/// itself, putting the new instance at a different address.
fn schema_fingerprint<H: std::hash::Hasher>(schema: &Schema, h: &mut H) {
    use std::hash::Hash;
    match schema {
        Schema::Object { properties, required_count, additional } => {
            0u8.hash(h);
            (Arc::as_ptr(properties) as usize).hash(h);
            required_count.hash(h);
            additional.hash(h);
        }
        Schema::Array { items, max_items } => {
            1u8.hash(h);
            (Arc::as_ptr(items) as usize).hash(h);
            max_items.hash(h);
        }
        Schema::StringEnum(opts) => {
            2u8.hash(h);
            (Arc::as_ptr(opts) as usize).hash(h);
        }
        Schema::StringAny {
            max_length,
            ascii_extended,
            prefix,
        } => {
            3u8.hash(h);
            max_length.hash(h);
            ascii_extended.hash(h);
            // Pointer identity is enough — same prefix Arc means
            // structurally equivalent constraint. Two distinct Arcs
            // with the same bytes would hash differently here, but
            // they should never occur in practice (one compile per
            // schema instance).
            prefix.as_ref().map(|p| Arc::as_ptr(p) as usize).hash(h);
        }
        Schema::Integer => { 4u8.hash(h); }
        Schema::Number => { 5u8.hash(h); }
        Schema::Boolean => { 6u8.hash(h); }
        Schema::Null => { 7u8.hash(h); }
        Schema::AnyOf(alts) => {
            8u8.hash(h);
            (Arc::as_ptr(alts) as usize).hash(h);
        }
    }
}

impl ValidatorState {
    /// Structural fingerprint of the state — used as the mask-cache
    /// key. Two states with the same fingerprint produce the same
    /// validity bitmask over the vocab.
    fn fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.root_complete.hash(&mut h);
        self.stack.len().hash(&mut h);
        for frame in &self.stack {
            frame.fingerprint(&mut h);
        }
        h.finish()
    }
}

impl Frame {
    /// Per-frame contribution to the state fingerprint. We hash the
    /// frame discriminant + every field that affects which bytes
    /// the parser accepts next. Schema-bearing frames hash by
    /// `Arc::as_ptr` (pointer identity) — cheap, correct because
    /// schemas are interned for a request.
    ///
    /// Deliberately EXCLUDED (these grow monotonically but don't
    /// change byte validity):
    /// - `Object.pairs_consumed`, `Object.required_count` (gates
    ///   close, not next-byte validity, and `next_idx` already
    ///   covers per-key state).
    /// - `Array.count` (gates close, but valid-next-bytes are stable
    ///   within an unbounded array; for bounded arrays the close
    ///   transition is gated by `count == max_items`).
    /// - `StringAny.char_count` when below cap (validity stable).
    fn fingerprint<H: std::hash::Hasher>(&self, h: &mut H) {
        use std::hash::Hash;
        match self {
            Frame::AwaitValue(schema) => {
                0u8.hash(h);
                schema_fingerprint(schema, h);
            }
            Frame::Object {
                properties,
                required_count,
                additional,
                next_idx,
                pairs_consumed: _,
                sub,
            } => {
                1u8.hash(h);
                (Arc::as_ptr(properties) as usize).hash(h);
                additional.hash(h);
                // `next_idx` is INCLUDED for the sub-states whose
                // byte-validity reads it (and for the close-bracket
                // gating against `required_count`). Initially we
                // excluded it on the hope of better cache reuse
                // across kv pairs, but that caused a real divergence:
                //
                //   schema: required ["content","evidence"]
                //   buffer: `..."content":"x","co`
                //   parser state: InKey { accumulated: [c, o] }, next_idx=1
                //
                // After "content" was consumed, next_idx ratcheted
                // to 1 — `any_property_starts_with` should now only
                // accept prefixes of "evidence". But the cache had a
                // mask computed at next_idx=0, where `c` (prefix of
                // "content") WAS valid. Same fingerprint → cache hit
                // → mask permitted `c` → token sampled → validate()
                // re-parsed the buffer, detected the duplicate-
                // property attempt, latched `emitted_invalid`, model
                // emitted EOG mid-generation. Reproduced on every
                // multi-required-property schema (engineering_atlas
                // Phase 1 included).
                //
                // The fix is to make next_idx part of the key.
                // It costs us one extra cached mask per (next_idx,
                // sub-discriminant) combination — typically 3-5
                // additional entries per object schema, well under
                // the 256-entry bound. The benefit is correctness.
                next_idx.hash(h);
                required_count.hash(h);
                std::mem::discriminant(sub).hash(h);
                match sub {
                    ObjectSub::InKey { accumulated } => accumulated.hash(h),
                    // `chosen` only affects byte-validity at AfterColon
                    // (the next byte must start a valid value of the
                    // chosen property's schema). At AwaitColon and
                    // InValue it's just bookkeeping for the next
                    // transition — valid bytes are fixed (`:` resp.
                    // `,`/`}`/ws) regardless of which property we
                    // just keyed. Excluding it from those branches
                    // lets the cache survive across kv pairs.
                    ObjectSub::AfterColon { chosen } => {
                        std::mem::discriminant(chosen).hash(h);
                        if let ChosenKeyKind::Typed(idx) = chosen {
                            idx.hash(h);
                        }
                    }
                    _ => {}
                }
            }
            Frame::Array {
                items,
                max_items,
                count,
                sub,
            } => {
                2u8.hash(h);
                (Arc::as_ptr(items) as usize).hash(h);
                // For bounded arrays, "at max" changes close validity.
                let at_max = max_items.map(|m| *count >= m).unwrap_or(false);
                at_max.hash(h);
                std::mem::discriminant(sub).hash(h);
            }
            Frame::StringEnum { opts, accumulated } => {
                3u8.hash(h);
                (Arc::as_ptr(opts) as usize).hash(h);
                accumulated.hash(h);
            }
            Frame::StringAny {
                consecutive_escapes,
                sub,
                char_count,
                max_length,
                ascii_extended,
                prefix,
                prefix_pos,
            } => {
                4u8.hash(h);
                consecutive_escapes.hash(h);
                std::mem::discriminant(sub).hash(h);
                prefix.as_ref().map(|p| Arc::as_ptr(p) as usize).hash(h);
                prefix_pos.hash(h);
                if let StringSub::InUnicode { remaining } = sub {
                    remaining.hash(h);
                }
                // **2026-05-17 bug fix.** The previous hash collapsed
                // `char_count = 0..(max_length - 1)` into a single
                // fingerprint bucket via a `near_cap` boolean. Real
                // production symptom: a mask computed at char_count=1
                // (room=199 for a maxLength=200 field) was reused at
                // char_count=195 (room=5), allowing the model to
                // sample a 10-char token like " adherence" that
                // overflowed the cap. The post-accept validator
                // caught the bad state, fired the EOS-latch warning,
                // and forced termination — costing ~22% of the SEP
                // pipeline's wall clock in phase1_terse retries.
                //
                // Fix: hash a bucketed `remaining_room` instead. Any
                // room value below `MAX_TOKEN_BYTES` is hashed as
                // itself (so each char of approach gets its own
                // mask). Anything above is saturated, yielding a
                // single shared fingerprint deep inside the field
                // (where every vocab token's byte-length fits with
                // headroom to spare). Cache growth is bounded by
                // `MAX_TOKEN_BYTES` distinct entries per capped
                // string field per request — sub-MB.
                //
                // `MAX_TOKEN_BYTES = 64` is a conservative bound for
                // BPE vocabs (Qwen3 + Darwin both max around 20-30
                // bytes per token); raising it has no correctness
                // cost, only a cache-fragmentation cost.
                const MAX_TOKEN_BYTES: usize = 64;
                let remaining_room_bucket: usize = max_length
                    .map(|m| m.saturating_sub(*char_count).min(MAX_TOKEN_BYTES))
                    .unwrap_or(MAX_TOKEN_BYTES);
                remaining_room_bucket.hash(h);
                max_length.is_some().hash(h);
                ascii_extended.hash(h);
            }
            Frame::Number { allow_fraction, sub } => {
                5u8.hash(h);
                allow_fraction.hash(h);
                std::mem::discriminant(sub).hash(h);
            }
            Frame::Keyword { word, pos } => {
                6u8.hash(h);
                word.hash(h);
                pos.hash(h);
            }
            Frame::AnyOf(alts) => {
                7u8.hash(h);
                (Arc::as_ptr(alts) as usize).hash(h);
            }
            Frame::Finished => {
                8u8.hash(h);
            }
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
                ascii_extended,
                prefix,
                prefix_pos,
            } => Self::step_string_any(
                consecutive_escapes,
                sub,
                char_count,
                *max_length,
                *ascii_extended,
                prefix.as_ref().map(|p| p.as_slice()),
                prefix_pos,
                byte,
            ),
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
        // Permit leading whitespace before the value. Combined with
        // the multi-EOG mask in `JsonConstraint::new`, the model
        // can either commit immediately or generate a few WS tokens
        // and then commit — neither degenerates into a loop because
        // the no-EOG-while-incomplete rule still applies.
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
            Schema::StringAny {
                max_length,
                ascii_extended,
                prefix,
            } => {
                if byte != b'"' {
                    return StepResult::Invalid;
                }
                StepResult::ReplaceConsumed(Frame::StringAny {
                    consecutive_escapes: 0,
                    sub: StringSub::InBody,
                    char_count: 0,
                    max_length: *max_length,
                    ascii_extended: *ascii_extended,
                    prefix: prefix.clone(),
                    prefix_pos: 0,
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
                        // Mirror the recursive validator's `more_pairs_possible`
                        // check (parse_object near line 437): a `,` after a
                        // value is only valid if there's still room for
                        // another property — either an unfilled typed slot
                        // or `additionalProperties: true`. Without this
                        // check, the masker accepts `,` after the last
                        // typed property has been consumed; the post-accept
                        // validator (full-buffer recursive parse) correctly
                        // sees Invalid; the diagnostic latch fires and
                        // forces EOS-only mode, truncating the document.
                        // This was the A3B-MoE "early-EOS" failure mode
                        // (memory: project_a3b_early_eos.md).
                        let more_pairs_possible = *next_idx < properties.len() || additional;
                        if !more_pairs_possible {
                            return StepResult::Invalid;
                        }
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
        ascii_extended: bool,
        prefix: Option<&[u8]>,
        prefix_pos: &mut usize,
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
        // Literal-prefix enforcement (R2). While in the prefix
        // segment, the ONLY legal next byte is the next prefix byte —
        // not escapes, not close-quote, not continuation bytes. The
        // mask sampler forces the prefix byte at every position until
        // the prefix is exhausted. After exhaustion this code path
        // is inert and the string body resumes normal walking.
        let in_prefix = matches!(prefix, Some(p) if *prefix_pos < p.len());
        if in_prefix {
            if let StringSub::InBody = sub {
                let p = prefix.unwrap();
                let expected = p[*prefix_pos];
                if byte != expected {
                    return StepResult::Invalid;
                }
                *prefix_pos += 1;
                if (byte & 0xC0) != 0x80 {
                    *char_count = char_count.saturating_add(1);
                }
                *consecutive_escapes = 0;
                return StepResult::Consumed;
            }
            // We're in an escape sub-state but still owe prefix bytes
            // — the prefix shouldn't contain escapes (caller produced
            // a literal prefix). Anything other than InBody here is
            // a bug in the caller.
            return StepResult::Invalid;
        }
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
                    // `ascii_extended`: reject 3+ byte UTF-8 starts
                    // (0xE0..=0xF7). Continuation bytes (0x80..=0xBF)
                    // are not new code-point starts and are accepted
                    // here — the prior 2-byte start was already
                    // validated. 2-byte starts (0xC2..=0xDF) and ASCII
                    // pass unchanged.
                    if ascii_extended && !is_continuation && b >= 0xE0 {
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
                // `ascii_extended` against `\uXXXX`: once we know the
                // first two hex digits we know whether the codepoint
                // would land in the 3-byte UTF-8 range (>= U+0800).
                // Reject early so the mask sampler steers the model
                // away from completing the escape.
                if ascii_extended {
                    let consumed_digits = 4u8 - *remaining;
                    let nib = match byte {
                        b'0'..=b'9' => byte - b'0',
                        b'a'..=b'f' => byte - b'a' + 10,
                        b'A'..=b'F' => byte - b'A' + 10,
                        _ => 0,
                    };
                    // After consuming this digit:
                    //  digit-0: cp top nibble = nib (cp >= nib<<12).
                    //  digit-1: cp top byte = prev<<4 | nib.
                    // A codepoint reaches 3-byte UTF-8 at >= 0x0800,
                    // i.e. top byte >= 0x08.
                    if consumed_digits == 0 && nib >= 1 {
                        // top nibble >= 1 means cp >= 0x1000 — always
                        // 3-byte UTF-8.
                        return StepResult::Invalid;
                    }
                    // (consumed_digits == 1 case: top nibble was 0,
                    // need second nibble >= 8 for cp >= 0x0800.)
                    if consumed_digits == 1 && nib >= 8 {
                        return StepResult::Invalid;
                    }
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
/// Upper bound on resident per-state masks. Sized to comfortably fit
/// the working set of any schema we ship — Phase 1 atlas extraction
/// visits ~30-50 distinct abstract states, literary_atlas at peak ~80.
/// At 152 KiB per entry (n_vocab booleans) the bound caps in-memory
/// cache cost at roughly 38 MiB worst-case per concurrent constraint,
/// well under the daemon's per-request budget. Schemas that explore
/// more distinct states (pathological enums, deep `anyOf` trees) trip
/// the bound and pay an extra cache-miss on the oldest insertion.
const MASK_CACHE_MAX_ENTRIES: usize = 256;

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
            Err(crate::llama::cpp::TokenToStringError::InsufficientBufferSpace(neg)) => {
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

/// Byte-keyed trie over the model's vocab. Each terminal node carries
/// the `LlamaToken` whose bytes end at that node; descending the trie
/// with a byte sequence and tracking the deepest terminal hit gives
/// the longest vocab token that's a prefix of the input.
///
/// Tier 2 jump-forward decoding queries this with the FSM's forced
/// byte run to find the biggest single token that covers as many of
/// those bytes as possible — saving a forward pass for each token
/// the trie consumes from the run.
///
/// Built once per model, cached for the daemon's lifetime alongside
/// `vocab_cache`. Memory cost: ~25-35 MB for a 150K-token vocab with
/// ~3-byte average length (HashMap-of-byte children per node). The
/// trie is read-only after construction, so it's safe to share via
/// `Arc` across threads.
pub struct VocabTrie {
    root: VocabTrieNode,
}

struct VocabTrieNode {
    /// Children indexed by the next byte in a token's byte sequence.
    /// Sparse via HashMap — most internal nodes have only a few
    /// children, so a 256-slot array would waste a lot of memory.
    children: HashMap<u8, Box<VocabTrieNode>>,
    /// Set iff a vocab token ends exactly at this node. None on
    /// purely-internal nodes (the bytes accumulated to here are a
    /// prefix of some token but not themselves a complete token).
    token: Option<LlamaToken>,
}

impl VocabTrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            token: None,
        }
    }
}

impl VocabTrie {
    /// Build a trie from a vocab byte map. Empty token byte sequences
    /// (holes for unknown-type tokens) are skipped. On the unlikely
    /// case of two tokens with identical byte sequences (BPE merges
    /// occasionally produce these), the smaller token id wins —
    /// arbitrary but deterministic.
    pub fn new(vocab_bytes: &[Vec<u8>]) -> Self {
        let mut root = VocabTrieNode::new();
        for (id, bytes) in vocab_bytes.iter().enumerate() {
            if bytes.is_empty() {
                continue;
            }
            let mut node = &mut root;
            for &b in bytes {
                node = node
                    .children
                    .entry(b)
                    .or_insert_with(|| Box::new(VocabTrieNode::new()));
            }
            if node.token.is_none() {
                node.token = Some(LlamaToken(id as i32));
            }
        }
        Self { root }
    }

    /// Walk `bytes` from the root, tracking the deepest terminal node
    /// hit. Returns `(token, consumed)` where `consumed` is the number
    /// of bytes the matched token covers from the start of `bytes`.
    /// `None` when no vocab token is a prefix of `bytes` (the input's
    /// first byte has no trie child).
    pub fn longest_match(&self, bytes: &[u8]) -> Option<(LlamaToken, usize)> {
        let mut node = &self.root;
        let mut best: Option<(LlamaToken, usize)> = None;
        for (i, &b) in bytes.iter().enumerate() {
            let Some(child) = node.children.get(&b) else {
                break;
            };
            node = child.as_ref();
            if let Some(t) = node.token {
                best = Some((t, i + 1));
            }
        }
        best
    }
}

/// Per-process cache of `VocabTrie` instances, keyed by `LlamaModel`
/// pointer. Mirrors `vocab_cache`'s shape: one entry per model,
/// populated lazily on first jump-forward request against that model.
fn vocab_trie_cache() -> &'static Mutex<HashMap<usize, Arc<VocabTrie>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<VocabTrie>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get-or-build the model's `VocabTrie`. Reuses `vocab_bytes_for` so
/// the trie and the bytes share their lifetime — both are persistent
/// for the daemon's lifetime once created.
pub(crate) fn vocab_trie_for(model: &LlamaModel) -> Arc<VocabTrie> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = vocab_trie_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(t) = guard.get(&key) {
            return t.clone();
        }
    }
    let vocab_bytes = vocab_bytes_for(model);
    let trie = Arc::new(VocabTrie::new(&vocab_bytes));
    let mut guard = vocab_trie_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, trie.clone());
    trie
}

/// Per-process cache of non-Latin token denylists, keyed by
/// `LlamaModel` pointer. Mirrors `vocab_cache`'s lifecycle.
fn non_latin_denylist_cache() -> &'static Mutex<HashMap<usize, Arc<Vec<bool>>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Vec<bool>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build a vocab-sized boolean bitmap where `true` means "the token's
/// rendered bytes contain a UTF-8 leading byte for a 3+ byte
/// sequence" (`0xE0..=0xF7`). Blocking these tokens makes CJK,
/// Devanagari, Hangul, Hiragana/Katakana, and other 3-byte+ scripts
/// unsampleable.
///
/// 2-byte UTF-8 leads (`0xC2..=0xDF` → Latin Extended, Greek,
/// Cyrillic, Arabic, Hebrew base) and ASCII pass through. Tokens
/// that are pure continuation bytes (`0x80..=0xBF`) are tails of a
/// multi-byte sequence — once the leads are blocked, those tails
/// have no preceding context to attach to and the BPE distribution
/// won't pick them in isolation; we leave them alone.
///
/// Why scan ANY byte in the token rather than just the first: a
/// single BPE token can encode `[ascii][CJK-lead][cont][cont][ascii]`
/// or similar mixed payloads. Flagging on any internal lead byte
/// catches those without needing per-token UTF-8 decoding.
///
/// Used by `ConstrainedSampler::sample` on every inference path
/// (not just `structured_output`) when the operator enables the
/// `SOVEREIGN_BLOCK_NON_LATIN` env var. Default OFF — some corpora
/// legitimately quote Chinese/Japanese characters.
pub fn non_latin_denylist_for(model: &LlamaModel) -> Arc<Vec<bool>> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = non_latin_denylist_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    let vocab = vocab_bytes_for(model);
    let denylist = build_non_latin_denylist(&vocab);
    let arc = Arc::new(denylist);
    let mut guard = non_latin_denylist_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, arc.clone());
    arc
}

/// Pure function variant — separate so the unit test can exercise
/// the bitmap construction against a synthetic vocab without
/// loading a real `LlamaModel`.
fn build_non_latin_denylist(vocab_bytes: &[Vec<u8>]) -> Vec<bool> {
    vocab_bytes
        .iter()
        .map(|bytes| bytes.iter().any(|b| (0xE0..=0xF7).contains(b)))
        .collect()
}

/// Apply a precomputed validity bitmask to the candidate array.
/// For each token id, if `valid[id]` is false, clamp its logit to
/// -INF. EOG tokens are handled separately (always permitted when
/// buffer is complete, never otherwise). Root-closed (buffer
/// complete) entries permit trailing whitespace; the cache only
/// covers the in-progress state, so root-closed states bypass the
/// cache path and run the parser fresh.
///
/// Serial iteration — measured at 9.5 ms per call under rayon
/// vs an expected ~0.7 ms of pure work, the work-stealing setup
/// cost dominates for this tiny per-element task. A tight
/// serial loop lets the CPU auto-vectorize the bitmask read
/// path and pays no thread-pool overhead. The full-vocab
/// parser pass below DOES benefit from rayon because each
/// candidate clones+walks the validator, which is hundreds of
/// nanoseconds per element.
fn apply_cached_mask(
    data: &mut LlamaTokenDataArray,
    valid: &[bool],
    eog_tokens: &[i32],
    buffer_is_complete: bool,
    vocab_bytes: &[Vec<u8>],
) {
    for entry in data.data.iter_mut() {
        let token_id = entry.id().0;
        if eog_tokens.contains(&token_id) {
            if !buffer_is_complete {
                entry.set_logit(f32::NEG_INFINITY);
            }
            continue;
        }
        if buffer_is_complete {
            // Trailing-whitespace branch; not cacheable per-state
            // because it depends on the model's bytes per id, not
            // on the parser state. Compute the cheap byte check.
            if let Some(bytes) = vocab_bytes.get(token_id as usize) {
                if !bytes.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            } else {
                entry.set_logit(f32::NEG_INFINITY);
            }
            continue;
        }
        // The hot path. valid[id] was computed by the cold-miss
        // parser pass; reading it is a single L1 load.
        let id = token_id as usize;
        if id >= valid.len() || !valid[id] {
            entry.set_logit(f32::NEG_INFINITY);
        }
    }
}

/// One entry in the per-state validity cache. `valid` is the
/// vocab-sized bitmask (the hot read for `apply_cached_mask`); `single`
/// records the lone surviving token when exactly one token is legal in
/// this state. The latter is the input to jump-forward decoding —
/// callers that opt in (via `forced_next_token`) can emit the token
/// without paying for a forward pass at that position.
///
/// Computed once per state on cache miss; both fields share the same
/// FIFO eviction lifetime under [`MASK_CACHE_MAX_ENTRIES`].
#[derive(Debug, Clone)]
struct MaskCacheEntry {
    valid: Arc<Vec<bool>>,
    single: Option<LlamaToken>,
}

/// Walk a freshly-computed validity bitmask and return `Some(token)` iff
/// exactly one position is `true`. Short-circuits on the second hit so
/// the cost is bounded by `2 * first_true_index` in the worst case.
fn single_survivor(valid: &[bool]) -> Option<LlamaToken> {
    let mut found: Option<LlamaToken> = None;
    for (i, &v) in valid.iter().enumerate() {
        if v {
            if found.is_some() {
                return None;
            }
            found = Some(LlamaToken(i as i32));
        }
    }
    found
}

/// Full-vocab walk of the FSM at `state`, returning the per-token
/// validity bitmask. The hot reused-elsewhere computation behind both
/// `JsonConstraint::mask` (which writes the bitmask onto a
/// `LlamaTokenDataArray` via `apply_cached_mask`) and
/// `JsonConstraint::forced_next_token` (which only needs the bitmask
/// to detect single-survivor states). Pure of side effects.
///
/// `buffer_is_complete=true` is the "root-closed" branch: only EOG
/// and whitespace tokens are legal. `buffer_is_complete=false` runs
/// the per-candidate `advance_bytes` parse — the cost driver, which
/// is why we cache the resulting bitmask per FSM fingerprint.
fn compute_validity_bitmask(
    state: &ValidatorState,
    vocab_bytes: &[Vec<u8>],
    eog_tokens: &[i32],
    buffer_is_complete: bool,
) -> Vec<bool> {
    use rayon::iter::IntoParallelIterator;
    let n_vocab = vocab_bytes.len();
    (0..n_vocab)
        .into_par_iter()
        .map_init(
            || state.clone(),
            |worker_state, id| {
                let token_id = id as i32;
                if eog_tokens.contains(&token_id) {
                    return buffer_is_complete;
                }
                let bytes = match vocab_bytes.get(id) {
                    Some(b) if !b.is_empty() => b,
                    _ => return false,
                };
                if buffer_is_complete {
                    return bytes
                        .iter()
                        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
                }
                let mut candidate = worker_state.clone();
                !matches!(
                    candidate.advance_bytes(bytes),
                    ParseStatus::Invalid
                )
            },
        )
        .collect()
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
    /// Byte-keyed trie over the vocab. Used by Tier 2 jump-forward
    /// (`forced_next_run`) to longest-match the FSM's forced byte
    /// sequence against the largest single vocab token. Shared
    /// per-model via `vocab_trie_cache` so a 25-35 MB trie isn't
    /// rebuilt per request.
    vocab_trie: Arc<VocabTrie>,
    eos_token: i32,
    /// **Every** end-of-generation token id, not just `token_eos()`.
    /// Modern chat-tuned models expose multiple EOG tokens (Qwen's
    /// `<|im_end|>`, Llama's `<|eot_id|>`, gemma's `<end_of_turn>`),
    /// and `model.is_eog_token(...)` returns true for any of them
    /// while the streaming loop in `embedded.rs` terminates on the
    /// first one it samples. If we only mask `token_eos()`, the
    /// model can sample a different EOG as token 1 and exit before
    /// emitting a single byte of structured output — observed on
    /// Qwen3.5-9B-vOP under JSON-schema mode (2026-05-11 grammar
    /// probe: completion_tokens=1, content=" "). Computed once at
    /// construction by walking 0..n_vocab.
    eog_tokens: Vec<i32>,
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
    /// Per-state cached validity bitmask. After the full-vocab
    /// validation pass for a given parser state, store the result
    /// (one bool per token id) keyed on a hash of the state's
    /// structural shape. Consecutive `mask()` calls in the SAME
    /// state — the common case while emitting a string body, an
    /// array of objects, etc — skip the parser entirely and just
    /// reuse the cached bitmask. Captures the same speedup as
    /// Outlines / LM-Format-Enforcer's precomputed FSM tables
    /// without their upfront construction cost: we pay only for
    /// states the model actually visits, in the order they're hit.
    ///
    /// `Vec<bool>` of length n_vocab. ~152 KiB per cached state.
    /// A single Phase 1 extraction visits 30-50 distinct states
    /// (~5-7 MiB total). We keep every state we've ever seen — the
    /// model returns to the same handful repeatedly (string-body,
    /// after-colon, await-comma-or-close), so memoizing them all
    /// turns a `string → comma → next_key → string` cycle from
    /// "recompute string mask twice" into "reuse the cached one."
    /// Pre-this-change the cache was a single `Option<(fp, mask)>`
    /// that wiped on every state transition; the multi-entry map
    /// closes that gap. Bounded by [`MASK_CACHE_MAX_ENTRIES`] to
    /// stay well under the per-request VRAM budget.
    mask_cache: HashMap<u64, MaskCacheEntry>,
    /// Insertion-order tracking for the simple LRU bound on
    /// `mask_cache`. Approximate LRU — we only push on miss-and-
    /// insert, not on hits — but the working set for a Phase 1
    /// extraction is small enough (<50 states) that the bound is
    /// rarely tripped in practice. It exists as a guardrail against
    /// pathological schemas that explore hundreds of distinct
    /// abstract states.
    mask_cache_order: Vec<u64>,
    /// Optional cumulative timing telemetry. Built via `from_env()`
    /// when `SOVEREIGN_GRAMMAR_TIMING=1` so production runs pay zero
    /// extra cost. When present, each `mask()` and `accept()` call
    /// adds its wall-clock duration to the running totals; the Drop
    /// impl logs a summary so the operator can attribute per-turn
    /// latency between mask cost (logits walk × vocab) and the
    /// upstream prompt-eval that doesn't touch this struct at all.
    timing: Option<Mutex<TimingState>>,
}

/// Cumulative wall-clock counters for one constraint instance.
/// Reported once via `Drop`. All durations are in microseconds —
/// summing across the whole generation gives the total mask cost,
/// dividing by `mask_calls` gives the per-token mask latency.
#[derive(Debug, Default)]
struct TimingState {
    mask_calls: u64,
    mask_total_us: u64,
    /// Subset of `mask_calls` that hit the per-state validity cache
    /// (skipped the per-candidate parser pass).
    mask_cache_hits: u64,
    /// Cumulative duration of cache-hit mask calls.
    mask_hit_total_us: u64,
    /// Cumulative duration of cache-miss mask calls (full
    /// vocab × parser_walk pass + bitmask synthesis).
    mask_miss_total_us: u64,
    accept_calls: u64,
    accept_total_us: u64,
}

/// Build the optional timing slot from a generic env-var lookup.
/// Production calls go through `new()` which passes a `std::env`
/// closure; unit tests pass a stub closure so they can pin every
/// truthy/falsy/garbage shape without mutating process-global env
/// (which races against parallel test execution).
fn build_timing<F>(env_get: F) -> Option<Mutex<TimingState>>
where
    F: Fn(&str) -> Option<String>,
{
    let enabled = env_get("SOVEREIGN_GRAMMAR_TIMING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if enabled {
        Some(Mutex::new(TimingState::default()))
    } else {
        None
    }
}

impl JsonConstraint {
    /// Build a constraint from a JSON Schema and the model's vocab.
    pub fn new(schema: &Value, model: &LlamaModel) -> Result<Self, ConstraintError> {
        let compiled = compile_schema(schema)?;
        let vocab_bytes = vocab_bytes_for(model);
        let vocab_trie = vocab_trie_for(model);
        let eos_token = model.token_eos().0;
        // Enumerate every EOG token id so the mask can clamp them
        // all when the buffer is incomplete. Walk the vocab once;
        // is_eog_token is a cheap field lookup in llama-cpp.
        let n_vocab = model.n_vocab();
        let mut eog_tokens: Vec<i32> = Vec::new();
        for id in 0..n_vocab {
            if model.is_eog_token(LlamaToken(id)) {
                eog_tokens.push(id);
            }
        }
        tracing::debug!(
            count = eog_tokens.len(),
            primary_eos = eos_token,
            "JsonConstraint: enumerated EOG tokens"
        );
        let state = ValidatorState::new(compiled.clone());
        // Timing instrumentation: opt-in via env. Read once per
        // constructor call so the operator can flip the env at runtime
        // and the next request picks it up; existing constraints stay
        // on whatever decision was made when they were built (which
        // matches the Drop-based summary's natural scope).
        let timing = build_timing(|key| std::env::var(key).ok());
        Ok(Self {
            schema: compiled,
            emitted: Vec::new(),
            state,
            vocab_bytes,
            vocab_trie,
            eos_token,
            eog_tokens,
            emitted_invalid: false,
            mask_cache: HashMap::new(),
            mask_cache_order: Vec::new(),
            timing,
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
    pub fn mask(&mut self, data: &mut LlamaTokenDataArray) {
        let timing_start = self
            .timing
            .as_ref()
            .map(|_| std::time::Instant::now());

        let buffer_is_complete = matches!(self.state.eof_status(), ParseStatus::Complete);
        let vocab_bytes = &*self.vocab_bytes;
        let eog_tokens = &self.eog_tokens;

        // Latched-invalid: mute every non-EOG token.
        if self.emitted_invalid {
            data.data.par_iter_mut().for_each(|entry| {
                if !eog_tokens.contains(&entry.id().0) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            });
            self.record_mask_timing(timing_start, false);
            return;
        }

        // Compute the structural fingerprint of the current state.
        // If we have a cached validity bitmask for this fingerprint,
        // skip the per-candidate parser entirely — every cache hit
        // turns an O(vocab × parser_cost) pass into an O(vocab)
        // bitmask read. This is the same trick Outlines and
        // LM-Format-Enforcer use; we just compute the per-state
        // mask lazily on first hit instead of eagerly at startup.
        //
        // Multi-entry cache: every state we've ever computed stays
        // resident (subject to `MASK_CACHE_MAX_ENTRIES`). The earlier
        // single-entry Option wiped on every state transition, so a
        // `string → comma → next_key → string` cycle paid for the
        // string-body mask twice. The map closes that.
        let fingerprint = self.state.fingerprint();
        if let Some(entry) = self.mask_cache.get(&fingerprint) {
            let valid = entry.valid.clone();
            apply_cached_mask(data, &valid, eog_tokens, buffer_is_complete, vocab_bytes);
            self.record_mask_timing(timing_start, true);
            return;
        }

        // Cache miss — full-vocab parser pass. Build the bitmask in
        // place by writing the logit decision, then synthesize the
        // cache from the post-pass logits before any downstream
        // sampler mutates them.
        let state = &self.state;
        data.data.par_iter_mut().for_each_init(
            || state.clone(),
            |worker_state, entry| {
                let token_id = entry.id().0;
                if eog_tokens.contains(&token_id) {
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
                let mut candidate_state = worker_state.clone();
                if matches!(
                    candidate_state.advance_bytes(bytes),
                    ParseStatus::Invalid
                ) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            },
        );

        // Synthesize the bitmask from the post-pass logits. Each
        // entry's id is the token id; its logit is either finite
        // (valid in this state) or -INF (rejected). We DON'T cache
        // the buffer_is_complete branch's decision — that's a
        // different state-equivalence class (root-closed) and the
        // fingerprint differentiates it anyway.
        let n_vocab = vocab_bytes.len();
        let mut valid: Vec<bool> = vec![false; n_vocab];
        for entry in data.data.iter() {
            let id = entry.id().0 as usize;
            if id < n_vocab {
                valid[id] = entry.logit().is_finite();
            }
        }
        let single = single_survivor(&valid);
        self.insert_mask_cache(
            fingerprint,
            MaskCacheEntry { valid: Arc::new(valid), single },
        );

        self.record_mask_timing(timing_start, false);
    }

    /// Returns `Some(token)` iff exactly one token is legal in the
    /// current FSM state — the building block for jump-forward
    /// decoding. The caller can emit the returned token, advance the
    /// FSM via `accept`, and defer the forward pass into a later
    /// batched decode at the next ambiguous position.
    ///
    /// Returns `None` (let the sampler chain run) in three situations:
    /// 1. **`emitted_invalid` is latched.** Every non-EOG is masked,
    ///    so EOG would be the unique survivor — but bypassing the
    ///    sampler's EOG handling on the latched path is a footgun. Let
    ///    the sampler resolve the terminal.
    /// 2. **Buffer is structurally complete.** EOG + trailing
    ///    whitespace are legal; the sampler picks based on logits and
    ///    we don't want jump-forward to short-circuit that choice.
    /// 3. **Ambiguous state** — two or more tokens survive the mask.
    ///    The normal path: the sampler chain picks based on model
    ///    logits.
    ///
    /// First call on a state pays the full vocab × parser walk (same
    /// cost as the next `mask()` call would have paid); both populate
    /// the shared `mask_cache`, so subsequent calls on the same
    /// fingerprint are O(1).
    pub fn forced_next_token(&mut self) -> Option<LlamaToken> {
        if self.emitted_invalid {
            return None;
        }
        if matches!(self.state.eof_status(), ParseStatus::Complete) {
            return None;
        }
        let fingerprint = self.state.fingerprint();
        if let Some(entry) = self.mask_cache.get(&fingerprint) {
            return entry.single;
        }
        // Cache miss — compute the bitmask via the shared vocab walk
        // and store it. This pre-populates `mask_cache` so a subsequent
        // `mask()` call on the same state takes the hot cache-hit path.
        let vocab_bytes = self.vocab_bytes.clone();
        let eog_tokens = self.eog_tokens.clone();
        let valid = compute_validity_bitmask(&self.state, &vocab_bytes, &eog_tokens, false);
        let single = single_survivor(&valid);
        self.insert_mask_cache(
            fingerprint,
            MaskCacheEntry { valid: Arc::new(valid), single },
        );
        single
    }

    /// **Tier 2 jump-forward.** Walks the FSM byte-by-byte to discover
    /// the deterministic byte sequence emitted from the current state,
    /// then longest-matches that against the vocab to produce the
    /// largest possible single tokens covering it.
    ///
    /// **Does NOT mutate `self.state`** — the walk runs on a clone.
    /// The caller is responsible for `accept`-ing each returned token
    /// via the sampler (which advances inner chains + the constraint
    /// FSM in lockstep, same contract as the sampled-token path).
    /// This is the symmetric mirror of `forced_next_token`, which
    /// also returns without accepting.
    ///
    /// Returns the emitted token sequence. Empty when:
    /// - The constraint has latched invalid.
    /// - The buffer is structurally complete (sampler handles EOG).
    /// - The first FSM state is ambiguous at byte level.
    /// - The vocab trie has no token covering the first forced byte
    ///   (degenerate; shouldn't happen on a healthy BPE).
    ///
    /// Bounded by `max_bytes` of forced sequence per re-walk — once
    /// the byte run exhausts (or longest-match fails), returns
    /// whatever tokens fit.
    ///
    /// **Composes with Tier 1.** A caller running both should try
    /// `forced_next_token` first (O(1) cache hit when warm) and fall
    /// through to this on miss. The two tiers are complementary:
    /// Tier 1 catches "exactly one vocab token survives the mask" via
    /// the existing per-vocab walk; Tier 2 catches the BPE-skeleton
    /// case where many tokens survive but the FSM forces a byte run
    /// that one or two large tokens can cover.
    pub fn forced_next_run(&mut self, max_bytes: usize) -> Vec<LlamaToken> {
        let mut out = Vec::new();
        if self.emitted_invalid {
            return out;
        }
        if matches!(self.state.eof_status(), ParseStatus::Complete) {
            return out;
        }
        // Probe walks the FSM without touching `self.state`. Each
        // iteration: derive the deterministic byte run from probe's
        // current state, longest-match it against the vocab to pick
        // a token, advance probe by exactly the token's bytes (the
        // first `consumed` bytes of the run), repeat. The byte run
        // is re-derived each iteration because consuming a token may
        // expose forced bytes beyond what the previous walk reached.
        let mut probe = self.state.clone();
        loop {
            let bytes = probe.forced_byte_run(max_bytes);
            if bytes.is_empty() {
                break;
            }
            let Some((token, consumed)) = self.vocab_trie.longest_match(&bytes) else {
                // No vocab token starts with the next forced byte —
                // degenerate state; surrender the rest of the run to
                // the sampler.
                break;
            };
            // Sanity: a healthy trie always advances by ≥1 byte on a
            // hit. Guard against zero-byte matches (which would loop
            // forever) defensively.
            if consumed == 0 {
                break;
            }
            // Advance the probe by exactly the token's bytes. We use
            // the byte-run slice rather than re-looking-up
            // `vocab_bytes[token]` because the trie's `consumed`
            // already tells us how many bytes the token covers.
            if matches!(
                probe.advance_bytes(&bytes[..consumed]),
                ParseStatus::Invalid
            ) {
                // Self-consistency violation: the trie thought these
                // bytes formed a vocab token, but the FSM rejects.
                // Bail rather than emit a token the caller would
                // accept and desync the real FSM on.
                break;
            }
            out.push(token);
        }
        out
    }

    /// Insert a freshly-computed entry into the mask cache, enforcing
    /// the entry-count bound. When the bound is tripped we evict the
    /// oldest insertion (approximate FIFO; the state distribution is
    /// small enough that true LRU isn't worth the extra bookkeeping).
    fn insert_mask_cache(&mut self, fingerprint: u64, entry: MaskCacheEntry) {
        if self.mask_cache.contains_key(&fingerprint) {
            // Defensive — should be unreachable on the miss path, but
            // overwrite rather than double-insert if we hit it.
            self.mask_cache.insert(fingerprint, entry);
            return;
        }
        while self.mask_cache.len() >= MASK_CACHE_MAX_ENTRIES {
            if let Some(old_fp) = self.mask_cache_order.first().copied() {
                self.mask_cache_order.remove(0);
                self.mask_cache.remove(&old_fp);
            } else {
                break;
            }
        }
        self.mask_cache.insert(fingerprint, entry);
        self.mask_cache_order.push(fingerprint);
    }

    /// Internal helper: when timing is enabled and `start` was
    /// captured, fold the elapsed wall-clock into the cumulative
    /// counter. No-op when timing is disabled, so the hot path stays
    /// branch-prediction friendly.
    fn record_mask_timing(&mut self, start: Option<std::time::Instant>, cache_hit: bool) {
        if let (Some(t), Some(s)) = (self.timing.as_ref(), start) {
            let elapsed_us = s.elapsed().as_micros() as u64;
            if let Ok(mut state) = t.lock() {
                state.mask_calls += 1;
                state.mask_total_us += elapsed_us;
                if cache_hit {
                    state.mask_cache_hits += 1;
                    state.mask_hit_total_us += elapsed_us;
                } else {
                    state.mask_miss_total_us += elapsed_us;
                }
            }
        }
    }

    /// Internal helper: same shape as `record_mask_timing` but for
    /// the accept-bookkeeping path. Kept separate so the per-call
    /// counter stays attributable.
    fn record_accept_timing(&self, start: Option<std::time::Instant>) {
        if let (Some(t), Some(s)) = (self.timing.as_ref(), start) {
            let elapsed_us = s.elapsed().as_micros() as u64;
            if let Ok(mut state) = t.lock() {
                state.accept_calls += 1;
                state.accept_total_us += elapsed_us;
            }
        }
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
        let timing_start = self
            .timing
            .as_ref()
            .map(|_| std::time::Instant::now());
        // Any EOG token — primary eos_token or any of the model's
        // chat-template terminators (`<|im_end|>`, `<|eot_id|>`, …) —
        // is a terminal no-op. The streaming loop in embedded.rs
        // exits on `is_eog_token(...)` so accept() should never
        // advance the buffer with the EOG's text bytes (which would
        // desync the parser).
        if self.eog_tokens.contains(&token.0) {
            self.record_accept_timing(timing_start);
            return;
        }
        let Some(bytes) = self.vocab_bytes.get(token.0 as usize).cloned() else {
            tracing::warn!(
                token_id = token.0,
                "JsonConstraint::accept: chosen token is out of vocab range — emitted buffer will desync from response"
            );
            self.record_accept_timing(timing_start);
            return;
        };
        self.emitted.extend_from_slice(&bytes);
        // Multi-entry cache means accept() never has to evict — the
        // entry for the post-transition state is computed lazily on
        // the next mask() miss and stays resident from there on.
        // This is the load-bearing change vs the old single-Option
        // cache, which had to wipe on every transition.
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
        self.record_accept_timing(timing_start);
    }

    /// True once the emitted bytes form a complete schema-conforming
    /// document (only trailing whitespace / EOS would follow).
    pub fn is_root_complete(&self) -> bool {
        matches!(self.state.eof_status(), ParseStatus::Complete)
    }
}

/// Drop-time summary: when the constraint goes out of scope (i.e.
/// the generation finished — naturally or by deadline) and timing
/// instrumentation is enabled, emit one tracing::info line with
/// cumulative call counts and total/avg microseconds for both
/// `mask()` and `accept()`. The line is greppable as
/// `grammar_timing:` and every field is a discrete key=value pair so
/// downstream log pipelines can extract it without regex.
///
/// This Drop is a no-op when `SOVEREIGN_GRAMMAR_TIMING` was unset
/// at construction time (`self.timing.is_none()`), so production
/// runs pay zero cost.
impl Drop for JsonConstraint {
    fn drop(&mut self) {
        let Some(t) = self.timing.as_ref() else { return };
        let state = match t.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if state.mask_calls == 0 && state.accept_calls == 0 {
            return;
        }
        let mask_avg_us = if state.mask_calls > 0 {
            state.mask_total_us / state.mask_calls
        } else {
            0
        };
        let accept_avg_us = if state.accept_calls > 0 {
            state.accept_total_us / state.accept_calls
        } else {
            0
        };
        let cache_misses = state.mask_calls.saturating_sub(state.mask_cache_hits);
        let mask_hit_avg_us = if state.mask_cache_hits > 0 {
            state.mask_hit_total_us / state.mask_cache_hits
        } else {
            0
        };
        let mask_miss_avg_us = if cache_misses > 0 {
            state.mask_miss_total_us / cache_misses
        } else {
            0
        };
        tracing::info!(
            mask_calls = state.mask_calls,
            mask_total_us = state.mask_total_us,
            mask_avg_us,
            mask_cache_hits = state.mask_cache_hits,
            mask_cache_misses = cache_misses,
            mask_hit_total_us = state.mask_hit_total_us,
            mask_hit_avg_us,
            mask_miss_total_us = state.mask_miss_total_us,
            mask_miss_avg_us,
            accept_calls = state.accept_calls,
            accept_total_us = state.accept_total_us,
            accept_avg_us,
            emitted_len = self.emitted.len(),
            "grammar_timing: per-constraint summary"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pattern_literal_prefix_compiles_to_prefix_field() {
        // R2: `pattern: "^apply_patch "` compiles into a literal
        // prefix on Schema::StringAny.
        let s = compile_schema(&json!({
            "type": "string",
            "pattern": "^apply_patch "
        }))
        .unwrap();
        match s {
            Schema::StringAny { prefix, .. } => {
                let bytes = prefix.expect("prefix populated").as_slice().to_vec();
                assert_eq!(bytes, b"apply_patch ".to_vec());
            }
            other => panic!("expected StringAny, got {other:?}"),
        }
    }

    #[test]
    fn pattern_with_metacharacter_rejected() {
        let err = compile_schema(&json!({
            "type": "string",
            "pattern": "^apply.*"
        }))
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("metacharacter"),
            "should reject regex metacharacter; got {msg}"
        );
    }

    #[test]
    fn pattern_without_caret_anchor_rejected() {
        let err = compile_schema(&json!({
            "type": "string",
            "pattern": "apply_patch"
        }))
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("literal-prefix subset"),
            "should reject non-anchored pattern; got {msg}"
        );
    }

    #[test]
    fn string_with_prefix_only_accepts_matching_prefix_bytes() {
        // Walk a small string-only schema with prefix "ab"; the body
        // walker must reject any first byte other than 'a' and any
        // second byte other than 'b'. Uses `advance` which feeds bytes
        // through the runtime stack machine.
        let schema = compile_schema(&json!({
            "type": "string",
            "pattern": "^ab"
        }))
        .unwrap();
        let mut state = ValidatorState::new(schema.clone());
        // Open quote allowed.
        assert!(!matches!(state.advance(b'"'), ParseStatus::Invalid));
        // Wrong first byte must be rejected.
        let mut state2 = state.clone();
        assert_eq!(state2.advance(b'x'), ParseStatus::Invalid);
        // Correct first byte advances.
        assert!(!matches!(state.advance(b'a'), ParseStatus::Invalid));
        // Wrong second byte still rejected.
        let mut state3 = state.clone();
        assert_eq!(state3.advance(b'z'), ParseStatus::Invalid);
        // Closing quote inside prefix is rejected (must finish prefix first).
        let mut state4 = state.clone();
        assert_eq!(state4.advance(b'"'), ParseStatus::Invalid);
        // Correct second byte completes the prefix.
        assert!(!matches!(state.advance(b'b'), ParseStatus::Invalid));
        // After the prefix, free-form body: any non-control byte OK.
        assert!(!matches!(state.advance(b'c'), ParseStatus::Invalid));
        // Closing quote now allowed (no max_length).
        let result = state.advance(b'"');
        assert!(
            !matches!(result, ParseStatus::Invalid),
            "expected string to close cleanly, got {result:?}"
        );
    }

    // ─── State-fingerprint cache-key tests ──────────────────────
    //
    // The mask cache keys on `ValidatorState::fingerprint()`. These
    // tests pin the invariants the cache assumes:
    //   1. Identical states fingerprint identically (cache hit).
    //   2. Bytes advancing through a string body DON'T change the
    //      fingerprint (high cache hit rate inside strings).
    //   3. Structural transitions DO change the fingerprint (forces
    //      cache miss → recompute).
    //   4. Monotonic counters (array.count below cap, object pairs)
    //      are EXCLUDED from the fingerprint — bumping them doesn't
    //      invalidate the cache.
    //
    // A bug in any of these would either crash performance (always
    // miss) or produce wrong outputs (cache reused across a state
    // that genuinely changed). Both are exactly the failure modes
    // a naive `top_k` pre-filter shipped previously, which is why
    // this cache is the right structural fix instead of an approx.

    fn state_for(schema_json: serde_json::Value) -> ValidatorState {
        let s = compile_schema(&schema_json).unwrap();
        ValidatorState::new(s)
    }

    #[test]
    fn fingerprint_is_stable_for_identical_states() {
        let a = state_for(json!({"type":"object","required":["x"],"properties":{"x":{"type":"string"}}}));
        let b = state_for(json!({"type":"object","required":["x"],"properties":{"x":{"type":"string"}}}));
        // Different Arc identities but structurally identical schemas
        // — fingerprints SHOULD differ because we hash by ptr. This
        // is the conservative direction; it's fine because two
        // different constraint instances each have their own cache.
        // Same state cloned IS guaranteed identical.
        let cloned = a.clone();
        assert_eq!(a.fingerprint(), cloned.fingerprint());
        // Cross-instance: must NOT match (different Arc ptrs).
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_stable_through_string_body_bytes() {
        // Inside a free-form string body, ASCII bytes that don't
        // trigger escape / close keep the parser in StringBody —
        // fingerprint must be invariant so the mask cache survives
        // every char of a long content field. (This is the dominant
        // cache-hit path in production: most tokens emitted are
        // string body chars.)
        let mut s = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string"}}
        }));
        // Walk into the value position.
        let _ = s.advance_bytes(b"{\"x\":\"");
        let fp_before = s.fingerprint();
        let _ = s.advance_bytes(b"hello, world. Some content here.");
        let fp_after = s.fingerprint();
        assert_eq!(
            fp_before, fp_after,
            "string body bytes shouldn't change fingerprint"
        );
    }

    #[test]
    fn fingerprint_changes_on_structural_transition() {
        // Entering an object → fingerprint shifts.
        let mut s = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string"}}
        }));
        let fp0 = s.fingerprint();
        let _ = s.advance_bytes(b"{");
        let fp1 = s.fingerprint();
        assert_ne!(fp0, fp1, "fingerprint must change on `{{` (AwaitValue → Object)");

        // Object → in-key — another structural shift.
        let _ = s.advance_bytes(b"\"");
        let fp2 = s.fingerprint();
        assert_ne!(fp1, fp2, "fingerprint must change on `\"` (AwaitKey → InKey)");
    }

    /// Regression for the 2026-05-17 SEP-pipeline profiling false-
    /// negative incident: the in-house JsonConstraint's mask cache was
    /// keyed by `near_cap = char_count >= max_length - 1`, collapsing
    /// every `char_count` from `0` to `max_length - 2` into a single
    /// fingerprint. A mask computed at `char_count = 0` (any multi-
    /// char token fits) was reused at `char_count = max_length - 30`,
    /// where the 10-char token it had marked valid would overflow the
    /// cap. The per-candidate validator catches the overflow at
    /// `advance_bytes`, but the cached mask doesn't re-run that
    /// check — it just trusts the cached `valid[]` array.
    ///
    /// Symptom in production: ~30-50% of Phase 1 chat calls against
    /// Qwen3.6-35B-A3B emit a token that overflows a string cap, the
    /// `accept()` self-check fires the EOS-latch warning, the next
    /// `mask()` call forces termination, the response is structurally
    /// invalid, and the runner dispatches a phase1_terse retry —
    /// burning ~22% of total pipeline wall on extraneous LLM work.
    ///
    /// This test pins the bug: identical schemas at different
    /// `char_count` values inside a capped string field should NOT
    /// share a fingerprint when their `(max_length - char_count)`
    /// remaining-room buckets would yield different validity masks
    /// for the same vocab.
    ///
    /// Fix landed in the companion edit: hash `char_count` directly
    /// (not just `near_cap`) when `max_length` is set, so each room-
    /// value gets its own cache entry. Memory cost: up to
    /// `max_length` cache entries per string field per request,
    /// bounded by the request's schema shape.
    #[test]
    fn fingerprint_distinguishes_capped_string_states_by_remaining_room() {
        // Free-form string with maxLength = 50 — small enough that
        // boundary cases hit quickly, large enough that "near_cap=false"
        // covers char_count values where validity actually differs.
        //
        // Compile ONCE so all three states share the same Schema Arc
        // identities — matches production, where one request's compiled
        // schema is used for the whole streaming sample. Per-state
        // recompiles produce divergent fingerprints for spurious Arc-ptr
        // reasons (not validity-relevant), which would mask the bug.
        let base = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string","maxLength":50}}
        }));
        let mut s_low = base.clone();
        let mut s_mid = base.clone();
        let mut s_high = base.clone();
        // Walk all three into a string-body state at different
        // char_count values: 1, 30, 45.
        let _ = s_low.advance_bytes(b"{\"x\":\"a");
        let _ = s_mid.advance_bytes(b"{\"x\":\"");
        let _ = s_mid.advance_bytes(&[b'a'; 30]);
        let _ = s_high.advance_bytes(b"{\"x\":\"");
        let _ = s_high.advance_bytes(&[b'a'; 45]);

        // All three are in `Frame::StringAny` with the same schema,
        // differing only in `char_count` (1, 30, 45). Cap is 50.
        let fp_low = s_low.fingerprint();
        let fp_mid = s_mid.fingerprint();
        let fp_high = s_high.fingerprint();

        // Per the bug: a 10-byte token like " adherence" is valid at
        // char_count=1 (room=49) but INVALID at char_count=45 (room=5).
        // If fp_low == fp_high, the cache will reuse the mask from
        // char_count=1 at char_count=45, allowing the model to sample
        // a token that overflows.
        let valid_low = {
            let mut probe = s_low.clone();
            probe.advance_bytes(b" adherence")
        };
        let valid_high = {
            let mut probe = s_high.clone();
            probe.advance_bytes(b" adherence")
        };
        assert_eq!(
            valid_low,
            ParseStatus::Incomplete,
            "10-char token must be valid at char_count=1 (room=49)"
        );
        assert_eq!(
            valid_high,
            ParseStatus::Invalid,
            "10-char token must be invalid at char_count=45 (room=5)"
        );

        // The fingerprints MUST differ when the byte-validity differs.
        // Today (pre-fix) they collide: both hash near_cap=false.
        // After the fix: char_count enters the hash directly so fp_low
        // != fp_mid != fp_high (or at least fp_low != fp_high).
        assert_ne!(
            fp_low, fp_high,
            "fingerprint must distinguish char_count=1 from char_count=45 — \
             they produce different validity masks (10-char tokens valid at 1, invalid at 45)"
        );
        // fp_mid (char_count=30, room=20) is also distinguishable —
        // a 25-char token would be valid at fp_low but invalid at fp_mid.
        // This is the "many-bucket" character: not just near-cap vs
        // not-near-cap; every char of room can flip validity for some
        // token.
        assert_ne!(
            fp_low, fp_mid,
            "fingerprint must distinguish char_count=1 from char_count=30 — \
             tokens between 20 and 49 chars long flip validity here"
        );
    }

    #[test]
    fn fingerprint_ignores_array_count_below_cap() {
        // `Array.count` increments after each item but valid next
        // bytes are constant (until count hits max_items). Excluding
        // count from the fingerprint lets the cache survive across
        // every array item.
        let mut s = state_for(json!({
            "type":"object","required":["xs"],
            "properties":{"xs":{"type":"array","items":{"type":"string"}}}
        }));
        let _ = s.advance_bytes(b"{\"xs\":[\"a\",");
        let fp1 = s.fingerprint();
        let _ = s.advance_bytes(b"\"b\",");
        let fp2 = s.fingerprint();
        let _ = s.advance_bytes(b"\"c\",");
        let fp3 = s.fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp2, fp3);
    }

    #[test]
    fn fingerprint_distinguishes_object_position_across_kv_pairs() {
        // After each kv pair completes, `next_idx` advances to the
        // next declared property. The fingerprint MUST reflect this
        // because the set of valid next key prefixes depends on it:
        // after "a" is consumed the model can only emit a prefix of
        // "b" or "c", not "a" again. Pre-this-fix the fingerprint
        // excluded next_idx and produced a real divergence —
        // `InKey { accumulated: [c] }` was mask-permitted after "a"
        // because the cached mask was computed at next_idx=0 where
        // the schema's first property started with "c". See the
        // `fingerprint_changes_when_next_idx_advances` test below
        // for the canonical reproducer of that bug.
        let mut s = state_for(json!({
            "type":"object",
            "required":["a","b","c"],
            "properties":{
                "a":{"type":"string"},
                "b":{"type":"string"},
                "c":{"type":"string"}
            },
            "additionalProperties": false
        }));
        let _ = s.advance_bytes(b"{\"a\":\"x\"");
        let fp1 = s.fingerprint();
        let _ = s.advance_bytes(b",\"b\":\"y\"");
        let fp2 = s.fingerprint();
        // Both states are in `Object { sub: InValue { chosen } }`
        // BUT with different `next_idx` values (1 vs 2 after the
        // respective bumps). They MUST differ — same fingerprint
        // would mean the mask cache can reuse a next_idx=0 mask
        // for a next_idx=1 state, the exact pollution that allowed
        // the masker-divergence bug.
        assert_ne!(
            fp1, fp2,
            "fingerprint must encode next_idx so the mask cache \
             cannot reuse a next_idx=N mask for a next_idx=N+1 state"
        );
    }

    #[test]
    fn fingerprint_changes_when_next_idx_advances_for_in_key_state() {
        // Canonical reproducer for the cache-pollution bug fixed
        // 2026-05-12: with schema required=["content","evidence"]
        // the model emitted `{"content":"...","co` and the mask
        // permitted the `c` byte even though "co" is not a prefix
        // of "evidence". Root cause: the fingerprint at
        // `InKey { accumulated: [] }` was identical for next_idx=0
        // and next_idx=1, so the mask computed when "content" was
        // valid got reused when only "evidence" should be.
        //
        // The fingerprint must differ between these states. Once
        // the cache stores them separately, the InKey mask at
        // next_idx=1 correctly rejects `c` (not a prefix of
        // "evidence"), preventing the duplicate-key emission that
        // tripped `emitted_invalid` mid-generation.
        let mut s_fresh = state_for(json!({
            "type":"object","required":["content","evidence"],
            "properties":{
                "content":{"type":"string"},
                "evidence":{"type":"string"}
            }
        }));
        // State A: just opened object, about to read first key.
        // next_idx=0, sub=AwaitFirstKeyOrClose → after `"`, InKey [].
        let _ = s_fresh.advance_bytes(b"{\"");
        let fp_at_key_idx0 = s_fresh.fingerprint();

        let mut s_advanced = state_for(json!({
            "type":"object","required":["content","evidence"],
            "properties":{
                "content":{"type":"string"},
                "evidence":{"type":"string"}
            }
        }));
        // State B: same shape (InKey { accumulated: [] }) but
        // next_idx=1 (after content was consumed).
        let _ = s_advanced.advance_bytes(b"{\"content\":\"x\",\"");
        let fp_at_key_idx1 = s_advanced.fingerprint();

        assert_ne!(
            fp_at_key_idx0, fp_at_key_idx1,
            "InKey {{accumulated: []}} at next_idx=0 vs next_idx=1 \
             must have distinct fingerprints — they accept different \
             starting bytes (anything-prefix-of-content vs prefix-of-evidence)"
        );
    }

    #[test]
    fn fingerprint_differs_at_string_max_length_cap() {
        // When `max_length` is set and we're at/near the cap, valid
        // next bytes change (only `"` is permitted). Fingerprint
        // must reflect this so the cache doesn't reuse below-cap
        // bitmask after the cap.
        let mut s = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string","maxLength":3}}
        }));
        let _ = s.advance_bytes(b"{\"x\":\"a");
        let fp_below = s.fingerprint();
        let _ = s.advance_bytes(b"bc");  // now at maxLength=3
        let fp_at_cap = s.fingerprint();
        assert_ne!(
            fp_below, fp_at_cap,
            "fingerprint must shift when StringAny hits max_length"
        );
    }

    #[test]
    fn fingerprint_ignores_string_char_count_below_cap() {
        // Without max_length, char_count is purely monotonic and
        // doesn't change valid-next-byte set. Fingerprint stable
        // across char emissions.
        let mut s = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string"}}
        }));
        let _ = s.advance_bytes(b"{\"x\":\"a");
        let fp1 = s.fingerprint();
        let _ = s.advance_bytes(b"bcdefghijklmnop");
        let fp2 = s.fingerprint();
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_cycle_returns_to_same_value_for_recurring_states() {
        // Multi-entry cache correctness contract: parsing two string
        // bodies separated by structural transitions must produce the
        // SAME fingerprint inside each body, so the cache reuses the
        // first body's mask for the second body without recomputing.
        //
        // Pre-this-change the single-Option cache wiped on every
        // transition; the test above (`fingerprint_stable_through_
        // string_body_bytes`) proved the fingerprint was stable within
        // one body, but not that two SEPARATE bodies share a key.
        // That's the load-bearing invariant for the multi-entry cache:
        // a recurring state must return to its prior fingerprint.
        let mut s = state_for(json!({
            "type":"object","required":["a","b"],
            "properties":{
                "a":{"type":"string"},
                "b":{"type":"string"}
            }
        }));
        // Advance into the first string body.
        let _ = s.advance_bytes(b"{\"a\":\"");
        let fp_in_a_body = s.fingerprint();
        // Close `"a"` field and traverse to inside `"b"`.
        let _ = s.advance_bytes(b"alpha\",\"b\":\"");
        let fp_in_b_body = s.fingerprint();
        // Different schema position (different Object.sub.chosen
        // history) — fingerprints across distinct properties may
        // legitimately differ; the contract we depend on is that
        // ANY single property's body fingerprint is consistent
        // across re-entries. Add a second value of the same field
        // through array context to make the re-entry contract direct.
        let mut s2 = state_for(json!({
            "type":"array",
            "items": {"type":"string"}
        }));
        let _ = s2.advance_bytes(b"[\"first");
        let fp_first_body = s2.fingerprint();
        let _ = s2.advance_bytes(b"\",\"second");
        let fp_second_body = s2.fingerprint();
        assert_eq!(
            fp_first_body, fp_second_body,
            "two string-body states inside the same array-of-strings \
             schema must share a fingerprint so the multi-entry cache \
             reuses the mask"
        );
        // Sanity: the inter-body fingerprints differ from each other
        // when properties are different (per-property `chosen` matters
        // for AfterColon but not for InValue body — these are inside
        // the value, so should DIFFER only by schema-path. Use object
        // schema with distinct types to make this real):
        let _ = fp_in_a_body;
        let _ = fp_in_b_body;
    }

    #[test]
    fn validate_accepts_leading_whitespace_before_root() {
        // step_await_value consumes leading whitespace, so validate()
        // must agree. Without alignment, the masker's emitted_invalid
        // latch fires spuriously when the model samples a single
        // space token (the canonical instant-EOG trap on multi-EOG
        // models like Qwen3.5).
        let s = compile_schema(&serde_json::json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string"}}
        }))
        .unwrap();
        assert_eq!(validate(&s, b" "), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b"\n"), ParseStatus::Incomplete);
        assert_eq!(validate(&s, b" {\"x\":\"a\"}"), ParseStatus::Complete);
        assert_eq!(validate(&s, b"{\"x\":\"a\"}\n"), ParseStatus::Complete);
    }

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

    /// Regression: A3B-MoE early-EOS failure family.
    ///
    /// Live evidence (2026-05-04 daemon log under
    /// Nemotron-Cascade-2-30B-A3B running atlas Phase 1 on al-Farabi):
    /// the masker accepted a multi-byte token `b" ],\n"` after the
    /// last typed property (claims) had been consumed. The full-buffer
    /// validator correctly rejected the resulting prefix as Invalid
    /// (no more properties allowed under `additionalProperties: false`),
    /// the JsonConstraint::accept diagnostic latched to EOS-only mode,
    /// and the model was forced to emit EOS — truncating the JSON
    /// document mid-write. The fix added a `more_pairs_possible` check
    /// in `step_object`'s `AwaitCommaOrClose → b','` arm so the masker
    /// matches `parse_object`'s comma rule (line 437/448).
    #[test]
    fn comma_after_last_typed_property_is_invalid_in_both_paths() {
        // Two-property closed schema: `a` then `b`. A `,` after the
        // value of `b` has no more pairs to fill — both validator
        // paths must agree this is Invalid.
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"}
            }
        }))
        .unwrap();
        let prefix = br#"{"a":"x","b":"y","#;
        // Pre-fix: validate() returned Invalid but validate_incremental
        // returned Incomplete — the disagreement triggered the latch.
        assert_eq!(validate(&s, prefix), ParseStatus::Invalid);
        assert_eq!(validate_incremental(&s, prefix), ParseStatus::Invalid);
    }

    /// Mirror of the live failure: required+optional mix with a Phase
    /// 1-shaped property compilation order. After the last optional
    /// has been picked, a trailing comma must be Invalid in both paths.
    #[test]
    fn comma_after_last_optional_property_invalid_in_both_paths() {
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "section_id": {"type": "string"},
                "questions_raised": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "claims": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["section_id", "questions_raised"]
        }))
        .unwrap();
        // Required pair, required pair, then the LAST optional (claims) —
        // post-compile order: [section_id, questions_raised, claims].
        // After `claims:[]` the typed cursor is past end; `,` is Invalid.
        let prefix = br#"{"section_id":"x","questions_raised":["q"],"claims":[],"#;
        assert_eq!(validate(&s, prefix), ParseStatus::Invalid);
        assert_eq!(validate_incremental(&s, prefix), ParseStatus::Invalid);
    }

    /// Negative control: same shape but with `additionalProperties: true`
    /// — a trailing comma is still permitted because more pairs ARE
    /// possible (any additional key can follow).
    #[test]
    fn comma_after_last_typed_property_valid_when_additional_true() {
        let s = compile_schema(&json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"}
            }
        }))
        .unwrap();
        let prefix = br#"{"a":"x","b":"y","#;
        assert_eq!(validate(&s, prefix), ParseStatus::Incomplete);
        assert_eq!(validate_incremental(&s, prefix), ParseStatus::Incomplete);
    }

    // ---- Timing instrumentation env parsing ----

    fn fake_env(key: &'static str, value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        let v = value.map(|s| s.to_string());
        move |k: &str| if k == key { v.clone() } else { None }
    }

    #[test]
    fn timing_disabled_when_env_unset() {
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", None)).is_none());
    }

    #[test]
    fn timing_enabled_when_env_one() {
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some("1"))).is_some());
    }

    #[test]
    fn timing_enabled_when_env_true_case_insensitive() {
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some("True"))).is_some());
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some("TRUE"))).is_some());
    }

    #[test]
    fn timing_disabled_when_env_zero_or_garbage() {
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some("0"))).is_none());
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some("yes"))).is_none());
        assert!(super::build_timing(fake_env("SOVEREIGN_GRAMMAR_TIMING", Some(""))).is_none());
    }

    // ─── x-asciiExtended (CJK-block flag) ──────────────────────────
    //
    // The drift report writer observed occasional non-Latin token
    // drift ("或", "生成") leaking into JSON string bodies under
    // grammar-constrained generation. The default StringAny char-set
    // accepts any UTF-8; `x-asciiExtended: true` restricts strings to
    // ASCII + 2-byte UTF-8 (codepoints U+0000–U+07FF), which keeps
    // accented Latin names like "café"/"Björk" while rejecting CJK
    // and other 3+ byte UTF-8 scripts.

    fn ascii_extended_schema(max_length: Option<u64>) -> serde_json::Value {
        let mut prop = serde_json::json!({"type": "string", "x-asciiExtended": true});
        if let Some(m) = max_length {
            prop["maxLength"] = serde_json::json!(m);
        }
        serde_json::json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": prop}
        })
    }

    #[test]
    fn ascii_extended_compiles_from_keyword() {
        let s = compile_schema(&ascii_extended_schema(None)).unwrap();
        // Drill into the property schema and verify the flag landed.
        let Schema::Object { properties, .. } = s else {
            panic!("expected object");
        };
        let (_, inner) = &properties[0];
        match inner {
            Schema::StringAny {
                ascii_extended, ..
            } => assert!(*ascii_extended, "x-asciiExtended must propagate"),
            other => panic!("expected StringAny, got {other:?}"),
        }
    }

    #[test]
    fn ascii_extended_default_false_when_keyword_absent() {
        let s = compile_schema(&json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string"}}
        }))
        .unwrap();
        let Schema::Object { properties, .. } = s else {
            panic!();
        };
        match &properties[0].1 {
            Schema::StringAny { ascii_extended, .. } => assert!(!*ascii_extended),
            _ => panic!(),
        }
    }

    #[test]
    fn ascii_extended_rejects_cjk_leading_byte() {
        // U+6216 ("或") encodes to E6 88 96 in UTF-8 — the leading
        // byte E6 must be rejected at the StringAny body position.
        let mut s = state_for(ascii_extended_schema(None));
        assert_eq!(s.advance_bytes(b"{\"x\":\""), ParseStatus::Incomplete);
        let cjk = [0xE6_u8, 0x88, 0x96];
        assert_eq!(
            s.advance_bytes(&cjk),
            ParseStatus::Invalid,
            "0xE6 leading byte must be rejected under x-asciiExtended"
        );
    }

    #[test]
    fn ascii_extended_accepts_latin1_supplement() {
        // U+00E9 ("é") encodes to C3 A9 — a 2-byte UTF-8 sequence
        // which must pass under x-asciiExtended (accented Latin
        // names are legitimate content).
        let mut s = state_for(ascii_extended_schema(None));
        let _ = s.advance_bytes(b"{\"x\":\"");
        let cafe = b"caf\xC3\xA9";
        assert_eq!(s.advance_bytes(cafe), ParseStatus::Incomplete);
        assert_eq!(s.advance_bytes(b"\"}"), ParseStatus::Complete);
    }

    #[test]
    fn ascii_extended_accepts_plain_ascii() {
        let mut s = state_for(ascii_extended_schema(None));
        let _ = s.advance_bytes(b"{\"x\":\"");
        assert_eq!(s.advance_bytes(b"hello world"), ParseStatus::Incomplete);
        assert_eq!(s.advance_bytes(b"\"}"), ParseStatus::Complete);
    }

    #[test]
    fn ascii_extended_rejects_unicode_escape_for_cjk() {
        // `中` is U+4E2D ("中"). The first hex digit `4` already
        // implies codepoint >= 0x4000, well above the 0x0800 cutoff,
        // so the validator must reject as soon as we see `4`.
        let mut s = state_for(ascii_extended_schema(None));
        let _ = s.advance_bytes(b"{\"x\":\"");
        assert_eq!(s.advance_bytes(b"\\u"), ParseStatus::Incomplete);
        assert_eq!(
            s.advance_bytes(b"4"),
            ParseStatus::Invalid,
            "first hex nibble >= 1 means cp >= 0x1000 — must reject"
        );
    }

    #[test]
    fn ascii_extended_rejects_unicode_escape_at_boundary() {
        // `ࠀ` is the exact threshold — must reject on the second
        // hex digit (`8`) once the first (`0`) is consumed.
        let mut s = state_for(ascii_extended_schema(None));
        let _ = s.advance_bytes(b"{\"x\":\"");
        assert_eq!(s.advance_bytes(b"\\u0"), ParseStatus::Incomplete);
        assert_eq!(s.advance_bytes(b"8"), ParseStatus::Invalid);
    }

    #[test]
    fn ascii_extended_accepts_unicode_escape_for_2byte_codepoint() {
        // `é` ("é") is 2-byte UTF-8 — must pass even with the
        // flag set. The first 3 nibbles (0, 0, E) keep cp < 0x0800.
        let mut s = state_for(ascii_extended_schema(None));
        let _ = s.advance_bytes(b"{\"x\":\"");
        assert_eq!(s.advance_bytes(b"\\u00E9"), ParseStatus::Incomplete);
        assert_eq!(s.advance_bytes(b"\"}"), ParseStatus::Complete);
    }

    #[test]
    fn ascii_extended_off_still_accepts_cjk() {
        // Default behaviour preserved: schemas without the keyword
        // still accept CJK characters (some Wikipedia articles
        // legitimately quote Chinese / Japanese terms).
        let mut s = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string"}}
        }));
        let _ = s.advance_bytes(b"{\"x\":\"");
        let cjk = [0xE6_u8, 0x88, 0x96];
        assert_eq!(s.advance_bytes(&cjk), ParseStatus::Incomplete);
        assert_eq!(s.advance_bytes(b"\"}"), ParseStatus::Complete);
    }

    // ─── Throughput bench: ascii_extended vs default ──────────────
    //
    // Run with:
    //   cargo test --release -p sovereign-inference --no-default-features \
    //     -- --ignored bench_ascii_extended_advance_throughput --nocapture
    //
    // `#[ignore]` so it doesn't slow the regular test suite. Reports
    // ns/byte for both configurations against a representative
    // engineering_atlas Phase 1 payload (claims array of 12 items).
    // The grammar mask's hot path is `ValidatorState::advance(byte)`;
    // the ascii_extended check adds one comparison per non-continuation
    // byte inside StringSub::InBody. Expected overhead < 5%.
    #[test]
    #[ignore]
    fn bench_ascii_extended_advance_throughput() {
        let payload = sample_engineering_payload();
        let schema_plain = json!({
            "type": "object",
            "required": ["claims"],
            "additionalProperties": false,
            "properties": {
                "claims": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["content", "code_anchors"],
                        "additionalProperties": false,
                        "properties": {
                            "content": {"type": "string"},
                            "code_anchors": {
                                "type": "array",
                                "items": {"type": "string"}
                            },
                            "evidence_excerpt": {"type": "string"}
                        }
                    }
                }
            }
        });
        let schema_restricted = json!({
            "type": "object",
            "required": ["claims"],
            "additionalProperties": false,
            "properties": {
                "claims": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["content", "code_anchors"],
                        "additionalProperties": false,
                        "properties": {
                            "content": {"type": "string", "x-asciiExtended": true},
                            "code_anchors": {
                                "type": "array",
                                "items": {"type": "string", "x-asciiExtended": true}
                            },
                            "evidence_excerpt": {"type": "string", "x-asciiExtended": true}
                        }
                    }
                }
            }
        });

        let compiled_plain = compile_schema(&schema_plain).unwrap();
        let compiled_restricted = compile_schema(&schema_restricted).unwrap();

        // Warmup
        for _ in 0..3 {
            run_one(&compiled_plain, payload.as_bytes());
            run_one(&compiled_restricted, payload.as_bytes());
        }

        let iters = 200usize;
        let bytes_per_iter = payload.len() as u64;
        let total_bytes = bytes_per_iter * iters as u64;

        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(run_one(
                &compiled_plain,
                payload.as_bytes(),
            ));
        }
        let plain_elapsed = t0.elapsed();

        let t1 = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(run_one(
                &compiled_restricted,
                payload.as_bytes(),
            ));
        }
        let restricted_elapsed = t1.elapsed();

        let plain_ns_per_byte =
            plain_elapsed.as_nanos() as f64 / total_bytes as f64;
        let restricted_ns_per_byte =
            restricted_elapsed.as_nanos() as f64 / total_bytes as f64;
        let overhead_pct =
            (restricted_ns_per_byte / plain_ns_per_byte - 1.0) * 100.0;

        eprintln!(
            "bench_ascii_extended_advance_throughput:\n  \
             payload: {} bytes × {} iters = {} total bytes\n  \
             plain      : {:>8.2} ns/byte ({:>6} µs total)\n  \
             restricted : {:>8.2} ns/byte ({:>6} µs total)\n  \
             overhead   : {:>+6.2}%",
            payload.len(),
            iters,
            total_bytes,
            plain_ns_per_byte,
            plain_elapsed.as_micros(),
            restricted_ns_per_byte,
            restricted_elapsed.as_micros(),
            overhead_pct,
        );

        // Soft regression gate: if ascii_extended somehow doubles the
        // hot-path cost something has gone very wrong (e.g. a UTF-8
        // decode crept into the inner loop). 50% would already be
        // pathological; we set the bar high enough that noise on a
        // loaded laptop won't flake.
        assert!(
            overhead_pct < 50.0,
            "ascii_extended overhead {overhead_pct:.2}% exceeds 50% — \
             check that the InBody hot path didn't acquire allocations"
        );
    }

    fn run_one(schema: &Schema, payload: &[u8]) -> ParseStatus {
        let mut state = ValidatorState::new(schema.clone());
        state.advance_bytes(payload)
    }

    /// Synthetic payload modelled on engineering_atlas Phase 1 output —
    /// 12 claims with content + code_anchors + evidence_excerpt. Pure
    /// ASCII (the realistic post-fix case) so both configurations see
    /// the same bytes; the difference is purely the per-byte branch.
    fn sample_engineering_payload() -> String {
        let mut claims = Vec::with_capacity(12);
        for i in 0..12 {
            claims.push(format!(
                r#"{{"content":"The watcher in `lint_runner` debounces file events at 250ms and reruns cargo check across all configured packages; this is claim {i} of the synthetic batch and contains enough body text to exercise the StringAny InBody hot path for several hundred bytes per claim.","code_anchors":["sovereign/crates/sovereign-tools/src/code/lint_watcher.rs:142","sovereign/crates/sovereign-tools/src/code/lint_watcher.rs:198"],"evidence_excerpt":"if debounce.elapsed() >= Duration::from_millis(250) {{ trigger_rerun(); }}"}}"#
            ));
        }
        format!(r#"{{"claims":[{}]}}"#, claims.join(","))
    }

    // ─── non_latin_denylist bitmap (free-form sampling path) ───────
    //
    // The grammar mask only fires on `structured_output` requests.
    // To cover free-form chat / completion paths, `ConstrainedSampler`
    // optionally consumes a vocab-sized bitmap built by
    // `build_non_latin_denylist`. These tests pin the construction
    // invariants — the wiring into `ConstrainedSampler::sample` is
    // exercised by integration runs (requires a loaded LlamaModel,
    // out of scope here).

    #[test]
    fn non_latin_denylist_flags_cjk_lead_byte() {
        // "或" → E6 88 96. The lead byte E6 lands in 0xE0..=0xF7,
        // so the token must be flagged.
        let vocab = vec![
            b"hello".to_vec(),         // ASCII only → keep
            vec![0xE6, 0x88, 0x96],    // CJK "或" → block
        ];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false, true]);
    }

    #[test]
    fn non_latin_denylist_keeps_latin1_supplement() {
        // "café" → 63 61 66 C3 A9. C3 is a 2-byte UTF-8 lead
        // (0xC2..=0xDF), so it must NOT be flagged — accented Latin
        // is legitimate content.
        let vocab = vec![vec![0x63, 0x61, 0x66, 0xC3, 0xA9]];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false]);
    }

    #[test]
    fn non_latin_denylist_keeps_pure_continuation_tokens() {
        // A token that is pure UTF-8 continuation bytes (0x80..=0xBF)
        // is a tail-half of a multi-byte sequence. Once the lead-byte
        // tokens are blocked, these tails have no preceding context to
        // attach to and the BPE distribution won't pick them in
        // isolation. We deliberately don't flag them — flagging would
        // also block tokens like the lower halves of accented Latin
        // chars when they appear standalone, which has no upside.
        let vocab = vec![vec![0x88, 0x96]];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false]);
    }

    #[test]
    fn non_latin_denylist_flags_4byte_lead() {
        // U+1F600 (😀, emoji) → F0 9F 98 80. The lead byte F0 is in
        // 0xE0..=0xF7 → flagged. (Emoji aren't the primary target
        // but they're 4-byte UTF-8 and the heuristic is the same;
        // operators who want emoji should leave the env flag off.)
        let vocab = vec![vec![0xF0, 0x9F, 0x98, 0x80]];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![true]);
    }

    #[test]
    fn non_latin_denylist_flags_mixed_payload_with_inner_lead() {
        // BPE tokens can be byte fragments that span chars. A token
        // encoding `[ascii][CJK-lead][cont]...` must be flagged even
        // though it doesn't START with a CJK lead. (Real example: a
        // BPE token like b" 中" — the leading space is ASCII, then
        // the CJK char follows.)
        let vocab = vec![vec![b' ', 0xE4, 0xB8, 0xAD]]; // " 中"
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![true]);
    }

    #[test]
    fn non_latin_denylist_handles_empty_token() {
        // Some special / control tokens render as empty bytes —
        // must not blow up and must default to NOT flagged.
        let vocab = vec![Vec::<u8>::new()];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false]);
    }

    #[test]
    fn ascii_extended_fingerprint_differs_from_default() {
        // The flag MUST be part of the fingerprint — otherwise the
        // mask cache could reuse a permissive mask for a restricted
        // state and let CJK bytes through.
        let s_default = state_for(json!({
            "type":"object","required":["x"],
            "properties":{"x":{"type":"string"}}
        }));
        let s_restricted = state_for(ascii_extended_schema(None));
        // Different Arc identities anyway, so they'd differ. The
        // meaningful check is that two `Schema::StringAny` values
        // with different `ascii_extended` produce different
        // schema_fingerprint contributions.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let mut h1 = DefaultHasher::new();
        super::schema_fingerprint(
            &Schema::StringAny {
                max_length: None,
                ascii_extended: false,
                prefix: None,
            },
            &mut h1,
        );
        let mut h2 = DefaultHasher::new();
        super::schema_fingerprint(
            &Schema::StringAny {
                max_length: None,
                prefix: None,
                ascii_extended: true,
            },
            &mut h2,
        );
        assert_ne!(h1.finish(), h2.finish());
        // And the full validator fingerprints differ too (sanity).
        assert_ne!(s_default.fingerprint(), s_restricted.fingerprint());
    }

    // ─── forced_next_token / jump-forward tests ───────────────────
    //
    // Pin the single-legal-token short-circuit that future jump-forward
    // decoding reads. Tests construct a `JsonConstraint` against a
    // hand-crafted vocab so we control which tokens are legal at each
    // FSM state without needing a real model.

    /// Build a JsonConstraint with a synthetic vocab. Available only in
    /// the test module — production code goes through
    /// `JsonConstraint::new` which derives vocab from `LlamaModel`.
    fn constraint_with_vocab(
        schema_json: serde_json::Value,
        vocab_bytes: Vec<Vec<u8>>,
        eog_tokens: Vec<i32>,
    ) -> JsonConstraint {
        let compiled = compile_schema(&schema_json).unwrap();
        let state = ValidatorState::new(compiled.clone());
        let vocab_trie = Arc::new(VocabTrie::new(&vocab_bytes));
        JsonConstraint {
            schema: compiled,
            emitted: Vec::new(),
            state,
            vocab_bytes: Arc::new(vocab_bytes),
            vocab_trie,
            eos_token: 0,
            eog_tokens,
            emitted_invalid: false,
            mask_cache: HashMap::new(),
            mask_cache_order: Vec::new(),
            timing: None,
        }
    }

    /// Vocab that lets us reach unambiguous + ambiguous states on the
    /// same enum schema. Token-id layout:
    ///   0 `{`         1 `}`         2 `"`         3 `"x`
    ///   4 `:`         5 `a`         6 `b`         7 `xy` (off-schema)
    fn jump_fwd_vocab() -> Vec<Vec<u8>> {
        vec![
            b"{".to_vec(),
            b"}".to_vec(),
            b"\"".to_vec(),
            b"\"x".to_vec(),
            b":".to_vec(),
            b"a".to_vec(),
            b"b".to_vec(),
            b"xy".to_vec(),
        ]
    }

    /// Schema that forces a single byte (`a`) at the value position via
    /// a one-element string enum. After advancing into the value
    /// position, only the `a` token is legal — exactly the single-
    /// survivor case jump-forward is meant to detect.
    fn jump_fwd_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        })
    }

    #[test]
    fn forced_next_token_returns_single_survivor_in_deterministic_state() {
        let mut c = constraint_with_vocab(jump_fwd_schema(), jump_fwd_vocab(), vec![]);
        // Advance directly through the parser; we control the bytes
        // emitted so we don't need a sampler in the loop.
        let st = c.state.advance_bytes(b"{\"x\":\"");
        assert!(
            !matches!(st, ParseStatus::Invalid),
            "fixture bytes must not poison the FSM"
        );
        // Only token 5 (`a`) survives the mask at this position — the
        // value body must start with the enum's first byte.
        assert_eq!(c.forced_next_token(), Some(LlamaToken(5)));
    }

    #[test]
    fn forced_next_token_returns_none_in_ambiguous_state() {
        let mut c = constraint_with_vocab(jump_fwd_schema(), jump_fwd_vocab(), vec![]);
        // After `{` the FSM expects a key prefix — both token 2 (`"`)
        // and token 3 (`"x`) are valid prefixes. Two survivors → None.
        let _ = c.state.advance_bytes(b"{");
        assert_eq!(c.forced_next_token(), None);
    }

    #[test]
    fn forced_next_token_returns_none_when_latched_invalid() {
        let mut c = constraint_with_vocab(jump_fwd_schema(), jump_fwd_vocab(), vec![]);
        // Drive to a state that would otherwise have a single survivor,
        // then latch invalid. The unique-survivor short-circuit must
        // defer to the sampler's EOG handling.
        let _ = c.state.advance_bytes(b"{\"x\":\"");
        c.emitted_invalid = true;
        assert_eq!(c.forced_next_token(), None);
    }

    #[test]
    fn forced_next_token_returns_none_when_buffer_is_complete() {
        let mut c = constraint_with_vocab(jump_fwd_schema(), jump_fwd_vocab(), vec![]);
        // Drive to a structurally complete buffer.
        let st = c.state.advance_bytes(b"{\"x\":\"a\"}");
        assert!(
            matches!(st, ParseStatus::Complete),
            "fixture must reach Complete; got {:?}",
            st
        );
        // EOG + whitespace tokens are legal here — the sampler picks.
        // forced_next_token must not short-circuit that choice.
        assert_eq!(c.forced_next_token(), None);
    }

    #[test]
    fn forced_next_token_populates_shared_mask_cache() {
        let mut c = constraint_with_vocab(jump_fwd_schema(), jump_fwd_vocab(), vec![]);
        let _ = c.state.advance_bytes(b"{\"x\":\"");
        let fp = c.state.fingerprint();
        assert!(!c.mask_cache.contains_key(&fp), "cache must start cold");
        let _ = c.forced_next_token();
        assert!(
            c.mask_cache.contains_key(&fp),
            "forced_next_token must pre-populate the mask cache so a \
             subsequent mask() call takes the hot path"
        );
        // And the cached entry's `single` field matches the returned
        // value — sanity on the share.
        assert_eq!(
            c.mask_cache.get(&fp).unwrap().single,
            Some(LlamaToken(5))
        );
    }

    // ─── ValidatorState byte-walk tests (Tier 2 building block) ──

    #[test]
    fn forced_next_byte_returns_some_in_deterministic_state() {
        // After advancing through `{"x":"`, the schema's enum value
        // "a" forces the next byte to be `a`. This is the byte-level
        // equivalent of the Tier 1 single-survivor check, but doesn't
        // depend on the vocab structure — we're asking the FSM
        // directly.
        let mut s = state_for(json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        }));
        let _ = s.advance_bytes(b"{\"x\":\"");
        assert_eq!(s.forced_next_byte(), Some(b'a'));
    }

    #[test]
    fn forced_next_byte_returns_none_when_multiple_bytes_legal() {
        // After `{`, the FSM expects either `"` (start key) or
        // whitespace before the key. Two legal byte families → None.
        let mut s = state_for(json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string"}}
        }));
        let _ = s.advance_bytes(b"{");
        assert_eq!(s.forced_next_byte(), None);
    }

    #[test]
    fn forced_next_byte_returns_none_when_root_complete() {
        let mut s = state_for(json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        }));
        let st = s.advance_bytes(b"{\"x\":\"a\"}");
        assert!(matches!(st, ParseStatus::Complete));
        assert_eq!(s.forced_next_byte(), None);
    }

    #[test]
    fn forced_byte_run_walks_deterministic_sequence() {
        // After `{"x":"`, we're inside the StringEnum body. The only
        // legal next byte is `a` (the enum value). After `a`, the
        // enum is satisfied so the only legal next byte is `"`
        // (close string). After that, the FSM is back in the object
        // expecting `,` or `}` with whitespace allowed — ambiguous.
        //
        // We start at `{"x":"` rather than `{"x":` because the
        // colon-to-value boundary allows whitespace, so multiple
        // bytes are legal there and the run would terminate empty.
        let mut s = state_for(json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        }));
        let _ = s.advance_bytes(b"{\"x\":\"");
        let run = s.forced_byte_run(64);
        assert_eq!(
            run, b"a\"",
            "forced run from inside the enum body must spell out the \
             enum value + close quote (then terminate at the \
             whitespace-ambiguous post-value position)"
        );
    }

    #[test]
    fn forced_byte_run_caps_at_max_bytes() {
        // Same fixture; cap below the natural run length of 2.
        let mut s = state_for(json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        }));
        let _ = s.advance_bytes(b"{\"x\":\"");
        let run = s.forced_byte_run(1);
        assert_eq!(run.len(), 1, "cap honored");
        assert_eq!(run, b"a", "cap takes the prefix of the natural run");
    }

    // ─── VocabTrie tests ─────────────────────────────────────────

    #[test]
    fn vocab_trie_longest_match_returns_longest_prefix_token() {
        // Vocab with overlapping prefixes: token 1 covers `,"`, token 2
        // covers `,"name`. `longest_match` must return token 2 (the
        // longer match) when the input contains both.
        let vocab = vec![
            b"".to_vec(),
            b",\"".to_vec(),
            b",\"name".to_vec(),
            b",".to_vec(),
        ];
        let trie = VocabTrie::new(&vocab);
        let (tok, consumed) = trie.longest_match(b",\"name\":\"...").unwrap();
        assert_eq!(tok, LlamaToken(2), "longest match wins over shorter alternative");
        assert_eq!(consumed, 6, "consumed = byte length of `,\"name`");
    }

    #[test]
    fn vocab_trie_longest_match_falls_back_to_shorter_when_no_full_match() {
        // Vocab has `,` (1 byte) and `,"name` (6 bytes). Input starts
        // with `,"o...` — `,"name` doesn't match (`o` != `n`), but `,"`
        // also doesn't match because we didn't add it. Should return
        // the shorter `,` match (1 byte).
        let vocab = vec![
            b"".to_vec(),
            b",".to_vec(),
            b",\"name".to_vec(),
        ];
        let trie = VocabTrie::new(&vocab);
        let (tok, consumed) = trie.longest_match(b",\"other").unwrap();
        assert_eq!(tok, LlamaToken(1));
        assert_eq!(consumed, 1, "single-byte match wins when longer path is interrupted");
    }

    #[test]
    fn vocab_trie_longest_match_returns_none_when_first_byte_unknown() {
        // Vocab has only `a` tokens; input starts with `b`.
        let vocab = vec![
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
        ];
        let trie = VocabTrie::new(&vocab);
        assert!(trie.longest_match(b"bc").is_none());
        assert!(trie.longest_match(b"").is_none());
    }

    #[test]
    fn vocab_trie_longest_match_handles_prefix_only_internal_node() {
        // `ab` is in vocab but `a` is not. Input is `ab...`. The trie
        // descends through the internal `a` node (no terminal there)
        // and lands on the `ab` terminal — that's the match.
        let vocab = vec![
            b"".to_vec(),
            b"ab".to_vec(),
        ];
        let trie = VocabTrie::new(&vocab);
        let (tok, consumed) = trie.longest_match(b"abc").unwrap();
        assert_eq!(tok, LlamaToken(1));
        assert_eq!(consumed, 2);
    }

    #[test]
    fn vocab_trie_longest_match_when_input_is_strict_prefix_of_token() {
        // Input is shorter than any vocab token but does descend the
        // trie. No terminal hit along the way → None.
        let vocab = vec![
            b"".to_vec(),
            b"abc".to_vec(),
        ];
        let trie = VocabTrie::new(&vocab);
        assert!(
            trie.longest_match(b"ab").is_none(),
            "no terminal along the descent — strict prefix doesn't match"
        );
    }

    // ─── forced_next_run tests (Tier 2 integration) ──────────────

    #[test]
    fn forced_next_run_emits_largest_matching_token() {
        // Vocab includes a single token covering `a"` (the whole
        // post-`{"x":"` forced run). Tier 2 must emit that one token,
        // not two single-byte tokens.
        let vocab = vec![
            b"{".to_vec(),       // 0
            b"}".to_vec(),       // 1
            b"\"".to_vec(),      // 2
            b":".to_vec(),       // 3
            b"a".to_vec(),       // 4
            b"a\"".to_vec(),     // 5  ← preferred (covers 2 bytes)
            b"x".to_vec(),       // 6
        ];
        let schema = json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        });
        let mut c = constraint_with_vocab(schema, vocab, vec![]);
        let _ = c.state.advance_bytes(b"{\"x\":\"");
        let run = c.forced_next_run(64);
        assert_eq!(run, vec![LlamaToken(5)], "must pick the longest matching token");
    }

    #[test]
    fn forced_next_run_chains_multiple_tokens_until_ambiguity() {
        // No multi-byte token spans the full `a"` run. Tier 2 should
        // emit two single-byte tokens (`a`, then `"`), advancing the
        // FSM between them.
        let vocab = vec![
            b"{".to_vec(),
            b"}".to_vec(),
            b"\"".to_vec(),    // 2
            b":".to_vec(),
            b"a".to_vec(),     // 4
        ];
        let schema = json!({
            "type": "object",
            "required": ["x"],
            "properties": {"x": {"type": "string", "enum": ["a"]}},
            "additionalProperties": false
        });
        let mut c = constraint_with_vocab(schema, vocab, vec![]);
        let _ = c.state.advance_bytes(b"{\"x\":\"");
        let run = c.forced_next_run(64);
        assert_eq!(
            run,
            vec![LlamaToken(4), LlamaToken(2)],
            "two single-byte tokens covering the forced byte run"
        );
    }

    #[test]
    fn forced_next_run_empty_when_state_ambiguous() {
        let vocab = jump_fwd_vocab();
        let mut c = constraint_with_vocab(jump_fwd_schema(), vocab, vec![]);
        let _ = c.state.advance_bytes(b"{");
        assert!(c.forced_next_run(64).is_empty());
    }

    #[test]
    fn forced_next_run_empty_when_latched_invalid() {
        let vocab = jump_fwd_vocab();
        let mut c = constraint_with_vocab(jump_fwd_schema(), vocab, vec![]);
        let _ = c.state.advance_bytes(b"{\"x\":\"");
        c.emitted_invalid = true;
        assert!(c.forced_next_run(64).is_empty());
    }

    #[test]
    fn forced_next_run_empty_when_buffer_complete() {
        let vocab = jump_fwd_vocab();
        let mut c = constraint_with_vocab(jump_fwd_schema(), vocab, vec![]);
        let _ = c.state.advance_bytes(b"{\"x\":\"a\"}");
        assert!(c.forced_next_run(64).is_empty());
    }

    #[test]
    fn forced_byte_run_empty_when_first_state_ambiguous() {
        // After `{` with multi-key schema, the FSM expects either a
        // key opening `"` or whitespace. Ambiguous → run empty.
        let mut s = state_for(json!({
            "type": "object",
            "required": ["a", "b"],
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"}
            }
        }));
        let _ = s.advance_bytes(b"{");
        let run = s.forced_byte_run(64);
        assert!(run.is_empty(), "ambiguous state must return empty run");
    }
}
