// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h4-gate` — NATIVE_GROUNDING §7.3's H4 gate, offline.
//!
//! Replays frozen chaos transcripts through the span resolver and the margin
//! fold, calibrates the margin floor on one split, and reports agreement with
//! the incumbent's per-claim verdicts on the other. No judge is re-invoked; the
//! incumbent's verdicts are read out of the transcripts it already froze.
//!
//! # The split, and why it falls this way
//!
//! §7.1 fixes three data roles. This gate honours them rather than inventing a
//! split: the **calibration** transcripts fit the floor and are never scored for
//! the verdict, and the **held-out** transcripts produce the number. Which file
//! plays which role is the caller's `--calibrate` / `--holdout` choice, and the
//! verdict artifact records it, because a floor fitted on the data it is scored
//! against is not a measurement.
//!
//! # The unit of comparison is the CLAIM, not the sentence
//!
//! The incumbent's retained per-claim record (`GateOutcome.claims` →
//! `epistemic_state.holdings[]`) is keyed on the ladder's *extracted claims* —
//! paraphrases, not spans of the answer. To compare verdict-for-verdict, the
//! mechanical side scores the same unit: `max_i margin(claim, chunk_i)` over the
//! turn's k-capped pool, through the same fold the sentence sweep uses
//! (`sentence_sweep::margin_over_pool` — one formula, two units).
//!
//! Joining each claim to the answer sentence carrying it was the alternative,
//! and it was rejected on a seam: the join already exists as
//! `surgical::best_match`, but it is a private `fn` inside `surgical`, so using
//! it would have meant widening the visibility of an existing runtime path (the
//! order forbids editing them) or re-deriving its overlap matcher in the new
//! module (principle 8's smell — two implementations of one formula). Scoring
//! the incumbent's own unit needs neither. The sentence sweep still runs on
//! every held-out turn: it supplies the audit wall-time bar and the per-sentence
//! artifact.
//!
//! # The high-margin restriction
//!
//! §7.3 H4 restricts agreement to claims the incumbent judged with high margin,
//! `|vp − τ| > 0.2`, because incumbent verdicts flip near the boundary
//! (ARCH §18.5). `violation_prob` is a per-TURN quantity, so the restriction is
//! evaluated per turn and applied to that turn's claims. **A claim on a turn
//! with no `vp` is not high-margin and is not low-margin — it is
//! could-not-judge**, counted and reported separately, because a run that never
//! consulted the Critic said nothing about that turn's confidence (§18.3).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use sovereign_core::runtime::native_grounding::sentence_sweep::{
    self, SentenceScorer,
};
use sovereign_core::runtime::native_grounding::span_resolver;
use sovereign_eval::flywheel::operating_curve::{self as oc, ScoredPair};

use super::{scorer, transcript};

/// §7.3 H4: "≥0.90 verdict agreement on claims the incumbent judged with high
/// margin".
const BAR_AGREEMENT: f64 = 0.90;
/// §7.3 H4: "audit time ≤ 2s p50".
const BAR_AUDIT_P50_MS: u128 = 2000;
/// §7.3 H4 kill: "if span/margin scoring disagrees with the incumbent on >25%
/// of high-margin claims".
const KILL_DISAGREEMENT: f64 = 0.25;
/// §7.3 H4: the high-margin band half-width around τ.
const DEFAULT_HIGH_MARGIN_BAND: f64 = 0.2;
/// The adjudication sample the kill path must prepare.
const ADJUDICATION_SAMPLE_N: usize = 20;

/// One claim, scored both ways.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimScore {
    /// Which transcript this came from.
    pub source: String,
    /// `calibration` or `holdout`. Recorded on the row so a scores file can
    /// never be re-split after the fact.
    pub split: String,
    pub turn_id: String,
    pub claim_index: usize,
    pub claim: String,
    /// The raw ledger string, kept so a reader can audit the interpretation.
    pub incumbent_verification: String,
    /// The interpreted verdict. See `transcript::Holding::supported` — and note
    /// that `failed_once` is the NEGATIVE class.
    pub incumbent_supported: bool,
    /// `max_i margin(claim, chunk_i)`. `null` = could-not-judge.
    pub margin: Option<f32>,
    pub best_chunk: Option<usize>,
    /// The span resolver's label for the claim text.
    pub span: String,
    pub turn_violation_prob: Option<f64>,
    pub turn_gate_action: Option<String>,
    /// `|vp − τ| > band`, with a turn that has no vp counted as NOT high-margin.
    pub high_margin: bool,
    /// Wall time of the whole-turn sentence sweep this claim's turn incurred.
    pub turn_sweep_ms: u128,
}

