//! Verifier-gated abstractive persistence (T1 P1.2).
//!
//! The RAPTOR builder's abstractive path writes LLM prose into the
//! knowledge base. This module is the gate that stops unverified prose
//! from persisting: decompose the candidate summary into claims
//! (production `extract_claim_list` register), judge every claim
//! against the cluster's OWN member texts (production
//! `claim_chunk_support` forced-choice register, same τ/early-exit/cap
//! as the P0.3 faithfulness lane), and only persist on pass. The
//! builder retries once with a faithful prompt variant, then falls
//! back to the extractive floor (T1 P1.1) — so a failing summary
//! degrades to quotes, never to silence and never to fabrication.
//!
//! `SummaryVerifier` is the claim+evidence→verdict seam VERIFIER_V0.md
//! §dotted-edge requires: the interim impl wraps the judge registers;
//! verifier-v0 replaces the impl, not the callers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{claim_chunk_support, extract_claim_list};
use sovereign_core::traits::InferenceProvider;

/// Same support threshold the faithfulness lane (`bench faithfulness`)
/// and the runtime gate use — the lane's rates predict this gate's
/// behavior only while these stay identical. This governs the PER-CLAIM
/// register; the whole-summary probe has its own calibrated cut
/// ([`WHOLE_SUMMARY_TAU`]) because it is a different unit.
pub const SUPPORTED_TAU: f64 = 0.5;
/// The whole-summary probe's calibrated cut, set 2026-09-22 from BOTH
/// distributions (instrument: sovereign-cli-llm
/// `tests/summary_verifier_instrument.rs`, 12 clusters, production
/// registers): faithful summaries scored 0.469–0.653 (n=10); cross-cluster
/// corruption scored 0.960–0.984 (n=6). A cut at 0.8 separates with ≥0.14
/// margin on each side. NOT the per-claim 0.5 — that would fail every
/// faithful summary (the defect this replaced); NOT a one-directional
/// tune — the corruption side sits 0.16 above the cut. Single-name
/// corruption is NOT this probe's job (a swapped token in ~1,900 chars
/// moves the gestalt by +0.04–0.10); the deterministic veto owns names.
pub const WHOLE_SUMMARY_TAU: f64 = 0.8;
/// Early exit once a passage clearly supports the claim (gate parity).
pub const EARLY_EXIT_SUPPORT: f64 = 0.95;
/// Max member passages probed per claim (gate/lane parity, CHUNK_CAP).
pub const MEMBER_CAP: usize = 12;
/// Claim decomposition budget per summary (lane parity).
const MAX_CLAIMS: usize = 4;
/// The claim-extraction "question" for a RAPTOR summary (lane parity).
const NODE_QUESTION: &str = "Summarize the passages.";
/// The whole-summary faithfulness claim, prefixed to the summary text
/// and judged against the cluster's own member window by the audit
/// pass's joint register (`claim_violation_joint`) — the same
/// multi-passage forced-choice the deep-research audit runs, not a new
/// register.
///
/// WHY A WHOLE-SUMMARY PROBE (measured 2026-09-22, instrument
/// `sovereign-cli-llm tests/summary_verifier_instrument.rs`, 6 clusters,
/// production registers): the incumbent conjunction — zero unsupported
/// over ≤4 claims, each ≥ τ from the fast judge — failed 6/6
/// abstractive summaries DETERMINISTICALLY (0 test-retest flips;
/// negative control 6/6 correct), with failures at 0-of-2 and 0-of-3
/// claims. No counting rule fixes a probe that scores narrative
/// paraphrase under τ claim-by-claim; the unit was wrong. The summary
/// is judged once, as a whole, against all its members — the per-claim
/// decomposition now feeds stats and the retry hint, never the pass
/// bar.
const FAITHFULNESS_CLAIM_PREFIX: &str =
    "This summary is faithful to the passages: every person, event, and \
     relation it asserts appears in them.\n\nSUMMARY:\n\"\"\"\n";

/// Outcome of verifying one summary against its member texts.
#[derive(Debug, Clone)]
pub struct SummaryVerdict {
    pub claims_total: usize,
    pub claims_unsupported: usize,
    /// The whole-summary faithfulness probe's violation probability
    /// (lower = more faithful). `None` = the probe did not answer —
    /// the verdict is not a measurement and `passed()` is false.
    pub whole_summary_violation: Option<f64>,
}

