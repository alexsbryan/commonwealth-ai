// SPDX-License-Identifier: AGPL-3.0-or-later
//! `LlguidanceConstraint` — adapter between the upstream `llguidance`
//! grammar engine and our `ConstrainedSampler` byte-loop.
//!
//! The **sole** schema/grammar constraint engine since 2026-05-22; the
//! in-house `JsonConstraint` FSM it originally sat in parallel with
//! was retired (see `LLGUIDANCE_MIGRATION_AUDIT.md`). API shape:
//!
//! ```text
//! mask(data)        → clamp grammar-illegal candidates to -inf (prod path)
//! step()/allows()   → mask-compute + per-token query (probe path)
//! accept_llama(tok) → commit the sample, advance the parser
//! is_stopped()      → grammar satisfied, generation may end
//! failure()         → Some(cause) once the matcher has errored (latched)
//! ```
//!
//! Accepts an arbitrary Lark grammar with `%json {…}` rules and
//! top-level alternation (`start: text_branch | tool_envelope` closes
//! the agent-bench `parse_failed_envelope` + `loop_trap` classes), or
//! a bare JSON Schema via `from_schema_value`.
//!
//! Builds its own `TokTrie` from `vocab_cache::vocab_bytes_for(model)`
//! — the same per-model byte view the sampler loop observes. This is
//! load-bearing: the 2026-04-26 llguidance failure (silent fallthrough,
//! unconstrained decode) was vocab misalignment from the binding's own
//! token env. Sampler and matcher must share one vocab source.
//!
//! Factory caching: one `ParserFactory` per `LlamaModel` (keyed by
//! pointer identity, mirroring `vocab_cache`). Building the factory's
//! tokenizer/TokTrie is the expensive bit; sharing across requests
//! keeps per-request cost on the parser create + matcher step paths.
//!
//! ## Failure loudness contract (2026-06-11)
//!
//! Matcher errors LATCH: the first error logs ONE
//! `tracing::error!(target: "llguidance.health")` event and sets
//! `failure()`; every subsequent mask fails closed silently (debug
//! level — no per-token spam). The decode loops in
//! `embedded/model_slot.rs` poll `ConstrainedSampler::constraint_failure`
//! and abort the request instead of burning the token budget on
//! clamp-garbage until the deadline. Historical failure mode this
//! guards: April 2026's matcher errors surfaced only as an upstream
//! stderr `printf` while decode ran fully unconstrained — constraint
//! bugs masquerading as model bugs for four days of triage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;

use llguidance::{
    api::TopLevelGrammar,
    toktrie::{ApproximateTokEnv, SimpleVob, TokEnv, TokRxInfo, TokTrie},
    Matcher, ParserFactory,
};

use crate::llama::cpp::model::LlamaModel;
use crate::llama::cpp::token::data_array::LlamaTokenDataArray;
use crate::llama::cpp::token::LlamaToken;

#[derive(Debug, Error)]
pub enum LlgError {
    #[error("llguidance: parser create failed: {0}")]
    ParserCreate(String),
    #[error("llguidance: matcher error: {0}")]
    Matcher(String),
}

/// One per request. Wraps an llguidance `Matcher` against a per-model
/// `ParserFactory`. Holds the most recently computed mask between
/// `step()` and `allows()` calls so the sampler can query per-token
/// without re-running the parser.
pub struct LlguidanceConstraint {
    matcher: Matcher,
    /// `Some` after `step()`; cleared by `accept()`. Allowing a stale
    /// mask to be queried across an `accept()` boundary would let the
    /// sampler use an out-of-date validity bitmap on the next token.
    last_mask: Option<SimpleVob>,
    /// Latched on the first matcher error (`compute_mask` /
    /// `consume_token*`). Once set, `mask()` fails closed without
    /// re-driving the errored matcher, and the decode loop is expected
    /// to abort the request (see module doc "Failure loudness
    /// contract"). Never cleared — a matcher that has errored is not
    /// recoverable within a request.
    failed: Option<String>,
}