/// Five-number summary. Emitted rather than a mean because §7.3 asks for the
/// margin DISTRIBUTION, so a thin or lopsided set cannot hide behind an average.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Distribution {
    pub n: usize,
    pub min: f64,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
    pub max: f64,
}

impl Distribution {
    pub fn of(mut v: Vec<f64>) -> Option<Self> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
        Some(Self {
            n: v.len(),
            min: v[0],
            p25: q(0.25),
            median: q(0.5),
            p75: q(0.75),
            max: v[v.len() - 1],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H4Outcome {
    /// Both §7.3 bars met.
    Beat,
    /// Disagreement under the kill line but a bar unmet.
    Survives,
    /// Disagreement above 25% — the order's PARK path.
    Killed,
    /// The measurement could not be made. Never a pass, never a fail.
    CouldNotJudge,
}

/// What the gate could not measure, named rather than omitted.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotRun {
    pub what: String,
    pub why: String,
    pub what_would_be_needed: String,
}

/// A turn whose claims the incumbent audited but which the mechanical side
/// cannot replay, with the reason.
///
/// This exists because the population it describes is not noise: the longform
/// (`present-maximal-*`) turns are where the incumbent's ~1,400-LOC ladder and
/// its ~35 judge calls live, and they are the class H4 exists to replace. A
/// gate that silently scored only the short-form turns would report a number
/// about the easy half and call it H4.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UnreplayableTurn {
    pub source: String,
    pub turn_id: String,
    pub n_holdings: usize,
    /// How many of those holdings are the incumbent's NEGATIVE class. This is
    /// the number that decides whether the measurement is possible at all.
    pub n_unsupported: usize,
    pub answer_chars: usize,
    pub evidence_chunks: usize,
    pub violation_prob: Option<f64>,
    pub gate_action: Option<String>,
    /// Which field(s) the replay needed and did not find.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct H4Verdict {
    pub outcome: H4Outcome,
    /// Set exactly when `outcome` is `CouldNotJudge`: why the measurement §7.3
    /// specifies could not be made from these artifacts. A gate that cannot
    /// judge says so; it does not fall through to pass or fail.
    pub could_not_judge_reason: Option<String>,
    /// Turns the incumbent audited that the mechanical side cannot replay, with
    /// the missing field named per turn.
    pub unreplayable_turns_with_holdings: Vec<UnreplayableTurn>,
    pub criterion: String,
    // ── (a) agreement on high-margin claims ──
    pub agreement: Option<f64>,
    pub n_high_margin: usize,
    pub n_agree: usize,
    pub n_disagree: usize,
    /// High-margin claims split by the incumbent's own verdict, so a one-sided
    /// label set is visible rather than hidden inside an accuracy.
    pub high_margin_supported: usize,
    pub high_margin_unsupported: usize,
    /// Claims excluded because their turn carried no `violation_prob` — the
    /// Critic was never asked. Could-not-judge, reported, never defaulted.
    pub excluded_no_violation_prob: usize,
    /// Claims excluded because they fell inside the flip band.
    pub excluded_low_margin: usize,
    /// Claims excluded because the mechanical side could not score them.
    pub excluded_unscoreable: usize,
    pub margin_distribution_high_margin: Option<Distribution>,
    pub violation_prob_distribution: Option<Distribution>,
    // ── (b) audit latency ──
    pub audit_p50_ms: Option<u128>,
    pub audit_p90_ms: Option<u128>,
    pub n_turns_timed: usize,
    pub incumbent_judge_calls_per_gated_longform_turn: &'static str,
    // ── (c) fidelity deltas ──
    pub citation_and_grounding_fidelity_delta: NotRun,
    // ── the floor and its provenance ──
    pub floor: f64,
    pub floor_selection_rule: &'static str,
    pub calibration_source: Vec<String>,
    pub calibration_n_claims: usize,
    pub calibration_auroc: f64,
    pub holdout_source: Vec<String>,
    // ── the bars, restated in the artifact ──
    pub bar_agreement: f64,
    pub bar_audit_p50_ms: u128,
    pub kill_disagreement: f64,
    pub tau: f64,
    pub high_margin_band: f64,
}

// ─────────────────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_h4_gate(rest: &[String]) -> i32 {
    let mut calibrate: Vec<PathBuf> = Vec::new();
    let mut holdout: Vec<PathBuf> = Vec::new();
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h4");
    let mut rerank_model: Option<PathBuf> = None;
    let mut from_scores: Option<PathBuf> = None;
    let mut tau: Option<f64> = None;
    let mut band = DEFAULT_HIGH_MARGIN_BAND;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--calibrate" => calibrate.push(PathBuf::from(val!("--calibrate"))),
            "--holdout" => holdout.push(PathBuf::from(val!("--holdout"))),
            "--out-dir" => out_dir = PathBuf::from(val!("--out-dir")),
            "--rerank-model" => rerank_model = Some(PathBuf::from(val!("--rerank-model"))),
            "--from-scores" => from_scores = Some(PathBuf::from(val!("--from-scores"))),
            "--tau" => match val!("--tau").parse() {
                Ok(v) => tau = Some(v),
                Err(e) => {
                    eprintln!("error: --tau: {e}");
                    return 2;
                }
            },
            "--high-margin" => match val!("--high-margin").parse() {
                Ok(v) => band = v,
                Err(e) => {
                    eprintln!("error: --high-margin: {e}");
                    return 2;
                }
            },
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                print_help();
                return 2;
            }
        }
        i += 1;
    }

    // The SHARED production τ, not a private default — the 0.5-vs-0.9 fork
    // (note bbcdf7ec) is exactly the smell one decider exists to prevent.
    let tau = tau.unwrap_or_else(sovereign_core::runtime::grounding_gate_threshold);

    match run(
        &calibrate, &holdout, &out_dir, rerank_model, from_scores, tau, band,
    )
    .await
    {
        Ok(v) => {
            print_report(&v);
            match v.outcome {
                H4Outcome::Beat | H4Outcome::Survives => 0,
                H4Outcome::Killed => 3,
                H4Outcome::CouldNotJudge => 1,
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    calibrate: &[PathBuf],
    holdout: &[PathBuf],
    out_dir: &Path,
    rerank_model: Option<PathBuf>,
    from_scores: Option<PathBuf>,
    tau: f64,
    band: f64,
) -> Result<H4Verdict, String> {
    if calibrate.is_empty() || holdout.is_empty() {
        return Err(
            "both --calibrate and --holdout are required. A floor fitted on the data it is \
             scored against is not a measurement, so this command will not run one split."
                .into(),
        );
    }
    for p in calibrate {
        if holdout.contains(p) {
            return Err(format!(
                "{} appears in BOTH splits — refusing rather than reporting a leaked verdict",
                p.display()
            ));
        }
    }

    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;

    // ── scores: replayed from a frozen file, or measured with the model ──
    let mut unreplayable: Vec<UnreplayableTurn> = Vec::new();
    let scores: Vec<ClaimScore> = match &from_scores {
        Some(p) => {
            eprintln!("[h4] replaying frozen scores from {} — NO model loaded", p.display());
            read_scores(p)?
        }
        None => {
            let rerank_path = scorer::resolve_rerank_path(rerank_model)?;
            let sc = scorer::load(&rerank_path)?;
            let mut all = Vec::new();
            for (paths, split) in [(calibrate, "calibration"), (holdout, "holdout")] {
                for p in paths {
                    let (rows, un) = score_transcript(p, split, &sc, tau, band).await?;
                    all.extend(rows);
                    unreplayable.extend(un);
                }
            }
            let path = out_dir.join("h4_claim_scores.jsonl");
            write_scores(&path, &all)?;
            eprintln!("[out] wrote claim scores → {}", path.display());
            all
        }
    };

    let cal: Vec<&ClaimScore> = scores.iter().filter(|s| s.split == "calibration").collect();
    let hold: Vec<&ClaimScore> = scores.iter().filter(|s| s.split == "holdout").collect();
    if cal.is_empty() || hold.is_empty() {
        return Err(format!(
            "split is empty on one side (calibration {}, holdout {}) — nothing to measure",
            cal.len(),
            hold.len()
        ));
    }

    // ── the floor: fitted on calibration ONLY, with its curve committed ──
    let cal_pairs: Vec<ScoredPair> = cal
        .iter()
        .filter_map(|s| {
            s.margin.map(|m| ScoredPair {
                id: format!("{}#{}", s.turn_id, s.claim_index),
                corpus_id: s.source.clone(),
                // Orientation (operating_curve's contract): higher score =
                // more "answerable". Here that is `supported`, so the curve's
                // honesty_recall is recall on the UNSUPPORTED class and its
                // false_alarm is supported claims wrongly flagged.
                answerable: s.incumbent_supported,
                score: m,
            })
        })
        .collect();
    let curve = oc::build("h4_claim_margin", &cal_pairs).map_err(|e| {
        format!(
            "cannot calibrate the floor: {e}. ({} calibration claims, {} supported / {} not) — \
             a single-class calibration split cannot produce a floor, and this refuses rather \
             than inventing one.",
            cal_pairs.len(),
            cal_pairs.iter().filter(|p| p.answerable).count(),
            cal_pairs.iter().filter(|p| !p.answerable).count()
        )
    })?;
    let curve_path = out_dir.join("h4_margin.calibration.curve.json");
    std::fs::write(
        &curve_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&curve).map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| format!("write {}: {e}", curve_path.display()))?;
    eprintln!("[out] wrote the floor's operating curve → {}", curve_path.display());

    let floor = curve.best_balanced_accuracy_threshold;

    // ── (a) agreement, on the held-out split, high-margin claims only ──
    let mut n_agree = 0usize;
    let mut n_disagree = 0usize;
    let mut excluded_no_vp = 0usize;
    let mut excluded_low_margin = 0usize;
    let mut excluded_unscoreable = 0usize;
    let mut hm_margins: Vec<f64> = Vec::new();
    let mut hm_sup = 0usize;
    let mut hm_unsup = 0usize;
    let mut disagreements: Vec<&ClaimScore> = Vec::new();

    for s in &hold {
        let Some(m) = s.margin else {
            excluded_unscoreable += 1;
            continue;
        };
        if s.turn_violation_prob.is_none() {
            excluded_no_vp += 1;
            continue;
        }
        if !s.high_margin {
            excluded_low_margin += 1;
            continue;
        }
        hm_margins.push(m as f64);
        if s.incumbent_supported {
            hm_sup += 1;
        } else {
            hm_unsup += 1;
        }
        let mechanical = (m as f64) >= floor;
        if mechanical == s.incumbent_supported {
            n_agree += 1;
        } else {
            n_disagree += 1;
            disagreements.push(s);
        }
    }
    let n_high_margin = n_agree + n_disagree;
    let agreement = (n_high_margin > 0).then(|| n_agree as f64 / n_high_margin as f64);

    // ── (b) audit wall time, over the turns that carried high-margin claims ──
    let mut timed: BTreeMap<&str, u128> = BTreeMap::new();
    for s in &hold {
        if s.high_margin && s.turn_violation_prob.is_some() {
            timed.insert(s.turn_id.as_str(), s.turn_sweep_ms);
        }
    }
    let mut ms: Vec<u128> = timed.values().copied().collect();
    ms.sort_unstable();
    let audit_p50 = (!ms.is_empty()).then(|| ms[ms.len() / 2]);
    let audit_p90 = (!ms.is_empty()).then(|| ms[(ms.len() * 9 / 10).min(ms.len() - 1)]);

    let vp_dist = Distribution::of(
        hold.iter()
            .filter_map(|s| s.turn_violation_prob)
            .collect::<Vec<_>>(),
    );

    // ── the verdict ──
    //
    // The single-class guard is the load-bearing one. §7.3 H4's bars are about
    // a mechanism that must get BOTH directions right: ">=0.90 agreement" and
    // ">25% disagreement kills" both presume the held-out set can catch a
    // mechanism that over-accepts AND one that over-rejects. Scored against a
    // set where the incumbent said "supported" to everything, agreement
    // degenerates into a true-positive rate — a number a scorer that always
    // answers "supported" maxes out. Reporting that as 0.9x agreement would be
    // the §18.1 smell: a check with no failing input you can name.
    let (outcome, cnj) = match agreement {
        None => (
            H4Outcome::CouldNotJudge,
            Some("no high-margin claims survived the restriction — there is nothing to agree or disagree with".to_string()),
        ),
        Some(_) if hm_sup == 0 || hm_unsup == 0 => (
            H4Outcome::CouldNotJudge,
            Some(format!(
                "the held-out high-margin set is SINGLE-CLASS ({hm_sup} supported / \
                 {hm_unsup} not supported). §7.3 H4's bars presume both error directions are \
                 at risk; against a one-valued label set, agreement collapses to a \
                 true-positive rate that a scorer answering \"supported\" unconditionally \
                 would max out. The number below is reported as a diagnostic and is NOT the \
                 gate's verdict."
            )),
        ),
        Some(a) if 1.0 - a > KILL_DISAGREEMENT => (H4Outcome::Killed, None),
        Some(a) if a >= BAR_AGREEMENT && audit_p50.is_some_and(|p| p <= BAR_AUDIT_P50_MS) => {
            (H4Outcome::Beat, None)
        }
        Some(_) => (H4Outcome::Survives, None),
    };

    let verdict = H4Verdict {
        outcome,
        could_not_judge_reason: cnj,
        unreplayable_turns_with_holdings: unreplayable,
        criterion: format!(
            "NATIVE_GROUNDING.md §7.3 H4 — beat if agreement >= {BAR_AGREEMENT} on high-margin \
             claims (|vp - tau| > {band}, tau = {tau}) AND audit p50 <= {BAR_AUDIT_P50_MS} ms; \
             kill if disagreement > {KILL_DISAGREEMENT}"
        ),
        agreement,
        n_high_margin,
        n_agree,
        n_disagree,
        high_margin_supported: hm_sup,
        high_margin_unsupported: hm_unsup,
        excluded_no_violation_prob: excluded_no_vp,
        excluded_low_margin,
        excluded_unscoreable,
        margin_distribution_high_margin: Distribution::of(hm_margins),
        violation_prob_distribution: vp_dist,
        audit_p50_ms: audit_p50,
        audit_p90_ms: audit_p90,
        n_turns_timed: ms.len(),
        incumbent_judge_calls_per_gated_longform_turn:
            "~35 (NATIVE_GROUNDING.md §2, citing DEFAULTS_LEDGER.md:848) — cited, NOT measured \
             by this run: the chaos ResultRow carries no per-stage timing, so this gate cannot \
             put a measured incumbent number beside its own.",
        citation_and_grounding_fidelity_delta: NotRun {
            what: "citation_fidelity / grounding_fidelity deltas via rescore (§7.3 H4 metric c)"
                .into(),
            why: "computing a DELTA needs an H4-derived variant of those facets, which means \
                  teaching the chaos scorer to read them off the mechanical attribution. That \
                  is the chaos-scorer cutover, which this order places explicitly out of scope."
                .into(),
            what_would_be_needed:
                "the cutover order: score.rs facets fed from segments/margins, then one rescore \
                 pass over these same frozen transcripts, incumbent vs native, no regeneration."
                    .into(),
        },
        floor,
        floor_selection_rule:
            "the calibration curve's best-balanced-accuracy threshold (operating_curve::build), \
             chosen by rule rather than by hand so the floor has no free parameter",
        calibration_source: calibrate.iter().map(|p| p.display().to_string()).collect(),
        calibration_n_claims: cal_pairs.len(),
        calibration_auroc: curve.auroc,
        holdout_source: holdout.iter().map(|p| p.display().to_string()).collect(),
        bar_agreement: BAR_AGREEMENT,
        bar_audit_p50_ms: BAR_AUDIT_P50_MS,
        kill_disagreement: KILL_DISAGREEMENT,
        tau,
        high_margin_band: band,
    };

    let vpath = out_dir.join("h4_verdict.json");
    std::fs::write(
        &vpath,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&verdict).map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| format!("write {}: {e}", vpath.display()))?;
    eprintln!("[out] wrote verdict → {}", vpath.display());

    // The kill path owes the operator a sample, not an opinion.
    if verdict.outcome == H4Outcome::Killed {
        let path = out_dir.join("h4_adjudication_sample.md");
        write_adjudication_sample(&path, &disagreements, floor, tau)?;
        eprintln!("[out] wrote the adjudication sample → {}", path.display());
    }

    Ok(verdict)
}