impl SummaryVerdict {
    /// Blocking pass bar: the whole-summary probe answered and came in
    /// under τ ([`WHOLE_SUMMARY_TAU`]). Claim
    /// decomposition no longer decides — it feeds `claims_unsupported`
    /// and the retry hint (see [`FAITHFULNESS_CLAIM_PREFIX`] for the
    /// measurement that changed this). A probe that did not answer is
    /// not a pass: an unverifiable summary still falls to the floor.
    pub fn passed(&self) -> bool {
        matches!(self.whole_summary_violation, Some(v) if v < WHOLE_SUMMARY_TAU)
    }
}

/// The claim+evidence→verdict seam. `None` = the verifier itself
/// failed (judge unreachable on every probe) — callers must treat
/// that as "not verified", never as a pass.
#[async_trait::async_trait]
pub trait SummaryVerifier: Send + Sync {
    async fn verify(&self, summary: &str, member_texts: &[String]) -> Option<SummaryVerdict>;
}

/// Interim verifier: the production judge registers, verbatim.
pub struct JudgeSummaryVerifier {
    inference: Arc<dyn InferenceProvider>,
}

impl JudgeSummaryVerifier {
    pub fn new(inference: Arc<dyn InferenceProvider>) -> Self {
        Self { inference }
    }
}

#[async_trait::async_trait]
impl SummaryVerifier for JudgeSummaryVerifier {
    async fn verify(&self, summary: &str, member_texts: &[String]) -> Option<SummaryVerdict> {
        let members: Vec<String> = member_texts.iter().take(MEMBER_CAP).cloned().collect();
        // THE PASS DECIDER — one whole-summary probe over the
        // cluster's own member window, through the audit pass's joint
        // register. `n_stable = members.len()`: a single call shares the
        // whole window (the deep-research audit's single-call shape).
        let faithfulness_claim = format!("{FAITHFULNESS_CLAIM_PREFIX}{summary}\n\"\"\"");
        let whole = sovereign_core::runtime::claim_violation_joint(
            &self.inference,
            &faithfulness_claim,
            &members,
            members.len(),
            members.len(),
            ShardingPrivacy::LocalOnly,
        )
        .await?;
        if whole < WHOLE_SUMMARY_TAU {
            // Passed: decomposition is stats-only here, and skipping the
            // per-claim fan-out is the economics win of judging the unit
            // once. Zero-claim extraction no longer blocks a probe that
            // answered.
            return Some(SummaryVerdict {
                claims_total: 0,
                claims_unsupported: 0,
                whole_summary_violation: Some(whole),
            });
        }
        // Failed the whole-summary bar: decompose for the retry hint and
        // the stats line. Decomposition failing is not a verifier failure
        // here — the pass decision is already made and recorded.
        let claims = extract_claim_list(
            &self.inference,
            NODE_QUESTION,
            summary,
            MAX_CLAIMS,
            ShardingPrivacy::LocalOnly,
        )
        .await
        .unwrap_or_default();
        let mut unsupported = 0usize;
        for claim in &claims {
            let mut max_support = 0.0f64;
            // Clusters are small (leaf target ~20); members go in build
            // order — the faithfulness lane's claim-conditioned ranking
            // matters for 200-chunk windows, not here.
            for passage in &members {
                match claim_chunk_support(
                    &self.inference,
                    passage,
                    claim,
                    ShardingPrivacy::LocalOnly,
                )
                .await
                {
                    Some(s) => {
                        if s > max_support {
                            max_support = s;
                        }
                        if max_support >= EARLY_EXIT_SUPPORT {
                            break;
                        }
                    }
                    None => {}
                }
            }
            if max_support < SUPPORTED_TAU {
                unsupported += 1;
            }
        }
        Some(SummaryVerdict {
            claims_total: claims.len(),
            claims_unsupported: unsupported,
            whole_summary_violation: Some(whole),
        })
    }
}

/// Which abstractive summaries get verified.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VerifyPolicy {
    /// Verify every abstractive summary (blocking).
    On,
    /// Verify a deterministic fraction in (0,1] — SP3 economics for
    /// large corpora (10–15% above ~1.5k nodes). Unsampled summaries
    /// persist unverified by explicit, priced choice.
    Sample(f32),
    Off,
}

impl VerifyPolicy {
    /// Parse `on` | `off` | `sample:<p>` (p in (0,1]).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => {
                if let Some(p) = other.strip_prefix("sample:") {
                    let p: f32 = p
                        .parse()
                        .map_err(|_| format!("sample rate not a number: {p}"))?;
                    if p > 0.0 && p <= 1.0 {
                        Ok(Self::Sample(p))
                    } else {
                        Err(format!("sample rate must be in (0, 1], got {p}"))
                    }
                } else {
                    Err(format!(
                        "unknown verify policy `{other}` (on | off | sample:<p>)"
                    ))
                }
            }
        }
    }

    /// Deterministic per-cluster selection — FNV-1a over a stable
    /// cluster key, no RNG state, so re-runs and checkpoint resumes
    /// verify the same clusters.
    pub fn selects(&self, cluster_key: &str) -> bool {
        match self {
            Self::Off => false,
            Self::On => true,
            Self::Sample(p) => {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for b in cluster_key.bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
                ((h % 10_000) as f32) < p * 10_000.0
            }
        }
    }
}

