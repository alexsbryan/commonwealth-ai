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
    /// `None` with a non-empty [`Self::name_violations`] is its own
    /// honest record: blocked deterministically, the probe not spent.
    pub whole_summary_violation: Option<f64>,
    /// Registry entities the summary names that NO member text carries
    /// (the deterministic name veto). Empty = no veto. The vocabulary
    /// is the atlas's OWN extracted entity names — data, never a
    /// word-class guess (the 2026-09-22 operator deletion of the
    /// stopword/prefix containment veto; its 9/12 faithful
    /// false-veto rate is the number this registry shape must beat,
    /// measured on the same instrument).
    pub name_violations: Vec<String>,
}

impl SummaryVerdict {
    /// Blocking pass bar: the whole-summary probe answered and came in
    /// under τ ([`WHOLE_SUMMARY_TAU`]), AND the name veto is silent.
    /// Claim decomposition no longer decides — it feeds
    /// `claims_unsupported` and the retry hint (see
    /// [`FAITHFULNESS_CLAIM_PREFIX`] for the measurement that changed
    /// this). A probe that did not answer is not a pass: an
    /// unverifiable summary still falls to the floor.
    pub fn passed(&self) -> bool {
        self.name_violations.is_empty()
            && matches!(self.whole_summary_violation, Some(v) if v < WHOLE_SUMMARY_TAU)
    }
}

/// The corpus's own entity vocabulary, for the deterministic name veto.
///
/// Built from the atlas's extracted Entity atoms (canonical name +
/// aliases), folded for case/whitespace. A summary that names a
/// registry entity NONE of its member texts carry is misattributed —
/// the smallest corruption that changes WHO a summary is about while
/// leaving its narrative shape intact, and exactly the hole the
/// whole-summary gestalt probe cannot see (a swapped token in ~1,900
/// chars moves it +0.04–0.10). Matching is exact token lookup of
/// KNOWN names against text — no word-class guessing, no prefix or
/// stopword rules; those were the deleted veto's false-veto machine.
#[derive(Debug, Clone)]
pub struct SummaryNameRegistry {
    /// One entry per ENTITY: its folded forms (canonical + aliases)
    /// and the canonical display form for verdicts. Entity-level, not
    /// per-name: a summary may use any form of an entity some member
    /// carries, in any other form — that is cohesion, the exact case
    /// the deleted per-name containment veto false-vetoed 9/12 on.
    entities: Vec<EntityForms>,
}

#[derive(Debug, Clone)]
struct EntityForms {
    display: String,
    forms: Vec<String>,
}

