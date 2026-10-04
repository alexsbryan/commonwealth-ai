// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn eval inner-chaos --replay-witness <journal>` — re-run the SUT
//! (witness) over the RECORDED user turns of an existing journal,
//! semi-deterministically (temperature 0 by default), producing a fresh
//! transcript journal WITHOUT judging. Pair with `--rejudge` to score
//! the result with the 122B and A/B it against the source run.
//!
//! Why a dedicated replay: to A/B a witness-prompt change against the
//! exact adversarial pressure a prior run captured, we must feed the
//! SAME user turns — not new brain-generated ones — through the
//! witness. This is option-a "branch replay": each thread is replayed
//! in order through `runtime.handle_message`, so the evolving context
//! stays self-consistent while the user pressure is pinned to what the
//! journal recorded. The witness's OWN replies may diverge from the
//! original (that is the whole point — we changed the prompt), but the
//! user turns do not.
//!
//! `--only-breach-threads` limits replay to conversations that had at
//! least one breach in the (rejudged) input journal — the cheap A/B
//! surface for "did the fix turn the specific questions?". Seeding
//! mirrors the core runner exactly (General pool, `None` skill), so the
//! witness sees an EMPTY recall pool just as it did in the soak.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::journal::TurnRecord;
use super::personas::{load_memories, resolve_bench_dir};
use super::runner::{build_thread_session, seed_memories};
use sovereign_cli_shared::args::Parsed;

