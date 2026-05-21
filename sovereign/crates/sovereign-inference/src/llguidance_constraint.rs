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
    /// Build a constraint from a Lark grammar string and the model
    /// the request is bound to. Returns `Err` if the grammar fails
    /// to compile against the model's tokenizer (typically a syntax
    /// error in the Lark or a referenced JSON Schema the embedded
    /// `%json {…}` rule rejects).
    pub fn new(lark_grammar: &str, model: &LlamaModel) -> Result<Self, LlgError> {
        let factory = factory_for(model);
        let grammar = TopLevelGrammar::from_lark(lark_grammar.to_string());
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
    /// On `compute_mask` failure (parser hit an unrecoverable state),
    /// every non-EOS candidate is clamped — same fail-closed posture
    /// as `JsonConstraint`'s `emitted_invalid` latch. The sampler
    /// will then emit EOS and the streaming loop exits.
    pub fn mask(&mut self, data: &mut LlamaTokenDataArray) {
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
    pub fn accept_llama(&mut self, token: LlamaToken) {
        if let Err(e) = self.matcher.consume_token(token.0 as u32) {
            tracing::warn!(error = %e, token = token.0, "llguidance: consume_token failed");
        }
        self.last_mask = None;
    }

    /// Forced tokens the grammar emits deterministically. Equivalent
    /// to our `forced_next_run` Tier-2 jump-forward: when the parser
    /// has only one legal continuation across multiple tokens, return
    /// them upfront so the sampler can batch them into the next
    /// decode without paying a forward pass per forced token.
    ///
    /// Callers MUST commit any forced tokens via `accept()` (or use
    /// `Matcher::consume_tokens` directly via `accept_run`) to keep
    /// the parser state in lockstep.
    pub fn forced_ff_tokens(&mut self) -> Vec<u32> {
        self.matcher.compute_ff_tokens()
    }

    /// Commit a forced-run of tokens (the result of `forced_ff_tokens`).
    /// Bulk version of `accept` for tokens the parser already
    /// promised; uses `consume_tokens` so the matcher does the run in
    /// one call.
    pub fn accept_run(&mut self, tokens: &[u32]) -> Result<(), LlgError> {
        self.matcher
            .consume_tokens(tokens)
            .map_err(|e| LlgError::Matcher(e.to_string()))?;
        self.last_mask = None;
        Ok(())
    }
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
    format!(
        "start: text_branch | tool_envelope\n\
         text_branch: /[^{{](.|\\n)*/\n\
         tool_envelope: %json {{ {envelope_schema_json} }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternation_grammar_renders_with_schema_body() {
        let schema = r#"{"type":"object","properties":{"name":{"type":"string"}}}"#;
        let lark = build_tool_alternation_grammar(schema);
        // Both branches present.
        assert!(lark.contains("text_branch | tool_envelope"));
        // Text branch excludes leading '{'.
        assert!(lark.contains("[^{](.|\\n)*"));
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
