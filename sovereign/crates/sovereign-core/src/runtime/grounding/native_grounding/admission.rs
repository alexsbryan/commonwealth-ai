// SPDX-License-Identifier: AGPL-3.0-or-later
//! H1 — the answerability admission stage.
//!
//! **The one job of this stage:** decide, BEFORE a token is generated,
//! whether the sealed evidence can answer the question. It hands a typed
//! [`GroundingVerdict`] to whatever comes next and it checks nothing else.
//! No downstream stage re-derives answerability, and this stage does not
//! look at claims, spans, citations or prose.
//!
//! **Why the reranker margin and not `top_cosine`.** The incumbent
//! early-decline signal (`evidence::evidence_early_decline`) is a cosine
//! floor, and cosine is trained on topical similarity, not containment —
//! which is exactly why its floor was never calibrated past the "in-topic
//! thin" failure. The kill gate (`NATIVE_GROUNDING.md §7.3`, artifacts in
//! `sovereign/bench/calibration/h1-port/`) measured both on the same 4,207
//! pairs:
//!
//! | signal | AUROC | honesty-recall @ 5% false alarm |
//! |---|---|---|
//! | `rerank_margin` | **0.8990** | **0.665** |
//! | `top_cosine` | 0.7994 | 0.235 |
//!
//! 2.8x the honesty at the same cost in wrongly refused answers. The
//! delta cleared the kill bar in 1,000 of 1,000 paired bootstrap
//! resamples.
//!
//! **The instrument is the reranker this runtime already carries.**
//! `RuntimeContext::rerank_fn` is a `corpus_engine::RerankFn` —
//! `(query, docs) -> Vec<f32>` of raw cross-encoder logits, the exact
//! function the kill gate scored through (`inference_to_rerank_fn` wraps
//! `InferenceProvider::rerank_batch`, and so did the bench). No new model,
//! no new slot, no second scorer (ARCH §19: the inventory outranks the
//! plan).
//!
//! **Absence is reported, never defaulted (ARCH §18.3).** If the flag is
//! on but no margin can be obtained — no reranker wired, no rerank scores
//! on the chunks, the rerank call failed — this stage returns
//! [`AdmissionOutcome::NoInstrument`] with a reason and the caller falls
//! through to the incumbent path. It never substitutes cosine for the
//! margin and calls the result an answerability score: that substitution
//! is precisely the 0.10-AUROC error the kill gate measured.

use serde::Deserialize;
use sovereign_contracts::types::{DeciderId, GroundingDecision, GroundingVerdict};
use std::sync::OnceLock;

use corpus_engine::ScoredChunk;

/// The env knob. Read here and nowhere else, so "is the native path on?"
/// has one implementation and one name (ARCH §10.6).
pub(crate) const NATIVE_GROUNDING_ENV: &str = "SOVEREIGN_NATIVE_GROUNDING";

/// Experimental per-corpus operating-point overrides (Step 3 D5,
/// order native-grounding-step3-tuning). Answerability units in (0, 1),
/// same space as the committed `thresholds`. Read once, inside the same
/// accessor every decision already goes through, so there is still exactly
/// one decider — with a loudly-traced provenance when the ruler is not the
/// committed one. An invalid value REFUSES (panics) rather than running
/// the experiment against a silently-wrong ruler (ARCH §18.3): these are
/// experiment knobs, and a misspelt threshold that fell back to the
/// committed fit would judge a tuning that never ran.
pub(crate) const NG_TAU_ABSTAIN_ENV: &str = "SOVEREIGN_NG_TAU_ABSTAIN";
pub(crate) const NG_TAU_ANSWER_ENV: &str = "SOVEREIGN_NG_TAU_ANSWER";

/// H1's pool size — `answerability(q) = max_i margin(q, chunk_i)` over the
/// top-k retrieved chunks, k ≤ 8 (`NATIVE_GROUNDING.md §5 H1`). The same
/// bound the kill gate scored under, so the runtime reads the calibration
/// at the operating point it was fitted at.
const POOL_K: usize = 8;