impl LlguidanceConstraint {
    /// Build a constraint from EITHER a Lark grammar string or a
    /// JSON Schema string. Detects format by first non-whitespace
    /// byte:
    ///   * `{` → JSON Schema (canonical tool-call entry, strict
    ///     closure enforcement via `TopLevelGrammar::from_json_schema`)
    ///   * anything else → Lark grammar
    ///
    /// 2026-05-21 finding (`from_json_schema_rejects_incomplete_envelope`
    /// test): `%json {schema}` inside Lark does NOT enforce schema
    /// closure as strictly as the direct `from_json_schema` entry.
    /// For tool-call envelopes we want strict closure — every
    /// envelope produced under grammar must be parseable JSON — so
    /// the schema path is the canonical first-principles route.
    /// Lark stays available for callers that genuinely need
    /// alternation (text vs structured), but the recommended
    /// pattern for tool calls is `from_json_schema(envelope_schema)`.
    pub fn new(grammar_or_schema: &str, model: &LlamaModel) -> Result<Self, LlgError> {
        Self::with_factory(&factory_for(model), grammar_or_schema)
    }

    /// Model-free constructor used by the in-module tests: builds a
    /// one-shot `ParserFactory` over a synthetic `TokEnv` so the
    /// differential tests can drive the REAL mask/accept path without
    /// a GGUF. Production goes through `new` (cached factory).
    #[cfg(test)]
    pub(crate) fn new_with_tok_env(
        tok_env: TokEnv,
        grammar_or_schema: &str,
    ) -> Result<Self, LlgError> {
        let factory = ParserFactory::new_simple(&tok_env)
            .map_err(|e| LlgError::ParserCreate(format!("test factory: {e}")))?;
        Self::with_factory(&factory, grammar_or_schema)
    }

