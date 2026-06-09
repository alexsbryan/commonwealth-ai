// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former 9669-line `embedded.rs` (PR5b). One slot /
//! concern per file; re-exported flat through `embedded/mod.rs` so every
//! `crate::embedded::<Item>` path stays valid.
#![allow(unused_imports)]
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use crate::llama::cpp::context::params::{LlamaContextParams, LlamaContextType};
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{
    EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

/// Which "role" the sampler is currently filling. The same primary
/// slot serves multiple cognitive tasks per turn — picking the next
/// tool and authoring its arguments behave very differently from
/// each other and from filling content inside a JSON string value
/// (paths, file contents, Rust source). Investment #16 (2026-05-13):
/// instead of one global temperature, the sampler holds a per-role
/// `LlamaSampler` chain and the generation loop picks which to pull
/// each token from based on its position in the emit stream.
///
/// Roles are intentionally coarse-grained at v1 — easy to extend
/// (e.g. add `Decision` for the first K tokens after a `:` in a JSON
/// envelope), but the two-way split captures the dominant cost:
/// high-T exploration helps tool selection escape attractors,
/// low-T content fills paths/code without char-level drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerRole {
    /// Default: tool envelope structure, decision points, anywhere
    /// outside a JSON string. Per-request `temperature` applies.
    Explore,
    /// Inside a JSON string value (path, command text, file
    /// content). Greedy or low-T to suppress char-level drift like
    /// `/Users/ale xsbryan` observed in the 2026-05-13 codex smoke.
    Content,
}

pub struct ConstrainedSampler {
    /// Per-role inner chains. `accept()` advances both so the DRY /
    /// penalty trackers stay in sync regardless of which one
    /// produced any given token.
    inner_explore: LlamaSampler,
    inner_content: LlamaSampler,
    /// Sole schema/grammar constraint engine. Replaces the in-house
    /// `JsonConstraint` retired 2026-05-22 (see
    /// `LLGUIDANCE_MIGRATION_AUDIT.md`). `build_sampler` builds this
    /// from either `request.lark_grammar` (lark + JSON-Schema +
    /// top-level alternation, used for tool envelopes) or
    /// `request.structured_output` (JSON Schema body run through the
    /// `additionalProperties:false` walker before serialising).
    llg_constraint: Option<crate::llguidance_constraint::LlguidanceConstraint>,
    /// URL-allowlist constraint. When `Some`, every sampled token's
    /// bytes are simulated against a byte-trie of allowed URLs; tokens
    /// whose bytes would start or extend a URL outside the allowlist
    /// get clamped to `-INFINITY`. Prose tokens (anything that doesn't
    /// look like the start of an `http://` / `https://` sequence) pass
    /// through unchanged.
    ///
    /// Skipped when the JSON-schema `constraint` above is also active:
    /// tool-call argument URLs are validated by the schema instead,
    /// and stacking the two state machines would deadlock the byte
    /// stream (the JSON FSM emits braces and quotes that the URL FSM
    /// would treat as URL terminators).
    url_constraint: Option<crate::url_constraint::UrlAllowlistConstraint>,
    /// Evidence-id allowlist constraint (Tier 2 of the tool-framework
    /// expansion). When `Some`, every sampled token's bytes are
    /// simulated against a byte-trie of allowed `ev-Tn-NNNN` handles;
    /// tokens that would extend `[ev-T…` into a non-existent id get
    /// clamped to `-INFINITY`. Prose tokens (anything that doesn't
    /// look like the start of an `[ev-T` citation) pass through
    /// unchanged.
    ///
    /// Skipped when the JSON-schema `constraint` above is also
    /// active — same byte-stream-deadlock rationale as the URL
    /// constraint (JSON FSM emits `]` which the citation FSM would
    /// treat as a terminator).
    evidence_id_constraint: Option<crate::evidence_id_constraint::EvidenceIdAllowlistConstraint>,
    /// Vocab-sized bitmap of tokens whose rendered bytes contain a
    /// 3+ byte UTF-8 leading byte (CJK / Devanagari / Hangul / etc.).
    /// When `Some`, `sample()` clamps those tokens' logits to
    /// `-INFINITY` on every step, regardless of whether a JSON
    /// constraint is active. Populated by `build_sampler` when the
    /// `SOVEREIGN_BLOCK_NON_LATIN` env var is set.
    non_latin_denylist: Option<std::sync::Arc<Vec<bool>>>,
}

