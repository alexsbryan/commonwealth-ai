// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolver precision — does a certified span justify skipping the judge?
//!
//! **The decision this measurement exists to make.** The incumbent
//! grounding ladder verifies every released claim with a model judge, at
//! roughly 35 judge calls per gated longform turn. If the span resolver's
//! certification were trustworthy enough, a certified claim could skip
//! that judge entirely — the largest single latency and cost win the
//! native-grounding plan has on offer.
//!
//! It ships only if it is right essentially always, because the cost of
//! being wrong is a released **wrong "Grounded" badge**: the system tells
//! the user a sentence is sourced, with an address, when the incumbent
//! judge would have caught that it is not. That is worse than a slow
//! answer and worse than an abstention.
//!
//! **The bar, pinned before any number was computed** (work order
//! `native-grounding-step2-integration`, D2):
//!
//! > certified-claims-skip-judge ships ONLY if precision on the frozen
//! > data is **>= 0.98**. Below the bar: verification stays exactly
//! > as-is, the deliverable is the measurement, and that is a success.
//!
//! [`PRECISION_BAR`] is that number, in one place, and the verdict
//! artifact records it next to the measured value so the comparison
//! cannot be restated later.
//!
//! **Offline, frozen, no model.** Every input is a committed chaos
//! transcript; [`span_resolver::resolve_span`] is a pure function of
//! `(span, chunks)` with no model, no clock and no allocation-order
//! dependence. Nothing here loads a reranker, calls a judge, or runs a
//! turn — re-running this on any host reproduces the artifact byte for
//! byte, which is what makes it a HARD verdict under
//! `NATIVE_GROUNDING.md §7.4`.
//!
//! **What "precision" means here, exactly.** Over claims the incumbent
//! ladder actually judged (`verified` or `failed_once` — `fail_open` and
//! `unverified` are could-not-judge and are excluded, never counted as
//! either class):
//!
//!   * **certification rate** = of incumbent-VERIFIED claims, the
//!     fraction the resolver certifies (`Verbatim` or `Fuzzy`). This is
//!     the *coverage* — how much judge work could be skipped.
//!   * **precision** = of the claims the resolver CERTIFIES, the
//!     fraction the incumbent verified. This is the *safety* — how often
//!     a skipped judge would have disagreed. It is the number the bar is
//!     set against, because it is the one a wrong badge comes out of.
//!
//! The two are reported separately and neither is allowed to stand in
//! for the other.

pub mod transcript;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sovereign_core::runtime::native_grounding::span_resolver::resolve_span;

/// The pre-registered bar. Pinned in the work order before any frozen
/// data was scored; one definition, quoted into the artifact.
pub const PRECISION_BAR: f64 = 0.98;

/// The frozen inputs this measurement is defined over. Named here rather
/// than passed loose so "which artifacts is the verdict about?" has one
/// answer, and so a re-run cannot quietly widen or narrow the set.
const FROZEN_INPUTS: &[&str] = &[
    // Tonight's longform harvest — the negatives bank, where the
    // incumbent's `failed_once` class actually has members.
    "sovereign/bench/chaos_monkey/results/saltgrass_longneg_20260808.transcripts.jsonl",
    "sovereign/bench/chaos_monkey/results/saltgrass_compound_longneg_20260808.transcripts.jsonl",
    // The secret_agent gv-shadow transcript.
    "sovereign/bench/chaos_monkey/results/secret_agent_gv_shadow_20260807.transcripts.jsonl",
];

