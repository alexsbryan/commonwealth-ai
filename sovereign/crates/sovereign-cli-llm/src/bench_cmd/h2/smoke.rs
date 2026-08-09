// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h2-smoke` — validate the instrument before the result.
//!
//! ARCH §18.4, and the order's deliverable 3: before any statistic is computed
//! over a draw, two things must be true of the draw itself, and neither is
//! knowable from the gate's output.
//!
//! 1. **Does the sampler diverge at all?** The order names the failure
//!    explicitly — *"if multi-seq sampling cannot produce non-degenerate value
//!    diversity (all k samples byte-identical at temp 0.7 across turns — that
//!    is a sampler finding, report it)"*. k identical samples give one cluster,
//!    `semantic_entropy = 0`, `agreement = 1.0` — a reading indistinguishable
//!    from genuine unanimity. Every subsequent number would be a measurement of
//!    a broken sampler.
//! 2. **Does the same seed base reproduce the draw?** If not, no H2 artifact is
//!    replayable and the statistic is not an instrument.
//!
//! The evidence is READ from a frozen chaos transcript. No probe is generated,
//! no bank is run, nothing is written back. The only model loaded is the
//! generator, for `--turns` turns' worth of k×≤24 tokens.

use std::path::PathBuf;

use sovereign_core::model_family::ModelFamily;
use sovereign_inference::k_sample::{KSampleDecoder, KSampleDraw};

use super::super::h4::transcript;

/// What the smoke saw, per turn.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SmokeTurn {
    pub id: String,
    pub question: String,
    pub chunks: usize,
    pub k: usize,
    pub seed_base: u32,
    pub seeds: Vec<u32>,
    pub raw: Vec<String>,
    pub values: Vec<Option<String>>,
    pub distinct_raw: usize,
    pub all_identical: bool,
    pub prompt_tokens: usize,
    pub decoded_tokens: usize,
    pub elapsed_ms: u128,
    /// The second draw at the same seed base — the reproducibility check.
    /// `None` when `--no-repeat` was passed.
    pub repeat_raw: Option<Vec<String>>,
    /// Did the repeat reproduce the first draw byte-for-byte?
    pub reproducible: Option<bool>,
}

/// The smoke's verdict over all turns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SmokeReport {
    pub model: String,
    pub k: usize,
    pub temperature: f32,
    pub turns: Vec<SmokeTurn>,
    /// Turns whose k samples were all byte-identical. Non-empty is the
    /// order's sampler finding.
    pub degenerate_turns: Vec<String>,
    /// Turns whose repeat draw did not reproduce. Non-empty means no H2
    /// artifact from this host is replayable.
    pub irreproducible_turns: Vec<String>,
    /// The instrument's verdict. Four values, never two (principle 5).
    pub outcome: SmokeOutcome,
    pub notes: Vec<String>,
}

/// Four verdicts, not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeOutcome {
    /// Diverse AND reproducible. The instrument is fit to measure with.
    InstrumentValidated,
    /// Every turn's k draws were byte-identical. The sampler finding.
    Degenerate,
    /// Draws did not reproduce at a fixed seed base.
    Irreproducible,
    /// Neither could be established (no replayable turn, model absent).
    CouldNotJudge,
}

