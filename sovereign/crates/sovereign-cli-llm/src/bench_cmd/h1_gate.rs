// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h1-gate` — the offline H1 measurement.
//!
//! `NATIVE_GROUNDING.md §7.3` H1: score every calibration pair with BOTH
//! candidate answerability signals and emit their operating curves side by
//! side, so the kill criterion can be applied to numbers rather than to an
//! impression.
//!
//!   * **rerank margin** — `answerability(q) = max_i margin(q, chunk_i)`
//!     over the pool (k <= 8), batched through `RerankSlot::score_batch`.
//!     This is H1's proposal.
//!   * **top_cosine** — `max_i cos(embed(q), embed(chunk_i))`, the shipped
//!     -dark early-decline signal H1 has to beat. Computed here exactly as
//!     `runtime/evidence.rs:400` derives it: the best cosine similarity
//!     over the pool.
//!
//! **No generation happens.** Both signals are scored from the retrieved
//! pool alone, which is why §8 lists this phase as "the first real win or
//! the first real kill" at offline cost.
//!
//! **Two things this command refuses to do.**
//!
//! 1. **Substitute a missing model.** `SOVEREIGN_RERANK_MODEL_PATH` is
//!    default-inert. If neither it nor `--rerank-model` names a file that
//!    exists, the command reports the absence and exits non-zero. It does
//!    not fall back to cosine-only and it does not emit a partial verdict —
//!    a kill-gate artifact with one of its two curves missing is worse than
//!    no artifact (ARCH §18.3).
//! 2. **Load a slot the box cannot hold.** The rerank and embed slots go
//!    through `capacity::check_fit` first (§8 residency plan; the 64 GB
//!    SIGTERM incident, note `b57b0cd5`).
//!
//! The scored pairs are written out alongside the curves. That is the
//! determinism seam: `--from-scores` rebuilds every curve and the verdict
//! from a frozen score file with no model loaded at all, so the metric
//! half of the instrument can be re-run and diffed independently of the
//! model half.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_eval::flywheel::calibration::{self as cal, CalibrationPair};
use sovereign_eval::flywheel::operating_curve::{self as oc, ScoredPair};
use sovereign_inference::capacity::{self, SlotPlan};
use sovereign_inference::embedded::EmbedOnlyProvider;
use sovereign_inference::hardware::HardwareProfile;
use sovereign_inference::reranker_standalone::StandaloneReranker;

/// Signal names. One definition each — they key the artifact filenames,
/// the curve's `signal` field and the verdict, and a typo in any one of
/// them would produce a report that looks fine and compares nothing.
const SIGNAL_MARGIN: &str = "rerank_margin";
const SIGNAL_COSINE: &str = "top_cosine";

/// Both scores for one pair, before they are split into two curves.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PairScores {
    id: String,
    corpus_id: String,
    answerable: bool,
    /// Corpus family: `sep` or `literary`. §7.3 asks for the split.
    family: String,
    rerank_margin: f32,
    top_cosine: f32,
}

/// `sep-*` atlases are one substrate; everything else in this set is the
/// literary minority (brothers-karamazov-book-1).
fn family_of(corpus_id: &str) -> &'static str {
    if corpus_id.starts_with("sep-") {
        "sep"
    } else {
        "literary"
    }
}