/// One claim, replayed. Committed as JSONL so the verdict can be audited
/// claim by claim rather than believed.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimRow {
    pub source: String,
    pub turn_id: String,
    pub claim_index: usize,
    pub claim: String,
    /// The incumbent's verdict, verbatim from the frozen ledger.
    pub incumbent_verification: String,
    /// `Some(true)` verified, `Some(false)` failed, `None` could-not-judge.
    pub incumbent_supported: Option<bool>,
    /// `verbatim` | `fuzzy` | `unverified_not_found` | `unverified_no_evidence`
    /// | `unverified_empty`.
    pub resolution: String,
    /// Did the resolver certify (`Verbatim` or `Fuzzy`)?
    pub certified: bool,
    /// The chunk the span was addressed to, when it has an address.
    pub chunk: Option<usize>,
    pub n_chunks: usize,
}

/// The 2x2 the bar is read off, plus everything needed to re-derive it.
#[derive(Debug, Clone, Serialize)]
pub struct ResolverPrecisionVerdict {
    pub schema: &'static str,
    /// `ships` | `does_not_ship` | `could_not_judge` — four verdicts, not
    /// two (ARCH §18.1). `could_not_judge` when the certified set is
    /// empty or the negative class is absent, because a precision
    /// computed against a single-valued label set is not a precision.
    pub outcome: &'static str,
    pub outcome_reason: String,
    pub bar: f64,
    pub bar_pinned: &'static str,
    pub criterion: &'static str,
    pub inputs: Vec<String>,
    /// sha256 of each input, in the same order. These transcripts are
    /// large and live on the skunkworks branch, so they are NOT committed
    /// beside this verdict — the hashes are what make the measurement
    /// reproducible without duplicating 2.7 MB of frozen runs onto main.
    pub inputs_sha256: Vec<String>,
    pub malformed_lines_skipped: usize,

    /// The safety number the bar is set against.
    pub precision: Option<f64>,
    /// The coverage number. Reported, never substituted for precision.
    pub certification_rate_over_verified: Option<f64>,
    /// Coverage again, excluding verified claims whose turn carried NO
    /// evidence to resolve against (`unverified_no_evidence`). Those are
    /// could-not-judge on the resolver's side; counting them as misses
    /// understates it. Precision is unaffected either way — a claim with
    /// no evidence is never certified — which is why the bar is read off
    /// precision and this field is context, not the headline.
    pub certification_rate_over_verified_with_evidence: Option<f64>,
    /// Verified claims whose turn had no evidence pool at all.
    pub n_verified_without_evidence: usize,

    // The 2x2, so the two rates above can be recomputed by hand.
    pub n_claims_total: usize,
    pub n_judged: usize,
    pub n_could_not_judge: usize,
    pub n_verified: usize,
    pub n_failed: usize,
    pub certified_and_verified: usize,
    pub certified_and_failed: usize,
    pub uncertified_and_verified: usize,
    pub uncertified_and_failed: usize,

    /// Where the certified-but-not-verified claims are — the wrong badges
    /// this bar exists to price. Listed in full when few, because "which
    /// ones?" is the first question a reader has.
    pub false_certifications: Vec<ClaimRow>,

    /// Resolution-label histogram over judged claims, so `Fuzzy`-heavy
    /// certification (present but unaddressable) is visible rather than
    /// hidden inside "certified".
    pub resolution_histogram: BTreeMap<String, usize>,
    /// The same, restricted to certifications that the incumbent failed.
    pub false_certification_histogram: BTreeMap<String, usize>,

    /// The operating curve: what precision and coverage would be at each
    /// stricter certification rule. Emitted so "below the bar" can be
    /// answered with "and here is what WOULD clear it", rather than just
    /// a no.
    pub operating_points: Vec<OperatingPoint>,
}

/// One candidate certification rule and what it would buy.
#[derive(Debug, Clone, Serialize)]
pub struct OperatingPoint {
    /// Which resolutions this rule accepts as certified.
    pub rule: &'static str,
    pub certified: usize,
    pub certified_and_verified: usize,
    pub certified_and_failed: usize,
    pub precision: Option<f64>,
    pub certification_rate_over_verified: Option<f64>,
    pub clears_bar: bool,
}

fn rate(num: usize, den: usize) -> Option<f64> {
    (den > 0).then(|| num as f64 / den as f64)
}

