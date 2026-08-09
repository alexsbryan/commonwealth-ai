// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h2b-arms` — stage 1: decode the three arms.
//!
//! The only stage that loads a generator, and the only stage that costs hours.
//! It writes one JSONL row per pair **as it goes**, so a run cut short by a
//! closing daemon window leaves a usable prefix rather than nothing, and
//! `--resume` picks it up by id.
//!
//! Everything downstream — equivalence, the statistics, the kill bar — reads
//! that file and never the model. That is deliberate: the 36B and the reranker
//! cannot both be resident on this host, and a design that needed them together
//! would have made the decision-grade run impossible rather than slow.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::PathBuf;

use sovereign_core::model_family::ModelFamily;
use sovereign_eval::flywheel::calibration::{self as cal, CalibrationPair};
use sovereign_eval::flywheel::det_checks::value_present;
use sovereign_inference::k_sample::{
    build_parametric_prompt, build_value_prompt, clean_value, DrawSampling, KSampleDecoder,
    KSampleDraw, PARAMETRIC_SYSTEM_MESSAGE, VALUE_SYSTEM_MESSAGE,
};

use super::{family_of, ArmOutcome, ArmRecord, PairArms};

/// Every arm is `k=1`: H2b's perturbation is the evidence, so drawing more than
/// one sample per arm would reintroduce the axis the amendment removed and make
/// a disagreement ambiguous between the two causes.
const ARM_K: u8 = 1;

/// Seed base. Inert under greedy decoding (`build_sampler` never builds a
/// `dist` stage below T=0.01), and passed anyway so the artifact records what
/// was asked for rather than leaving a reader to infer that it did not matter.
/// The value is H2's, so the two orders' artifacts share a provenance.
const SEED_BASE: u32 = 1592590337;