impl ConstrainedSampler {
    /// Per-token: pull candidates from ctx, mask via constraint (if
    /// any), apply the rest of the chain, return the selected token.
    ///
    /// Performance note (2026-05-12): the mask is currently full-vocab
    /// O(152K × per-candidate-parser-cost). A previous attempt to
    /// pre-filter via `LlamaSampler::top_k(2048)` before the mask
    /// achieved 2.3× speedup on the trivial-prompt probe but
    /// produced incorrect output on real prompts (Chinese tokens
    /// slipped through the grammar). The root issue is that
    /// per-candidate incremental parsing is structurally wrong;
    /// the right fix is a precomputed token-acceptance table per
    /// FSM state (the pattern LM-Format-Enforcer and Outlines use).
    /// Until that lands, the slower-but-correct mask stays.
    pub fn sample(
        &mut self,
        ctx: &crate::llama::cpp::context::LlamaContext<'_>,
        idx: i32,
        role: SamplerRole,
    ) -> LlamaToken {
        let mut data = if idx < 0 {
            ctx.token_data_array()
        } else {
            ctx.token_data_array_ith(idx)
        };
        // Constraint dispatch: llguidance claims the byte stream when
        // active (lark grammar or schema). URL + evidence-id FSMs are
        // skipped when llg owns the mask — stacking the state machines
        // would deadlock (each engine rejects tokens the other allows).
        // When no schema/grammar is active, URL + citation FSMs compose
        // freely since `http(s)://` and `[ev-T` live in disjoint byte
        // patterns.
        if let Some(llg) = self.llg_constraint.as_mut() {
            llg.mask(&mut data);
        } else {
            if let Some(uc) = self.url_constraint.as_ref() {
                uc.mask(&mut data);
            }
            if let Some(eic) = self.evidence_id_constraint.as_ref() {
                eic.mask(&mut data);
            }
        }
        // Non-Latin denylist: independent of the JSON-schema mask, so
        // it covers free-form chat and non-`structured_output` paths
        // that the grammar layer doesn't reach. A single L1 lookup per
        // candidate; the bitmap was built once at slot-load.
        if let Some(deny) = self.non_latin_denylist.as_ref() {
            for entry in data.data.iter_mut() {
                let id = entry.id().0 as usize;
                if deny.get(id).copied().unwrap_or(false) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            }
        }
        // MIGRATION 2026-05-17: llama-cpp-4 0.2.x's `apply_sampler`
        // takes `&mut LlamaSampler` (was `&LlamaSampler` in 0.1.x).
        // The mutability moved because samplers now carry per-call
        // state (e.g. DRY's repetition history) that the previous
        // API hid behind `&self`. Take &mut via match — the parent
        // struct's wrapping `Mutex` already serialises access.
        let inner = match role {
            SamplerRole::Explore => &mut self.inner_explore,
            SamplerRole::Content => &mut self.inner_content,
        };
        data.apply_sampler(inner);
        data.selected_token()
            .expect("sampler chain failed to select a token")
    }

    /// Advance both inner chains (DRY / penalties state stays in
    /// sync regardless of which role produced this token) and the
    /// constraint state machine.
    pub fn accept(&mut self, token: LlamaToken) {
        self.inner_explore.accept(token);
        self.inner_content.accept(token);
        // Constraint accept mirrors `sample`'s mask dispatch.
        // URL + evidence-id FSMs always advance below — their cursor
        // must stay synchronised with emitted bytes even when the
        // schema/grammar mask was the gating constraint.
        if let Some(llg) = self.llg_constraint.as_mut() {
            llg.accept_llama(token);
        }
        // Advance URL FSM unconditionally (even when JSON constraint
        // is active and the URL mask was skipped) so the cursor stays
        // synchronised with the actual emitted byte stream. The state
        // machine only matters once an `http://` / `https://` marker
        // appears in prose, and feeding it every token costs ~1 lookup.
        if let Some(uc) = self.url_constraint.as_mut() {
            uc.accept(token);
        }
        // Same synchronisation discipline for the evidence-id FSM —
        // cursor must track every emitted token, masked or not.
        if let Some(eic) = self.evidence_id_constraint.as_mut() {
            eic.accept(token);
        }
    }