/// Score every readable claim in one transcript.
async fn score_transcript(
    path: &Path,
    split: &str,
    sc: &dyn SentenceScorer,
    tau: f64,
    band: f64,
) -> Result<(Vec<ClaimScore>, Vec<UnreplayableTurn>), String> {
    let (turns, skipped) = transcript::load(path)?;
    if skipped > 0 {
        eprintln!("[h4] WARN: {} unreadable line(s) in {}", skipped, path.display());
    }
    let source = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut out = Vec::new();
    let mut unreplayable = Vec::new();
    let mut n_turns = 0usize;
    for t in &turns {
        if t.holdings().is_empty() {
            continue;
        }
        // A turn the incumbent audited but we cannot: census it with the field
        // that is missing, rather than dropping it out of the denominator.
        let evidence = t
            .retrieved_chunks
            .iter()
            .filter(|c| !c.trim().is_empty())
            .count();
        let mut missing: Vec<String> = Vec::new();
        if evidence == 0 {
            missing.push("retrieved_chunks".into());
        }
        if t.violation_prob.is_none() {
            missing.push("violation_prob".into());
        }
        if !t.is_replayable() || !missing.is_empty() {
            unreplayable.push(UnreplayableTurn {
                source: source.clone(),
                turn_id: t.id.clone(),
                n_holdings: t.holdings().len(),
                n_unsupported: t
                    .holdings()
                    .iter()
                    .filter(|h| h.supported() == Some(false))
                    .count(),
                answer_chars: t.answer.trim().len(),
                evidence_chunks: evidence,
                violation_prob: t.violation_prob,
                gate_action: t.gate_action.clone(),
                missing,
            });
        }
        if !t.is_replayable() {
            continue;
        }
        n_turns += 1;
        // The whole-turn sweep: its wall time is the (b) measurement, and its
        // rows are the committed per-sentence artifact.
        let swept = sentence_sweep::sweep(&t.question, &t.answer, &t.retrieved_chunks, sc)
            .await
            .map_err(|e| format!("sweep {}: {e}", t.id))?;
        let (pool, _) = sentence_sweep::capped_pool(&t.retrieved_chunks);
        let high_margin = t
            .violation_prob
            .is_some_and(|vp| (vp - tau).abs() > band);

        for (claim_index, h) in t.holdings().iter().enumerate() {
            let Some(incumbent_supported) = h.supported() else {
                // fail_open / unverified / an unknown word: the incumbent did
                // not reach a verdict, so there is nothing to agree with.
                continue;
            };
            let scored = sentence_sweep::margin_over_pool(&h.claim, &pool, sc)
                .await
                .map_err(|e| format!("claim {} of {}: {e}", claim_index, t.id))?;
            out.push(ClaimScore {
                source: source.clone(),
                split: split.to_string(),
                turn_id: t.id.clone(),
                claim_index,
                claim: h.claim.clone(),
                incumbent_verification: h.verification.clone(),
                incumbent_supported,
                margin: scored.map(|(m, _)| m),
                best_chunk: scored.map(|(_, i)| i),
                span: span_resolver::resolve_span(&h.claim, &pool).label().to_string(),
                turn_violation_prob: t.violation_prob,
                turn_gate_action: t.gate_action.clone(),
                high_margin,
                turn_sweep_ms: swept.elapsed_ms,
            });
        }
    }
    eprintln!(
        "[h4] {split}: {} claims from {n_turns} turns of {} ({} turn(s) had audited claims we \
         cannot replay)",
        out.len(),
        path.display(),
        unreplayable.len()
    );
    Ok((out, unreplayable))
}