impl SummaryNameRegistry {
    /// Read the atlas's EXTRACTED Entity atoms from `atlas/atoms.json`
    /// — the writer's extracted-atoms artifact. `ontology.json` beside
    /// it is the DECLARATION (policies, not atoms): the 2026-09-24
    /// census armed against it, the veto silently stayed off, and the
    /// run's own stats line (`name registry 0 names`) was the only
    /// tell. This doc comment and the fixture shape below are the pin
    /// against repeating that. Fails when the file cannot be read or
    /// parsed — the CALLER decides whether that is absence (corpora
    /// without an atlas) or a defect.
    pub fn from_atlas_atoms(path: &std::path::Path) -> Result<Self, String> {
        let raw =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        let atoms = value
            .get("atoms")
            .and_then(|a| a.as_array())
            .ok_or_else(|| format!("{}: no atoms array", path.display()))?;
        let mut entities: Vec<EntityForms> = Vec::new();
        for atom in atoms {
            if atom.get("atom_type").and_then(|t| t.as_str()) != Some("Entity") {
                continue;
            }
            let data = atom
                .get("data")
                .ok_or_else(|| format!("{}: Entity atom without data", path.display()))?;
            let display = data
                .get("canonical_name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let mut forms: Vec<String> = Vec::new();
            for form in std::iter::once(display.clone()).chain(
                data.get("aliases")
                    .and_then(|a| a.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str())
                    .map(str::to_string),
            ) {
                let folded = fold(&form);
                if folded.len() >= 3 && !forms.contains(&folded) {
                    forms.push(folded);
                }
            }
            if !forms.is_empty() {
                entities.push(EntityForms { display, forms });
            }
        }
        Ok(Self { entities })
    }

    /// Registry entities present in `summary` but absent from every
    /// member text. Display forms returned, sorted for stable verdicts.
    pub fn violations(&self, summary: &str, member_texts: &[String]) -> Vec<String> {
        let summary_folded = fold(summary);
        let members_folded = member_texts.iter().map(|m| fold(m)).collect::<Vec<_>>();
        let mut out = Vec::new();
        for entity in &self.entities {
            let in_summary = entity
                .forms
                .iter()
                .any(|f| contains_token(&summary_folded, f));
            let in_members = members_folded
                .iter()
                .any(|m| entity.forms.iter().any(|f| contains_token(m, f)));
            if in_summary && !in_members {
                out.push(entity.display.clone());
            }
        }
        out.sort();
        out
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// The folded forms registered for this word (any-form lookup);
    /// empty when the word is not in the vocabulary. The instrument
    /// uses this to aim its name-swap control at REAL entities.
    pub fn forms_of(&self, word: &str) -> &[String] {
        let folded = fold(word);
        self.entities
            .iter()
            .find(|e| e.forms.iter().any(|f| f == &folded))
            .map(|e| e.forms.as_slice())
            .unwrap_or(&[])
    }
}
/// the instrument and tests. Production loads via
/// [`SummaryNameRegistry::from_atlas_atoms`].
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RegistryEntityFixture {
    /// Canonical form, used in verdicts.
    pub display: String,
    /// Every form that counts as this entity (canonical + aliases).
    pub forms: Vec<String>,
}

impl SummaryNameRegistry {
    pub fn from_entities(fixtures: Vec<RegistryEntityFixture>) -> Self {
        let mut entities = Vec::new();
        for f in fixtures {
            let mut forms = Vec::new();
            for form in f.forms {
                let folded = fold(&form);
                if folded.len() >= 3 && !forms.contains(&folded) {
                    forms.push(folded);
                }
            }
            if !forms.is_empty() {
                entities.push(EntityForms {
                    display: f.display,
                    forms,
                });
            }
        }
        Self { entities }
    }
}

/// Text normalization for name matching — A CLOSED SET of four, each
/// one line of justification, and deliberately nothing else:
///   1. case-fold + whitespace collapse (extractor casing is noise);
///   2. diacritic fold (the corpus itself spells Histiæa AND Histiaea);
///   3. possessive clitic strip ("Verloc's" is about Verloc);
///   4. separator equivalence (hyphen ≡ space in compound names).
/// NO stopwords, NO prefixes/stemming, NO per-corpus exceptions — the
/// 2026-09-22 deletion of the containment veto is the incident this
/// list is bounded against. A faithful-summary false veto is fixed by
/// a test plus a normalization HERE, or by fixing the vocabulary;
/// never by an exception.
fn fold(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let lowered = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let no_diacritics: String = lowered
        .nfd()
        .filter(|c| unicode_normalization::char::canonical_combining_class(*c) == 0)
        .collect();
    no_diacritics
        .replace('\u{2019}', "'")
        .replace("'s ", " ")
        .replace("'s", " ")
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `haystack.contains(needle)` with token boundaries: the characters
/// immediately around the match must not continue a word, so the
/// registry name "aman" does not match "amanda". Direct lookup of a
/// KNOWN name — not a heuristic over unknown tokens.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before = haystack[..start]
            .chars()
            .next_back()
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);
        let after = haystack[end..]
            .chars()
            .next()
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);
        if !before && !after {
            return true;
        }
        from = start + 1;
    }
    false
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
    /// When set, a summary naming a registry entity its members never
    /// carry is vetoed BEFORE any judge call — deterministic, free, and
    /// recorded as `name_violations` (the gestalt probe is not spent on
    /// a verdict already decided).
    name_registry: Option<Arc<SummaryNameRegistry>>,
}

impl JudgeSummaryVerifier {
    pub fn new(inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            inference,
            name_registry: None,
        }
    }

    /// Attach the corpus's atlas entity vocabulary (see
    /// [`SummaryNameRegistry`]). Absent = the veto is off and behavior
    /// is exactly pre-registry; the CALLER reports absence.
    pub fn with_name_registry(mut self, registry: Arc<SummaryNameRegistry>) -> Self {
        self.name_registry = Some(registry);
        self
    }

    /// Attach the registry read from `<index_dir>/atlas/ontology.json`.
    /// Returns `(verifier, names_loaded)`; `0` means the veto is OFF and
    /// the reason is on the log — absence (`no atlas/ontology.json`:
    /// corpora without an atlas, where there is no vocabulary by
    /// construction) logs at info, an UNREADABLE atlas file logs at
    /// warn, because that is a defect degrading the gate, not a
    /// shape of corpus. Either way the build proceeds: the gestalt
    /// probe still owns faithfulness, and the summary line's
    /// `name registry 0 names` makes the neutered state visible.
    pub fn attach_atlas_registry(self, index_dir: &std::path::Path) -> (Self, usize) {
        let path = index_dir.join("atlas").join("atoms.json");
        if !path.exists() {
            tracing::info!(
                path = %path.display(),
                "summary verifier: no atlas ontology — name veto off for this corpus"
            );
            return (self, 0);
        }
        match SummaryNameRegistry::from_atlas_atoms(&path) {
            Ok(registry) if !registry.is_empty() => {
                let n = registry.len();
                tracing::info!(
                    path = %path.display(),
                    names = n,
                    "summary verifier: name veto armed from the atlas vocabulary"
                );
                (self.with_name_registry(Arc::new(registry)), n)
            }
            Ok(_) => {
                tracing::warn!(
                    path = %path.display(),
                    "summary verifier: atlas ontology holds no Entity atoms — name veto off"
                );
                (self, 0)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "summary verifier: atlas ontology UNREADABLE — name veto off (defect, not absence)"
                );
                (self, 0)
            }
        }
    }
}