pub async fn cmd_h2_smoke(args: &[String]) -> i32 {
    let mut transcript_path: Option<PathBuf> = None;
    let mut model: Option<PathBuf> = None;
    let mut out_dir = PathBuf::from("sovereign/bench/calibration/h2");
    let mut k: u8 = 5;
    let mut turns: usize = 2;
    let mut seed_base: u32 = 1592590337;
    let mut n_ctx: u32 = 8192;
    let mut repeat = true;

    let mut i = 0;
    while i < args.len() {
        let take = |i: usize| args.get(i + 1).cloned();
        match args[i].as_str() {
            "--transcript" => {
                if let Some(v) = take(i) {
                    transcript_path = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--model" => {
                if let Some(v) = take(i) {
                    model = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--out-dir" => {
                if let Some(v) = take(i) {
                    out_dir = PathBuf::from(v);
                    i += 1;
                }
            }
            "--k" => {
                if let Some(v) = take(i).and_then(|s| s.parse().ok()) {
                    k = v;
                    i += 1;
                }
            }
            "--turns" => {
                if let Some(v) = take(i).and_then(|s| s.parse().ok()) {
                    turns = v;
                    i += 1;
                }
            }
            "--seed-base" => {
                if let Some(v) = take(i).and_then(|s| s.parse().ok()) {
                    seed_base = v;
                    i += 1;
                }
            }
            "--n-ctx" => {
                if let Some(v) = take(i).and_then(|s| s.parse().ok()) {
                    n_ctx = v;
                    i += 1;
                }
            }
            "--no-repeat" => repeat = false,
            "--help" | "-h" => {
                eprintln!(
                    "svrn bench flywheel h2-smoke --transcript <chaos.transcripts.jsonl> \
                     --model <generator.gguf> [--k 5] [--turns 2] [--seed-base N] \
                     [--n-ctx 8192] [--no-repeat] [--out-dir <dir>]"
                );
                return 0;
            }
            other => {
                eprintln!("error: unknown h2-smoke flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(tpath) = transcript_path else {
        eprintln!("error: --transcript is required — the smoke reads sealed evidence from a FROZEN transcript, it does not generate probes");
        return 2;
    };
    let Some(mpath) = model else {
        eprintln!(
            "error: --model is required. There is no default and no fallback: a k-sample \
             draw IS the generator's sampling distribution, so there is nothing to \
             substitute (§18.3)."
        );
        return 2;
    };
    if !mpath.is_file() {
        eprintln!("error: generator not found at {mpath:?} — refusing rather than reporting a draw nobody can reproduce");
        return 2;
    }

    let (rows, skipped) = match transcript::load(&tpath) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if skipped > 0 {
        eprintln!("[h2] {skipped} unreadable transcript line(s) skipped");
    }
    let replayable: Vec<_> = rows.iter().filter(|r| r.is_replayable()).take(turns).collect();
    if replayable.is_empty() {
        eprintln!(
            "error: no replayable turn in {} — a turn needs both a released answer and \
             sealed evidence",
            tpath.display()
        );
        return 1;
    }
    eprintln!(
        "[h2] smoke over {} turn(s), k={k}, seed_base={seed_base}",
        replayable.len()
    );

    let decoder = match KSampleDecoder::load(&mpath, ModelFamily::Qwen3, n_ctx, k as u32, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: load generator: {e}");
            return 1;
        }
    };

    let mut out_turns = Vec::new();
    for t in &replayable {
        eprintln!("[h2] drawing {}", t.id);
        let draw = match decoder.draw(&t.question, &t.retrieved_chunks, k, seed_base) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: draw {}: {e}", t.id);
                return 1;
            }
        };
        let (repeat_raw, reproducible) = if repeat {
            match decoder.draw(&t.question, &t.retrieved_chunks, k, seed_base) {
                Ok(d2) => {
                    let same = d2.raw == draw.raw;
                    (Some(d2.raw), Some(same))
                }
                Err(e) => {
                    eprintln!("error: repeat draw {}: {e}", t.id);
                    return 1;
                }
            }
        } else {
            (None, None)
        };
        out_turns.push(summarise(t.id.clone(), t.question.clone(), t.retrieved_chunks.len(), seed_base, &draw, repeat_raw, reproducible));
    }

    let report = judge(mpath.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string(), k as usize, out_turns);

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: create {}: {e}", out_dir.display());
        return 1;
    }
    let path = out_dir.join("h2_sampler_smoke.json");
    let json = serde_json::to_string_pretty(&report).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, format!("{json}\n")) {
        eprintln!("error: write {}: {e}", path.display());
        return 1;
    }

    println!("H2 sampler smoke — {:?}", report.outcome);
    for t in &report.turns {
        println!(
            "  {:36} distinct {}/{}  repro {}  {} prompt tok, {} decoded, {} ms",
            t.id,
            t.distinct_raw,
            t.k,
            match t.reproducible {
                Some(true) => "yes",
                Some(false) => "NO",
                None => "n/a",
            },
            t.prompt_tokens,
            t.decoded_tokens,
            t.elapsed_ms
        );
        for (n, r) in t.raw.iter().enumerate() {
            println!("      [{}] seed {:>10}  {}", n, t.seeds[n], r.trim());
        }
    }
    for n in &report.notes {
        println!("  note: {n}");
    }
    println!("  artifact {}", path.display());

    match report.outcome {
        SmokeOutcome::InstrumentValidated => 0,
        SmokeOutcome::Degenerate | SmokeOutcome::Irreproducible => 3,
        SmokeOutcome::CouldNotJudge => 1,
    }
}