pub(crate) async fn cmd_h1_gate(rest: &[String]) -> i32 {
    let mut set = PathBuf::from("sovereign/bench/calibration/native_grounding_calibration.jsonl.gz");
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h1");
    let mut rerank_model: Option<PathBuf> = None;
    let mut embed_model: Option<PathBuf> = None;
    let mut limit: Option<usize> = None;
    let mut from_scores: Option<PathBuf> = None;

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
            "--set" => set = PathBuf::from(val!("--set")),
            "--out-dir" => out_dir = PathBuf::from(val!("--out-dir")),
            "--rerank-model" => rerank_model = Some(PathBuf::from(val!("--rerank-model"))),
            "--embed-model" => embed_model = Some(PathBuf::from(val!("--embed-model"))),
            "--from-scores" => from_scores = Some(PathBuf::from(val!("--from-scores"))),
            "--limit" => match val!("--limit").parse() {
                Ok(v) => limit = Some(v),
                Err(_) => {
                    eprintln!("error: --limit must be a usize");
                    return 2;
                }
            },
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    // ── the scores: either replayed from a frozen file, or measured ──
    let scores: Vec<PairScores> = if let Some(p) = &from_scores {
        match read_scores(p) {
            Ok(s) => {
                eprintln!("[h1] replaying {} frozen score(s) from {p:?} — no model loaded", s.len());
                s
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        let mut pairs = match cal::read_pairs(&set) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        // Deterministic subsetting: the set is already in a stable order,
        // so a --limit smoke run scores a prefix, not a sample.
        if let Some(n) = limit {
            pairs.truncate(n);
        }
        eprintln!("[h1] {} pair(s) from {set:?}", pairs.len());
        match measure(&pairs, rerank_model, embed_model).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {out_dir:?}: {e}");
        return 1;
    }
    if from_scores.is_none() {
        let sp = out_dir.join("h1_scores.jsonl");
        if let Err(e) = write_scores(&sp, &scores) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!("[out] scores → {sp:?}");
    }

    // ── curves: overall, then split by corpus family (§7.3) ──
    let mut families: Vec<String> = scores.iter().map(|s| s.family.clone()).collect();
    families.sort();
    families.dedup();

    let mut verdict = None;
    for slice in std::iter::once(None).chain(families.iter().map(Some)) {
        let subset: Vec<&PairScores> = match slice {
            None => scores.iter().collect(),
            Some(f) => scores.iter().filter(|s| &s.family == f).collect(),
        };
        let tag = slice.map_or_else(|| "overall".to_string(), Clone::clone);

        let margin: Vec<ScoredPair> = subset.iter().map(|s| to_scored(s, s.rerank_margin)).collect();
        let cosine: Vec<ScoredPair> = subset.iter().map(|s| to_scored(s, s.top_cosine)).collect();

        let (mc, cc) = match (
            oc::build(SIGNAL_MARGIN, &margin),
            oc::build(SIGNAL_COSINE, &cosine),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                // A family too thin to curve is REPORTED, not silently
                // dropped and not padded into existence. The literary
                // family is 19 pairs and this is exactly the path it may
                // take.
                eprintln!("[h1] {tag}: no curve — {e}");
                continue;
            }
        };
        for c in [&mc, &cc] {
            let p = out_dir.join(format!("h1_{}.{tag}.curve.json", c.signal));
            match serde_json::to_string_pretty(c) {
                Ok(s) => {
                    if let Err(e) = std::fs::write(&p, s + "\n") {
                        eprintln!("error: write {p:?}: {e}");
                        return 1;
                    }
                }
                Err(e) => {
                    eprintln!("error: serialize curve: {e}");
                    return 1;
                }
            }
            eprintln!("[out] curve → {p:?}");
        }
        report_curves(&tag, &mc, &cc);

        match oc::h1_verdict(&mc, &cc) {
            Ok(v) => {
                if slice.is_none() {
                    verdict = Some(v);
                }
            }
            Err(e) => {
                eprintln!("error: {tag}: {e}");
                return 1;
            }
        }
    }

    let Some(v) = verdict else {
        eprintln!("error: no overall verdict was produced — the set could not be curved at all");
        return 1;
    };
    let vp = out_dir.join("h1_verdict.json");
    match serde_json::to_string_pretty(&v) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&vp, s + "\n") {
                eprintln!("error: write {vp:?}: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serialize verdict: {e}");
            return 1;
        }
    }
    eprintln!("\n════════ H1 KILL GATE ════════");
    eprintln!("  {}", v.criterion);
    eprintln!("  rerank_margin AUROC : {:.4}", v.rerank_margin_auroc);
    eprintln!("  top_cosine    AUROC : {:.4}", v.top_cosine_auroc);
    eprintln!("  delta               : {:+.4}", v.delta);
    eprintln!("  VERDICT             : {:?}", v.outcome);
    eprintln!("[out] verdict → {vp:?}");

    // The gate is allowed to fail, and a failed gate must be visible in the
    // exit code — but a KILL is a successful RUN. Exit 0 on Beat/Survives,
    // 3 on Killed, so a caller can tell "H1 died" from "the harness broke"
    // (which exits 1).
    match v.outcome {
        oc::H1Outcome::Beat | oc::H1Outcome::Survives => 0,
        oc::H1Outcome::Killed => 3,
    }
}

fn to_scored(s: &PairScores, score: f32) -> ScoredPair {
    ScoredPair {
        id: s.id.clone(),
        corpus_id: s.corpus_id.clone(),
        answerable: s.answerable,
        score,
    }
}

fn report_curves(tag: &str, margin: &oc::OperatingCurve, cosine: &oc::OperatingCurve) {
    eprintln!(
        "\n── {tag} ({} pairs: {} answerable / {} absent) ──",
        margin.n_pairs, margin.n_answerable, margin.n_absent
    );
    eprintln!("  {:<16} {:>8}  {}", "signal", "AUROC", "honesty-recall @ false-alarm 5% / 10% / 20%");
    for c in [margin, cosine] {
        let at = |b: u32| {
            c.honesty_recall_at_false_alarm
                .get(&b)
                .map_or(f64::NAN, |r| r.honesty_recall)
        };
        eprintln!(
            "  {:<16} {:>8.4}  {:.3} / {:.3} / {:.3}",
            c.signal,
            c.auroc,
            at(5),
            at(10),
            at(20)
        );
    }
}