/// The committed calibration, as data (ARCH §6 — config is data, not
/// code). Deriving it lives in
/// `sovereign/bench/calibration/h1-port/fit_admission_calibration.py`;
/// this file is its only output and this `include_str!` is its only
/// reader, so there is exactly one Platt fit and one pair of thresholds
/// in the workspace.
const CALIBRATION_JSON: &str =
    include_str!("../../../../../../bench/calibration/h1-port/h1_admission_calibration.json");

#[derive(Debug, Deserialize)]
struct Calibration {
    platt: Platt,
    thresholds: Thresholds,
}

#[derive(Debug, Deserialize)]
struct Platt {
    a: f64,
    b: f64,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    /// Below this calibrated answerability: `Abstain`.
    tau_abstain: f64,
    /// At or above this calibrated answerability: `Answer`. Between the
    /// two: `Hedge`.
    tau_answer: f64,
}

/// Where the thresholds in force came from. Glassbox (ARCH §9): every
/// admission event names its ruler, so a transcript can never be read
/// against the wrong operating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TauSource {
    CommittedCalibration,
    EnvOverride,
}

impl TauSource {
    fn as_str(self) -> &'static str {
        match self {
            TauSource::CommittedCalibration => "committed_calibration",
            TauSource::EnvOverride => "env_override",
        }
    }
}

/// Apply the experimental overrides to the committed thresholds. Pure —
/// env is read by the caller, so tests never mutate process env (which
/// races under the parallel suite). `Err` is a refusal, not a fallback.
fn apply_tau_overrides(
    base: &Thresholds,
    abstain: Option<&str>,
    answer: Option<&str>,
) -> Result<(Thresholds, TauSource), String> {
    if abstain.is_none() && answer.is_none() {
        return Ok((
            Thresholds {
                tau_abstain: base.tau_abstain,
                tau_answer: base.tau_answer,
            },
            TauSource::CommittedCalibration,
        ));
    }
    let parse = |name: &str, v: Option<&str>, fallback: f64| -> Result<f64, String> {
        match v {
            None => Ok(fallback),
            Some(s) => {
                let x: f64 = s
                    .trim()
                    .parse()
                    .map_err(|e| format!("{name}={s:?} is not a number: {e}"))?;
                if !(0.0..1.0).contains(&x) || x <= 0.0 {
                    return Err(format!(
                        "{name}={x} is outside (0, 1) — answerability units, not margin units"
                    ));
                }
                Ok(x)
            }
        }
    };
    let tau_abstain = parse(NG_TAU_ABSTAIN_ENV, abstain, base.tau_abstain)?;
    let tau_answer = parse(NG_TAU_ANSWER_ENV, answer, base.tau_answer)?;
    if tau_abstain >= tau_answer {
        return Err(format!(
            "degenerate band: tau_abstain {tau_abstain} >= tau_answer {tau_answer}"
        ));
    }
    Ok((
        Thresholds {
            tau_abstain,
            tau_answer,
        },
        TauSource::EnvOverride,
    ))
}

fn calibration() -> &'static Calibration {
    static CAL: OnceLock<Calibration> = OnceLock::new();
    CAL.get_or_init(|| {
        // A parse failure here is a build-time-committed artifact being
        // malformed, which is not a runtime condition to degrade around.
        serde_json::from_str(CALIBRATION_JSON)
            .expect("h1_admission_calibration.json is committed alongside this module")
    })
}

