// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthesis-side iteration harnesses for the recall extension.
//!
//! The 2026-07-09 A/B pinned the recall bottleneck to SYNTHESIS: the
//! right memory reaches the witness's rendered window in 15/16 turns,
//! yet faithful recall stays at 12.5% — the witness defers past the
//! candidate or welds record-meta fabrications, and 100% of confabs in
//! BOTH arms happened with the right memory rendered. Fixing that
//! needs tight, isolated iterations, so this module provides two
//! surfaces cheaper than the full `--recall` thread bench:
//!
//! - **`--calibrate-mem-grounding`** — a deterministic gate for the
//!   grounding VERIFIER (`memory_grounding.rs`): hand-labeled
//!   (reply, entries) cases from real bench receipts, both polarities.
//!   Rubric/rendering changes must pass before they may gate replies.
//!   Seconds per case, no witness turns.
//! - **`--recall-synth`** — a single-turn synthesis probe: seed once,
//!   build the atlas once, then send each plant's verbatim callback as
//!   a one-turn conversation through the REAL runtime (grounding gate
//!   included) and score the reply with the recall judge. `--threads N`
//!   = samples per plant (default 2). Isolates synthesis: retrieval is
//!   pinned good by `SOVEREIGN_MEM_PICK=1` (set it when probing).

use std::collections::BTreeMap;
use std::time::Instant;

use serde::Deserialize;
use sovereign_core::runtime::memory_grounding::verify_recall_grounding;
use sovereign_core::traits::MemoryScope;
use sovereign_core::types::Memory;

use super::personas::resolve_bench_dir;
use super::recall::{
    build_seed_set, load_recall_fixture, parse_recall_verdict, recall_judge_request,
    RecallCategory, RecallRunOptions,
};
use super::runner::{build_thread_session, pinned_or_shared, seed_memories, WITNESS_SKILL};
use super::transcript::TranscriptTurn;

// ── Verifier calibration ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GroundingCase {
    id: String,
    user: String,
    reply: String,
    gold_grounded: bool,
    /// Expected `denied_match`: absent = don't check; 0 = must be
    /// None (an honest denial); N≥1 = must point at entry N (a false
    /// denial of an in-view entry).
    #[serde(default)]
    gold_denied_match: Option<usize>,
    #[serde(default)]
    note: String,
    entries: Vec<GroundingEntry>,
}

#[derive(Debug, Deserialize)]
struct GroundingEntry {
    content: String,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GroundingBank {
    #[serde(rename = "case", default)]
    cases: Vec<GroundingCase>,
}

fn entry_to_memory(e: &GroundingEntry, idx: usize) -> Memory {
    let created_at = e
        .date
        .as_deref()
        .and_then(|d| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .ok()
                .and_then(|nd| nd.and_hms_opt(12, 0, 0))
                .map(|dt| dt.and_utc().timestamp())
        })
        .unwrap_or(0);
    Memory {
        id: format!("cal-{idx}"),
        content: e.content.clone(),
        source: "mem_grounding_calibration".into(),
        confidence: 0.9,
        created_at,
        last_used: created_at,
        // The date prefix renders only for entries with a source
        // conversation — mirror the production rows the witness sees.
        source_conversation_id: e.date.as_ref().map(|_| "cal-conv".to_string()),
        ..Default::default()
    }
}

