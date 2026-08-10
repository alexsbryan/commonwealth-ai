// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Measured** capability record for a loaded slot — what this model's
//! memory module can actually do, established by probing it, not by
//! matching its architecture string.
//!
//! # Why this exists
//!
//! One physical property — *can this model survive a partial KV
//! operation* — was declared in six places, in three vocabularies, and
//! measured in none:
//!
//! | where | vocabulary |
//! |---|---|
//! | `prompt_helpers::is_recurrent_arch` | `qwen*moe` + mamba/rwkv/deltanet/ssm |
//! | `gates::fast_short_untested_recurrent_arch` | mamba/rwkv/deltanet/ssm **only** |
//! | `gates::fast_short_qwen_moe_biteback` | `qwen*moe` only |
//! | `cpu_compat::is_cpu_incompatible_arch` | a different axis again |
//! | `ModelQuirks::has_recurrent_layers` | per-family, consulted only when arch is empty |
//! | upstream `llm_arch_supports_rs_rollback` | the only authoritative one — unconsulted |
//!
//! Six declarations of one fact is the §10.6 failure mode: "a duplicated
//! decider diverges and you get a plausible number, with nothing red
//! anywhere." It produced the dense-`qwen35` miss (2026-06-09 — the arch
//! name has no "moe", so the ladder passed a hybrid as pure attention),
//! two FastShort ladders of different width, and a repro harness that
//! recommended deleting a gate it had never exercised.
//!
//! # The probe, and the one detail that makes it correct
//!
//! **It must cross a decode-pass boundary.** Measured 2026-08-10
//! (`tests/rs_rollback_spike.rs`, deterministic across runs) on `qwen35`
//! with the snapshot ring raised to 64:
//!
//! ```text
//! gen_before=0 → faithful [1, 2, 4, 8, 16, 32] · corrupt []
//! gen_before=4 → faithful [4, 8, 32]           · corrupt [1, 2, 16]
//! ```
//!
//! Within one decode pass the rollback is faithful at every depth. Once
//! generated single-token passes must be unwound first — i.e. on every
//! real second request — replaying IDENTICAL tokens at IDENTICAL
//! positions yields a DIFFERENT continuation. A probe that skipped the
//! generation step would report `Safe` and be wrong.
//!
//! # Why the return value of `seq_rm` is not the measurement
//!
//! §18.1: *assert on something the subject cannot author.* `seq_rm`
//! returning `true` is llama.cpp saying "I did it" — and on `qwen35` at
//! `n_rs_seq >= 64` it says so while corrupting the state. The
//! continuation comparison is on generated output, which the subject
//! cannot fake. Hence [`PartialKvVerdict::AcceptedButUnfaithful`], the
//! state no boolean can express.
//!
//! That variant is also what makes note `923ca1e1` ("do not raise
//! `n_rs_seq`") **structural rather than remembered** (§7.1): raise it
//! and the probe detects the unfaithfulness and vetoes on its own,
//! instead of relying on someone having read the note.
//!
//! # Scope of this pass
//!
//! The partial-KV axis only. FastShort's batched-decode axis is a
//! different capability needing a different probe (a saturated burst),
//! and this host has no weights whose arch matches the set
//! `fast_short_gate` still vetoes — so it is recorded as
//! [`FastShortCapability::Unprobed`] rather than given a verdict nobody
//! earned (§18.2: four verdicts, and `could-not-judge` is one of them).
//!
//! # Why the detector compares LOGITS, not sampled tokens
//!
//! The first version compared greedy continuations and **had a measured
//! false-negative rate**: `qwen35moe` probed `Safe` at `n_rs_seq=64`
//! while the fuller sweep showed that model corrupt at every depth.
//! `probe_parameter_sensitivity` isolated why — holding depth fixed:
//!
//! ```text
//! prefill=192   gen_before=1 → CORRUPT   gen_before=2 → CORRUPT
//! prefill=512   gen_before=1 → CORRUPT   gen_before=2 → FAITHFUL
//! prefill=1421  gen_before=1 → FAITHFUL  gen_before=2 → FAITHFUL
//! ```
//!
//! Corruption *visibility* is content- and length-dependent: greedy
//! argmax absorbs a perturbed state whenever the top token wins by more
//! than the perturbation. Tuning the constants moved the holes around
//! rather than closing them (raising generate to 4 and continuation to
//! 24 made the 2B report `Safe` too). **Sampled-token equality is a
//! lossy detector of state divergence.**
//!
//! The logit vector is the direct observable. Calibration
//! (`logit_delta_calibration`, L2 over the full vocabulary) shows the
//! argmax is the *worst* thing to look at — `top1` was unchanged in
//! every row, including the corrupting ones, while L2 moved by 17-25x.
//!
//! # Why it is a RATIO, not a threshold
//!
//! | model | floor (gen=0) | signal (gen>=1) | ratio |
//! |---|---|---|---|
//! | `qwen35moe` 36B | 97.6 | 1656-1793 | **17x** |
//! | `qwen35` 2B | 19.9 | 459-644 | **23x** |
//! | `gemma4` (correct) | 94.3 | 94.3 | **1.00x** |
//!
//! The absolute floors differ 5x across models — `gemma4`'s *correct*
//! delta (94.3) is larger than `qwen35`'s *floor* (19.9) — so any fixed
//! constant would misclassify somebody in a zoo. The probe therefore
//! runs two arms differing in exactly one variable (whether a
//! decode-pass boundary is crossed) and compares them to each other.
//! **Each model supplies its own control**, which is the same
//! differential discipline the gate repro uses, applied per-model.
//!
//! Decision logic below is pure and unit-tested weight-free; the probe
//! is the only part that touches a context.