fn write_scores(path: &Path, rows: &[ClaimScore]) -> Result<(), String> {
    let mut f = std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    for r in rows {
        writeln!(
            f,
            "{}",
            serde_json::to_string(r).map_err(|e| e.to_string())?
        )
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn read_scores(path: &Path) -> Result<Vec<ClaimScore>, String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<ClaimScore>(line)
                .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?,
        );
    }
    if out.is_empty() {
        return Err(format!(
            "{} holds 0 scores — there is nothing to judge",
            path.display()
        ));
    }
    Ok(out)
}

/// The kill path's deliverable: a hand-adjudication sample, one command per
/// claim. §7.3 H4's kill criterion is a CONJUNCTION — mechanical disagreement
/// AND hand-adjudication siding with the incumbent — and the second half is the
/// operator's call, not this command's.
fn write_adjudication_sample(
    path: &Path,
    disagreements: &[&ClaimScore],
    floor: f64,
    tau: f64,
) -> Result<(), String> {
    let mut s = String::new();
    s.push_str("# H4 kill path — adjudication sample\n\n");
    s.push_str(&format!(
        "The mechanical attribution disagreed with the incumbent on more than \
         {:.0}% of high-margin claims, so §7.3 H4's kill criterion is HALF met. The other \
         half is hand-adjudication, and it belongs to the operator: if adjudication sides \
         with the incumbent, mechanical-only attribution is insufficient and H4 keeps a \
         forced-choice escalation rung for contested sentences. If it sides with the \
         mechanism, the incumbent's verdicts were the noisy ones.\n\n\
         Floor: {floor:.4} (margin >= floor reads as supported). τ = {tau}.\n\n\
         Below are up to {} disagreeing claims. The `score-answer` seam \
         (chaos_monkey.rs:1307) is one command per claim.\n\n",
        KILL_DISAGREEMENT * 100.0,
        ADJUDICATION_SAMPLE_N
    ));
    s.push_str("| # | turn | incumbent | mechanical | margin | vp | span | claim |\n");
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    for (i, d) in disagreements.iter().take(ADJUDICATION_SAMPLE_N).enumerate() {
        s.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            d.turn_id,
            if d.incumbent_supported { "supported" } else { "NOT supported" },
            if d.margin.is_some_and(|m| (m as f64) >= floor) { "supported" } else { "NOT supported" },
            d.margin.map_or("null".into(), |m| format!("{m:.3}")),
            d.turn_violation_prob.map_or("null".into(), |v| format!("{v:.3}")),
            d.span,
            d.claim.replace('|', "\\|").chars().take(120).collect::<String>(),
        ));
    }
    s.push_str("\n## Per-claim adjudication commands\n\n```bash\n");
    for d in disagreements.iter().take(ADJUDICATION_SAMPLE_N) {
        s.push_str(&format!(
            "svrn bench chaos-monkey score-answer --id {} --claim {:?}\n",
            d.turn_id, d.claim
        ));
    }
    s.push_str("```\n");
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