/// Load both slots (behind the capacity gate) and score every pair.
async fn measure(
    pairs: &[CalibrationPair],
    rerank_model: Option<PathBuf>,
    embed_model: Option<PathBuf>,
) -> Result<Vec<PairScores>, String> {
    // ── the reranker: named, or refused ──
    let rerank_path = match rerank_model.or_else(|| {
        std::env::var("SOVEREIGN_RERANK_MODEL_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
    }) {
        Some(p) => p,
        None => {
            return Err(
                "no reranker. `SOVEREIGN_RERANK_MODEL_PATH` is unset (it is default-inert) and \
                 --rerank-model was not given. H1's whole claim is about the rerank margin, so \
                 there is nothing to measure and nothing to substitute — pass --rerank-model \
                 <path-to-gguf>."
                    .into(),
            );
        }
    };
    if !rerank_path.is_file() {
        return Err(format!(
            "reranker not found at {rerank_path:?} — refusing rather than scoring cosine alone \
             and emitting a half-verdict"
        ));
    }
    let embed_path = match embed_model {
        Some(p) => p,
        None => resolve_configured_embed_model()?,
    };
    if !embed_path.is_file() {
        return Err(format!("embed model not found at {embed_path:?}"));
    }

    // ── §8 residency plan: fit check BEFORE either slot loads ──
    let hw = HardwareProfile::detect();
    let plans = vec![
        SlotPlan {
            role: "rerank".into(),
            path: rerank_path.clone(),
            n_seq_max: 8,
            n_ctx: 8192,
        },
        SlotPlan {
            role: "embed".into(),
            path: embed_path.clone(),
            n_seq_max: 8,
            n_ctx: 8192,
        },
    ];
    let report = capacity::check_fit(&plans, &hw);
    if report.fits {
        eprintln!(
            "[h1] capacity: {} MiB required, {} MiB available (after {} MiB reserved) — fits",
            report.total_required_mb, report.available_mb, report.safety_reserved_mb
        );
    } else if capacity::check_skipped_by_env() {
        eprintln!("[h1] capacity check FAILED but is disabled by env — proceeding as instructed");
        eprintln!("{}", report.refuse_message());
    } else {
        return Err(format!(
            "capacity check refused the two slots this measurement needs:\n{}",
            report.refuse_message()
        ));
    }

    eprintln!("[h1] loading reranker {rerank_path:?} …");
    let reranker = StandaloneReranker::load(&rerank_path, ModelFamily::Reranker, None)
        .map_err(|e| format!("load reranker: {e}"))?;
    eprintln!("[h1] loading embedder {embed_path:?} …");
    let embedder: Arc<dyn InferenceProvider> =
        Arc::new(EmbedOnlyProvider::load(&embed_path, ModelFamily::Qwen3Embedding)
            .map_err(|e| format!("load embed model: {e}"))?);

    // Chunk embeddings are heavily shared: a claim's answerable and absent
    // pools overlap by k-1 members, and neighbouring claims in an article
    // reuse the same passages. Cache by exact text.
    let mut embed_cache: HashMap<String, Vec<f32>> = HashMap::new();
    let mut out = Vec::with_capacity(pairs.len());
    let started = std::time::Instant::now();

    for (n, p) in pairs.iter().enumerate() {
        // H1's answerability: the BEST margin over the pool.
        let margins = reranker
            .rerank_batch(&p.question, &p.chunks)
            .await
            .map_err(|e| format!("rerank {}: {e}", p.id))?;
        let rerank_margin = margins
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);

        // top_cosine, derived the way runtime/evidence.rs:400 does: the
        // best cosine similarity over the pool.
        let q = embed_one(&embedder, &mut embed_cache, &p.question).await?;
        let mut top_cosine = f32::NEG_INFINITY;
        for c in &p.chunks {
            let v = embed_one(&embedder, &mut embed_cache, c).await?;
            top_cosine = top_cosine.max(cosine_similarity(&q, &v));
        }

        out.push(PairScores {
            id: p.id.clone(),
            corpus_id: p.corpus_id.clone(),
            answerable: p.answerable,
            family: family_of(&p.corpus_id).to_string(),
            rerank_margin,
            top_cosine,
        });

        if (n + 1) % 100 == 0 || n + 1 == pairs.len() {
            let el = started.elapsed().as_secs_f64();
            let rate = (n + 1) as f64 / el.max(1e-9);
            eprintln!(
                "[h1] {}/{} pairs  {:.1}/s  elapsed {:.0}s  eta {:.0}s  (embed cache {} entries)",
                n + 1,
                pairs.len(),
                rate,
                el,
                (pairs.len() - n - 1) as f64 / rate.max(1e-9),
                embed_cache.len()
            );
        }
    }
    Ok(out)
}