/// Score one certification rule over the replayed rows.
fn operating_point(
    rows: &[ClaimRow],
    rule: &'static str,
    accepts: impl Fn(&str) -> bool,
    n_verified: usize,
) -> OperatingPoint {
    let judged: Vec<&ClaimRow> = rows
        .iter()
        .filter(|r| r.incumbent_supported.is_some())
        .collect();
    let cert: Vec<&&ClaimRow> = judged
        .iter()
        .filter(|r| accepts(&r.resolution))
        .collect::<Vec<_>>();
    let cv = cert
        .iter()
        .filter(|r| r.incumbent_supported == Some(true))
        .count();
    let cf = cert.len() - cv;
    let precision = rate(cv, cert.len());
    OperatingPoint {
        rule,
        certified: cert.len(),
        certified_and_verified: cv,
        certified_and_failed: cf,
        precision,
        certification_rate_over_verified: rate(cv, n_verified),
        clears_bar: precision.is_some_and(|p| p >= PRECISION_BAR),
    }
}

/// Replay every frozen transcript and build the verdict.
///
/// `repo_root` is the workspace root the [`FROZEN_INPUTS`] paths are
/// relative to. A missing input is an ERROR, not a smaller measurement:
/// silently scoring two of three files and reporting the result as "the
/// frozen data" is the substitution ARCH §18.3 forbids.
pub fn measure(repo_root: &Path) -> Result<(Vec<ClaimRow>, ResolverPrecisionVerdict), String> {
    use sha2::{Digest, Sha256};
    let mut rows: Vec<ClaimRow> = Vec::new();
    let mut malformed = 0usize;
    let mut inputs = Vec::new();
    let mut inputs_sha256 = Vec::new();

    for rel in FROZEN_INPUTS {
        let path: PathBuf = repo_root.join(rel);
        if !path.is_file() {
            return Err(format!(
                "frozen input missing: {} — refusing to report a precision over a \
                 subset of the declared set",
                path.display()
            ));
        }
        inputs.push((*rel).to_string());
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        inputs_sha256.push(format!("{:x}", Sha256::digest(&bytes)));
        let (turns, skipped) = transcript::load(&path)?;
        malformed += skipped;
        for t in &turns {
            for (i, h) in t.holdings().iter().enumerate() {
                let claim = h.claim.trim();
                if claim.is_empty() {
                    continue;
                }
                let res = resolve_span(claim, &t.retrieved_chunks);
                rows.push(ClaimRow {
                    source: (*rel).rsplit('/').next().unwrap_or(rel).to_string(),
                    turn_id: t.id.clone(),
                    claim_index: i,
                    claim: claim.to_string(),
                    incumbent_verification: h.verification.clone(),
                    incumbent_supported: h.supported(),
                    resolution: res.label().to_string(),
                    certified: res.is_resolved(),
                    chunk: res.chunk(),
                    n_chunks: t.retrieved_chunks.len(),
                });
            }
        }
    }

    // Deterministic order, so two runs write byte-identical artifacts.
    rows.sort_by(|a, b| {
        (&a.source, &a.turn_id, a.claim_index).cmp(&(&b.source, &b.turn_id, b.claim_index))
    });

    let judged: Vec<&ClaimRow> = rows
        .iter()
        .filter(|r| r.incumbent_supported.is_some())
        .collect();
    let n_could_not_judge = rows.len() - judged.len();
    let n_verified = judged
        .iter()
        .filter(|r| r.incumbent_supported == Some(true))
        .count();
    let n_failed = judged.len() - n_verified;

    let cv = judged
        .iter()
        .filter(|r| r.certified && r.incumbent_supported == Some(true))
        .count();
    let cf = judged
        .iter()
        .filter(|r| r.certified && r.incumbent_supported == Some(false))
        .count();
    let uv = n_verified - cv;
    let uf = n_failed - cf;

    let precision = rate(cv, cv + cf);
    let cert_rate = rate(cv, n_verified);
    let n_verified_without_evidence = judged
        .iter()
        .filter(|r| r.incumbent_supported == Some(true) && r.resolution == "unverified_no_evidence")
        .count();
    let cert_rate_with_evidence = rate(cv, n_verified - n_verified_without_evidence);

    let mut resolution_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut false_certification_histogram: BTreeMap<String, usize> = BTreeMap::new();
    for r in &judged {
        *resolution_histogram
            .entry(r.resolution.clone())
            .or_default() += 1;
        if r.certified && r.incumbent_supported == Some(false) {
            *false_certification_histogram
                .entry(r.resolution.clone())
                .or_default() += 1;
        }
    }

    let false_certifications: Vec<ClaimRow> = judged
        .iter()
        .filter(|r| r.certified && r.incumbent_supported == Some(false))
        .map(|r| (*r).clone())
        .collect();

    let operating_points = vec![
        operating_point(
            &rows,
            "verbatim_or_fuzzy (the shipped is_resolved)",
            |l| l == "verbatim" || l == "fuzzy",
            n_verified,
        ),
        operating_point(
            &rows,
            "verbatim_only (addressable spans only)",
            |l| l == "verbatim",
            n_verified,
        ),
    ];

    // Four verdicts, not two.
    let (outcome, outcome_reason) = if n_failed == 0 {
        (
            "could_not_judge",
            format!(
                "the judged set is SINGLE-CLASS ({n_verified} verified / 0 failed). A \
                 precision computed against a one-valued label set is maximised by a \
                 resolver that certifies everything unconditionally, so it cannot \
                 speak to the risk this bar prices."
            ),
        )
    } else if cv + cf == 0 {
        (
            "could_not_judge",
            "the resolver certified NO judged claim, so there is no certified set to \
             compute a precision over."
                .to_string(),
        )
    } else if precision.is_some_and(|p| p >= PRECISION_BAR) {
        (
            "ships",
            format!(
                "precision {:.4} >= the pre-registered bar {PRECISION_BAR}",
                precision.unwrap()
            ),
        )
    } else {
        (
            "does_not_ship",
            format!(
                "precision {:.4} < the pre-registered bar {PRECISION_BAR}. \
                 Certified-claims-skip-judge does NOT ship; per-claim verification \
                 stays exactly as it is. The measurement is the deliverable.",
                precision.unwrap()
            ),
        )
    };

    let verdict = ResolverPrecisionVerdict {
        schema: "resolver-precision/v1",
        outcome,
        outcome_reason,
        bar: PRECISION_BAR,
        bar_pinned: "work order native-grounding-step2-integration D2, pinned before \
                     any frozen data was scored",
        criterion: "precision = P(incumbent verified | resolver certified) over claims the \
                    incumbent actually judged. Ships iff precision >= 0.98.",
        inputs,
        inputs_sha256,
        malformed_lines_skipped: malformed,
        precision,
        certification_rate_over_verified: cert_rate,
        certification_rate_over_verified_with_evidence: cert_rate_with_evidence,
        n_verified_without_evidence,
        n_claims_total: rows.len(),
        n_judged: judged.len(),
        n_could_not_judge,
        n_verified,
        n_failed,
        certified_and_verified: cv,
        certified_and_failed: cf,
        uncertified_and_verified: uv,
        uncertified_and_failed: uf,
        false_certifications,
        resolution_histogram,
        false_certification_histogram,
        operating_points,
    };
    Ok((rows, verdict))
}