use crate::llama::cpp::context::LlamaContext;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::{AddBos, LlamaModel};
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;

/// Prompt tokens the probe prefills. Must land in ONE ubatch for the
/// decode-pass boundary to exist (chat slots run `n_ubatch >= 512`).
const PROBE_PREFILL: usize = 192;
/// Tokens generated before the rollback — this is what creates the
/// single-token decode passes the rollback has to unwind. Zero would
/// make the probe report `Safe` on a model that corrupts in production.
const PROBE_GENERATE: usize = 2;
/// Prompt tokens discarded by the rollback. Total rollback span is
/// `PROBE_ROLLBACK + PROBE_GENERATE`. Chosen to resemble a real
/// prefix-cache rollback rather than the minimum: an MTP slot
/// (`n_rs_seq=4`) accepts shallow rollbacks but not this one, and a
/// depth-1 probe would return a verdict that doesn't describe the
/// workload.
const PROBE_ROLLBACK: usize = 16;
/// How many times the cross-pass logit delta may exceed the model's own
/// within-pass delta before the rollback is called unfaithful.
///
/// Measured 2026-08-10 (`rs_rollback_spike::logit_delta_calibration`),
/// L2 over the full logit vector:
///
/// | model | floor (gen=0) | signal (gen>=1) | ratio |
/// |---|---|---|---|
/// | `qwen35moe` 36B | 97.6 | 1656-1793 | **17x** |
/// | `qwen35` 2B | 19.9 | 459-644 | **23x** |
/// | `gemma4` (correct) | 94.3 | 94.3 | **1.00x** |
///
/// Correct rollbacks sit at 1.0x; corrupting ones at 17-25x. 4.0 sits
/// an order of magnitude clear of both sides. Note the absolute floors
/// differ 5x ACROSS models (19.9 vs 97.6), which is exactly why this is
/// a ratio and not a constant.
const NOISE_RATIO_LIMIT: f32 = 4.0;
/// Guard against a division blow-up when a model's within-pass delta is
/// essentially zero.
const MIN_FLOOR_L2: f32 = 1e-3;

/// Where a capability value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// This machine ran the probe against these weights.
    Measured,
    /// Inferred from the gguf architecture string — a guess, kept only
    /// as a fallback and as a cross-check.
    DeclaredByArch,
    /// Neither measured nor inferable.
    Unknown,
}

/// Can this slot keep part of its KV cache across requests?
///
/// Four verdicts (§18.2). Collapsing `AcceptedButUnfaithful` into
/// either `Safe` or `Refused` is how this decision reports the opposite
/// of the truth: it is *accepted by llama.cpp* and *wrong*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialKvVerdict {
    /// The memory module refused the partial removal. Today's verdict
    /// for `qwen35` / `qwen35moe` at `n_rs_seq=0`.
    Refused,
    /// Truncation ACCEPTED, but replaying identical tokens produced a
    /// different continuation. The dangerous middle.
    AcceptedButUnfaithful,
    /// Accepted AND replay-faithful across a decode-pass boundary.
    Safe,
    /// The probe did not run or could not complete.
    CouldNotJudge { reason: &'static str },
}