pub async fn cmd_h2b_arms(args: &[String]) -> i32 {
    let mut set = PathBuf::from("sovereign/bench/calibration/native_grounding_calibration.jsonl.gz");
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h2b");
    let mut out_name = "h2b_arms.jsonl".to_string();
    let mut model: Option<PathBuf> = None;
    let mut limit: Option<usize> = None;
    let mut n_ctx: u32 = 8192;
    let mut repeat_every: usize = 25;
    let mut resume = false;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match args.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--set" => set = PathBuf::from(val!("--set")),
            "--out-dir" => out_dir = PathBuf::from(val!("--out-dir")),
            "--out-name" => out_name = val!("--out-name"),
            "--model" => model = Some(PathBuf::from(val!("--model"))),
            "--n-ctx" => match val!("--n-ctx").parse() {
                Ok(v) => n_ctx = v,
                Err(_) => {
                    eprintln!("error: --n-ctx must be a u32");
                    return 2;
                }
            },
            "--limit" => match val!("--limit").parse() {
                Ok(v) => limit = Some(v),
                Err(_) => {
                    eprintln!("error: --limit must be a usize");
                    return 2;
                }
            },
            "--repeat-every" => match val!("--repeat-every").parse() {
                Ok(v) => repeat_every = v,
                Err(_) => {
                    eprintln!("error: --repeat-every must be a usize (0 = never)");
                    return 2;
                }
            },
            "--resume" => resume = true,
            "--help" | "-h" => {
                print_help();
                return 0;
            }
            other => {
                eprintln!("error: unknown h2b-arms flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(mpath) = model else {
        eprintln!(
            "error: --model is required. There is no default and no fallback: the arms ARE the \
             generator's decode, so there is nothing to substitute (§18.3)."
        );
        return 2;
    };
    if !mpath.is_file() {
        eprintln!("error: generator not found at {mpath:?} — refusing rather than emitting arms nobody can reproduce");
        return 2;
    }

    let pairs = match cal::read_pairs(&set) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let selected = stratify(&pairs, limit);
    eprintln!(
        "[h2b] {} of {} pair(s) from {set:?} — {} answerable / {} absent",
        selected.len(),
        pairs.len(),
        selected.iter().filter(|p| p.answerable).count(),
        selected.iter().filter(|p| !p.answerable).count(),
    );

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {out_dir:?}: {e}");
        return 1;
    }
    let out_path = out_dir.join(&out_name);

    // ── resume: which ids are already frozen ────────────────────────
    let done: BTreeSet<String> = if resume && out_path.is_file() {
        match read_arms(&out_path) {
            Ok(rows) => rows.into_iter().map(|r| r.id).collect(),
            Err(e) => {
                eprintln!("error: --resume could not read {out_path:?}: {e}");
                return 1;
            }
        }
    } else {
        BTreeSet::new()
    };
    if !done.is_empty() {
        eprintln!("[h2b] --resume: {} pair(s) already frozen, skipping", done.len());
    }
    let todo: Vec<&&CalibrationPair> = selected.iter().filter(|p| !done.contains(&p.id)).collect();
    if todo.is_empty() {
        eprintln!("[h2b] nothing to do — every selected pair is already in {out_path:?}");
        return 0;
    }

    let decoder = match KSampleDecoder::load(&mpath, ModelFamily::Qwen3, n_ctx, ARM_K as u32, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: load generator: {e}");
            return 1;
        }
    };
    let greedy = DrawSampling::greedy();
    let mut sink = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: open {out_path:?}: {e}");
            return 1;
        }
    };

    let started = std::time::Instant::now();
    let mut skipped_too_long = 0usize;
    for (n, p) in todo.iter().enumerate() {
        let prompt_a = build_value_prompt(&p.question, &p.chunks);
        let prompt_b = build_value_prompt(&p.question, &[]);
        let prompt_p = build_parametric_prompt(&p.question);

        let want_repeat = repeat_every > 0 && n % repeat_every == 0;
        let draw = |prompt: &str, system: &str| {
            decoder.draw_prompt(prompt, system, ARM_K, SEED_BASE, greedy)
        };

        let a = match draw(&prompt_a, VALUE_SYSTEM_MESSAGE) {
            Ok(d) => d,
            Err(e) => {
                // A pair whose evidence pool does not fit the context is
                // REPORTED and skipped by name, never truncated: truncating the
                // evidence would change what arm A saw while the artifact still
                // claimed the full pool.
                eprintln!("[h2b] skip {} — arm A: {e}", p.id);
                skipped_too_long += 1;
                continue;
            }
        };
        let repeat_stable = if want_repeat {
            match draw(&prompt_a, VALUE_SYSTEM_MESSAGE) {
                Ok(d2) => Some(d2.raw == a.raw),
                Err(e) => {
                    eprintln!("error: repeat draw {}: {e}", p.id);
                    return 1;
                }
            }
        } else {
            None
        };
        let b = match draw(&prompt_b, VALUE_SYSTEM_MESSAGE) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: arm B {}: {e}", p.id);
                return 1;
            }
        };
        let pp = match draw(&prompt_p, PARAMETRIC_SYSTEM_MESSAGE) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: arm P {}: {e}", p.id);
                return 1;
            }
        };

        let arm_p = record(&pp);
        let row = PairArms {
            id: p.id.clone(),
            corpus_id: p.corpus_id.clone(),
            family: family_of(&p.corpus_id).to_string(),
            answerable: p.answerable,
            question: p.question.clone(),
            n_chunks: p.chunks.len(),
            evidence_index: p.evidence_index,
            parametric_known: leaked(p, &arm_p.value),
            arm_a: record(&a),
            arm_b: record(&b),
            arm_p,
            repeat_stable,
        };
        match serde_json::to_string(&row) {
            Ok(s) => {
                if let Err(e) = writeln!(sink, "{s}").and_then(|()| sink.flush()) {
                    eprintln!("error: write {out_path:?}: {e}");
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("error: serialize {}: {e}", p.id);
                return 1;
            }
        }

        if (n + 1) % 25 == 0 || n + 1 == todo.len() {
            let el = started.elapsed().as_secs_f64();
            let rate = (n + 1) as f64 / el.max(1e-9);
            eprintln!(
                "[h2b] {}/{} pairs  {:.2}/s  elapsed {:.0}s  eta {:.0}s",
                n + 1,
                todo.len(),
                rate,
                el,
                (todo.len() - n - 1) as f64 / rate.max(1e-9),
            );
        }
    }
    if skipped_too_long > 0 {
        eprintln!(
            "[h2b] {skipped_too_long} pair(s) skipped: their evidence pool exceeds n_ctx={n_ctx}. \
             Reported rather than truncated — a truncated pool is a different measurement wearing \
             the same id."
        );
    }
    eprintln!(
        "[h2b] done in {:.0}s → {out_path:?}",
        started.elapsed().as_secs_f64()
    );
    0
}

/// Fold one k=1 draw into its frozen record.
fn record(d: &KSampleDraw) -> ArmRecord {
    let raw = d.raw.first().cloned().unwrap_or_default();
    let value = clean_value(&raw);
    let finished_eog = d.finished_eog.first().copied().unwrap_or(false);
    ArmRecord {
        outcome: ArmOutcome::classify(&value, finished_eog),
        raw,
        value,
        finished_eog,
        prompt_tokens: d.prompt_tokens,
        decoded_tokens: d.decoded_tokens,
        mean_token_margin: d.mean_token_margin(0),
        elapsed_ms: d.elapsed_ms,
    }
}