    /// True when the active llguidance grammar has reached its accept
    /// state — generation can stop because the schema is satisfied.
    /// Returns `false` when no llguidance constraint is active OR the
    /// grammar still has open requirements (e.g. unclosed braces).
    ///
    /// The generation loop polls this after every `accept` to know
    /// when to break out cleanly. Without this check, the chat
    /// template's post-JSON tail (`</think>`, `</tool_call>`) gets
    /// emitted as free-text content because the mask becomes a no-op
    /// once `is_stopped` flips (see `LlguidanceConstraint::mask`'s
    /// short-circuit). Net effect was 5+ KiB of post-JSON prose
    /// inflating tokens past the per-turn cap; observed 2026-05-22
    /// in the agent-bench scaffolded run on Qwen 3.6-A3B.
    ///
    /// JsonConstraint has no equivalent — the brace-balance tracker
    /// in the generation loop is its stop signal. This method is
    /// scoped to llguidance only.
    pub fn grammar_is_stopped(&self) -> bool {
        self.llg_constraint
            .as_ref()
            .map(|llg| llg.is_stopped())
            .unwrap_or(false)
    }

    /// Tier 1 jump-forward — single-token shortcut when the FSM has
    /// exactly one legal continuation. Always returns `None` after the
    /// JsonConstraint retirement: llguidance's `compute_ff_tokens`
    /// covers the same ground as `forced_next_run` below (Tier 2),
    /// and `ApproximateTokEnv` empties out for non-canonical BPE
    /// tokenisations anyway (audit §3.C). Callers still call this for
    /// API symmetry — keeping the method preserves the original
    /// two-tier shape and lets a custom `TokenizerEnv` future-PR add
    /// a real Tier 1 hook here without a caller diff.
    pub fn forced_next_token(&mut self) -> Option<LlamaToken> {
        None
    }

    /// **Tier 2 jump-forward.** Returns deterministic-prefix tokens
    /// from `Matcher::compute_ff_tokens`. Tokens are NOT pre-consumed;
    /// the caller must `accept(token)` per emit to advance both the
    /// DRY trackers and the llguidance matcher. `max_bytes` is a soft
    /// cap on the returned token count. Empty when no llguidance
    /// constraint is active OR no deterministic prefix exists at the
    /// current parser state. `ApproximateTokEnv` returns empty on
    /// non-canonical BPE tokenisations; that's expected — the Tier 2
    /// path falls through to ordinary sampling.
    pub fn forced_next_run(&mut self, max_bytes: usize) -> Vec<LlamaToken> {
        let Some(llg) = self.llg_constraint.as_mut() else {
            return Vec::new();
        };
        llg.forced_ff_tokens()
            .into_iter()
            .take(max_bytes)
            .map(|id| LlamaToken(id as i32))
            .collect()
    }
}

/// Resolved sampling parameters for a single role. Picked from
/// ModelQuirks based on `SamplingMode` + per-request
/// overrides.
struct ResolvedSampling {
    mode: SamplingMode,
    temp: f32,
    top_k: i32,
    top_p: f32,
    presence_pen: f32,
}