/// The thresholds actually in force: the committed calibration unless the
/// experimental env overrides are set, in which case the override — traced
/// loudly, once here and per-decision via `tau_source`. Every decision
/// path reads THIS accessor; nothing reads `calibration().thresholds`
/// directly for deciding, so there is one decider (ARCH §10.6).
fn effective_thresholds() -> &'static (Thresholds, TauSource) {
    static EFF: OnceLock<(Thresholds, TauSource)> = OnceLock::new();
    EFF.get_or_init(|| {
        let abstain = std::env::var(NG_TAU_ABSTAIN_ENV).ok();
        let answer = std::env::var(NG_TAU_ANSWER_ENV).ok();
        let (t, source) = apply_tau_overrides(
            &calibration().thresholds,
            abstain.as_deref(),
            answer.as_deref(),
        )
        .unwrap_or_else(|why| {
            // Refuse, don't substitute: running the A/B on the
            // committed thresholds while the operator believes an
            // override is in force would judge a tuning that never
            // ran (ARCH §18.3).
            panic!("refusing tau override: {why}")
        });
        if source == TauSource::EnvOverride {
            tracing::warn!(
                tau_abstain = t.tau_abstain,
                tau_answer = t.tau_answer,
                committed_tau_abstain = calibration().thresholds.tau_abstain,
                committed_tau_answer = calibration().thresholds.tau_answer,
                "native-grounding H1: EXPERIMENTAL tau override active — decisions are \
                 not on the committed operating point"
            );
        }
        (t, source)
    })
}

/// Map a raw cross-encoder logit to a calibrated probability in 0..1.
///
/// The one implementation of `NATIVE_GROUNDING.md §6`'s
/// `answerability: f32` field. Monotone, so thresholding in margin space
/// and thresholding in answerability space agree by construction — which
/// is why the artifact carries both and this module only uses the latter.
fn answerability_from_margin(margin: f32) -> f32 {
    let c = calibration();
    let z = c.platt.a * margin as f64 + c.platt.b;
    let p = if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    };
    p as f32
}

/// Is the native grounding path enabled? Off unless the knob is
/// explicitly truthy — dark means dark.
pub(crate) fn native_grounding_enabled() -> bool {
    std::env::var(NATIVE_GROUNDING_ENV)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

/// Where the margin came from. Glassbox: a decision that cannot say which
/// instrument produced its number is not finished (ARCH §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarginSource {
    /// Reused from `metadata["rerank_score"]`, written by
    /// `CorpusIndex::search_with_rerank` on this very query. Free — the
    /// cross-encoder already ran during retrieval.
    RetrievalRerankScore,
    /// One batched `RerankFn` call over the top-k chunk texts, because
    /// retrieval did not rerank.
    FreshRerankCall,
}

impl MarginSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MarginSource::RetrievalRerankScore => "retrieval_rerank_score",
            MarginSource::FreshRerankCall => "fresh_rerank_call",
        }
    }
}

