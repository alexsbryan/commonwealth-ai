// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel h4-sweep` — run the sentence-margin sweep over a frozen
//! chaos transcript and emit one row per sentence.
//!
//! Offline in the sense that matters: no daemon, no synthesis, no judge, no
//! Critic. One local 0.6B cross-encoder scores `(sentence, chunk)` pairs, and
//! everything else is deterministic. The transcript is read, never rewritten.
//!
//! **This command decides nothing.** It emits margins; the floor that turns a
//! margin into a verdict is calibrated by `h4-gate` and committed beside the
//! curve that justifies it (principle 2). That separation is why this file has
//! no threshold constant in it.
//!
//! **Per-turn wall time is a product, not a side effect.** `turn_elapsed_ms` on
//! every row is the §7.3 H4 audit-latency measurement — the quantity that has
//! to come in at ≤2s p50 against the incumbent's ~35 judge calls. It is
//! measured here, once, by the thing actually doing the work.

use std::io::Write;
use std::path::PathBuf;

use sovereign_core::runtime::native_grounding::sentence_sweep::{
    self, SentenceRow, SentenceScorer,
};

use super::{scorer, transcript};

/// One emitted row: a sentence, its margin, and the turn context needed to join
/// it back to the incumbent's verdict later.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SweepRow {
    pub turn_id: String,
    pub sentence_index: usize,
    pub text: String,
    /// `null` is could-not-judge (no evidence, or no content), never "low".
    pub margin: Option<f32>,
    pub best_chunk: Option<usize>,
    /// The span resolver's verdict for this sentence, as its stable label.
    pub span: String,
    /// Labels of the structural vetoes that fired. Usually empty.
    pub vetoes: Vec<String>,
    // ── turn context, copied so a row is self-contained ──
    pub gate_action: Option<String>,
    /// The incumbent Critic's per-TURN violation probability, when the run that
    /// produced this transcript recorded one. `null` means the Critic was never
    /// asked — not that the turn was clean.
    pub violation_prob: Option<f64>,
    /// How many claims the incumbent's ladder audited on this turn.
    pub n_holdings: usize,
    pub evidence_chunks: usize,
    pub k_cap_applied: bool,
    /// Wall time for the WHOLE turn's sweep, repeated on each of its rows.
    pub turn_elapsed_ms: u128,
}

fn row_of(
    t: &transcript::ReplayTurn,
    s: &SentenceRow,
    res: &sentence_sweep::SweepResult,
) -> SweepRow {
    SweepRow {
        turn_id: t.id.clone(),
        sentence_index: s.index,
        text: s.text.clone(),
        margin: s.margin,
        best_chunk: s.best_chunk,
        span: s.span.label().to_string(),
        vetoes: s.vetoes.iter().map(|v| v.label().to_string()).collect(),
        gate_action: t.gate_action.clone(),
        violation_prob: t.violation_prob,
        n_holdings: t.holdings().len(),
        evidence_chunks: res.evidence_chunks,
        k_cap_applied: res.k_cap_applied,
        turn_elapsed_ms: res.elapsed_ms,
    }
}

/// p50 of a slice of durations. Returns `None` for an empty slice — an absent
/// measurement, reported rather than defaulted to 0.
pub fn p50(mut v: Vec<u128>) -> Option<u128> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