pub(crate) fn build_sampler(
    model: &LlamaModel,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> ConstrainedSampler {
    // Two ways to engage llguidance:
    //   1. `request.lark_grammar` — pre-built Lark string (tool envelope
    //      + alternation, set by `sovereign-mesh::inference_adapter`
    //      when tools are present).
    //   2. `request.structured_output` — JSON Schema body; the schema
    //      runs through `default_additional_properties_false` before
    //      serialising so the in-house JsonConstraint's non-spec
    //      strictness (`additionalProperties: false`) is preserved at
    //      the engine boundary.
    //
    // Compile failure on either path warns and falls back to free-form
    // sampling so the request still produces output rather than
    // 503'ing. See `LLGUIDANCE_MIGRATION_AUDIT.md` for the rollout
    // history.
    let llg_constraint = if let Some(lark) = request.lark_grammar.as_deref() {
        match crate::llguidance_constraint::LlguidanceConstraint::new(lark, model) {
            Ok(c) => {
                tracing::info!(
                    grammar_bytes = lark.len(),
                    "grammar-constrained decoding enabled (llguidance, lark)"
                );
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "LlguidanceConstraint compile failed (lark) — falling back to free-form sampling"
                );
                None
            }
        }
    } else if let Some(schema) = request.structured_output.as_ref() {
        match crate::llguidance_constraint::LlguidanceConstraint::from_schema_value(schema, model) {
            Ok(c) => {
                tracing::info!("grammar-constrained decoding enabled (llguidance, schema)");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "LlguidanceConstraint compile failed (schema) — falling back to free-form sampling"
                );
                None
            }
        }
    } else {
        None
    };

    // URL-allowlist constraint: built when the caller declares a
    // non-empty allowlist on the request. Shares vocab_bytes storage
    // via `vocab_cache::vocab_bytes_for` so URL + evidence-id +
    // llguidance all observe the same per-model byte mapping.
    let url_constraint = request.url_allowlist.as_deref().and_then(|urls| {
        let vocab_bytes = crate::vocab_cache::vocab_bytes_for(model);
        let constraint = crate::url_constraint::UrlAllowlistConstraint::new(urls, vocab_bytes);
        tracing::info!(
            url_count = urls.len(),
            constructed = constraint.is_some(),
            "url_allowlist constraint constructed"
        );
        constraint
    });

    // Evidence-id allowlist constraint (Tier 2 of tool-framework
    // expansion). Same per-request shape as the URL constraint —
    // built when `CompletionRequest.evidence_id_allowlist` is
    // non-empty. Shares the per-model vocab_bytes cache.
    let evidence_id_constraint = request.evidence_id_allowlist.as_deref().and_then(|ids| {
        let vocab_bytes = crate::vocab_cache::vocab_bytes_for(model);
        let constraint =
            crate::evidence_id_constraint::EvidenceIdAllowlistConstraint::new(ids, vocab_bytes);
        tracing::info!(
            ev_id_count = ids.len(),
            constructed = constraint.is_some(),
            "evidence_id_allowlist constraint constructed"
        );
        constraint
    });

    // Non-Latin token denylist: opt-in via `SOVEREIGN_BLOCK_NON_LATIN`.
    // Built once per model and cached for the daemon's lifetime.
    // Default OFF — some corpora legitimately need CJK tokens.
    let non_latin_denylist = if non_latin_block_enabled() {
        Some(crate::vocab_cache::non_latin_denylist_for(model))
    } else {
        None
    };

    // Sampling parameters — picked from `ModelQuirks`. Three modes:
    //   * **instruct** — enable_thinking=false (any tools state).
    //                    Used by codex CLI traffic, atlas Phase 1,
    //                    structured-output extracts.
    //   * **code**     — enable_thinking=true AND tools present.
    //                    Reasoning-heavy coding work.
    //   * **think**    — enable_thinking=true AND no tools.
    //                    General reasoning (chat, planning, atlas
    //                    discourse work).
    // Each mode reads `<mode>_*` quirks fields; missing values fall
    // back to `default_*` (the think profile). Per-request overrides
    // (`request.temperature`, `request.top_p`, `request.top_k`)
    // still win over the picked profile so an operator can poke any
    // knob without rewriting quirks.
    // Per-role profile selection. Priority order:
    //   1. **Explicit caller override** via `request.sampling_mode`.
    //      Both roles use that mode. Lets non-codex callers (atlas
    //      pipelines, ATOS, eval harnesses) pin a profile without
    //      spoofing `enable_thinking` + tools signals.
    //   2. **Auto-picker** based on the role + request shape:
    //      * Explore — outside JSON string fields, between tool
    //        boundaries. Maps to Instruct when tools are present
    //        (picking what to call); otherwise the no-tools-mode
    //        below.
    //      * Content — inside a tool_call JSON string value (the
    //        `cmd` body of exec_command, where apply_patch lives).
    //        Maps to Code when tools are present (composing code);
    //        otherwise the no-tools-mode below.
    //      No-tools mode falls back to Think unless the request
    //      sets `enable_thinking: false`, in which case Instruct.
    //
    // The model's token-level structure (which role it's currently
    // in) drives the auto-pick, not any call-site heuristic.
    let no_tools_mode = match request.enable_thinking {
        Some(false) => SamplingMode::Instruct,
        _ => SamplingMode::Think,
    };
    let has_tools = request
        .tools
        .as_ref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    let (explore_mode, content_mode) = match request.sampling_mode {
        Some(explicit) => (explicit, explicit),
        None if has_tools => (SamplingMode::Instruct, SamplingMode::Code),
        None => (no_tools_mode, no_tools_mode),
    };
    let resolve = |mode: SamplingMode| -> ResolvedSampling {
        let (m_temp, m_top_k, m_top_p, m_presence) = match mode {
            SamplingMode::Instruct => (
                quirks.instruct_temperature,
                quirks.instruct_top_k,
                quirks.instruct_top_p,
                quirks.instruct_presence_penalty,
            ),
            SamplingMode::Code => (
                quirks.code_temperature,
                quirks.code_top_k,
                quirks.code_top_p,
                quirks.code_presence_penalty,
            ),
            SamplingMode::Think => (None, None, None, None),
        };
        ResolvedSampling {
            mode,
            temp: request
                .temperature
                .unwrap_or(m_temp.unwrap_or(quirks.default_temperature)),
            top_k: request
                .top_k
                .or(m_top_k)
                .or(quirks.default_top_k)
                .unwrap_or(40) as i32,
            top_p: request
                .top_p
                .unwrap_or(m_top_p.unwrap_or(quirks.default_top_p)),
            presence_pen: m_presence.unwrap_or(quirks.default_presence_penalty),
        }
    };
    let explore = resolve(explore_mode);
    let content = resolve(content_mode);
    tracing::debug!(
        explore_mode = ?explore.mode,
        explore_temp = explore.temp,
        explore_top_p = explore.top_p,
        explore_presence = explore.presence_pen,
        content_mode = ?content.mode,
        content_temp = content.temp,
        content_top_p = content.top_p,
        content_presence = content.presence_pen,
        "sampler-profile per-role selection"
    );
    // Sampler-stage params (rep / freq / min_p) read from quirks.
    // Qwen card recommends 1.0 / 0.0 / 0.0 across all modes;
    // llama-cpp tradition for other families is 1.15 / 0.1 / 0.05
    // (the historical hardcoded values, now preserved via the
    // serde-default compat helpers in `sovereign_core::model_family`).
    let rep_pen: f32 = quirks.default_repetition_penalty;
    let freq_pen: f32 = quirks.default_frequency_penalty;
    let min_p_threshold: f32 = quirks.default_min_p;
    // Sequence breakers tell DRY where one "thought unit" ends and another
    // begins — any of these tokens resets the repeated-suffix detector.
    let breakers: &[&[u8]] = &[b"\n", b".", b"?", b"!", b":", b"\"", b"*"];

    // Content-role temperature: defaults to greedy (0.0) so the
    // apply_patch envelope syntax (markers, delimiters) stays
    // disciplined. Gym 005/008/009 regression 2026-05-13 confirmed
    // T=0.6 inside `cmd` strings drops `***` prefixes ~40% of runs
    // because the model's next-token prob for `***` isn't always
    // the argmax at T=0.6.
    //
    // The OTHER Code-profile knobs (top_p=0.95, presence=0.0,
    // top_k=20) still apply — those affect token-set shape, not
    // greedy-vs-sample. So content gets Code's discipline on every
    // axis except T, where envelope precision wins.
    //
    // `SOVEREIGN_CONTENT_TEMPERATURE` env overrides (e.g. for
    // debugging tokenizer drift on long path strings, where some
    // sampling temperature might help vs. a tokenizer-locked greedy
    // path).
    let content_temp = std::env::var("SOVEREIGN_CONTENT_TEMPERATURE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| (0.0..=2.0).contains(v))
        .unwrap_or(0.0);

    let build_chain = |params: &ResolvedSampling, chain_temp: f32| {
        let mut samplers: Vec<LlamaSampler> = Vec::new();
        // MIGRATION 2026-05-17: llama-cpp-4 0.2.x reshaped DRY:
        //   * Added `n_ctx_train` as the second positional arg — scopes
        //     DRY's repetition-detection window; querying the model
        //     gives us the model's actual training span instead of a
        //     hard-coded guess.
        //   * `dry` became a method on `&LlamaSampler` rather than a
        //     free constructor. Chain off `greedy()` (a no-op identity
        //     sampler at this position in the chain) to get a base we
        //     can call `.dry(...)` against. Semantically equivalent to
        //     the old constructed-fresh form.
        samplers.push(LlamaSampler::greedy().dry(
            model,
            model.n_ctx_train() as i32,
            0.8,
            1.75,
            2,
            -1,
            breakers.iter().copied(),
        ));
        samplers.push(LlamaSampler::penalties(
            128,
            rep_pen,
            freq_pen,
            params.presence_pen,
        ));
        if chain_temp < 0.01 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::top_k(params.top_k));
            samplers.push(LlamaSampler::min_p(min_p_threshold, 1));
            samplers.push(LlamaSampler::top_p(params.top_p, 1));
            samplers.push(LlamaSampler::temp(chain_temp));
            samplers.push(LlamaSampler::dist(rand_seed()));
        }
        LlamaSampler::chain_simple(samplers)
    };

    ConstrainedSampler {
        inner_explore: build_chain(&explore, explore.temp),
        inner_content: build_chain(&content, content_temp),
        llg_constraint,
        url_constraint,
        evidence_id_constraint,
        non_latin_denylist,
    }
}

/// Read `SOVEREIGN_BLOCK_NON_LATIN` once per `build_sampler` call.
/// Accepts the same truthy spellings as `SOVEREIGN_GRAMMAR_TIMING`
/// (`1` / `true`, case-insensitive) so operators have a consistent
/// toggle convention across grammar features.
fn non_latin_block_enabled() -> bool {
    match std::env::var("SOVEREIGN_BLOCK_NON_LATIN") {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
}