impl PartialKvVerdict {
    /// The only verdict that permits partial keep. Everything else —
    /// including "we could not tell" — vetoes (§18.3: absence is
    /// reported, never defaulted into a pass).
    pub fn permits_partial_keep(&self) -> bool {
        matches!(self, Self::Safe)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::AcceptedButUnfaithful => "accepted-but-unfaithful",
            Self::Safe => "safe",
            Self::CouldNotJudge { .. } => "could-not-judge",
        }
    }
}

/// FastShort's continuous-batching axis — deliberately unprobed this
/// pass. See the module doc's "Scope" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastShortCapability {
    Unprobed { reason: &'static str },
}

impl FastShortCapability {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unprobed { .. } => "unprobed",
        }
    }
}

/// What this machine established about one loaded slot.
#[derive(Debug, Clone)]
pub struct ModelCapabilities {
    /// gguf `general.architecture`. Retained as EVIDENCE — it is no
    /// longer the thing that decides. Deliberately a `String` and not
    /// an enum: the set of arch names is open (new arches ship
    /// continuously) and this is a wire value read from gguf metadata
    /// (§2.2).
    pub arch: String,
    pub partial_kv: PartialKvVerdict,
    pub partial_kv_provenance: Provenance,
    /// What the DECLARED sources say between them — libllama's
    /// `is_recurrent`/`is_hybrid` flags OR the arch ladder.
    ///
    /// Until 2026-08-10 this was also what the gate did, and it was
    /// named `declarations_say_unsafe` accordingly. Since the flip the gate
    /// follows the measurement, so the honest name is what the
    /// declarations claim — keeping the old one would have left a
    /// field asserting something no longer true (§1.1).
    pub declarations_say_unsafe: bool,
    /// What libllama's own `is_recurrent`/`is_hybrid` flags say — the
    /// gate's clause 0, added 2026-06-09 because the arch ladder missed
    /// dense hybrids.
    ///
    /// Recorded SEPARATELY from `declarations_say_unsafe` (their OR) because
    /// without it the shadow data cannot answer whether the ladder is
    /// ever load-bearing. "The gate vetoed" and "the ladder was
    /// necessary for the veto" are different claims, and only the
    /// second justifies keeping the ladder.
    pub libllama_flags_say_unsafe: bool,
    /// What `is_recurrent_arch` says **on its own**. Recorded
    /// separately because it is demonstrably wrong for dense hybrids —
    /// `qwen35` contains neither "moe" nor any of the
    /// mamba/rwkv/deltanet/ssm markers, so the ladder passes it while
    /// libllama's flags (gate clause 0, added 2026-06-09) catch it.
    /// Keeping the two apart stops a known-wrong clause from drowning
    /// the signal that the *gate* is wrong.
    pub arch_ladder_alone_says_unsafe: bool,
    pub fast_short: FastShortCapability,
}

impl ModelCapabilities {
    /// Assemble the record for one loaded model, deriving the two
    /// cross-check fields from the model itself.
    ///
    /// **One decider (§10.6).** `declarations_say_unsafe` must be computed the
    /// same way everywhere it is compared, or a shadow sweep can report
    /// agreement the production path would not. Both the slot-load path
    /// and the offline sweep go through here; neither re-derives it.
    ///
    /// The caller supplies the verdict because *whether the probe ran at
    /// all* is the caller's business (a distributed child does not
    /// probe, and `SOVEREIGN_CAPABILITY_PROBE=0` skips it) — and a
    /// not-run probe must arrive as `CouldNotJudge`, never as a default
    /// (§18.3).
    pub fn observe(
        model: &LlamaModel,
        arch: &str,
        partial_kv: PartialKvVerdict,
        partial_kv_provenance: Provenance,
    ) -> Self {
        let arch_ladder_alone_says_unsafe = super::prompt_helpers::is_recurrent_arch(arch);
        let libllama_flags_say_unsafe = model.is_recurrent() || model.is_hybrid();
        Self {
            arch: arch.to_string(),
            partial_kv,
            partial_kv_provenance,
            declarations_say_unsafe: libllama_flags_say_unsafe || arch_ladder_alone_says_unsafe,
            libllama_flags_say_unsafe,
            arch_ladder_alone_says_unsafe,
            fast_short: FastShortCapability::Unprobed {
                reason: "batched-decode axis needs a burst probe; no local weights match \
                         the set fast_short_gate still vetoes",
            },
        }
    }

