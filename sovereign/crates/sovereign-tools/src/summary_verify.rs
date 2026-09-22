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
    /// Proper nouns the summary asserts that NO member text carries —
    /// the deterministic name veto (see [`absent_proper_nouns`]).
    /// Non-empty fails the summary outright: name-level fabrication is
    /// a containment invariant, not a judgement call.
    pub fabricated_names: Vec<String>,
}

impl SummaryVerdict {
    /// Blocking pass bar: the deterministic name veto found nothing, AND
    /// the whole-summary probe answered and came in under τ. Claim
    /// decomposition no longer decides — it feeds `claims_unsupported`
    /// and the retry hint (see [`FAITHFULNESS_CLAIM_PREFIX`] for the
    /// measurement that changed this). A probe that did not answer is
    /// not a pass: an unverifiable summary still falls to the floor.
    pub fn passed(&self) -> bool {
        self.fabricated_names.is_empty()
            && matches!(self.whole_summary_violation, Some(v) if v < SUPPORTED_TAU)
    }
}

/// The DETERMINISTIC name veto (2026-09-22, reviewer ruling on the
/// whole-summary register): every proper noun a summary asserts must be
/// carried by at least one member text.
///
/// The pre-reg's own prior evidence says 9 of 23 misattributions trace to
/// summary content, and misattribution is a name-level failure. A judge is
/// not needed to enforce a containment invariant (ARCH principle 10), and
/// the instrument measured that a gestalt probe CANNOT resolve it: a single
/// swapped token in ~1,900 characters moves the whole-summary score by
/// +0.04–0.10 against a faithful band of 0.56–0.92 — no threshold
/// separates, because the unit is wrong. The probe keeps what it can
/// resolve (wrong-cluster arcs, invented events); names move to code.
///
/// Rules, all deterministic:
/// - a word is a proper noun iff it is alphabetic, ≥3 chars, starts
///   uppercase, and never appears lowercase anywhere in the summary
///   (a word the summary itself uses lowercase is a common word);
/// - a name passes when any member text contains it case-insensitively,
///   OR carries a word sharing a ≥5-character prefix with it
///   ("Athenian" ↔ "Athens", "Lampsacene" ↔ "Lampsacus") — derived
///   adjectives do not false-veto while a fabricated name, which shares
///   a prefix with nothing, fails;
/// - returns the offenders, in order of first assertion.
/// Sentence-initial function words no name veto should ever count. A
/// closed list (ARCH principle 9): these are grammar, not vocabulary,
/// and the veto's unit is the name.
const NAME_VETO_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "but", "or", "in", "on", "at", "for", "with", "from", "to", "of",
    "as", "by", "when", "while", "after", "before", "it", "its", "he", "she", "they", "we", "you",
    "his", "her", "their", "there", "this", "that", "these", "those", "what", "who", "whom", "why",
    "how", "all", "some", "no", "not", "one", "two", "then", "so",
];