async fn embed_one(
    embedder: &Arc<dyn InferenceProvider>,
    cache: &mut HashMap<String, Vec<f32>>,
    text: &str,
) -> Result<Vec<f32>, String> {
    if let Some(v) = cache.get(text) {
        return Ok(v.clone());
    }
    let v = embedder
        .embed(text)
        .await
        .map_err(|e| format!("embed: {e}"))?;
    cache.insert(text.to_string(), v.clone());
    Ok(v)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

/// The embed model the daemon is configured with. Read from the same
/// `SetupConfig` the daemon reads, so this measurement uses production's
/// embedder rather than a second opinion about which one that is.
fn resolve_configured_embed_model() -> Result<PathBuf, String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load()
        .map_err(|e| format!("load setup config (for the embed model path): {e}"))?;
    let p = cfg.models.embed.clone();
    if p.as_os_str().is_empty() {
        return Err(
            "no embed model configured (`models.embed`) — pass --embed-model <path>".into(),
        );
    }
    Ok(p)
}

fn read_scores(path: &Path) -> Result<Vec<PairScores>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<PairScores>(line)
                .map_err(|e| format!("{path:?} line {}: {e}", n + 1))?,
        );
    }
    if out.is_empty() {
        return Err(format!("{path:?} holds 0 scores — there is nothing to curve"));
    }
    Ok(out)
}

fn write_scores(path: &Path, scores: &[PairScores]) -> Result<(), String> {
    let mut body = String::new();
    for s in scores {
        body.push_str(&serde_json::to_string(s).map_err(|e| format!("serialize {}: {e}", s.id))?);
        body.push('\n');
    }
    std::fs::write(path, body).map_err(|e| format!("write {path:?}: {e}"))
}

fn print_help() {
    eprintln!(
        "svrn bench flywheel h1-gate — NATIVE_GROUNDING §7.3's H1 measurement, offline.\n\
         \n\
         Scores every calibration pair with BOTH answerability signals — the rerank-slot\n\
         margin (max over the pool) and top_cosine — emits an operating curve for each\n\
         (overall and split by corpus family), and applies the §7.3 kill criterion.\n\
         \n\
         Flags:\n\
         \x20 --set <jsonl|jsonl.gz>  calibration set\n\
         \x20 --out-dir <dir>         where curves, scores and the verdict land\n\
         \x20 --rerank-model <gguf>   default: $SOVEREIGN_RERANK_MODEL_PATH (default-inert;\n\
         \x20                         its absence is reported, never worked around)\n\
         \x20 --embed-model <gguf>    default: the configured models.embed\n\
         \x20 --limit N               score only the first N pairs (smoke run)\n\
         \x20 --from-scores <jsonl>   rebuild curves + verdict from a frozen score file,\n\
         \x20                         loading NO model — the determinism seam\n\
         \n\
         Exit: 0 = H1 beat or survived, 3 = H1 killed (a successful run either way),\n\
         \x20     1 = the harness could not measure, 2 = usage."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_is_orientation_not_magnitude() {
        assert!((cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Degenerate inputs return 0 rather than NaN — a NaN would poison
        // the whole curve through `max`.
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn family_split_names_the_two_substrates() {
        assert_eq!(family_of("sep-abduction"), "sep");
        assert_eq!(family_of("brothers-karamazov-book-1"), "literary");
    }

    #[test]
    fn a_score_file_round_trips_and_an_empty_one_is_refused() {
        let s = vec![PairScores {
            id: "cal:x:1:present".into(),
            corpus_id: "sep-abduction".into(),
            answerable: true,
            family: "sep".into(),
            rerank_margin: 1.25,
            top_cosine: 0.61,
        }];
        let p = std::env::temp_dir().join("h1_scores_roundtrip.jsonl");
        write_scores(&p, &s).unwrap();
        let back = read_scores(&p).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, s[0].id);
        assert!((back[0].rerank_margin - 1.25).abs() < 1e-6);

        let empty = std::env::temp_dir().join("h1_scores_empty.jsonl");
        std::fs::write(&empty, "\n").unwrap();
        assert!(read_scores(&empty).unwrap_err().contains("0 scores"));
    }
}