    fn with_factory(factory: &ParserFactory, grammar_or_schema: &str) -> Result<Self, LlgError> {
        let trimmed = grammar_or_schema.trim_start();
        let grammar = if trimmed.starts_with('{') {
            let schema: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| LlgError::ParserCreate(format!("json schema parse: {e}")))?;
            TopLevelGrammar::from_json_schema(schema)
        } else {
            TopLevelGrammar::from_lark(grammar_or_schema.to_string())
        };
        let parser = factory.create_parser(grammar);
        let matcher = Matcher::new(parser);
        if matcher.is_error() {
            return Err(LlgError::ParserCreate(
                matcher.get_error().unwrap_or_else(|| "unknown".into()),
            ));
        }
        Ok(Self {
            matcher,
            last_mask: None,
            failed: None,
        })
    }

    /// Latched matcher-failure cause, if any. The decode loop polls
    /// this (via `ConstrainedSampler::constraint_failure`) and aborts
    /// the request — continuing past a failed matcher only produces
    /// fail-closed clamp-garbage until the deadline.
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Record a matcher error. First error is LOUD (single
    /// `tracing::error!` on the `llguidance.health` target — grep for
    /// that target when triaging "structured output suddenly garbage");
    /// repeats are debug-level so a 150K-token-budget request can't
    /// flood the journal (the pre-2026-06 behavior was a warn per
    /// sampled token: ten-thousand-line logs that buried the cause).
    fn record_failure(&mut self, op: &'static str, err: &str) {
        if self.failed.is_some() {
            tracing::debug!(op, error = %err, "llguidance: matcher error (already failed)");
            return;
        }
        tracing::error!(
            target: "llguidance.health",
            op,
            error = %err,
            "llguidance constraint engine FAILED — masks fail closed from here and the \
             decode loop aborts this request with an explicit error instead of emitting \
             unparseable output. If you are triaging \"model produces garbage under \
             structured_output\": this event is the cause, not the model. \
             SOVEREIGN_TRACE_LLGUIDANCE=1 traces per-token matcher state."
        );
        self.failed = Some(format!("{op}: {err}"));
    }

    /// Build a constraint from a JSON Schema value. Applies
    /// `default_additional_properties_false` to preserve the in-house
    /// `JsonConstraint` default before serialising and delegating to
    /// `new` (which routes through `TopLevelGrammar::from_json_schema`
    /// when the leading byte is `{`).
    ///
    /// This is the canonical entry point for migrating the 11
    /// `structured_output: Some(schema)` call sites that don't set
    /// `additionalProperties` explicitly. See
    /// `LLGUIDANCE_MIGRATION_AUDIT.md` §3.A.
    pub fn from_schema_value(
        schema: &serde_json::Value,
        model: &LlamaModel,
    ) -> Result<Self, LlgError> {
        let mut walked = schema.clone();
        default_additional_properties_false(&mut walked);
        let serialised = serde_json::to_string(&walked)
            .map_err(|e| LlgError::ParserCreate(format!("serialise walked schema: {e}")))?;
        Self::new(&serialised, model)
    }

    /// Compute the next-token mask. Call once per sampling step
    /// before any `allows()` queries.
    pub fn step(&mut self) -> Result<(), LlgError> {
        let mask = match self.matcher.compute_mask() {
            Ok(m) => m,
            Err(e) => {
                let msg = e.to_string();
                self.record_failure("compute_mask", &msg);
                return Err(LlgError::Matcher(msg));
            }
        };
        self.last_mask = Some(mask);
        Ok(())
    }

    /// Query whether a token id is allowed under the current mask.
    /// Returns `false` when `step()` has not been called since the
    /// last `accept()` — fail-closed is the safe default for a
    /// sampler that would otherwise let any token through.
    pub fn allows(&self, token_id: u32) -> bool {
        self.last_mask
            .as_ref()
            .map(|m| m.is_allowed(token_id))
            .unwrap_or(false)
    }

    /// Commit the sampled token to the parser. Clears the cached
    /// mask so a subsequent `allows()` without an intervening
    /// `step()` will fail closed.
    pub fn accept(&mut self, token_id: u32) -> Result<(), LlgError> {
        if let Err(e) = self.matcher.consume_token(token_id) {
            let msg = format!("token {token_id}: {e}");
            self.record_failure("consume_token", &msg);
            return Err(LlgError::Matcher(msg));
        }
        self.last_mask = None;
        Ok(())
    }

    /// True once the grammar is satisfied — the model is permitted
    /// to stop here. The sampler may still emit EOS or be cut off by
    /// the wall-cap.
    pub fn is_stopped(&self) -> bool {
        self.matcher.is_stopped()
    }

    /// JsonConstraint-shaped entry point for `ConstrainedSampler`.
    /// Computes the next-token mask via llguidance and clamps every
    /// disallowed candidate's logit to `-INFINITY` in place. Mirrors
    /// `JsonConstraint::mask` so the sampler integration is a
    /// straight enum-dispatch.
    ///
    /// **Stop-state passthrough.** Once the grammar reaches its
    /// accept state (`is_stopped() == true`), llguidance's
    /// `compute_mask` returns "parser stopped in compute_mask".
    /// The model is allowed to emit anything from here — typically
    /// the chat template's closing marker (`</tool_call>`) then EOS.
    /// Without this short-circuit, every post-acceptance token gets
    /// clamped → fail-closed → ten thousand warnings + a stuck
    /// streaming loop. Observed 2026-05-21 under the canonical
    /// `from_json_schema` path immediately after the envelope
    /// closed.
    pub fn mask(&mut self, data: &mut LlamaTokenDataArray) {
        // A latched failure never recovers within a request: fail
        // closed without re-driving the errored matcher. The decode
        // loop's `constraint_failure` poll aborts before this clamp
        // can matter, but the clamp stays as defence in depth — a
        // loop that forgets to poll must not decode unconstrained.
        if self.failed.is_some() {
            for entry in data.data.iter_mut() {
                entry.set_logit(f32::NEG_INFINITY);
            }
            self.last_mask = None;
            return;
        }
        if self.matcher.is_stopped() {
            self.last_mask = None;
            return;
        }
        let mask = match self.matcher.compute_mask() {
            Ok(m) => m,
            Err(e) => {
                self.record_failure("compute_mask", &e.to_string());
                for entry in data.data.iter_mut() {
                    entry.set_logit(f32::NEG_INFINITY);
                }
                self.last_mask = None;
                return;
            }
        };
        for entry in data.data.iter_mut() {
            let id = entry.id().0 as u32;
            if !mask.is_allowed(id) {
                entry.set_logit(f32::NEG_INFINITY);
            }
        }
        self.last_mask = Some(mask);
    }

    /// JsonConstraint-shaped accept. Takes `LlamaToken` (not raw u32)
    /// so call sites that already operate on `LlamaToken` don't have
    /// to convert. Internally maps to `consume_token`.
    ///
    /// Per-token diagnostic (gated on `SOVEREIGN_TRACE_LLGUIDANCE=1`)
    /// logs the matcher's `is_stopped()` + `is_accepting()` after
    /// each commit. Used to investigate the 2026-05-21 finding that
    /// the model emits `<tool_call>{...incomplete...}</tool_call>`
    /// despite the `%json {schema}` constraint requiring full
    /// closure. We need to see EXACTLY when the matcher transitions
    /// to accepting state — if it accepts mid-JSON, the schema
    /// isn't enforcing closure as expected.
    pub fn accept_llama(&mut self, token: LlamaToken) {
        // Mirror the mask short-circuit: once grammar accepts, the
        // parser is in a stopped state and consume_token returns
        // "parser stopped". Token tracking after the grammar's
        // acceptance is the chat template's responsibility, not
        // ours.
        if self.matcher.is_stopped() {
            self.last_mask = None;
            return;
        }
        if let Err(e) = self.matcher.consume_token(token.0 as u32) {
            self.record_failure("consume_token", &format!("token {}: {e}", token.0));
        }
        if trace_enabled() {
            let stopped = self.matcher.is_stopped();
            let accepting = self.matcher.is_accepting().unwrap_or(false);
            tracing::info!(
                token = token.0,
                stopped,
                accepting,
                "llguidance:trace token committed"
            );
        }
        self.last_mask = None;
    }

    /// Forced tokens the grammar emits deterministically. Equivalent
    /// to our `forced_next_run` Tier-2 jump-forward: when the parser
    /// has only one legal continuation across multiple tokens, return
    /// them upfront so the sampler can batch them into the next
    /// decode without paying a forward pass per forced token.
    pub fn forced_ff_tokens(&mut self) -> Vec<u32> {
        self.matcher.compute_ff_tokens()
    }

    /// Commit a forced-run of tokens (the result of `forced_ff_tokens`).
    pub fn accept_run(&mut self, tokens: &[u32]) -> Result<(), LlgError> {
        if let Err(e) = self.matcher.consume_tokens(tokens) {
            let msg = e.to_string();
            self.record_failure("consume_tokens", &msg);
            return Err(LlgError::Matcher(msg));
        }
        self.last_mask = None;
        Ok(())
    }
}