pub fn absent_proper_nouns(summary: &str, member_texts: &[String]) -> Vec<String> {
    const PREFIX_MIN: usize = 5;
    let words: Vec<&str> = summary
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    // Words the summary itself uses in lowercase form — a capitalized
    // spelling of one of these is a sentence-initial common word, not a
    // name. Built from ORIGINAL-lowercase words only, or every
    // capitalized word would disqualify itself.
    let lower_forms: std::collections::HashSet<String> = words
        .iter()
        .filter(|w| w.chars().next().is_some_and(|c| !c.is_uppercase()))
        .map(|w| w.to_lowercase())
        .collect();
    let mut member_words: Vec<String> = Vec::new();
    for text in member_texts {
        for w in text.split(|c: char| !c.is_alphabetic()) {
            let l = w.to_lowercase();
            if l.chars().count() >= 4 {
                member_words.push(l);
            }
        }
    }
    let carries = |name: &str| {
        member_words.iter().any(|m| {
            m == name
                || m.chars()
                    .zip(name.chars())
                    .take(PREFIX_MIN)
                    .filter(|(a, b)| a == b)
                    .count()
                    >= PREFIX_MIN
        })
    };
    let mut absent: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for w in words {
        let chars = w.chars().count();
        if chars < 3
            || NAME_VETO_STOPWORDS.contains(&w.to_lowercase().as_str())
            || !w.chars().next().is_some_and(char::is_uppercase)
            || lower_forms.contains(&w.to_lowercase())
        {
            continue;
        }
        let l = w.to_lowercase();
        if seen.contains(&l) {
            continue;
        }
        if !carries(&l) {
            seen.insert(l.clone());
            absent.push((*w).to_string());
        }
    }
    absent
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
        // THE NAME VETO — deterministic, first, and terminal. A summary
        // asserting a proper noun no member carries fails without a model
        // call; the offenders are named in the verdict for the retry hint.
        let fabricated_names = absent_proper_nouns(summary, &members);
        if !fabricated_names.is_empty() {
            tracing::info!(
                names = ?fabricated_names,
                "summary-verify: deterministic name veto — no member text carries these"
            );
            return Some(SummaryVerdict {
                claims_total: 0,
                claims_unsupported: 0,
                whole_summary_violation: None,
                fabricated_names,
            });
        }
        // THE GESTALT PASS DECIDER — one whole-summary probe over the
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
        if whole < SUPPORTED_TAU {
            // Passed: decomposition is stats-only here, and skipping the
            // per-claim fan-out is the economics win of judging the unit
            // once. Zero-claim extraction no longer blocks a probe that
            // answered.
            return Some(SummaryVerdict {
                claims_total: 0,
                claims_unsupported: 0,
                whole_summary_violation: Some(whole),
                fabricated_names: Vec::new(),
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
            fabricated_names: Vec::new(),
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
            fabricated_names: Vec::new(),
        };
        assert!(probe_passed.passed());
        let probe_failed_with_clean_claims = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.9),
            fabricated_names: Vec::new(),
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
            fabricated_names: Vec::new(),
        };
        assert!(
            !claims_only_pass.passed(),
            "no probe answer is not a pass — unverifiable falls to the floor"
        );
        // The name veto outranks the probe: a grounded-score summary that
        // asserts a name no member carries still fails.
        let vetoed = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.1),
            fabricated_names: vec!["Zarkon".into()],
        };
        assert!(
            !vetoed.passed(),
            "name-level fabrication is a containment invariant, not a judgement"
        );
    }

    /// The deterministic name veto, on the four shapes that decide
    /// whether it is shippable: derived adjectives pass via the prefix
    /// rule, fabricated names fail, common words never count, and the
    /// summary's own lowercase usage disqualifies a capitalized form.
    #[test]
    fn the_name_veto_catches_fabrications_and_passes_derived_forms() {
        use super::absent_proper_nouns;
        let members = vec![
            "Elizabeth came into Captain Beck's house; the Juno was ready \
             for sea again, and Salve saw the Athens fleet from the harbour."
                .to_string(),
        ];
        // Carried verbatim, carried by prefix (Athenian ← Athens), and a
        // common word the summary also uses lowercase: all pass.
        assert_eq!(
            absent_proper_nouns(
                "Elizabeth watches; the Athenian fleet; the house was quiet.",
                &members
            ),
            Vec::<String>::new(),
            "verbatim name, derived adjective, and common word all carried"
        );
        // A fabricated name fails, and is named.
        assert_eq!(
            absent_proper_nouns("Elizabeth met Zarkon at the harbour.", &members),
            vec!["Zarkon".to_string()],
            "a name no member carries is the veto's exact catch"
        );
        // A word the SUMMARY itself uses lowercase is a common word, not
        // a name — capitalization at sentence start must not veto.
        assert_eq!(
            absent_proper_nouns("Harbour lights. The harbour was foggy.", &members),
            Vec::<String>::new(),
            "sentence-initial 'Harbour' with lowercase usage elsewhere is a common word"
        );
        // The prefix rule's documented trade, pinned: a fabrication that
        // shares ≥5 initial chars with a member root (Salveberg ← Salve)
        // ESCAPES the veto — that is the price of passing Athenian ←
        // Athens, and the gestalt probe remains the backstop for it. A
        // fabrication sharing nothing still fails.
        assert_eq!(
            absent_proper_nouns("Salveberg appeared.", &members),
            Vec::<String>::new(),
            "root-sharing inventions escape by design; the rule trades them \
             for derived-form passes — change this ONLY together with PREFIX_MIN"
        );
        assert_eq!(
            absent_proper_nouns("Salve met Quisling at the pier.", &members),
            vec!["Quisling".to_string()],
            "a fabrication sharing no root with any member is caught"
        );
    }
}