/// **P1's leak test, and it never looks at arm A.**
///
/// True when arm P produced a value that is literally present in the claim's
/// own supporting passage — the passage arm P was not shown. The presence
/// kernel is the incumbent's (`value_present`, ported in
/// `flywheel/det_checks.rs` from `value_presence.rs`), so "the model produced
/// the gold value" means here exactly what it means in the production grounding
/// gate (principle 8).
///
/// **Only measurable on answerable pairs.** An absent pair has no supporting
/// passage in the set — that is what makes it absent — so there is no gold to
/// leak and the flag is `false` by definition, not by measurement. The verdict
/// reports the leak rate over answerable pairs alone for exactly this reason;
/// a rate over all pairs would be diluted by pairs where the question could not
/// have been asked.
pub fn leaked(pair: &CalibrationPair, arm_p_value: &Option<String>) -> bool {
    let (Some(idx), Some(v)) = (pair.evidence_index, arm_p_value.as_ref()) else {
        return false;
    };
    match pair.chunks.get(idx) {
        Some(support) => value_present(v, std::slice::from_ref(support)),
        None => false,
    }
}

/// Deterministic stratified subsample.
///
/// Four strata — the cross of `answerable` and [`super::split_of`] — each
/// sampled by an **even stride** over the set's own stable order rather than by
/// RNG. Two properties follow and both matter: the same `--limit` selects the
/// same pairs on any host (so a partial run and a full run are comparable), and
/// the class balance and the split balance of the subsample match the full
/// set's, so a held-out AUROC computed on 1,000 pairs is estimating the same
/// quantity a 4,207-pair one would.
///
/// `None` keeps everything, in order.
pub fn stratify<'a>(pairs: &'a [CalibrationPair], limit: Option<usize>) -> Vec<&'a CalibrationPair> {
    let Some(target) = limit else {
        return pairs.iter().collect();
    };
    if target >= pairs.len() {
        return pairs.iter().collect();
    }
    let mut strata: Vec<Vec<&CalibrationPair>> = vec![Vec::new(); 4];
    for p in pairs {
        let key = usize::from(p.answerable) * 2
            + usize::from(super::split_of(&p.corpus_id) == super::Split::Holdout);
        strata[key].push(p);
    }
    let total = pairs.len() as f64;
    let mut out: Vec<&CalibrationPair> = Vec::with_capacity(target);
    for s in &strata {
        if s.is_empty() {
            continue;
        }
        let want = ((target as f64) * (s.len() as f64) / total).round() as usize;
        let want = want.clamp(1, s.len());
        // Even stride: index i·|s|/want, so the picks spread across the whole
        // stratum instead of clustering in its prefix (which, since the set is
        // ordered by corpus then claim, would otherwise sample a handful of
        // articles exhaustively rather than many articles thinly).
        for i in 0..want {
            let idx = (i * s.len()) / want;
            out.push(s[idx]);
        }
    }
    // Restore the set's own order so the artifact is diffable against a
    // full-set run.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Read a frozen arms file. Used by `--resume` and by the gate.
pub fn read_arms(path: &std::path::Path) -> Result<Vec<PairArms>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<PairArms>(line)
                .map_err(|e| format!("{path:?} line {}: {e}", n + 1))?,
        );
    }
    if out.is_empty() {
        return Err(format!(
            "{path:?} holds 0 arm rows — there is nothing to score, and a verdict over nothing \
             would be a verdict about the harness"
        ));
    }
    Ok(out)
}