/// The member texts the name veto and the whole-summary probe judge a summary
/// against: every member the writer read (`raptor_atlas`'s
/// `build_abstractive_request` reads full member text since 2026-10-02).
/// [`MEMBER_CAP`] bounds the per-claim fan-out only; a judge reading the first
/// 12 of a ~20-member cluster can reject a faithful summary for naming what
/// members 13+ say.
fn judged_members(member_texts: &[String]) -> Vec<String> {
    member_texts.to_vec()
}

#[async_trait::async_trait]
impl SummaryVerifier for JudgeSummaryVerifier {
    async fn verify(&self, summary: &str, member_texts: &[String]) -> Option<SummaryVerdict> {
        let members = judged_members(member_texts);
        // The deterministic name veto FIRST: free, and decisive on the
        // corruption class the gestalt probe cannot see. The probe is
        // not spent behind an already-false verdict; `None` violation
        // + non-empty violations reads as "blocked by the veto".
        if let Some(registry) = &self.name_registry {
            let violations = registry.violations(summary, &members);
            if !violations.is_empty() {
                return Some(SummaryVerdict {
                    claims_total: 0,
                    claims_unsupported: 0,
                    whole_summary_violation: None,
                    name_violations: violations,
                });
            }
        }
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
                name_violations: Vec::new(),
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
            for passage in members.iter().take(MEMBER_CAP) {
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
            name_violations: Vec::new(),
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
    /// Entity names the name registry loaded (0 = no registry — either
    /// the corpus has no atlas or its ontology was unreadable; the
    /// build log names which). Reported so a silent veto absence —
    /// the neutered case — cannot pass as a clean gate.
    pub registry_names: AtomicUsize,
}

impl VerifyStats {
    pub fn bump(counter: &AtomicUsize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn summary_line(&self) -> String {
        format!(
            "verified {} · pass {} · retry {} (pass {}) · extractive fallback {} · verifier failures {} · name registry {} names",
            self.verified.load(Ordering::Relaxed),
            self.passed_first.load(Ordering::Relaxed),
            self.retried.load(Ordering::Relaxed),
            self.passed_retry.load(Ordering::Relaxed),
            self.fell_back.load(Ordering::Relaxed),
            self.verifier_failed.load(Ordering::Relaxed),
            self.registry_names.load(Ordering::Relaxed),
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
            name_violations: Vec::new(),
        };
        assert!(probe_passed.passed());
        // The measured faithful band's top (0.653) must pass the calibrated
        // cut — this is the pin that the 2026-09-22 calibration cannot be
        // silently reverted to the per-claim 0.5 (which failed everything).
        let in_faithful_band = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.65),
            name_violations: Vec::new(),
        };
        assert!(
            in_faithful_band.passed(),
            "the faithful band's measured maximum (0.653) must clear WHOLE_SUMMARY_TAU"
        );
        let probe_failed_with_clean_claims = SummaryVerdict {
            claims_total: 3,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.9),
            name_violations: Vec::new(),
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
            name_violations: Vec::new(),
        };
        assert!(
            !claims_only_pass.passed(),
            "no probe answer is not a pass — unverifiable falls to the floor"
        );
        // The name veto is its own blocker, even beside a passing probe.
        let vetoed = SummaryVerdict {
            claims_total: 0,
            claims_unsupported: 0,
            whole_summary_violation: Some(0.1),
            name_violations: vec!["Zarkon".into()],
        };
        assert!(
            !vetoed.passed(),
            "a summary naming an entity its members never carry must not pass"
        );
    }