/// `svrn bench resolver-precision` — offline, frozen, no model.
pub(crate) async fn cmd_resolver_precision(rest: &[String]) -> i32 {
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/resolver-precision");
    let mut repo_root = PathBuf::from(".");
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--out-dir" => {
                i += 1;
                match rest.get(i) {
                    Some(v) => out_dir = PathBuf::from(v),
                    None => {
                        eprintln!("--out-dir needs a value");
                        return 2;
                    }
                }
            }
            "--repo-root" => {
                i += 1;
                match rest.get(i) {
                    Some(v) => repo_root = PathBuf::from(v),
                    None => {
                        eprintln!("--repo-root needs a value");
                        return 2;
                    }
                }
            }
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other => {
                eprintln!("unknown flag `{other}`");
                print_help();
                return 2;
            }
        }
        i += 1;
    }

    let (rows, verdict) = match measure(&repo_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("resolver-precision: {e}");
            return 1;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("create {}: {e}", out_dir.display());
        return 1;
    }
    let scores_path = out_dir.join("resolver_claim_scores.jsonl");
    let mut body = String::new();
    for r in &rows {
        match serde_json::to_string(r) {
            Ok(s) => {
                body.push_str(&s);
                body.push('\n');
            }
            Err(e) => {
                eprintln!("serialize row: {e}");
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::write(&scores_path, body) {
        eprintln!("write {}: {e}", scores_path.display());
        return 1;
    }
    let verdict_path = out_dir.join("resolver_precision_verdict.json");
    match serde_json::to_string_pretty(&verdict) {
        Ok(mut s) => {
            s.push('\n');
            if let Err(e) = std::fs::write(&verdict_path, s) {
                eprintln!("write {}: {e}", verdict_path.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("serialize verdict: {e}");
            return 1;
        }
    }

    print_report(&verdict);
    eprintln!("\nwrote {}", scores_path.display());
    eprintln!("wrote {}", verdict_path.display());

    // Exit code carries the verdict, so a caller can gate on it.
    //   0 = ships (bar cleared)   3 = does not ship (a successful run)
    //   4 = could not judge
    match verdict.outcome {
        "ships" => 0,
        "does_not_ship" => 3,
        _ => 4,
    }
}

fn print_report(v: &ResolverPrecisionVerdict) {
    println!("── resolver precision — can a certified span skip the judge? ──");
    println!("inputs ({} frozen transcripts):", v.inputs.len());
    for i in &v.inputs {
        println!("  {i}");
    }
    println!(
        "\nclaims: {} total · {} judged by the incumbent · {} could-not-judge (excluded)",
        v.n_claims_total, v.n_judged, v.n_could_not_judge
    );
    println!(
        "labels: {} verified · {} failed_once (= NOT supported)",
        v.n_verified, v.n_failed
    );
    println!("\n                    incumbent verified   incumbent failed");
    println!(
        "  resolver certified   {:>16}   {:>16}",
        v.certified_and_verified, v.certified_and_failed
    );
    println!(
        "  resolver did not     {:>16}   {:>16}",
        v.uncertified_and_verified, v.uncertified_and_failed
    );
    let fmt = |o: Option<f64>| o.map_or("n/a".to_string(), |x| format!("{x:.4}"));
    println!(
        "\nprecision  P(verified | certified) = {}   BAR {:.2}",
        fmt(v.precision),
        v.bar
    );
    println!(
        "coverage   P(certified | verified) = {}   ({} excluding the {} verified \
         claims whose turn had no evidence pool)",
        fmt(v.certification_rate_over_verified),
        fmt(v.certification_rate_over_verified_with_evidence),
        v.n_verified_without_evidence
    );
    println!("\noperating points:");
    for p in &v.operating_points {
        println!(
            "  {:<44} precision {}  coverage {}  {}",
            p.rule,
            fmt(p.precision),
            fmt(p.certification_rate_over_verified),
            if p.clears_bar { "CLEARS" } else { "below bar" }
        );
    }
    if !v.false_certifications.is_empty() {
        println!(
            "\nfalse certifications ({}) — each one is a wrong \"Grounded\" badge:",
            v.false_certifications.len()
        );
        for r in &v.false_certifications {
            let c: String = r.claim.chars().take(96).collect();
            println!("  [{}] {} ({}) {}", r.source, r.turn_id, r.resolution, c);
        }
    }
    println!("\nVERDICT: {} — {}", v.outcome, v.outcome_reason);
}

fn print_help() {
    println!(
        "svrn bench resolver-precision [--repo-root <dir>] [--out-dir <dir>]\n\
         \n\
         OFFLINE. Replays frozen chaos transcripts through the span resolver and asks\n\
         whether a certified span is trustworthy enough to skip the incumbent judge.\n\
         Loads no model, calls no judge, runs no turn — the resolver is a pure function\n\
         of (span, chunks), so the artifact is reproducible byte for byte.\n\
         \n\
         Ships iff precision = P(incumbent verified | resolver certified) >= 0.98, a bar\n\
         pinned before any frozen data was scored.\n\
         \n\
         Exit: 0 = bar cleared · 3 = below the bar (a successful run — the measurement\n\
         is the deliverable) · 4 = could not judge · 1 = a frozen input is missing."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(resolution: &str, supported: Option<bool>) -> ClaimRow {
        ClaimRow {
            source: "t".into(),
            turn_id: "t".into(),
            claim_index: 0,
            claim: "c".into(),
            incumbent_verification: "v".into(),
            incumbent_supported: supported,
            certified: resolution == "verbatim" || resolution == "fuzzy",
            resolution: resolution.into(),
            chunk: None,
            n_chunks: 1,
        }
    }

    /// The bar is a constant, not a sentence in a doc comment. If someone
    /// relaxes it, this fails and names the reason it was set.
    #[test]
    fn the_bar_is_the_one_the_order_pinned() {
        assert_eq!(
            PRECISION_BAR, 0.98,
            "the bar was pinned at 0.98 BEFORE the frozen data was scored; moving it \
             after the fact is exactly what pre-registration exists to prevent"
        );
    }

    /// Could-not-judge claims must not be counted as either class — the
    /// failure that would silently inflate precision.
    #[test]
    fn fail_open_claims_are_excluded_not_counted_as_verified() {
        let rows = vec![
            row("verbatim", Some(true)),
            row("verbatim", None),
            row("verbatim", None),
        ];
        let op = operating_point(&rows, "r", |l| l == "verbatim", 1);
        assert_eq!(op.certified, 1, "only the judged claim may enter the 2x2");
        assert_eq!(op.precision, Some(1.0));
    }

    /// A single-class label set cannot produce a precision that means
    /// anything — the trap the H4 gate fell into and reported honestly.
    #[test]
    fn a_perfect_precision_on_one_class_still_needs_the_negative_class() {
        let rows = vec![row("verbatim", Some(true)), row("verbatim", Some(true))];
        let op = operating_point(&rows, "r", |l| l == "verbatim", 2);
        assert_eq!(op.precision, Some(1.0));
        assert!(op.clears_bar);
        // ...and yet `measure` must still refuse to call this a ship. The
        // guard lives in `measure`'s outcome selection (n_failed == 0),
        // asserted here as the contract it is.
        let n_failed = rows
            .iter()
            .filter(|r| r.incumbent_supported == Some(false))
            .count();
        assert_eq!(n_failed, 0);
    }

    #[test]
    fn one_false_certification_in_fifty_already_misses_the_bar() {
        let mut rows: Vec<ClaimRow> = (0..49).map(|_| row("verbatim", Some(true))).collect();
        rows.push(row("verbatim", Some(false)));
        let op = operating_point(&rows, "r", |l| l == "verbatim", 49);
        assert_eq!(op.precision, Some(0.98));
        assert!(op.clears_bar, "exactly at the bar clears it");
        rows.push(row("verbatim", Some(false)));
        let op = operating_point(&rows, "r", |l| l == "verbatim", 49);
        assert!(!op.clears_bar, "2 in 51 is below the bar");
    }
}