/// Exit 1 when a floor fails. Sensitivity = flagging gold-ungrounded
/// replies (a missed weld silently gates nothing); specificity =
/// passing gold-grounded ones (a verifier that flags correct recall
/// pushes the witness into deferral — the exact failure this loop is
/// fixing).
pub async fn run_mem_grounding_calibration(opts: &RecallRunOptions) -> Result<(), String> {
    let bench_dir = resolve_bench_dir(opts.bench_dir.as_ref())?;
    let bank_path = bench_dir.join("mem_grounding_calibration.toml");
    let bank: GroundingBank = toml::from_str(
        &std::fs::read_to_string(&bank_path)
            .map_err(|e| format!("read {}: {e}", bank_path.display()))?,
    )
    .map_err(|e| format!("parse {}: {e}", bank_path.display()))?;
    if bank.cases.is_empty() {
        return Err("mem-grounding bank is empty".into());
    }

    let skills_dir = crate::voice_eval::runner::resolve_skills_dir(opts.skills_dir.as_ref())
        .map_err(|e| e.to_string())?;
    let (session, _tmp) = build_thread_session(
        &skills_dir,
        opts.daemon_base.as_deref(),
        opts.chat_model.as_deref(),
        opts.temperature,
    )
    .await?;
    let verifier_inference = pinned_or_shared(
        &session,
        opts.judge_model.as_ref(),
        opts.chat_model.as_ref(),
    );

    println!(
        "\ninner-chaos MEM-GROUNDING verifier calibration — {} cases",
        bank.cases.len()
    );
    let (mut tp, mut fn_, mut tn, mut fp) = (0usize, 0usize, 0usize, 0usize);
    for case in &bank.cases {
        let memories: Vec<Memory> = case
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| entry_to_memory(e, i))
            .collect();
        let v = verify_recall_grounding(
            verifier_inference.as_ref(),
            &case.user,
            &case.reply,
            &memories,
        )
        .await;
        let denial_ok = match case.gold_denied_match {
            None => true,
            Some(0) => v.denied_match.is_none(),
            Some(n) => v.denied_match == Some(n),
        };
        let ok = v.grounded == case.gold_grounded && denial_ok;
        match (case.gold_grounded, v.grounded) {
            (false, false) => tp += 1,
            (false, true) => fn_ += 1,
            (true, true) => tn += 1,
            (true, false) => fp += 1,
        }
        if !denial_ok {
            // A denial-signal miss is a hard gate failure regardless of
            // the grounded/ungrounded tallies: a missed false-denial
            // leaves the trust-breaker ungated, and a spurious one
            // suppresses honest deferral.
            fp += 1;
            tn = tn.saturating_sub(1);
        }
        println!(
            "  {} gold={} judged={} denied_match={:?} {}{}",
            case.id,
            if case.gold_grounded {
                "grounded"
            } else {
                "UNGROUNDED"
            },
            if v.grounded { "grounded" } else { "UNGROUNDED" },
            v.denied_match,
            if ok { "OK" } else { "MISMATCH" },
            if ok {
                String::new()
            } else {
                format!(
                    "\n      unsupported=\"{}\"\n      note: {}",
                    v.unsupported, case.note
                )
            }
        );
    }
    let sens = tp as f64 / (tp + fn_).max(1) as f64;
    let spec = tn as f64 / (tn + fp).max(1) as f64;
    const SENS_FLOOR: f64 = 0.9;
    const SPEC_FLOOR: f64 = 0.9;
    println!("\n  sensitivity (flags real welds): {sens:.2} (floor {SENS_FLOOR})");
    println!("  specificity (passes honest replies): {spec:.2} (floor {SPEC_FLOOR})");
    if sens >= SENS_FLOOR && spec >= SPEC_FLOOR {
        println!("  verdict: PASS — this verifier may gate witness replies");
        Ok(())
    } else {
        Err("mem-grounding verifier failed a calibration floor".into())
    }
}

// ── Single-turn synthesis probe ───────────────────────────────────