pub(crate) async fn cmd_h4_sweep(rest: &[String]) -> i32 {
    let mut transcripts: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut rerank_model: Option<PathBuf> = None;
    let mut limit: Option<usize> = None;

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
            "--transcripts" => transcripts = Some(PathBuf::from(val!("--transcripts"))),
            "--out" => out = Some(PathBuf::from(val!("--out"))),
            "--rerank-model" => rerank_model = Some(PathBuf::from(val!("--rerank-model"))),
            "--limit" => match val!("--limit").parse() {
                Ok(n) => limit = Some(n),
                Err(e) => {
                    eprintln!("error: --limit: {e}");
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

    let Some(transcripts) = transcripts else {
        eprintln!("error: --transcripts is required");
        print_help();
        return 2;
    };
    let out = out.unwrap_or_else(|| {
        let stem = transcripts
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("transcripts")
            .trim_end_matches(".transcripts")
            .to_string();
        transcripts.with_file_name(format!("{stem}.h4_sweep.jsonl"))
    });

    match run(&transcripts, &out, rerank_model, limit).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

async fn run(
    transcripts: &std::path::Path,
    out: &std::path::Path,
    rerank_model: Option<PathBuf>,
    limit: Option<usize>,
) -> Result<(), String> {
    // The instrument is resolved and refused BEFORE anything is read, so an
    // absent reranker costs nothing and cannot be discovered halfway through.
    let rerank_path = scorer::resolve_rerank_path(rerank_model)?;

    let (mut turns, skipped) = transcript::load(transcripts)?;
    if skipped > 0 {
        eprintln!("[h4] WARN: {skipped} unreadable line(s) skipped — the counts below are over the rest");
    }
    if turns.is_empty() {
        return Err(format!(
            "{} holds 0 readable turns — there is nothing to sweep",
            transcripts.display()
        ));
    }
    if let Some(n) = limit {
        turns.truncate(n);
    }

    let n_total = turns.len();
    let (replayable, skipped_turns): (Vec<_>, Vec<_>) =
        turns.into_iter().partition(|t| t.is_replayable());
    if replayable.is_empty() {
        return Err(format!(
            "all {n_total} turns are unreplayable (no released answer, or no evidence) — \
             reporting that rather than emitting an empty sweep as a result"
        ));
    }

    let scorer = scorer::load(&rerank_path)?;
    eprintln!(
        "[h4] sweeping {} of {n_total} turns ({} unreplayable) from {}",
        replayable.len(),
        skipped_turns.len(),
        transcripts.display()
    );

    let mut rows: Vec<SweepRow> = Vec::new();
    let mut per_turn_ms: Vec<u128> = Vec::new();
    let mut pairs = 0usize;
    for t in &replayable {
        let res = sentence_sweep::sweep(
            &t.question,
            &t.answer,
            &t.retrieved_chunks,
            &scorer as &dyn SentenceScorer,
        )
        .await
        .map_err(|e| format!("sweep {}: {e}", t.id))?;
        pairs += res.scored_pairs;
        per_turn_ms.push(res.elapsed_ms);
        for s in &res.sentences {
            rows.push(row_of(t, s, &res));
        }
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut f = std::fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    for r in &rows {
        let line = serde_json::to_string(r).map_err(|e| format!("serialize: {e}"))?;
        writeln!(f, "{line}").map_err(|e| format!("write {}: {e}", out.display()))?;
    }

    let scoreable = rows.iter().filter(|r| r.margin.is_some()).count();
    eprintln!(
        "[h4] {} rows ({scoreable} scoreable) from {} turns, {pairs} pairs scored",
        rows.len(),
        replayable.len()
    );
    match p50(per_turn_ms.clone()) {
        Some(ms) => eprintln!(
            "[h4] per-turn audit wall time: p50 {ms} ms over {} turns (§7.3 H4 bar: <= 2000 ms)",
            per_turn_ms.len()
        ),
        None => eprintln!("[h4] per-turn audit wall time: NOT MEASURED (no turns swept)"),
    }
    if !skipped_turns.is_empty() {
        eprintln!(
            "[h4] {} turn(s) not swept (no answer or no evidence — could-not-judge, not scored): {}",
            skipped_turns.len(),
            skipped_turns
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    eprintln!("[out] wrote sweep rows → {}", out.display());
    Ok(())
}

fn print_help() {
    eprintln!(
        "svrn bench flywheel h4-sweep — NATIVE_GROUNDING §5 H4's sentence sweep, offline.\n\
         \n\
         Splits every released answer in a FROZEN chaos transcript with the lossless\n\
         splitter, scores each sentence against that turn's sealed evidence with the\n\
         rerank slot (max over the k<=8 pool), rides the deterministic vetoes and the\n\
         span resolver along, and writes one row per sentence. No daemon, no judge, no\n\
         Critic, and the transcript is never rewritten.\n\
         \n\
         Emits margins, NOT verdicts — the floor is calibrated by `h4-gate` and lives\n\
         beside its committed curve.\n\
         \n\
         Flags:\n\
         \x20 --transcripts <jsonl>   a chaos *.transcripts.jsonl (required)\n\
         \x20 --out <jsonl>           default: <stem>.h4_sweep.jsonl beside the input\n\
         \x20 --rerank-model <gguf>   default: $SOVEREIGN_RERANK_MODEL_PATH (default-inert;\n\
         \x20                         its absence is reported, never worked around)\n\
         \x20 --limit N               sweep only the first N turns (smoke run)\n\
         \n\
         Exit: 0 = swept, 1 = could not measure, 2 = usage."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p50_of_nothing_is_absent_not_zero() {
        assert_eq!(p50(vec![]), None, "an unmeasured latency must not read 0 ms");
        assert_eq!(p50(vec![7]), Some(7));
        assert_eq!(p50(vec![9, 1, 5]), Some(5));
    }

    #[test]
    fn a_row_is_self_contained_and_round_trips() {
        let r = SweepRow {
            turn_id: "t".into(),
            sentence_index: 0,
            text: "A sentence. ".into(),
            margin: Some(0.5),
            best_chunk: Some(1),
            span: "verbatim".into(),
            vetoes: vec!["absent_name_attribution".into()],
            gate_action: Some("released".into()),
            violation_prob: None,
            n_holdings: 2,
            evidence_chunks: 8,
            k_cap_applied: true,
            turn_elapsed_ms: 1234,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: SweepRow = serde_json::from_str(&s).unwrap();
        assert_eq!(back.turn_id, r.turn_id);
        assert_eq!(back.margin, r.margin);
        assert_eq!(back.turn_elapsed_ms, r.turn_elapsed_ms);
        assert!(
            s.contains("\"violation_prob\":null"),
            "an absent Critic verdict must serialize as null, not be dropped: {s}"
        );
    }
}