fn trace_enabled() -> bool {
    std::env::var("SOVEREIGN_TRACE_LLGUIDANCE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Per-model ParserFactory cache.
// ---------------------------------------------------------------------------

fn factory_cache() -> &'static Mutex<HashMap<usize, Arc<ParserFactory>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<ParserFactory>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve (or build) the shared `ParserFactory` for `model`. Mirrors
/// `vocab_bytes_for` — key on the `LlamaModel` raw-pointer identity.
/// The factory's `TokTrie` + `ApproximateTokEnv` are expensive (a
/// 150K-vocab model is ~25-35 MB of trie nodes on top of the vocab
/// itself), so reusing across requests keeps the per-request cost
/// bounded to parser creation + masks.
fn factory_for(model: &LlamaModel) -> Arc<ParserFactory> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = factory_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = guard.get(&key) {
            return f.clone();
        }
    }
    let tok_env = build_tok_env(model);
    let factory = ParserFactory::new_simple(&tok_env)
        .expect("ParserFactory::new_simple should not fail with a valid TokTrie");
    let arc = Arc::new(factory);
    let mut guard = factory_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, arc.clone());
    arc
}

/// Build a `TokEnv` for `model`. Uses the already-cached
/// `vocab_bytes_for` so we don't re-walk the model's vocab. The
/// `ApproximateTokEnv` impl is fine for our purposes: forced-token
/// (`compute_ff_tokens`) returns empty when tokenisation isn't
/// canonical, but the mask path — what we actually need — is exact.
fn build_tok_env(model: &LlamaModel) -> TokEnv {
    let vocab_bytes = crate::vocab_cache::vocab_bytes_for(model);
    let n_vocab = model.n_vocab();
    // TokRxInfo carries vocab size + EOS id (and optionally BOS / PAD).
    // We only need vocab_size + EOS to drive the mask; the rest stays
    // at default (None).
    let info = TokRxInfo::new(n_vocab as u32, model.token_eos().0 as u32);
    let trie = TokTrie::from(&info, vocab_bytes.as_ref());
    Arc::new(ApproximateTokEnv::new(trie))
}

// ---------------------------------------------------------------------------
// Schema preprocessing.
// ---------------------------------------------------------------------------

