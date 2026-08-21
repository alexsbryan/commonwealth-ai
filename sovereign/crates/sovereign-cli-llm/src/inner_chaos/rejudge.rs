// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn eval inner-chaos --rejudge <journal>` — re-score an existing
//! transcript journal with a (usually stronger) judge model, WITHOUT
//! re-running the SUT.
//!
//! The 2h soak collects transcripts with `--no-judge` (fast, no judge
//! contention on the shared daemon); this mode replays those transcripts
//! through the SAME `witness_judge_request` rubric + `parse_witness_verdict`
//! parser used live — so there is zero prompt drift between an inline judge
//! and a re-judge — but pins the judge to `--judge-model` (e.g. the 122B).
//!
//! Faithful reconstruction: the live judge sees the transcript INCLUDING
//! the current user turn but EXCLUDING the reply under audit (runner.rs
//! step 3). We rebuild exactly that per turn by grouping journal records
//! by `conv_id`, ordering by `turn`, and interleaving user/witness turns.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;

use super::journal::TurnRecord;
use super::judge::{parse_witness_verdict, witness_judge_request};
use super::personas::{load_memories, resolve_bench_dir};
use super::report;
use super::transcript::TranscriptTurn;
use sovereign_cli_shared::args::Parsed;

/// Entry point for `--rejudge <journal>`.
pub async fn run(flags: &Parsed, journal_path: PathBuf, bench_dir: Option<PathBuf>) -> i32 {
    let Some(judge_model) = flags
        .value("judge-model")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
    else {
        eprintln!(
            "inner-chaos --rejudge: --judge-model <id> is required (the whole point is to \
             re-score with a specific, usually stronger, judge — e.g. the 122B)."
        );
        return 2;
    };
    let daemon_base = flags
        .value("daemon")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "http://localhost:9741".to_string());

    // Seed memories are the fabricated_memory ground truth; identical for
    // every turn (the live runner seeds the whole memories.toml set).
    let bench_dir = match resolve_bench_dir(bench_dir.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("inner-chaos --rejudge: {e}");
            return 1;
        }
    };
    let memories = match load_memories(&bench_dir.join("memories.toml")) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("inner-chaos --rejudge: {e}");
            return 1;
        }
    };
    let seed: Vec<String> = memories.values().map(|m| m.content.clone()).collect();

    // Read + filter the journal. Keep only real SUT turns (a captured
    // response, no run-side error); error/empty records never had a reply
    // to audit.
    let raw = match std::fs::read_to_string(&journal_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "inner-chaos --rejudge: cannot read journal {}: {e}",
                journal_path.display()
            );
            return 1;
        }
    };
    let mut records: Vec<TurnRecord> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TurnRecord>(l).ok())
        .filter(|r| r.error.is_none() && !r.response.is_empty() && !r.user.is_empty())
        .collect();
    if records.is_empty() {
        eprintln!(
            "inner-chaos --rejudge: no judgeable turns in {} (need records with user+response)",
            journal_path.display()
        );
        return 1;
    }
    // Group by conversation, ordered by turn — so transcript reconstruction
    // is correct within each thread. Stable sort preserves journal order
    // for ties (there should be none: conv_id+turn is unique).
    records.sort_by(|a, b| a.conv_id.cmp(&b.conv_id).then(a.turn.cmp(&b.turn)));

    let v1 = format!("{}/v1", daemon_base.trim_end_matches('/'));
    let judge: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &judge_model, 8192));

    let total = records.len();
    eprintln!(
        "inner-chaos --rejudge: {total} turns from {}\n  judge-model: {judge_model}\n  daemon: {daemon_base}",
        journal_path.display()
    );

    let mut out: Vec<TurnRecord> = Vec::with_capacity(total);
    let mut cur_conv = String::new();
    let mut transcript: Vec<TranscriptTurn> = Vec::new();
    let mut judged = 0usize;
    let mut failed = 0usize;

    for (i, mut rec) in records.into_iter().enumerate() {
        if rec.conv_id != cur_conv {
            cur_conv = rec.conv_id.clone();
            transcript.clear();
        }
        // Transcript up to and INCLUDING this user turn, reply excluded —
        // mirrors runner.rs step 3 exactly.
        transcript.push(TranscriptTurn::user(rec.user.clone()));

        let req = witness_judge_request(&seed, &transcript, &rec.response);
        let verdict = match judge.complete(&req).await {
            Ok(resp) => parse_witness_verdict(&resp.text),
            Err(e) => {
                eprintln!("  [{}/{total}] judge inference failed: {e}", i + 1);
                None
            }
        };
        rec.judge_failed = verdict.is_none();
        if verdict.is_some() {
            judged += 1;
        } else {
            failed += 1;
        }
        match &verdict {
            Some(v) => eprintln!(
                "  [{}/{total}] {} t{}: {} red_lines={:?}",
                i + 1,
                rec.persona,
                rec.turn,
                v.category.as_str(),
                v.red_lines
            ),
            None => eprintln!(
                "  [{}/{total}] {} t{}: UNJUDGEABLE",
                i + 1,
                rec.persona,
                rec.turn
            ),
        }
        rec.verdict = verdict;
        // Advance the transcript with the audited reply for the next turn.
        transcript.push(TranscriptTurn::witness(rec.response.clone()));
        out.push(rec);
    }

    eprintln!("inner-chaos --rejudge: {judged} judged, {failed} unjudgeable of {total}");

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let report = report::build_report(&stamp, &out);
    report::print_text(&report);

    // Persist the re-judged journal alongside the report so the verdicts
    // are inspectable turn-by-turn.
    let rejudged_journal = journal_path.with_file_name(format!(
        "{}.rejudged-{}.jsonl",
        journal_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("journal"),
        judge_model.replace(['/', ' '], "_")
    ));
    if let Ok(mut f) = std::fs::File::create(&rejudged_journal) {
        use std::io::Write;
        for r in &out {
            if let Ok(line) = serde_json::to_string(r) {
                let _ = writeln!(f, "{line}");
            }
        }
        eprintln!(
            "inner-chaos --rejudge: re-judged journal at {}",
            rejudged_journal.display()
        );
    }

    if let Some(o) = flags
        .value("output")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
    {
        if let Err(e) = report::write_json(&PathBuf::from(&o), &report) {
            eprintln!("inner-chaos --rejudge: write report {o}: {e}");
        } else {
            eprintln!("inner-chaos --rejudge: report JSON at {o}");
        }
    }
    0
}