    /// The veto judges against every member the writer read: a name carried
    /// only by the 13th member is the cluster's own, not foreign.
    #[test]
    fn the_veto_sees_every_member_the_writer_read() {
        let registry = SummaryNameRegistry {
            entities: vec![EntityForms {
                display: "Verloc".into(),
                forms: vec!["verloc".into()],
            }],
        };
        let mut members: Vec<String> = (0..MEMBER_CAP)
            .map(|i| format!("Passage {i} follows the river."))
            .collect();
        members.push("Verloc keeps the shop.".into());
        let window = judged_members(&members);
        assert!(
            registry
                .violations("Verloc keeps a shop by the river.", &window)
                .is_empty(),
            "a name from member {} was judged foreign",
            MEMBER_CAP + 1
        );
    }

    #[test]
    fn the_registry_vetoes_a_foreign_name_and_passes_member_carried_aliases() {
        let registry = SummaryNameRegistry {
            entities: vec![
                EntityForms {
                    display: "Verloc".into(),
                    forms: vec!["verloc".into(), "winnie verloc".into()],
                },
                EntityForms {
                    display: "Great Torungen".into(),
                    forms: vec!["great torungen".into()],
                },
            ],
        };
        let members = vec!["Mr. Verloc kept his shop in Brett Street.".to_string()];
        // A member-carried entity is never a violation, whatever form
        // the summary uses: the alias "Winnie Verloc" over the
        // member's "Verloc" is cohesion — the exact case the deleted
        // per-name containment veto false-vetoed 9/12 on.
        assert!(registry
            .violations("Verloc closed the shop.", &members)
            .is_empty());
        assert!(registry
            .violations("Winnie Verloc wept.", &members)
            .is_empty());
        // Boundary safety cuts both ways: "Torungen" inside
        // "Torungenese" is not the entity, so the summary names
        // nothing its members lack.
        assert!(registry
            .violations("the Great Torungenese coast", &members)
            .is_empty());
        // The true positive: an entity no member carries, named by a
        // form the registry knows — the cross-cluster swap shape.
        let mut with_foreign = registry.clone();
        with_foreign.entities.push(EntityForms {
            display: "Zarkon".into(),
            forms: vec!["zarkon".into()],
        });
        assert_eq!(
            with_foreign.violations("Zarkon watched from the shop.", &members),
            vec!["Zarkon".to_string()]
        );
    }

    #[test]
    fn the_closed_normalization_set_is_the_only_forgiveness() {
        let registry = SummaryNameRegistry {
            entities: vec![EntityForms {
                display: "Histiæa".into(),
                forms: vec![fold("Histiæa")],
            }],
        };
        let members = vec![
            "The Histiæan tetrobols circulated widely.".to_string(),
            "Coins of Histiaea are rare.".to_string(),
        ];
        // Diacritic fold: the summary spells it the plain way, the
        // registry holds the ligature form — same entity.
        assert!(registry.violations("Histiaea's coins", &members).is_empty());
        // Possessive and separator fold.
        let registry = SummaryNameRegistry {
            entities: vec![EntityForms {
                display: "Great Torungen".into(),
                forms: vec![fold("Great Torungen")],
            }],
        };
        let members = vec!["The Great Torungen light was lit.".to_string()];
        assert!(registry
            .violations("the Great-Torungen lighthouse's lamp", &members)
            .is_empty());
    }

    #[test]
    fn the_registry_reads_entity_atoms_with_aliases_from_ontology_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ontology.json");
        std::fs::write(
            &path,
            r#"{"atoms":[
                {"atom_type":"Entity","data":{"id":"entity-0001","canonical_name":"Winnie Verloc","aliases":["Winnie"],"entity_type":"person"}},
                {"atom_type":"Claim","data":{"id":"claim-1","text":"x"}},
                {"atom_type":"Entity","data":{"id":"entity-0002","canonical_name":"Adolf Verloc","aliases":[]}}
            ]}"#,
        )
        .expect("write atoms fixture");
        let registry = SummaryNameRegistry::from_atlas_atoms(&path).expect("registry parses");
        assert_eq!(
            registry.len(),
            2,
            "two Entity atoms; the Claim atom contributes nothing"
        );
        // The alias rode in with its entity: members carry only the
        // canonical "Adolf Verloc", so a summary naming the alias
        // "Winnie" vetoes on the ENTITY it belongs to.
        let members = vec!["Adolf Verloc waited.".to_string()];
        let violations = registry.violations("Winnie waited too.", &members);
        assert_eq!(violations, vec!["Winnie Verloc".to_string()]);
        // And the canonical form itself, when carried, is no violation.
        assert!(registry
            .violations("Adolf Verloc waited too.", &members)
            .is_empty());
    }
}