    /// True when what this machine MEASURED contradicts what the
    /// declared sources claim.
    ///
    /// Before the 2026-08-10 flip this answered "would flipping the
    /// gate change behaviour". It now answers the standing question
    /// instead: the measurement is what the gate follows, so a
    /// contradiction means one of the two is stale — usually the
    /// declaration, occasionally a regressed probe.
    ///
    /// `CouldNotJudge` never reports a contradiction: nothing was
    /// measured, so a comparison either way would be fabricated.
    pub fn measurement_contradicts_declaration(&self) -> bool {
        match &self.partial_kv {
            PartialKvVerdict::CouldNotJudge { .. } => false,
            v => v.permits_partial_keep() == self.declarations_say_unsafe,
        }
    }

    /// True when the arch ladder alone is wrong about this model, even
    /// if the gate as a whole is right. Diagnostic only — it is the
    /// standing evidence for why the ladder must not become the sole
    /// decider, and it fires on every dense `qwen35`.
    pub fn arch_ladder_alone_is_wrong(&self) -> bool {
        match &self.partial_kv {
            PartialKvVerdict::CouldNotJudge { .. } => false,
            v => v.permits_partial_keep() == self.arch_ladder_alone_says_unsafe,
        }
    }
}

/// Is the probe enabled? Default ON; `SOVEREIGN_CAPABILITY_PROBE=0`
/// (also `false` / `off`) skips it and leaves the record
/// `CouldNotJudge`, which vetoes.
pub fn probe_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_CAPABILITY_PROBE").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

/// Deterministic probe text. Tokenized then truncated, so the exact
/// token count is stable for a given tokenizer without depending on one.
fn probe_text() -> String {
    "The archive keeper recorded each tide, each permit, and each lamp reading in turn. ".repeat(24)
}

/// Prefill `tokens[from..to]` at absolute positions, logits on the last.
fn prefill(ctx: &mut LlamaContext<'_>, tokens: &[LlamaToken], from: usize, to: usize) -> bool {
    if to <= from {
        return false;
    }
    let mut batch = LlamaBatch::new(to - from, 1);
    for (i, tok) in tokens[from..to].iter().enumerate() {
        if batch
            .add(*tok, (from + i) as i32, &[0], from + i == to - 1)
            .is_err()
        {
            return false;
        }
    }
    ctx.decode(&mut batch).is_ok()
}

/// Greedy-decode `n` tokens starting at absolute position `start_pos`,
/// returning them. Each is its own decode pass — which is precisely the
/// boundary the rollback must later cross.
fn generate(ctx: &mut LlamaContext<'_>, start_pos: usize, n: usize) -> Option<Vec<LlamaToken>> {
    let sampler = LlamaSampler::greedy();
    let mut out = Vec::with_capacity(n);
    let mut pos = start_pos;
    let mut next = sampler.sample(ctx, -1);
    out.push(next);
    for _ in 1..n {
        let mut b = LlamaBatch::new(1, 1);
        b.add(next, pos as i32, &[0], true).ok()?;
        ctx.decode(&mut b).ok()?;
        pos += 1;
        next = sampler.sample(ctx, -1);
        out.push(next);
    }
    Some(out)
}

/// Establish [`PartialKvVerdict`] for this context by measurement.
///
/// Leaves the KV cache fully cleared on every exit path — the caller's
/// first real request must not inherit probe state.
///
/// `pub` so `tests/rs_rollback_spike.rs` can validate the instrument
/// against a deliberately-corrupting context (`n_rs_seq=64` on a
/// hybrid) — §18.1: a check nobody has watched fail is not a check, and
/// the failure this exists to catch cannot be produced through the
/// normal chat-slot config.
pub fn probe_partial_kv(model: &LlamaModel, ctx: &mut LlamaContext<'_>) -> PartialKvVerdict {
    let verdict = probe_inner(model, ctx);
    // Whatever happened, hand back a clean context.
    ctx.clear_kv_cache();
    verdict
}