/// What the admission stage concluded — three states, not two, because
/// "could not judge" is a real verdict (ARCH §18.1).
#[derive(Debug)]
pub(crate) enum AdmissionOutcome {
    /// `SOVEREIGN_NATIVE_GROUNDING` is off. Returned before any work at
    /// all, so the incumbent path is byte-identical.
    Disabled,
    /// The flag is on but no answerability could be measured. The caller
    /// falls through to the incumbent path; nothing is substituted.
    NoInstrument { reason: &'static str },
    /// A verdict, with the evidence for how it was reached.
    Decided {
        verdict: GroundingVerdict,
        margin: f32,
        source: MarginSource,
        pool: usize,
        elapsed_ms: u64,
    },
}

/// Pull the raw rerank logits retrieval already computed, if it did.
///
/// `search_with_rerank` stringifies the logit at `{:.6}`, so this loses
/// resolution below 1e-6 — six orders of magnitude below the threshold
/// band (5.885 to 6.681), i.e. not a source of decision flips.
fn margins_from_metadata(chunks: &[ScoredChunk]) -> Option<Vec<f32>> {
    let mut out = Vec::with_capacity(chunks.len().min(POOL_K));
    for c in chunks.iter().take(POOL_K) {
        let raw = c.metadata.get("rerank_score")?;
        out.push(raw.parse::<f32>().ok()?);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// H1's decision, given a margin. Pure — unit-tested without env, without
/// a model, and without a runtime (ARCH §12).
pub(crate) fn decide_from_margin(margin: f32) -> (GroundingDecision, f32) {
    let a = answerability_from_margin(margin);
    let (t, _) = effective_thresholds();
    let decision = if a as f64 >= t.tau_answer {
        GroundingDecision::Answer
    } else if (a as f64) < t.tau_abstain {
        GroundingDecision::Abstain
    } else {
        GroundingDecision::Hedge
    };
    (decision, a)
}

/// Score the question against the sealed evidence and admit, hedge, or
/// abstain.
///
/// Returns [`AdmissionOutcome::Disabled`] *before touching anything* when
/// the flag is off: that early return is what makes the incumbent path
/// byte-identical, and it is the first statement in the function on
/// purpose.
pub(crate) async fn admit(
    question: &str,
    chunks: &[ScoredChunk],
    rerank_fn: Option<&corpus_engine::RerankFn>,
) -> AdmissionOutcome {
    if !native_grounding_enabled() {
        return AdmissionOutcome::Disabled;
    }
    if chunks.is_empty() {
        // Zero retrieval has its own parametric path (`knowledge_query.rs`
        // step 4a) and no pool to score. Not this stage's job.
        return AdmissionOutcome::NoInstrument {
            reason: "no chunks to score",
        };
    }

    let started = std::time::Instant::now();
    let (margins, source) = match margins_from_metadata(chunks) {
        Some(m) => (m, MarginSource::RetrievalRerankScore),
        None => {
            let Some(f) = rerank_fn else {
                return AdmissionOutcome::NoInstrument {
                    reason: "no reranker wired and retrieval left no rerank_score",
                };
            };
            let docs: Vec<String> = chunks
                .iter()
                .take(POOL_K)
                .map(|c| c.content.clone())
                .collect();
            match f(question, docs).await {
                Ok(m) if !m.is_empty() => (m, MarginSource::FreshRerankCall),
                Ok(_) => {
                    return AdmissionOutcome::NoInstrument {
                        reason: "reranker returned no scores",
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "native-grounding H1: rerank call failed — falling through to the \
                         incumbent path rather than substituting a different signal"
                    );
                    return AdmissionOutcome::NoInstrument {
                        reason: "rerank call failed",
                    };
                }
            }
        }
    };

    // H1's answerability: the BEST margin over the pool.
    let margin = margins.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !margin.is_finite() {
        return AdmissionOutcome::NoInstrument {
            reason: "no finite margin in the pool",
        };
    }
    let (decision, answerability) = decide_from_margin(margin);
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let (t, tau_source) = effective_thresholds();
    tracing::info!(
        decision = ?decision,
        answerability,
        margin,
        margin_source = source.as_str(),
        pool = margins.len(),
        tau_abstain = t.tau_abstain,
        tau_answer = t.tau_answer,
        tau_source = tau_source.as_str(),
        elapsed_ms,
        decided_by = "router",
        "native-grounding H1: answerability admission"
    );

    AdmissionOutcome::Decided {
        verdict: GroundingVerdict {
            decision,
            answerability,
            // H2's k-sample agreement gate is not built (H2b measured
            // +0.0010 over the margin — the margin already carries the
            // applicability signal). `None` says "not run", which is the
            // truth; a 0.0 would say "unanimous", which is not.
            semantic_entropy: None,
            agreement: None,
            decided_by: DeciderId::Router,
            // Segments are display, populated after generation by the
            // span resolver. A pre-generation verdict has no text yet.
            segments: Vec::new(),
        },
        margin,
        source,
        pool: margins.len(),
        elapsed_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed artifact's own numbers, quoted so a silent re-fit
    /// that changed the operating point fails here rather than in a bench
    /// run three weeks later.
    const ARTIFACT_TAU_ABSTAIN_MARGIN: f32 = 5.885_392_2;
    const ARTIFACT_TAU_ANSWER_MARGIN: f32 = 6.680_75;

    #[test]
    fn the_calibration_artifact_parses_and_is_the_one_fitted_offline() {
        let c = calibration();
        assert!(
            (c.platt.a - 0.852_843_642_835_961).abs() < 1e-12,
            "platt a drifted from the committed fit: {}",
            c.platt.a
        );
        assert!(
            (c.platt.b - -5.645_005_088_628_006_5).abs() < 1e-12,
            "platt b drifted from the committed fit: {}",
            c.platt.b
        );
        assert!(
            c.thresholds.tau_abstain < c.thresholds.tau_answer,
            "degenerate band"
        );
        assert!(c.thresholds.tau_abstain > 0.0 && c.thresholds.tau_answer < 1.0);
    }

    /// The two representations of the same operating point must agree.
    /// The artifact pins thresholds in answerability space; the curve it
    /// was read off pins them in margin space. Platt is monotone, so a
    /// margin either side of the margin threshold must land either side
    /// of the answerability threshold — if it ever did not, one of the
    /// two would be a second, disagreeing decider (ARCH §10.6).
    #[test]
    fn margin_space_and_answerability_space_thresholds_agree() {
        let eps = 0.001;
        assert_eq!(
            decide_from_margin(ARTIFACT_TAU_ABSTAIN_MARGIN - eps).0,
            GroundingDecision::Abstain
        );
        assert_eq!(
            decide_from_margin(ARTIFACT_TAU_ABSTAIN_MARGIN + eps).0,
            GroundingDecision::Hedge
        );
        assert_eq!(
            decide_from_margin(ARTIFACT_TAU_ANSWER_MARGIN - eps).0,
            GroundingDecision::Hedge
        );
        assert_eq!(
            decide_from_margin(ARTIFACT_TAU_ANSWER_MARGIN + eps).0,
            GroundingDecision::Answer
        );
    }

    #[test]
    fn the_extremes_decide_the_obvious_way() {
        assert_eq!(decide_from_margin(-10.0).0, GroundingDecision::Abstain);
        assert_eq!(decide_from_margin(10.0).0, GroundingDecision::Answer);
    }

    #[test]
    fn answerability_is_monotone_and_bounded() {
        let mut prev = -1.0f32;
        let mut m = -12.0f32;
        while m <= 12.0 {
            let a = answerability_from_margin(m);
            assert!(
                (0.0..=1.0).contains(&a),
                "answerability {a} out of range at {m}"
            );
            assert!(a >= prev, "not monotone at margin {m}: {a} < {prev}");
            prev = a;
            m += 0.25;
        }
    }

    /// The flag is off by default, and truthiness is explicit. A stray
    /// `SOVEREIGN_NATIVE_GROUNDING=0` must NOT turn the native path on —
    /// that class of bug is how a dark ship stops being dark.
    #[test]
    fn the_flag_is_off_unless_explicitly_truthy() {
        // Pure predicate over the string, stated the way the reader does,
        // so the test does not mutate process env (which races under the
        // parallel suite).
        let truthy = |v: &str| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        };
        for off in ["", "0", "false", "off", "no", " ", "2", "yes"] {
            assert!(!truthy(off), "{off:?} must not enable the native path");
        }
        for on in ["1", "true", "TRUE", "on", " 1 "] {
            assert!(truthy(on), "{on:?} must enable the native path");
        }
    }

    /// The experimental override is pure and testable without touching
    /// process env. Refusals are errors, never fallbacks: a misspelt
    /// override running on the committed thresholds would judge a tuning
    /// that never ran (ARCH §18.3).
    #[test]
    fn tau_overrides_apply_refuse_and_never_fall_back() {
        let base = Thresholds {
            tau_abstain: 0.3484894177749449,
            tau_answer: 0.5131544605334734,
        };
        // No override: the committed ruler, named as such.
        let (t, s) = apply_tau_overrides(&base, None, None).unwrap();
        assert_eq!(s, TauSource::CommittedCalibration);
        assert_eq!(t.tau_abstain, base.tau_abstain);
        assert_eq!(t.tau_answer, base.tau_answer);
        // Both set.
        let (t, s) = apply_tau_overrides(&base, Some("0.05"), Some("0.4")).unwrap();
        assert_eq!(s, TauSource::EnvOverride);
        assert_eq!(t.tau_abstain, 0.05);
        assert_eq!(t.tau_answer, 0.4);
        // Partial: abstain only, answer stays committed.
        let (t, s) = apply_tau_overrides(&base, Some("0.1"), None).unwrap();
        assert_eq!(s, TauSource::EnvOverride);
        assert_eq!(t.tau_abstain, 0.1);
        assert_eq!(t.tau_answer, base.tau_answer);
        // Refusals: not a number, out of range (margin units by mistake),
        // degenerate band. Each is an Err, none silently substitutes.
        assert!(apply_tau_overrides(&base, Some("abc"), None).is_err());
        assert!(apply_tau_overrides(&base, Some("5.885"), None).is_err());
        assert!(apply_tau_overrides(&base, Some("0.0"), None).is_err());
        assert!(apply_tau_overrides(&base, Some("0.6"), Some("0.5")).is_err());
        assert!(apply_tau_overrides(&base, None, Some("1.0")).is_err());
    }

    /// The kill gate's own honesty claim, replayed through the code path
    /// the runtime will actually take. This is the instrument validation
    /// (ARCH §18.4): if `decide_from_margin` disagreed with the frozen
    /// scores, every number in FINDINGS.md would be about a different
    /// function than the one that ships.
    ///
    /// **On the one-pair difference from FINDINGS.md.** FINDINGS reports
    /// false alarm 0.0497 (112 of 2,255) at this threshold; replaying the
    /// committed `h1_scores.jsonl` through this function gives 0.0501
    /// (113). That is not a disagreement about the threshold — it is the
    /// score FILE's precision. `h1_scores.jsonl` records margins rounded
    /// to six decimals while the curve's thresholds are full-precision
    /// f32, and exactly one answerable pair sits inside that ~2e-7 gap.
    /// The bound below is therefore stated as "within one pair", and the
    /// honesty-recall assertion — which no pair straddles — is exact.
    /// Diagnosed 2026-08-09 by recomputing all 4,200 curve points against
    /// the scores; every mismatch was this same rounding, none was a
    /// threshold error.
    #[test]
    fn the_committed_operating_point_reproduces_the_frozen_honesty_recall() {
        let scores = include_str!("../../../../../../bench/calibration/h1-port/h1_scores.jsonl");
        let (mut absent, mut absent_abstained) = (0u32, 0u32);
        let (mut answerable, mut answerable_abstained) = (0u32, 0u32);
        for line in scores.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let margin = v["rerank_margin"].as_f64().unwrap() as f32;
            let is_answerable = v["answerable"].as_bool().unwrap();
            let abstained = decide_from_margin(margin).0 == GroundingDecision::Abstain;
            if is_answerable {
                answerable += 1;
                answerable_abstained += u32::from(abstained);
            } else {
                absent += 1;
                absent_abstained += u32::from(abstained);
            }
        }
        assert_eq!(absent, 1952, "the frozen absent count");
        assert_eq!(answerable, 2255, "the frozen answerable count");
        let honesty_recall = f64::from(absent_abstained) / f64::from(absent);
        let false_alarm = f64::from(answerable_abstained) / f64::from(answerable);
        // FINDINGS.md, rerank_margin @ 5% false-alarm budget: 0.665.
        assert!(
            (honesty_recall - 0.664_959).abs() < 1e-4,
            "honesty-recall at the shipped threshold is {honesty_recall}, not the \
             frozen 0.6650 — the runtime and the verdict artifact disagree"
        );
        // One pair out of 2,255 is 0.000444 — the whole budget for the
        // rounding gap described above. A threshold that had actually
        // moved would shift this by far more than one pair.
        let one_pair = 1.0 / f64::from(answerable);
        assert!(
            (false_alarm - 0.049_667).abs() <= one_pair + 1e-6,
            "false alarm is {false_alarm}, more than one pair away from the \
             frozen 0.0497 — the shipped threshold and the verdict artifact \
             disagree about the operating point, which is not a rounding story"
        );
        assert!(
            false_alarm <= 0.0502,
            "false alarm {false_alarm} exceeds the 5% budget this operating \
             point was chosen under; D5's competence bar rests on it"
        );
    }
}