/// Walk a JSON-Schema document and inject `additionalProperties: false`
/// on every typed-object node that does not already set the field.
///
/// **Why this exists.** The in-house `JsonConstraint::compile_schema`
/// defaults `additionalProperties` to `false` for `type: object`
/// (non-spec — JSON Schema spec defaults to `true`). Callers across
/// `sovereign-core` rely on this implicit strictness: zero of the 11
/// `structured_output: Some(schema)` sites set `additionalProperties`
/// explicitly. `llguidance`'s `TopLevelGrammar::from_json_schema`
/// follows the spec, so a naive migration would let the model emit
/// trailing fields the JsonConstraint mask used to forbid.
///
/// Rather than touch all 11 call sites, this walker preserves the
/// in-house default at the engine boundary. Apply it to the schema
/// **before** passing to `LlguidanceConstraint::new`. Object subtrees
/// that explicitly set `additionalProperties: true` are left alone —
/// callers that genuinely want extensibility keep it.
///
/// Recurses into: `properties[*]`, `items`, `additionalProperties`
/// (when itself a schema object), `anyOf[*]`, `oneOf[*]`, `allOf[*]`,
/// `$defs[*]`, `definitions[*]`. Non-object values pass through
/// unchanged.
///
/// See `LLGUIDANCE_MIGRATION_AUDIT.md` §3.A.
pub fn default_additional_properties_false(schema: &mut serde_json::Value) {
    use serde_json::Value;
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    let is_typed_object = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s == "object")
        .unwrap_or(false);

    if is_typed_object && !obj.contains_key("additionalProperties") {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    }

    if let Some(props) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
        for (_k, sub) in props.iter_mut() {
            default_additional_properties_false(sub);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        default_additional_properties_false(items);
    }
    if let Some(ap) = obj.get_mut("additionalProperties") {
        if ap.is_object() {
            default_additional_properties_false(ap);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) {
            for sub in arr.iter_mut() {
                default_additional_properties_false(sub);
            }
        }
    }
    for key in ["$defs", "definitions"] {
        if let Some(defs) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
            for (_k, sub) in defs.iter_mut() {
                default_additional_properties_false(sub);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lark grammar builders.
// ---------------------------------------------------------------------------

/// Build the alternation grammar that lets the model emit EITHER a
/// `%json`-constrained tool envelope OR plain text. This is the
/// "escape hatch" grammar called out in the agent-bench scanner's
/// convergence finding: the model can break out of a tool loop by
/// emitting any text whose first character is not `{`, which the
/// downstream parser path treats as a `stop`-reason text turn.
///
/// The text branch's leading-byte exclusion (`[^{]`) is what
/// decides the branch — once the model samples a non-`{` first byte,
/// the parser commits to the text branch and the JSON envelope is
/// off the table for the rest of the turn.
///
/// `envelope_schema_json` should be the serialised JSON-Schema body
/// of the tool envelope (the same `tool_envelope_schema_for` output
/// the current `structured_output` path uses). Embedded via Lark's
/// `%json { … }` rule so llguidance's JSON-Schema compiler enforces
/// the shape exactly.
pub fn build_tool_alternation_grammar(envelope_schema_json: &str) -> String {
    // Three iterations of grammar design surfaced what works:
    //
    // 1. `start: text | %json {schema}` — model led with `<think>`
    //    which committed to the text branch on the first byte; tool
    //    envelope unreachable.
    //
    // 2. Add optional think-block prefix. Model emitted
    //    `<tool_call>{...}</tool_call>` after the think — text
    //    branch matched the `<` and swallowed the whole envelope
    //    as free text. Daemon marker-stop fired mid-JSON, parser
    //    rejected unbalanced body.
    //
    // 3. (this) Wrap the JSON in literal `<tool_call>` /
    //    `</tool_call>` markers as part of the grammar. The model
    //    can ONLY enter the tool branch via the literal opener.
    //    The plain_text branch is gated to not match `<tool_call>`,
    //    `<think>`, or `{` openers so the parser picks
    //    tool_envelope unambiguously.
    //
    // The Qwen chat template wraps tool calls in
    // `<tool_call>...</tool_call>` markers anyway — making them
    // part of the grammar means the marker-stop in `embedded.rs`
    // and llguidance's grammar end-condition agree on the same
    // boundary instead of fighting each other.
    format!(
        "start: think_block? body\n\
         think_block: /<think>([^<]|<[^\\/])*<\\/think>\\s*/\n\
         body: tool_envelope | plain_text\n\
         tool_envelope: \"<tool_call>\" /\\s*/ %json {envelope_schema_json} /\\s*<\\/tool_call>/\n\
         plain_text: /[^{{<](.|\\n)*|<[^t](.|\\n)*|<t[^ho](.|\\n)*|<th[^i](.|\\n)*|<tho(.|\\n)*|<to[^o](.|\\n)*|<too[^l](.|\\n)*|<tool[^_](.|\\n)*|<tool_[^c](.|\\n)*/\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternation_grammar_renders_with_schema_body() {
        let schema = r#"{"type":"object","properties":{"name":{"type":"string"}}}"#;
        let lark = build_tool_alternation_grammar(schema);
        // Optional think prefix + body alternation.
        assert!(lark.contains("think_block? body"));
        assert!(lark.contains("body: tool_envelope | plain_text"));
        // JSON branch embeds the schema body verbatim.
        assert!(lark.contains(r#""type":"object""#));
    }

    /// Smoke test of the llguidance plumbing without a real LlamaModel.
    /// Uses `ApproximateTokEnv::single_byte_env()` (one byte per token
    /// — every printable ASCII is its own token id) to confirm that:
    ///   1. A Lark grammar compiles via `TopLevelGrammar::from_lark`.
    ///   2. `ParserFactory::new_simple` + `create_parser` produce a
    ///      Matcher that isn't immediately error-state.
    ///   3. `compute_mask` returns a SimpleVob we can query.
    ///   4. `consume_token` advances state and `is_stopped` flips
    ///      after the grammar's accepted form is fully consumed.
    ///
    /// This isolates the llguidance integration from the LlamaModel
    /// vocab walk — if this test ever breaks we know the upstream
    /// crate's API drifted, not our model bridge.
    #[test]
    fn matcher_smoke_against_single_byte_env() {
        let tok_env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&tok_env).expect("factory");
        // Trivial Lark grammar: must emit exactly "hi".
        let grammar = TopLevelGrammar::from_lark("start: \"hi\"\n".to_string());
        let parser = factory.create_parser(grammar);
        let mut matcher = Matcher::new(parser);
        assert!(
            !matcher.is_error(),
            "matcher init: {:?}",
            matcher.get_error()
        );

        // Initial mask must allow `h` (token id 'h' = 0x68 under single-
        // byte env).
        let mask = matcher.compute_mask().expect("mask");
        assert!(mask.is_allowed(b'h' as u32));
        assert!(!mask.is_allowed(b'x' as u32));

        // Commit `h`. Next mask must allow `i`.
        matcher.consume_token(b'h' as u32).expect("consume h");
        let mask2 = matcher.compute_mask().expect("mask2");
        assert!(mask2.is_allowed(b'i' as u32));
        assert!(!mask2.is_allowed(b'h' as u32));

        // Commit `i`. Grammar accepts here.
        matcher.consume_token(b'i' as u32).expect("consume i");
        assert!(
            matcher.is_accepting().expect("is_accepting"),
            "matcher should accept after 'hi'"
        );
    }

    /// Canonical-path diagnostic: pure `TopLevelGrammar::from_json_schema`
    /// (no Lark wrapping, no markers). If llguidance enforces schema
    /// closure here, the failure mode we've been chasing (model
    /// emitting incomplete JSON despite schema constraint) is in the
    /// Lark+%json wrapper, not in the schema constraint itself. If
    /// llguidance ALSO accepts partial JSON via from_json_schema,
    /// the issue is upstream in llguidance.
    #[test]
    fn from_json_schema_rejects_incomplete_envelope() {
        let envelope_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "enum": ["edit"]},
                "arguments": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"},
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false,
                },
            },
            "required": ["name", "arguments"],
            "additionalProperties": false,
        });

        let tok_env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&tok_env).expect("factory");
        let grammar = TopLevelGrammar::from_json_schema(envelope_schema);
        let parser = factory.create_parser(grammar);
        let mut matcher = Matcher::new(parser);
        assert!(!matcher.is_error(), "json_schema grammar should compile");

        // Feed bytes simulating the model's incomplete emission:
        // `{"name":"edit","arguments":{"path":"x","content":"y"}` —
        // missing the final outer `}`. Each byte is a token in the
        // single-byte env. Consume them one by one.
        let incomplete = br#"{"name":"edit","arguments":{"path":"x","content":"y"}"#;
        for &b in incomplete {
            let mask = matcher.compute_mask().expect("mask");
            assert!(
                mask.is_allowed(b as u32),
                "byte {:?} should be allowed at this position",
                b as char
            );
            matcher.consume_token(b as u32).expect("consume");
        }
        // At this point — the WHOLE outer object isn't closed.
        // is_accepting() should be FALSE (grammar expects `}` next).
        let accepting = matcher.is_accepting().expect("is_accepting");
        assert!(
            !accepting,
            "grammar must NOT accept incomplete envelope (missing outer `}}`)"
        );
        // Consume `}` — now should accept.
        matcher.consume_token(b'}' as u32).expect("consume close");
        let accepting_after = matcher.is_accepting().expect("is_accepting after close");
        assert!(
            accepting_after,
            "grammar should accept after the final `}}`"
        );
    }

    // ── Differential + loudness tests (2026-06-11) ──────────────────
    //
    // These drive the PRODUCTION `mask()` / `accept_llama()` path —
    // not the raw Matcher like the smoke tests above — over the
    // single-byte env, so the contract they pin is the one the decode
    // loop actually sees.

    use crate::llama::cpp::token::data::LlamaTokenData;

    /// All 256 single-byte tokens, logits at 0.0 (i.e. "model would
    /// emit anything"); the constraint's job is to clamp the illegal
    /// ones to -inf.
    fn full_byte_array() -> LlamaTokenDataArray {
        LlamaTokenDataArray::new(
            (0..256)
                .map(|i| LlamaTokenData::new(LlamaToken(i), 0.0, 0.0))
                .collect(),
            false,
        )
    }

    fn allowed_bytes(data: &LlamaTokenDataArray) -> Vec<u8> {
        data.data
            .iter()
            .filter(|e| e.logit() > f32::NEG_INFINITY)
            .map(|e| e.id().0 as u8)
            .collect()
    }

    const TEST_SCHEMA: &str = r#"{
        "type": "object",
        "properties": {"a": {"type": "integer"}},
        "required": ["a"],
        "additionalProperties": false
    }"#;

    /// Walk the constraint to completion by always picking an
    /// allowed byte (preference-ordered so the walk terminates), and
    /// return the emitted bytes. Panics if the mask ever clamps
    /// EVERYTHING while healthy (that would be the fail-closed bug
    /// class) or the walk doesn't complete within `cap` steps.
    fn drive_to_completion(c: &mut LlguidanceConstraint, prefer: &[u8], cap: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..cap {
            if c.is_stopped() {
                return out;
            }
            let mut data = full_byte_array();
            c.mask(&mut data);
            let allowed = allowed_bytes(&data);
            assert!(
                !allowed.is_empty(),
                "mask clamped every candidate while the constraint is healthy \
                 (failure()={:?}, emitted so far: {:?}) — fail-closed must only \
                 happen after a latched matcher error",
                c.failure(),
                String::from_utf8_lossy(&out)
            );
            // Fallback: highest allowed byte, NOT lowest — the lowest
            // allowed byte is usually 0x20 (JSON permits inter-token
            // whitespace forever), and a space-picking walk never
            // terminates. Same trap the in-house enforcer documented
            // ("no leading whitespace" rule); llguidance legitimately
            // allows it, so the WALK must avoid it.
            let pick = *prefer
                .iter()
                .find(|b| allowed.contains(b))
                .unwrap_or_else(|| allowed.last().unwrap());
            out.push(pick);
            c.accept_llama(LlamaToken(pick as i32));
        }
        panic!(
            "constraint did not reach accept state within {cap} steps; emitted: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    /// THE differential guarantee: any byte sequence the production
    /// mask admits to completion parses as JSON conforming to the
    /// schema. This is the generic form of both historical constraint
    /// bugs — the in-house FSM's `step_object` comma bug (admitted
    /// invalid JSON) and the URL-FSM EOS bypass (admitted truncated
    /// output) were each "the mask admitted something it shouldn't".
    #[test]
    fn differential_mask_admits_only_schema_valid_completions() {
        // Greedy close-first walk: terminates fast, exercises the
        // happy path. Preference: close object, quote, the required
        // key, colon, a digit, open brace.
        let mut c = LlguidanceConstraint::new_with_tok_env(
            ApproximateTokEnv::single_byte_env(),
            TEST_SCHEMA,
        )
        .expect("constraint from schema");
        let bytes = drive_to_completion(&mut c, &[b'}', b'"', b'a', b':', b'7', b'{'], 64);
        let text = String::from_utf8(bytes).expect("mask admitted non-UTF8");
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "mask admitted a completion that does not parse as JSON: {text:?} ({e}) — \
                 the constraint engine is not enforcing the schema"
            )
        });
        assert!(
            parsed.get("a").map(|v| v.is_i64() || v.is_u64()).unwrap_or(false),
            "completion missing required integer field `a`: {text:?}"
        );
        assert_eq!(
            parsed.as_object().map(|o| o.len()),
            Some(1),
            "additionalProperties:false violated: {text:?}"
        );
        assert!(
            c.failure().is_none(),
            "healthy run must not latch a failure: {:?}",
            c.failure()
        );

        // Adversarial-preference walks: prefer bytes that would
        // CORRUPT JSON if admitted (commas, braces, quotes in odd
        // spots — the step_object bug shape). Whatever the mask
        // admits must still parse.
        for prefer in [
            &[b',', b',', b'}', b'"', b'a', b':', b'0', b'{'][..],
            &[b'"', b'}', b'{', b':', b'a', b',', b'9'][..],
            // `}` before the digit or the walk extends the integer
            // forever (digits are always legal mid-number).
            &[b'-', b'}', b'1', b'"', b'a', b':', b'{'][..],
        ] {
            let mut c = LlguidanceConstraint::new_with_tok_env(
                ApproximateTokEnv::single_byte_env(),
                TEST_SCHEMA,
            )
            .expect("constraint");
            let bytes = drive_to_completion(&mut c, prefer, 128);
            let text = String::from_utf8(bytes).expect("non-UTF8 admitted");
            assert!(
                serde_json::from_str::<serde_json::Value>(&text).is_ok(),
                "adversarial walk produced unparseable output the mask admitted: {text:?}"
            );
        }
    }

    /// The loudness contract: a matcher error LATCHES — `failure()`
    /// flips and stays, and every subsequent mask fails closed. The
    /// decode loops poll `constraint_failure` and abort; this pins
    /// the state machine they rely on. If this test fails, the
    /// April-2026 class returns: constraint failures that present as
    /// "the model suddenly produces garbage under structured_output".
    #[test]
    fn matcher_failure_latches_and_fails_closed() {
        let mut c = LlguidanceConstraint::new_with_tok_env(
            ApproximateTokEnv::single_byte_env(),
            TEST_SCHEMA,
        )
        .expect("constraint");
        assert!(c.failure().is_none());

        // 'x' is not a legal first byte for this schema ('{' is).
        // The production accept path swallows the Err but must latch.
        c.accept_llama(LlamaToken(b'x' as i32));
        let cause = c
            .failure()
            .expect("consume of grammar-illegal token must latch failure()")
            .to_string();
        assert!(
            cause.contains("consume_token"),
            "failure cause should name the failing op: {cause}"
        );

        // From here every mask fails closed — no token escapes.
        let mut data = full_byte_array();
        c.mask(&mut data);
        assert!(
            allowed_bytes(&data).is_empty(),
            "post-failure mask must clamp every candidate (defence in depth \
             for decode loops that fail to poll constraint_failure)"
        );
        // And the latch is permanent for the request.
        assert_eq!(c.failure(), Some(cause.as_str()));
    }

    #[test]
    fn build_tool_alternation_grammar_compiles_with_real_schema() {
        // Catch the failure mode that bit us 2026-05-21: a grammar
        // string that the helper produces but llguidance can't
        // parse. Earlier the helper wrapped `%json` in extra braces
        // (`%json { {"anyOf":[...]} }`) and llguidance rejected it
        // with "key must be a string at line 1 column 4". Pinning
        // this test catches that regression at unit-test time
        // instead of only at end-to-end smoke.
        let envelope_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "enum": ["write"]},
                "arguments": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"},
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false,
                },
            },
            "required": ["name", "arguments"],
            "additionalProperties": false,
        });
        let schema_json = serde_json::to_string(&envelope_schema).unwrap();
        let lark = build_tool_alternation_grammar(&schema_json);
        let tok_env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&tok_env).expect("factory");
        let grammar = TopLevelGrammar::from_lark(lark.clone());
        let parser = factory.create_parser(grammar);
        let matcher = Matcher::new(parser);
        assert!(
            !matcher.is_error(),
            "alternation grammar with %json schema must compile cleanly: {:?}\ngrammar:\n{}",
            matcher.get_error(),
            lark,
        );
    }

    #[test]
    fn matcher_smoke_alternation_first_byte_branches() {
        let tok_env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&tok_env).expect("factory");
        // Alternation grammar — toy text/JSON discriminator without
        // an embedded JSON Schema (full-shape test belongs in a
        // grammar-builder integration test that mounts an actual
        // tool envelope schema).
        let grammar_src = "start: text_branch | tool_envelope\n\
                           text_branch: /[^{](.|\\n)*/\n\
                           tool_envelope: \"{\" /[^}]*/ \"}\"\n";
        let grammar = TopLevelGrammar::from_lark(grammar_src.to_string());
        let parser = factory.create_parser(grammar);
        let mut matcher = Matcher::new(parser);
        assert!(
            !matcher.is_error(),
            "matcher init: {:?}",
            matcher.get_error()
        );

        // Both branches must be reachable from the start state: `{`
        // (commits to envelope) and at least one non-`{` byte
        // (commits to text branch). Test passes if both are in the
        // mask before the first commit.
        let mask = matcher.compute_mask().expect("mask");
        assert!(
            mask.is_allowed(b'{' as u32),
            "envelope branch must be reachable"
        );
        assert!(
            mask.is_allowed(b'h' as u32),
            "text branch must be reachable"
        );
    }
}