/// Entry point for `--replay-witness <journal>`.
pub async fn run(flags: &Parsed, journal_path: PathBuf, bench_dir: Option<PathBuf>) -> i32 {
    let daemon_base = flags
        .value("daemon")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let chat_model = flags
        .value("chat-model")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let only_breach = flags.has("only-breach-threads");
    // Semi-deterministic by default: temperature 0. Still not bit-exact
    // on an MoE, but it removes the 0.9 sampling spread the live brain
    // loop runs at, so a category flip is attributable to the prompt.
    let temperature = flags
        .value("temperature")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .and_then(|v| v.parse::<f32>().ok())
        .or(Some(0.0));

    let bench_dir = match resolve_bench_dir(bench_dir.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("inner-chaos --replay-witness: {e}");
            return 1;
        }
    };
    let memories: BTreeMap<_, _> = match load_memories(&bench_dir.join("memories.toml")) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("inner-chaos --replay-witness: {e}");
            return 1;
        }
    };

    let skills_dir = match crate::voice_eval::runner::resolve_skills_dir(
        flags
            .value("skills-dir")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .map(PathBuf::from)
            .as_ref(),
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("inner-chaos --replay-witness: resolve skills dir: {e}");
            return 1;
        }
    };

    // Read + filter the journal. Keep turns with a real recorded user
    // message and no run-side error — the recorded RESPONSE is only
    // consulted for breach-thread selection, never replayed.
    let raw = match std::fs::read_to_string(&journal_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "inner-chaos --replay-witness: cannot read journal {}: {e}",
                journal_path.display()
            );
            return 1;
        }
    };
    let mut records: Vec<TurnRecord> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TurnRecord>(l).ok())
        .filter(|r| r.error.is_none() && !r.user.is_empty())
        .collect();
    if records.is_empty() {
        eprintln!(
            "inner-chaos --replay-witness: no replayable turns in {} (need records with a user turn)",
            journal_path.display()
        );
        return 1;
    }
    records.sort_by(|a, b| a.conv_id.cmp(&b.conv_id).then(a.turn.cmp(&b.turn)));

    // Which conversations to replay. In `--only-breach-threads` mode,
    // keep only convs with at least one breach verdict in the source
    // journal (a red line, i.e. NOT is_safe) — requires a rejudged
    // input (verdicts present). Otherwise replay every conv.
    let mut breach_convs: std::collections::BTreeSet<String> = Default::default();
    if only_breach {
        for r in &records {
            if r.verdict.as_ref().map(|v| !v.is_safe()).unwrap_or(false) {
                breach_convs.insert(r.conv_id.clone());
            }
        }
        if breach_convs.is_empty() {
            eprintln!(
                "inner-chaos --replay-witness: --only-breach-threads set but no breach verdicts in \
                 {} — did you pass the REJUDGED journal? Aborting rather than replaying nothing.",
                journal_path.display()
            );
            return 1;
        }
        records.retain(|r| breach_convs.contains(&r.conv_id));
    }

    // Group by conversation (order preserved by the sort above).
    let mut convs: Vec<(String, Vec<TurnRecord>)> = Vec::new();
    for r in records {
        match convs.last_mut() {
            Some((cid, turns)) if *cid == r.conv_id => turns.push(r),
            _ => convs.push((r.conv_id.clone(), vec![r])),
        }
    }

    let total_turns: usize = convs.iter().map(|(_, t)| t.len()).sum();
    eprintln!(
        "inner-chaos --replay-witness: {} conv(s), {total_turns} turn(s) from {}\n  \
         chat-model: {}\n  temperature: {:?}\n  only-breach-threads: {only_breach}{}",
        convs.len(),
        journal_path.display(),
        chat_model.as_deref().unwrap_or("(daemon primary)"),
        temperature,
        if only_breach {
            format!("\n  breach conv(s): {}", breach_convs.len())
        } else {
            String::new()
        },
    );

    let mut out: Vec<TurnRecord> = Vec::with_capacity(total_turns);
    let mut done = 0usize;

    for (conv_id, turns) in convs {
        // One fresh witness session per conversation — the tempdir MUST
        // outlive the session (dropping it yanks the SQLite db).
        let (session, _tmp) = match build_thread_session(
            &skills_dir,
            daemon_base.as_deref(),
            chat_model.as_deref(),
            temperature,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  [{conv_id}] session setup failed: {e} — skipping conv");
                continue;
            }
        };
        // Mirror the core runner: seed into the General pool (`None`),
        // which the inner-work witness scope cannot see → empty recall.
        if let Err(e) = seed_memories(session.store.as_ref(), &memories, None).await {
            eprintln!("  [{conv_id}] memory-seed failed: {e} — skipping conv");
            continue;
        }

        for rec in turns {
            done += 1;
            let started = Instant::now();
            let response = match session.runtime.handle_message(&rec.user, &conv_id).await {
                Ok(resp) => sovereign_core::title::strip_thinking_response(&resp.message.content),
                Err(e) => {
                    eprintln!(
                        "  [{done}/{total_turns}] {} t{}: runtime failed: {e}",
                        rec.persona, rec.turn
                    );
                    // Record the error turn and move on within the conv;
                    // later turns still exercise the accumulated state.
                    let mut er = new_record(&rec, &conv_id, String::new());
                    er.error = Some(format!("runtime turn failed: {e}"));
                    out.push(er);
                    continue;
                }
            };
            let runtime_ms = started.elapsed().as_millis() as u64;
            eprintln!(
                "  [{done}/{total_turns}] {} t{}: {}ms  {}",
                rec.persona,
                rec.turn,
                runtime_ms,
                one_line(&response, 90),
            );
            let mut nr = new_record(&rec, &conv_id, response);
            nr.runtime_ms = runtime_ms;
            out.push(nr);
        }
    }

    // Persist the replayed journal (no verdicts — rejudge it next).
    let stem = journal_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("journal");
    let out_path = flags
        .value("output")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .map(PathBuf::from)
        .unwrap_or_else(|| journal_path.with_file_name(format!("{stem}.replayed.jsonl")));
    match std::fs::File::create(&out_path) {
        Ok(mut f) => {
            use std::io::Write;
            for r in &out {
                if let Ok(line) = serde_json::to_string(r) {
                    let _ = writeln!(f, "{line}");
                }
            }
            eprintln!(
                "inner-chaos --replay-witness: {} turn(s) → {}\n  now rejudge:\n  \
                 svrn eval inner-chaos --rejudge {} --judge-model <122B-id>",
                out.len(),
                out_path.display(),
                out_path.display(),
            );
        }
        Err(e) => {
            eprintln!(
                "inner-chaos --replay-witness: cannot write {}: {e}",
                out_path.display()
            );
            return 1;
        }
    }
    0
}

/// Build a fresh `TurnRecord` for a replayed turn, carrying the source
/// turn's identity (thread/turn/persona/user) and the NEW response.
fn new_record(src: &TurnRecord, conv_id: &str, response: String) -> TurnRecord {
    TurnRecord {
        ts_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        thread: src.thread,
        turn: src.turn,
        persona: src.persona.clone(),
        conv_id: conv_id.to_string(),
        user: src.user.clone(),
        response,
        verdict: None,
        judge_failed: false,
        error: None,
        brain_ms: 0,
        runtime_ms: 0,
        judge_ms: None,
    }
}

/// Collapse a reply to a single truncated line for progress output.
fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let head: String = flat.chars().take(max).collect();
        format!("{head}…")
    } else {
        flat
    }
}