/// One conversation per (plant, sample): the verbatim oblique callback
/// through `runtime.handle_message` (full production path: recall +
/// pick if enabled + grounding gate), scored by the recall judge.
pub async fn run_recall_synth(opts: &RecallRunOptions) -> Result<(), String> {
    let bench_dir = resolve_bench_dir(opts.bench_dir.as_ref())?;
    let fixture_path = opts
        .fixture_path
        .clone()
        .unwrap_or_else(|| bench_dir.join("recall_fixture.toml"));
    let fixture = load_recall_fixture(&fixture_path)?;
    let mut plants = fixture.plants.clone();
    if let Some(filter) = opts.plant_filter.as_deref() {
        plants.retain(|p| p.id == filter);
        if plants.is_empty() {
            return Err(format!("--plant `{filter}` matched no plant"));
        }
    }
    let samples = opts.max_threads.unwrap_or(2).max(1);

    let seed_set = build_seed_set(&fixture);
    let skills_dir = crate::voice_eval::runner::resolve_skills_dir(opts.skills_dir.as_ref())
        .map_err(|e| e.to_string())?;
    let (session, _tmp) = build_thread_session(
        &skills_dir,
        opts.daemon_base.as_deref(),
        opts.chat_model.as_deref(),
        opts.temperature,
    )
    .await?;
    seed_memories(session.store.as_ref(), &seed_set, Some(WITNESS_SKILL))
        .await
        .map_err(|e| format!("seed failed: {e}"))?;
    let scope = MemoryScope::Scoped(WITNESS_SKILL.to_string());
    let t = Instant::now();
    match sovereign_tools::mem_atlas::build_memory_atlas(
        &session.inference,
        session.store.as_ref(),
        &scope,
    )
    .await
    {
        Ok(n) => println!("memory atlas: {n} nodes in {}s", t.elapsed().as_secs()),
        Err(e) => println!("memory atlas build failed ({e}) — flat T1"),
    }
    let judge_inference = pinned_or_shared(
        &session,
        opts.judge_model.as_ref(),
        opts.chat_model.as_ref(),
    );
    let stamp = super::runner::unix_seconds();

    println!(
        "\ninner-chaos RECALL-SYNTH — {} plants × {samples} samples, single-turn, \
         SOVEREIGN_MEM_PICK={}",
        plants.len(),
        std::env::var("SOVEREIGN_MEM_PICK").unwrap_or_else(|_| "unset".into())
    );
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut judged = 0usize;
    for plant in &plants {
        let other_entries: Vec<String> = seed_set
            .iter()
            .filter(|(key, _)| {
                (key.starts_with("plant-") || key.starts_with("distractor-"))
                    && key.as_str() != format!("plant-{}", plant.id).as_str()
            })
            .map(|(_, seed)| seed.content.clone())
            .collect();
        let mut cells: Vec<String> = Vec::new();
        for s in 0..samples {
            let conv_id = format!("synth-{stamp}-{}-{s}", plant.id);
            let turn_started = Instant::now();
            let reply = match session
                .runtime
                .handle_message(&plant.oblique_callback, &conv_id)
                .await
            {
                Ok(r) => r.message.content,
                Err(e) => {
                    cells.push(format!("ERR({e})"));
                    continue;
                }
            };
            let turn_ms = turn_started.elapsed().as_millis() as u64;
            // The witness's in-view window this turn (metric v3) —
            // same env-gated production recall the runtime used.
            let in_view_contents: Vec<String> =
                sovereign_core::memory::recall_relevant_memories_embed_reranked(
                    session.inference.as_ref(),
                    session.store.as_ref(),
                    &scope,
                    &plant.oblique_callback,
                    5,
                    None,
                )
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|m| m.content)
                .collect();
            let transcript = vec![TranscriptTurn::user(plant.oblique_callback.clone())];
            let req = recall_judge_request(
                &plant.content,
                &other_entries,
                &in_view_contents,
                &transcript,
                &reply,
            );
            let verdict = match judge_inference.complete(&req).await {
                Ok(resp) => parse_recall_verdict(&resp.text),
                Err(_) => None,
            };
            match verdict {
                Some(v) => {
                    judged += 1;
                    *counts.entry(v.category.as_str()).or_insert(0) += 1;
                    cells.push(format!("{} ({}s)", v.category.as_str(), turn_ms / 1000));
                    // Receipts for everything that didn't land — the
                    // iteration surface. One line of why + the reply.
                    if !matches!(v.category, RecallCategory::FaithfulRecall) {
                        println!(
                            "    [{} #{s}] {}\n      why: {}\n      reply: {}",
                            plant.id,
                            v.category.as_str(),
                            v.why.chars().take(200).collect::<String>(),
                            reply
                                .replace('\n', " ")
                                .chars()
                                .take(260)
                                .collect::<String>()
                        );
                    }
                }
                None => cells.push("JUDGE-FAIL".into()),
            }
        }
        println!("  {:<28} {}", plant.id, cells.join(" | "));
    }
    println!("\n  totals over {judged} judged single turns:");
    for (cat, n) in &counts {
        println!(
            "    {cat:<16} {n:>2}/{judged} = {:.0}%",
            *n as f64 * 100.0 / judged.max(1) as f64
        );
    }
    let confab = counts
        .get(RecallCategory::Confabulated.as_str())
        .copied()
        .unwrap_or(0);
    let faithful = counts
        .get(RecallCategory::FaithfulRecall.as_str())
        .copied()
        .unwrap_or(0)
        + counts
            .get(RecallCategory::PartialRecall.as_str())
            .copied()
            .unwrap_or(0);
    println!("  headline: landed {faithful}/{judged}, confab {confab}/{judged}");
    Ok(())
}