/// Run-level counters, shared across the builder's concurrent cluster
/// fan-out. Read by the CLI at end-of-run (glassbox: the operator sees
/// what the gate did, not just that it ran).
#[derive(Debug, Default)]
pub struct VerifyStats {
    /// Summaries put to the verifier (first attempts).
    pub verified: AtomicUsize,
    /// Passed on first attempt.
    pub passed_first: AtomicUsize,
    /// Failed once, retried with the faithful prompt variant.
    pub retried: AtomicUsize,
    /// Passed on the retry.
    pub passed_retry: AtomicUsize,
    /// Exhausted both attempts → extractive floor.
    pub fell_back: AtomicUsize,
    /// Verifier itself failed (judge unreachable) → extractive floor.
    pub verifier_failed: AtomicUsize,
}

impl VerifyStats {
    pub fn bump(counter: &AtomicUsize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn summary_line(&self) -> String {
        format!(
            "verified {} · pass {} · retry {} (pass {}) · extractive fallback {} · verifier failures {}",
            self.verified.load(Ordering::Relaxed),
            self.passed_first.load(Ordering::Relaxed),
            self.retried.load(Ordering::Relaxed),
            self.passed_retry.load(Ordering::Relaxed),
            self.fell_back.load(Ordering::Relaxed),
            self.verifier_failed.load(Ordering::Relaxed),
        )
    }
}

/// Everything the builder needs to gate abstractive summaries.
pub struct VerifyCtx {
    pub verifier: Arc<dyn SummaryVerifier>,
    pub policy: VerifyPolicy,
    pub stats: Arc<VerifyStats>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_parse_roundtrip() {
        assert_eq!(VerifyPolicy::parse("on").unwrap(), VerifyPolicy::On);
        assert_eq!(VerifyPolicy::parse("off").unwrap(), VerifyPolicy::Off);
        assert_eq!(
            VerifyPolicy::parse("sample:0.12").unwrap(),
            VerifyPolicy::Sample(0.12)
        );
        assert!(VerifyPolicy::parse("sample:1.5").is_err());
        assert!(VerifyPolicy::parse("sometimes").is_err());
    }

    #[test]
    fn sample_selection_is_deterministic_and_roughly_proportional() {
        let policy = VerifyPolicy::Sample(0.12);
        let selected: Vec<bool> = (0..10_000)
            .map(|i| policy.selects(&format!("cluster-{i}")))
            .collect();
        // Deterministic: identical on re-evaluation.
        let again: Vec<bool> = (0..10_000)
            .map(|i| policy.selects(&format!("cluster-{i}")))
            .collect();
        assert_eq!(selected, again);
        let rate = selected.iter().filter(|s| **s).count() as f32 / 10_000.0;
        assert!((rate - 0.12).abs() < 0.02, "observed rate {rate}");
    }

    #[test]
    fn the_whole_summary_probe_decides_and_an_unanswered_probe_is_not_a_pass() {
        // The pass bar is the whole-summary probe (2026-09-22); claim
        // counts no longer decide it in either direction.
        let probe_passed = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.1),
        };
        assert!(probe_passed.passed());
        // The measured faithful band's top (0.653) must pass the calibrated
        // cut — this is the pin that the 2026-09-22 calibration cannot be
        // silently reverted to the per-claim 0.5 (which failed everything).
        let in_faithful_band = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.65),
        };
        assert!(
            in_faithful_band.passed(),
            "the faithful band's measured maximum (0.653) must clear WHOLE_SUMMARY_TAU"
        );
        let probe_failed_with_clean_claims = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.9),
        };
        assert!(
            !probe_failed_with_clean_claims.passed(),
            "zero unsupported claims must not outvote the probe that answered"
        );
        // The incumbent bar this replaces, pinned as the negative case:
        // claims were the decider, and zero-claims meant not-a-pass.
        let claims_only_pass = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 0,
            whole_summary_violation: None,
        };
        assert!(
            !claims_only_pass.passed(),
            "no probe answer is not a pass — unverifiable falls to the floor"
        );
    }
}