/// One rollback arm: prefill, optionally generate `gen_before` tokens
/// (creating single-token decode passes), roll back `PROBE_ROLLBACK`
/// prompt tokens, replay them, and return the resulting next-token
/// logits. `None` means llama.cpp refused the truncation.
///
/// The replay batch is `PROBE_ROLLBACK` tokens wide regardless of
/// `gen_before`, so two arms differ in EXACTLY one variable: whether a
/// decode-pass boundary was crossed. That is what lets the pair
/// self-calibrate.
fn rollback_arm(
    ctx: &mut LlamaContext<'_>,
    tokens: &[LlamaToken],
    gen_before: usize,
) -> Option<Option<Vec<f32>>> {
    ctx.clear_kv_cache();
    if !prefill(ctx, tokens, 0, PROBE_PREFILL) {
        return None;
    }
    if gen_before > 0 && generate(ctx, PROBE_PREFILL, gen_before).is_none() {
        return None;
    }
    let keep = PROBE_PREFILL - PROBE_ROLLBACK;
    match ctx.clear_kv_cache_seq(Some(0), Some(keep as u32), None) {
        Ok(true) => {}
        // Refused: nothing mutated. Distinct from a probe failure.
        Ok(false) => return Some(None),
        Err(_) => return None,
    }
    if !prefill(ctx, tokens, keep, PROBE_PREFILL) {
        return None;
    }
    Some(Some(ctx.get_logits().to_vec()))
}

