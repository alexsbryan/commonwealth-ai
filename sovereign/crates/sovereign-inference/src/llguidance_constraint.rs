//! `LlguidanceConstraint` — adapter between the upstream `llguidance`
//! grammar engine and our `ConstrainedSampler` byte-loop.
//!
//! Sits in parallel with `JsonConstraint`. Same shape:
//!
//! ```text
//! step()            → compute the mask for the next sampled token
//! allows(token_id)  → query the mask
//! accept(token_id)  → commit the sample, advance the parser
//! is_stopped()      → grammar satisfied, generation may end
//! ```
//!
//! Why a separate constraint: `JsonConstraint` validates byte-by-byte
//! against an in-house JSON-Schema FSM; `LlguidanceConstraint`
//! validates against an arbitrary Lark grammar with `%json {…}` rules
//! and top-level alternation. The grammar that closes the agent-bench
//! `parse_failed_envelope` + `loop_trap` failure classes is exactly
//! that shape: `start: text_branch | tool_envelope`. See
//! `memory/project_llguidance_readoption_plan.md` for the design.
//!
//! Re-uses `vocab_bytes_for(model)` from `json_constraint.rs` so the
//! tokenizer view stays consistent across both engines — same
//! `special=true` rendering, same per-model `Arc<Vec<Vec<u8>>>` cache.
//!
//! Factory caching: one `ParserFactory` per `LlamaModel` (keyed by
//! pointer identity, mirroring `vocab_cache`). Building the factory's
//! tokenizer/TokTrie is the expensive bit; sharing across requests
//! keeps per-request cost on the parser create + matcher step paths.

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
        let factory = factory_for(model);
        let trimmed = grammar_or_schema.trim_start();
        let grammar = if trimmed.starts_with('{') {
            let schema: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
                LlgError::ParserCreate(format!("json schema parse: {e}"))
            })?;
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
        })
    }

    /// Compute the next-token mask. Call once per sampling step
    /// before any `allows()` queries.
    pub fn step(&mut self) -> Result<(), LlgError> {
        let mask = self
            .matcher
            .compute_mask()
            .map_err(|e| LlgError::Matcher(e.to_string()))?;
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
        self.matcher
            .consume_token(token_id)
            .map_err(|e| LlgError::Matcher(e.to_string()))?;
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
        if self.matcher.is_stopped() {
            self.last_mask = None;
            return;
        }
        let mask = match self.matcher.compute_mask() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "llguidance: compute_mask failed — fail-closed");
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
            tracing::warn!(error = %e, token = token.0, "llguidance: consume_token failed");
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
        self.matcher
            .consume_tokens(tokens)
            .map_err(|e| LlgError::Matcher(e.to_string()))?;
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
    let vocab_bytes = crate::json_constraint::vocab_bytes_for(model);
    let n_vocab = model.n_vocab();
    // TokRxInfo carries vocab size + EOS id (and optionally BOS / PAD).
    // We only need vocab_size + EOS to drive the mask; the rest stays
    // at default (None).
    let info = TokRxInfo::new(n_vocab as u32, model.token_eos().0 as u32);
    let trie = TokTrie::from(&info, vocab_bytes.as_ref());
    Arc::new(ApproximateTokEnv::new(trie))
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
        assert!(!matcher.is_error(), "matcher init: {:?}", matcher.get_error());

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
        assert!(!matcher.is_error(), "matcher init: {:?}", matcher.get_error());

        // Both branches must be reachable from the start state: `{`
        // (commits to envelope) and at least one non-`{` byte
        // (commits to text branch). Test passes if both are in the
        // mask before the first commit.
        let mask = matcher.compute_mask().expect("mask");
        assert!(mask.is_allowed(b'{' as u32), "envelope branch must be reachable");
        assert!(mask.is_allowed(b'h' as u32), "text branch must be reachable");
    }
}