fn print_report(v: &H4Verdict) {
    eprintln!("\n═══ H4 GATE — NATIVE_GROUNDING §7.3 ═══");
    eprintln!("  outcome                {:?}", v.outcome);
    match v.agreement {
        Some(a) => eprintln!(
            "  (a) agreement          {a:.4}  ({}/{} high-margin claims)   bar >= {}",
            v.n_agree, v.n_high_margin, v.bar_agreement
        ),
        None => eprintln!("  (a) agreement          NOT MEASURED — no high-margin claims"),
    }
    eprintln!(
        "      label balance      {} supported / {} not supported",
        v.high_margin_supported, v.high_margin_unsupported
    );
    eprintln!(
        "      excluded           {} no vp (Critic never asked), {} inside the flip band, {} unscoreable",
        v.excluded_no_violation_prob, v.excluded_low_margin, v.excluded_unscoreable
    );
    if let Some(d) = &v.margin_distribution_high_margin {
        eprintln!(
            "      margin dist        n={} min={:.3} p25={:.3} med={:.3} p75={:.3} max={:.3}",
            d.n, d.min, d.p25, d.median, d.p75, d.max
        );
    }
    if let Some(d) = &v.violation_prob_distribution {
        eprintln!(
            "      vp dist (holdout)  n={} min={:.3} p25={:.3} med={:.3} p75={:.3} max={:.3}",
            d.n, d.min, d.p25, d.median, d.p75, d.max
        );
    }
    match v.audit_p50_ms {
        Some(p) => eprintln!(
            "  (b) audit p50          {p} ms over {} turns   bar <= {} ms",
            v.n_turns_timed, v.bar_audit_p50_ms
        ),
        None => eprintln!("  (b) audit p50          NOT MEASURED — no timed turns"),
    }
    eprintln!(
        "  (c) fidelity deltas    NOT RUN — {}",
        v.citation_and_grounding_fidelity_delta.why
    );
    if let Some(r) = &v.could_not_judge_reason {
        eprintln!("  COULD NOT JUDGE        {r}");
    }
    if !v.unreplayable_turns_with_holdings.is_empty() {
        let neg: usize = v
            .unreplayable_turns_with_holdings
            .iter()
            .map(|u| u.n_unsupported)
            .sum();
        eprintln!(
            "  unreplayable turns     {} turn(s) the incumbent audited but we cannot, carrying \
             {neg} of its NEGATIVE-class claims:",
            v.unreplayable_turns_with_holdings.len()
        );
        for u in &v.unreplayable_turns_with_holdings {
            eprintln!(
                "      {:28} holdings={} (unsupported {}) answer={}ch chunks={} vp={} missing={:?}",
                u.turn_id,
                u.n_holdings,
                u.n_unsupported,
                u.answer_chars,
                u.evidence_chunks,
                u.violation_prob.map_or("null".into(), |x| format!("{x:.3}")),
                u.missing
            );
        }
    }
    eprintln!(
        "  floor                  {:.4}  (calibration AUROC {:.4} over {} claims)",
        v.floor, v.calibration_auroc, v.calibration_n_claims
    );
    eprintln!("  calibration            {}", v.calibration_source.join(", "));
    eprintln!("  holdout                {}", v.holdout_source.join(", "));
    eprintln!("═══════════════════════════════════════\n");
}