/// Euclidean distance between two logit vectors. Aggregating over the
/// whole vocabulary is deliberately more stable than `max|delta|`, which
/// a single outlier coordinate can dominate.
fn l2(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

fn probe_inner(model: &LlamaModel, ctx: &mut LlamaContext<'_>) -> PartialKvVerdict {
    let needed = PROBE_PREFILL + PROBE_GENERATE + 8;
    if (ctx.n_ctx() as usize) < needed || (ctx.n_batch() as usize) < PROBE_PREFILL {
        return PartialKvVerdict::CouldNotJudge {
            reason: "context window smaller than the probe",
        };
    }
    let Ok(all) = model.str_to_token(&probe_text(), AddBos::Always) else {
        return PartialKvVerdict::CouldNotJudge {
            reason: "probe text did not tokenize",
        };
    };
    if all.len() < PROBE_PREFILL {
        return PartialKvVerdict::CouldNotJudge {
            reason: "tokenizer produced too few probe tokens",
        };
    }
    let tokens = &all[..PROBE_PREFILL];

    // Reference: a straight prefill, no reuse of any kind.
    ctx.clear_kv_cache();
    if !prefill(ctx, tokens, 0, PROBE_PREFILL) {
        return PartialKvVerdict::CouldNotJudge {
            reason: "reference prefill failed",
        };
    }
    let reference = ctx.get_logits().to_vec();

    // FLOOR arm: a rollback that stays WITHIN the prefill's decode pass.
    // Known-faithful on every model measured (2026-08-10 sweep:
    // `gen_before=0 → faithful` at every depth, on both hybrids), so
    // whatever delta it shows is this model's own float noise from the
    // narrower replay batch. Each model thus supplies its own control —
    // no absolute constant has to hold across the zoo.
    let floor = match rollback_arm(ctx, tokens, 0) {
        None => {
            return PartialKvVerdict::CouldNotJudge {
                reason: "floor arm failed to run",
            }
        }
        Some(None) => return PartialKvVerdict::Refused,
        Some(Some(l)) => l2(&reference, &l),
    };

    // SIGNAL arm: identical except that it must unwind generated
    // single-token passes first.
    let signal = match rollback_arm(ctx, tokens, PROBE_GENERATE) {
        None => {
            return PartialKvVerdict::CouldNotJudge {
                reason: "signal arm failed to run",
            }
        }
        Some(None) => return PartialKvVerdict::Refused,
        Some(Some(l)) => l2(&reference, &l),
    };

    let budget = floor.max(MIN_FLOOR_L2) * NOISE_RATIO_LIMIT;
    tracing::debug!(
        target: "capability",
        floor_l2 = floor,
        signal_l2 = signal,
        ratio = signal / floor.max(MIN_FLOOR_L2),
        budget,
        "capability: logit-delta probe"
    );
    if signal <= budget {
        PartialKvVerdict::Safe
    } else {
        PartialKvVerdict::AcceptedButUnfaithful
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(v: PartialKvVerdict, gate_vetoes: bool) -> ModelCapabilities {
        ModelCapabilities {
            arch: "test".into(),
            partial_kv: v,
            partial_kv_provenance: Provenance::Measured,
            declarations_say_unsafe: gate_vetoes,
            libllama_flags_say_unsafe: gate_vetoes,
            arch_ladder_alone_says_unsafe: gate_vetoes,
            fast_short: FastShortCapability::Unprobed { reason: "test" },
        }
    }

    #[test]
    fn only_safe_permits_partial_keep() {
        assert!(PartialKvVerdict::Safe.permits_partial_keep());
        assert!(!PartialKvVerdict::Refused.permits_partial_keep());
        assert!(!PartialKvVerdict::AcceptedButUnfaithful.permits_partial_keep());
    }

    #[test]
    fn could_not_judge_vetoes_rather_than_defaulting_to_a_pass() {
        // §18.3 — absence is reported, never defaulted into success.
        let v = PartialKvVerdict::CouldNotJudge { reason: "no probe" };
        assert!(!v.permits_partial_keep());
    }

    #[test]
    fn accepted_but_unfaithful_is_not_a_pass() {
        // THE state the old boolean could not express: llama.cpp said
        // yes and the output changed. Measured on qwen35 @ n_rs_seq>=64.
        assert!(!PartialKvVerdict::AcceptedButUnfaithful.permits_partial_keep());
        assert_ne!(
            PartialKvVerdict::AcceptedButUnfaithful,
            PartialKvVerdict::Safe
        );
        assert_ne!(
            PartialKvVerdict::AcceptedButUnfaithful,
            PartialKvVerdict::Refused
        );
    }

    #[test]
    fn agreement_when_gate_and_probe_both_say_unsafe() {
        // qwen35 / qwen35moe today: ladder vetoes, probe refuses.
        assert!(!caps(PartialKvVerdict::Refused, true).measurement_contradicts_declaration());
    }

    #[test]
    fn agreement_when_gate_and_probe_both_say_safe() {
        // gemma4 today.
        assert!(!caps(PartialKvVerdict::Safe, false).measurement_contradicts_declaration());
    }

    #[test]
    fn disagreement_when_gate_passed_a_model_the_probe_refuses() {
        // THE dense-qwen35 miss of 2026-06-09: arch string has no
        // "moe", so the ladder said safe; the hardware says otherwise.
        assert!(caps(PartialKvVerdict::Refused, false).measurement_contradicts_declaration());
    }

    #[test]
    fn disagreement_when_gate_vetoed_a_model_the_probe_clears() {
        // The over-application case — a gate costing a full prefill for
        // nothing. Must be just as visible as the unsafe direction.
        assert!(caps(PartialKvVerdict::Safe, true).measurement_contradicts_declaration());
    }

    #[test]
    fn unfaithful_disagrees_with_a_permissive_gate() {
        assert!(caps(PartialKvVerdict::AcceptedButUnfaithful, false)
            .measurement_contradicts_declaration());
    }

    #[test]
    fn could_not_judge_never_reports_disagreement() {
        // Nothing was measured, so there is nothing to disagree with —
        // reporting one either way would be a fabricated comparison.
        let v = PartialKvVerdict::CouldNotJudge { reason: "skipped" };
        assert!(!caps(v.clone(), true).measurement_contradicts_declaration());
        assert!(!caps(v, false).measurement_contradicts_declaration());
    }

    #[test]
    fn probe_text_is_long_enough_for_the_protocol() {
        // The probe needs PROBE_PREFILL tokens; tokenizers vary, but a
        // ~4 chars/token floor leaves generous headroom.
        assert!(probe_text().len() / 4 > PROBE_PREFILL);
    }

    #[test]
    fn rollback_span_crosses_a_decode_pass_boundary() {
        // The load-bearing property: the rollback must unwind generated
        // single-token passes AND re-enter the prompt's pass. If either
        // constant drifts to zero the probe silently starts measuring
        // the faithful regime and reports Safe on a corrupting model.
        assert!(PROBE_GENERATE > 0, "must generate before rolling back");
        assert!(PROBE_ROLLBACK > 0, "must reach back into the prompt pass");
        assert!(
            PROBE_ROLLBACK < PROBE_PREFILL,
            "rollback must fit the prompt"
        );
    }
}