/// Pure: fold one draw into its report row.
fn summarise(
    id: String,
    question: String,
    chunks: usize,
    seed_base: u32,
    draw: &KSampleDraw,
    repeat_raw: Option<Vec<String>>,
    reproducible: Option<bool>,
) -> SmokeTurn {
    SmokeTurn {
        id,
        question,
        chunks,
        k: draw.raw.len(),
        seed_base,
        seeds: draw.seeds.clone(),
        raw: draw.raw.clone(),
        values: draw.values.clone(),
        distinct_raw: draw.distinct_raw(),
        all_identical: draw.all_identical(),
        prompt_tokens: draw.prompt_tokens,
        decoded_tokens: draw.decoded_tokens,
        elapsed_ms: draw.elapsed_ms,
        repeat_raw,
        reproducible,
    }
}

/// Pure: the verdict over the turns. Extracted so every outcome branch is
/// testable with no model — including the two failure branches, which is what
/// makes this a gate rather than a print statement (§18.1).
pub fn judge(model: String, k: usize, turns: Vec<SmokeTurn>) -> SmokeReport {
    let degenerate_turns: Vec<String> = turns
        .iter()
        .filter(|t| t.all_identical)
        .map(|t| t.id.clone())
        .collect();
    let irreproducible_turns: Vec<String> = turns
        .iter()
        .filter(|t| t.reproducible == Some(false))
        .map(|t| t.id.clone())
        .collect();
    let checked_repro = turns.iter().any(|t| t.reproducible.is_some());

    let mut notes = Vec::new();
    let outcome = if turns.is_empty() {
        notes.push("no turn was drawn — nothing to validate".to_string());
        SmokeOutcome::CouldNotJudge
    } else if !irreproducible_turns.is_empty() {
        notes.push(format!(
            "{} turn(s) did not reproduce at a fixed seed base — no H2 artifact from \
             this host is replayable until that is understood",
            irreproducible_turns.len()
        ));
        SmokeOutcome::Irreproducible
    } else if degenerate_turns.len() == turns.len() {
        notes.push(
            "every turn's k draws were byte-identical. This is the order's named \
             sampler finding: entropy would read 0 and agreement 1.0 on every turn, \
             indistinguishable from genuine unanimity."
                .to_string(),
        );
        SmokeOutcome::Degenerate
    } else if !checked_repro {
        notes.push(
            "diversity confirmed but reproducibility was NOT checked (--no-repeat) — \
             the instrument is half-validated"
                .to_string(),
        );
        SmokeOutcome::CouldNotJudge
    } else {
        if !degenerate_turns.is_empty() {
            notes.push(format!(
                "{} of {} turns drew identically. Not a sampler defect on its own — a \
                 turn whose evidence pins one short value SHOULD agree — but a run \
                 where this dominates is measuring the prompt, not the distribution.",
                degenerate_turns.len(),
                turns.len()
            ));
        }
        SmokeOutcome::InstrumentValidated
    };

    SmokeReport {
        model,
        k,
        temperature: sovereign_inference::k_sample::DRAW_TEMPERATURE,
        turns,
        degenerate_turns,
        irreproducible_turns,
        outcome,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: &str, raw: &[&str], reproducible: Option<bool>) -> SmokeTurn {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        let mut seen: Vec<&String> = Vec::new();
        for r in &raw {
            if !seen.contains(&r) {
                seen.push(r);
            }
        }
        SmokeTurn {
            id: id.into(),
            question: "q".into(),
            chunks: 3,
            k: raw.len(),
            seed_base: 1,
            seeds: vec![0; raw.len()],
            values: raw.iter().map(|r| Some(r.clone())).collect(),
            distinct_raw: seen.len(),
            all_identical: seen.len() == 1,
            prompt_tokens: 100,
            decoded_tokens: 20,
            elapsed_ms: 5,
            repeat_raw: reproducible.map(|_| raw.clone()),
            reproducible,
            raw,
        }
    }

    #[test]
    fn a_diverse_reproducible_draw_validates_the_instrument() {
        let r = judge(
            "m".into(),
            3,
            vec![
                turn("t1", &["Quenholt", "Pellow", "Quenholt"], Some(true)),
                turn("t2", &["the inn", "the lock basin", "the inn"], Some(true)),
            ],
        );
        assert_eq!(r.outcome, SmokeOutcome::InstrumentValidated);
        assert!(r.degenerate_turns.is_empty());
    }

    #[test]
    fn every_turn_identical_is_the_samplers_finding_not_unanimity() {
        // The order's named stop condition, and the reason this command
        // exists. Watched to fail from the other direction by the test above.
        let r = judge(
            "m".into(),
            3,
            vec![
                turn("t1", &["Quenholt", "Quenholt", "Quenholt"], Some(true)),
                turn("t2", &["the inn", "the inn", "the inn"], Some(true)),
            ],
        );
        assert_eq!(r.outcome, SmokeOutcome::Degenerate);
        assert_eq!(r.degenerate_turns.len(), 2);
        assert!(r.notes[0].contains("byte-identical"));
    }

    #[test]
    fn irreproducibility_outranks_diversity() {
        // A diverse draw that does not reproduce is WORSE than a degenerate
        // one: it looks like a working instrument and is not. So it must win
        // the verdict, not be masked by the diversity check passing.
        let r = judge(
            "m".into(),
            3,
            vec![turn("t1", &["a", "b", "c"], Some(false))],
        );
        assert_eq!(r.outcome, SmokeOutcome::Irreproducible);
        assert_eq!(r.irreproducible_turns, vec!["t1".to_string()]);
    }

    #[test]
    fn some_turns_agreeing_is_not_a_failure_but_is_reported() {
        let r = judge(
            "m".into(),
            3,
            vec![
                turn("t1", &["Quenholt", "Quenholt", "Quenholt"], Some(true)),
                turn("t2", &["the inn", "the lock basin", "the inn"], Some(true)),
            ],
        );
        assert_eq!(r.outcome, SmokeOutcome::InstrumentValidated);
        assert_eq!(r.degenerate_turns, vec!["t1".to_string()]);
        assert!(
            r.notes.iter().any(|n| n.contains("1 of 2")),
            "a partial degeneracy must be counted in the artifact, not swallowed"
        );
    }

    #[test]
    fn no_turns_is_could_not_judge_not_validated() {
        let r = judge("m".into(), 5, vec![]);
        assert_eq!(r.outcome, SmokeOutcome::CouldNotJudge);
    }

    #[test]
    fn skipping_the_repeat_leaves_the_instrument_half_validated() {
        // §18.3: the absence of the reproducibility check is reported, never
        // defaulted into a pass.
        let r = judge("m".into(), 3, vec![turn("t1", &["a", "b", "c"], None)]);
        assert_eq!(r.outcome, SmokeOutcome::CouldNotJudge);
        assert!(r.notes[0].contains("half-validated"));
    }
}