fn print_help() {
    eprintln!(
        "svrn bench flywheel h4-gate — NATIVE_GROUNDING §7.3's H4 gate, offline.\n\
         \n\
         Replays FROZEN chaos transcripts through the span resolver and the margin fold,\n\
         calibrates the margin floor on one split, and reports agreement with the\n\
         incumbent's per-claim verdicts on the other. No judge is re-invoked.\n\
         \n\
         Flags:\n\
         \x20 --calibrate <jsonl>   transcript to FIT the floor on (repeatable, required)\n\
         \x20 --holdout <jsonl>     transcript to SCORE the verdict on (repeatable, required)\n\
         \x20 --out-dir <dir>       default sovereign/bench/calibration/h4\n\
         \x20 --rerank-model <gguf> default $SOVEREIGN_RERANK_MODEL_PATH (absence is reported)\n\
         \x20 --from-scores <jsonl> rebuild the verdict from a frozen score file, no model\n\
         \x20 --tau <f>             default: the SHARED production threshold\n\
         \x20 --high-margin <f>     half-width of the flip band, default 0.2\n\
         \n\
         The two splits may not overlap; a floor fitted on the data it is scored against\n\
         is not a measurement, and this command refuses to produce one.\n\
         \n\
         Exit: 0 = beat or survives, 3 = KILLED (a successful run — park with the sample),\n\
         \x20     1 = could not measure, 2 = usage."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(split: &str, supported: bool, margin: Option<f32>, vp: Option<f64>, hm: bool) -> ClaimScore {
        ClaimScore {
            source: "s".into(),
            split: split.into(),
            turn_id: "t".into(),
            claim_index: 0,
            claim: "c".into(),
            incumbent_verification: if supported { "verified".into() } else { "failed_once".into() },
            incumbent_supported: supported,
            margin,
            best_chunk: Some(0),
            span: "fuzzy".into(),
            turn_violation_prob: vp,
            turn_gate_action: None,
            high_margin: hm,
            turn_sweep_ms: 100,
        }
    }

    #[test]
    fn a_distribution_of_nothing_is_absent_not_zero() {
        assert!(Distribution::of(vec![]).is_none());
        let d = Distribution::of(vec![1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        assert_eq!(d.n, 5);
        assert_eq!(d.min, 1.0);
        assert_eq!(d.median, 3.0);
        assert_eq!(d.max, 5.0);
    }

    #[tokio::test]
    async fn overlapping_splits_are_refused() {
        let p = PathBuf::from("same.jsonl");
        let err = run(
            std::slice::from_ref(&p),
            std::slice::from_ref(&p),
            Path::new("/tmp/h4test"),
            None,
            None,
            0.9,
            0.2,
        )
        .await
        .expect_err("a leaked split must be refused");
        assert!(err.contains("BOTH splits"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_split_is_refused() {
        let err = run(&[], &[PathBuf::from("a")], Path::new("/tmp/h4test"), None, None, 0.9, 0.2)
            .await
            .expect_err("one-sided invocation must be refused");
        assert!(err.contains("both --calibrate and --holdout"), "{err}");
    }

    #[test]
    fn scores_round_trip_and_an_empty_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let rows = vec![cs("calibration", true, Some(1.5), Some(0.1), true)];
        write_scores(&p, &rows).unwrap();
        let back = read_scores(&p).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].incumbent_supported, true);

        let e = dir.path().join("empty.jsonl");
        std::fs::write(&e, "").unwrap();
        assert!(read_scores(&e).is_err(), "an empty score file verifies nothing");
    }

    #[test]
    fn the_verdict_bars_are_the_spec_bars() {
        // Guard against the numbers drifting away from §7.3 H4 silently.
        assert_eq!(BAR_AGREEMENT, 0.90);
        assert_eq!(BAR_AUDIT_P50_MS, 2000);
        assert_eq!(KILL_DISAGREEMENT, 0.25);
        assert_eq!(DEFAULT_HIGH_MARGIN_BAND, 0.2);
        assert_eq!(ADJUDICATION_SAMPLE_N, 20);
    }
}
