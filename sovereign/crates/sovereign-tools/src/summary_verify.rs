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
/// behavior only while these stay identical.
pub const SUPPORTED_TAU: f64 = 0.5;
/// Early exit once a passage clearly supports the claim (gate parity).
pub const EARLY_EXIT_SUPPORT: f64 = 0.95;
/// Max member passages probed per claim (gate/lane parity, CHUNK_CAP).
pub const MEMBER_CAP: usize = 12;
/// Claim decomposition budget per summary (lane parity).
const MAX_CLAIMS: usize = 4;
/// The claim-extraction "question" for a RAPTOR summary (lane parity).
const NODE_QUESTION: &str = "Summarize the passages.";

/// Outcome of verifying one summary against its member texts.
#[derive(Debug, Clone)]
pub struct SummaryVerdict {
    pub claims_total: usize,
    pub claims_unsupported: usize,
}

impl SummaryVerdict {
    /// Blocking pass bar: at least one claim extracted AND zero
    /// unsupported. Zero-claim decomposition is NOT a pass — an
    /// unverifiable summary is treated like a failing one (the safe
    /// direction; the extractive floor is always available).
    pub fn passed(&self) -> bool {
        self.claims_total > 0 && self.claims_unsupported == 0
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
        let claims = extract_claim_list(
            &self.inference,
            NODE_QUESTION,
            summary,
            MAX_CLAIMS,
            ShardingPrivacy::LocalOnly,
        )
        .await?;
        if claims.is_empty() {
            // Decomposition produced nothing — unverifiable, not a pass.
            return Some(SummaryVerdict {
                claims_total: 0,
                claims_unsupported: 0,
            });
        }
        let mut unsupported = 0usize;
        for claim in &claims {
            let mut max_support = 0.0f64;
            let mut checked = 0usize;
            // Clusters are small (leaf target ~20); when MEMBER_CAP
            // truncates, members go in build order — the faithfulness
            // lane's claim-conditioned ranking matters for 200-chunk
            // windows, not here.
            for passage in member_texts.iter().take(MEMBER_CAP) {
                match claim_chunk_support(
                    &self.inference,
                    passage,
                    claim,
                    ShardingPrivacy::LocalOnly,
                )
                .await
                {
                    Some(s) => {
                        checked += 1;
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
            if checked == 0 {
                // Judge dead for every probe — verifier failure, not a
                // verdict. A fabricated verdict would poison the gate.
                return None;
            }
            if max_support < SUPPORTED_TAU {
                unsupported += 1;
            }
        }
        Some(SummaryVerdict {
            claims_total: claims.len(),
            claims_unsupported: unsupported,
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
    fn zero_claims_is_not_a_pass() {
        let v = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
        };
        assert!(!v.passed());
        let ok = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 0,
        };
        assert!(ok.passed());
        let bad = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 1,
        };
        assert!(!bad.passed());
    }
}