fn print_help() {
    eprintln!(
        "svrn bench flywheel h2b-arms — H2b stage 1: the three-arm evidence counterfactual.\n\
         \n\
         Decodes each calibration pair three times, greedily: arm A with the evidence pool,\n\
         arm B with the chunks ablated (the order's counterfactual), arm P with the evidence\n\
         frame removed entirely (the leak detector — see counterfactual/mod.rs for why arm B\n\
         cannot serve as one). Writes one row per pair as it goes; nothing here loads a\n\
         reranker and nothing here computes a verdict.\n\
         \n\
         Flags:\n\
         \x20 --model <gguf>        REQUIRED. No default, no fallback.\n\
         \x20 --set <jsonl|gz>      calibration set (default: the committed one)\n\
         \x20 --limit N             deterministic STRATIFIED subsample of N pairs\n\
         \x20 --out-dir <dir>       default sovereign/bench/calibration/h2b\n\
         \x20 --out-name <file>     default h2b_arms.jsonl\n\
         \x20 --n-ctx N             default 8192\n\
         \x20 --repeat-every N      re-decode arm A every Nth pair as a determinism check\n\
         \x20                       (default 25; 0 disables — and the gate then says so)\n\
         \x20 --resume              skip pairs already present in the output file\n\
         \n\
         Exit: 0 = arms written, 1 = could not measure, 2 = usage."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_eval::flywheel::calibration::CalibrationPair;

    fn pair(id: &str, corpus: &str, answerable: bool, chunks: Vec<&str>, ev: Option<usize>) -> CalibrationPair {
        CalibrationPair {
            id: id.into(),
            corpus_id: corpus.into(),
            question: "q?".into(),
            chunks: chunks.into_iter().map(String::from).collect(),
            chunk_ids: vec![],
            evidence_index: ev,
            passage_source: "test".into(),
            answerable,
            witness: vec![],
            source_claim: "claim".into(),
            witness_in_pool: false,
        }
    }

    #[test]
    fn the_leak_test_fires_on_a_value_the_withheld_passage_contains() {
        let p = pair(
            "cal:sep-x:c1:present",
            "sep-x",
            true,
            vec!["a distractor", "Karl Yundt giggled grimly at the table."],
            Some(1),
        );
        assert!(
            leaked(&p, &Some("Karl Yundt".into())),
            "arm P named the gold value without being shown it — that is the leak"
        );
    }

    #[test]
    fn the_leak_test_does_not_fire_on_a_value_the_passage_does_not_carry() {
        // Watched to fail from the other side: a detector that always fired
        // would pass the test above and would flag every pair as leaked,
        // emptying the primary gate.
        let p = pair(
            "cal:sep-x:c1:present",
            "sep-x",
            true,
            vec!["a distractor", "Karl Yundt giggled grimly at the table."],
            Some(1),
        );
        assert!(!leaked(&p, &Some("Ossipon".into())));
        assert!(!leaked(&p, &None), "a refusal cannot leak a value");
    }

    #[test]
    fn an_absent_pair_has_no_gold_to_leak() {
        // Structural, not a heuristic: an absent pair carries no supporting
        // passage, so there is nothing to check containment against. Returning
        // `true` here on some near-match would put a `parametric_known`
        // exclusion on the negative class and quietly rebalance the gate.
        let p = pair("cal:sep-x:c1:absent", "sep-x", false, vec!["Karl Yundt giggled."], None);
        assert!(!leaked(&p, &Some("Karl Yundt".into())));
    }

    #[test]
    fn the_leak_test_indexes_the_supporting_passage_not_the_whole_pool() {
        // THE bug this shape exists to prevent: checking arm P's value against
        // the full pool would fire on any distractor that happened to contain
        // it, and the leak rate would then be a property of the pool builder.
        let p = pair(
            "cal:sep-x:c1:present",
            "sep-x",
            true,
            vec!["Ossipon spoke first.", "Karl Yundt giggled grimly."],
            Some(1),
        );
        assert!(!leaked(&p, &Some("Ossipon".into())), "a distractor is not the gold");
        assert!(leaked(&p, &Some("Karl Yundt".into())));
    }

    #[test]
    fn stratification_preserves_both_balances_and_is_deterministic() {
        let mut pairs = Vec::new();
        for c in 0..20 {
            for n in 0..10 {
                let corpus = format!("sep-{c:02}");
                pairs.push(pair(&format!("cal:{corpus}:c{n:02}:present"), &corpus, true, vec!["x"], Some(0)));
                pairs.push(pair(&format!("cal:{corpus}:c{n:02}:absent"), &corpus, false, vec!["x"], None));
            }
        }
        let a = stratify(&pairs, Some(100));
        let b = stratify(&pairs, Some(100));
        assert_eq!(
            a.iter().map(|p| &p.id).collect::<Vec<_>>(),
            b.iter().map(|p| &p.id).collect::<Vec<_>>(),
            "the same limit must select the same pairs, or a partial run is not \
             comparable with a full one"
        );
        assert!((a.len() as i64 - 100).abs() <= 4, "got {}", a.len());
        let ans = a.iter().filter(|p| p.answerable).count();
        assert!(
            (ans as i64 - (a.len() as i64) / 2).abs() <= 2,
            "class balance drifted: {ans} of {}",
            a.len()
        );
        // The stride must spread across articles rather than exhaust a prefix —
        // a subsample of 100 that touched 5 of 20 corpora would make the
        // held-out split a measurement of five essays.
        let corpora: BTreeSet<&String> = a.iter().map(|p| &p.corpus_id).collect();
        assert!(corpora.len() >= 15, "only {} corpora sampled", corpora.len());
    }

    #[test]
    fn no_limit_keeps_everything_and_an_oversized_limit_does_too() {
        let pairs = vec![pair("a", "sep-x", true, vec!["x"], Some(0))];
        assert_eq!(stratify(&pairs, None).len(), 1);
        assert_eq!(stratify(&pairs, Some(9999)).len(), 1);
    }
}
