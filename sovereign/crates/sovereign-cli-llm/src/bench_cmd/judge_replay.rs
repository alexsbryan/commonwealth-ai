// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench judge-replay …` — replay recorded judge inputs through the
//! PRODUCTION judge registers, offline.
//!
//! The problem this solves: every judge change to date was priced with live
//! 30-40 min adversarial arms to collect a handful of verdict flips, and the
//! per-claim joint register (`claim_violation_joint`, 10-16 calls per
//! production long-form turn) has no calibrated counterpart — tau=0.9
//! transfers from the singular chunk-judge register BY ANALOGY only (note
//! d474ac24). This verb replays a pinned case set — recorded
//! (claim, evidence window) pairs from the `SOVEREIGN_GATE_AUDIT_FORENSICS`
//! ledgers and the adversarial harvest transcripts — through whatever judge
//! configuration THIS BUILD compiled in, so a candidate register change is
//! priced in minutes against labeled specimens before any live arm.
//!
//! **A candidate configuration is a build, not a flag.** The harness has no
//! prompt knobs: it calls the same `sovereign-core` replay seams production
//! exports (`replay_claim_violation_joint` — pure delegation to the one
//! renderer, pinned by `replay_render_matches_the_joint_register`). To score
//! land C, check out land C and run the same verb on the same cases; the
//! output header fingerprints the register (system-turn hash, renderer bytes
//! per case) so two artifacts can never be silently from one build.
//!
//! Four registers, matching `GateCallMechanism`:
//!   `per_claim_judge` — the joint forced-choice (vp in [0,1], tau applies)
//!   `chunk_judge`     — the singular calibrated register (support in [0,1])
//!   `specifics_scan`  — generative; output is the flagged-item list
//!   `batched_support` — one prefill, N text A/B verdicts (order
//!                       `audit-economy` D1: recalibrated against the
//!                       per-claim register before any flip)
//!
//! Scoring/curves live in `bench/chaos_monkey/judge_replay_report.py`; case
//! extraction in `bench/chaos_monkey/judge_replay_cases.py`. This verb only
//! (1) renders, (2) calls the daemon, (3) writes verdicts — so the mechanical
//! facet is checkable with `--render-only` (no daemon, bit-stable).

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{
    chunk_judge_prompt, claim_chunk_support, replay_claim_violation_joint,
    replay_claims_support_batched, replay_judge_system_turn, replay_render_batched_claims_prompt,
    replay_render_claim_prompt, replay_scan_unsupported_specifics, CHUNK_JUDGE_SYSTEM,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;

use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Wide enough for the recorded windows: per-claim prompts run ~29-34k chars
/// (~8-9k tokens) and scan prompts up to ~45k chars. The daemon clamps to the
/// resident model's real window; this is the client-side declaration.
const PROVIDER_CTX: u32 = 32_768;

const HELP: Help = Help {
    command: "svrn bench judge-replay",
    summary: "Replay recorded judge inputs through THIS build's production judge registers.",
    sections: &[
        HelpSection::Usage(
            "svrn bench judge-replay --cases <cases.jsonl> [--out <verdicts.jsonl>] \
             [--base-url http://localhost:9741] [--model primary] [--register <name>] \
             [--filter <substr>] [--repeat N] [--render-only]",
        ),
        HelpSection::Notes(
            "Cases come from bench/chaos_monkey/judge_replay_cases.py (pinned set: \
             judge_replay_cases_v1.jsonl). Registers: per_claim_judge, chunk_judge, \
             specifics_scan, batched_support (case fields: shared_chunks + claims[]; \
             verdicts land in `batched`, aligned to claim order, null = parse gap). \
             --render-only renders prompts and fingerprints without \
             model calls (no daemon needed; bit-stable across repeats by construction). \
             --repeat N re-scores each case N times and reports verdict spread — one \
             run is not a measurement. Model calls go to the LOCAL daemon at \
             --base-url; the output header records model, base-url, and the system-turn \
             fingerprint of the register this build compiled in.",
        ),
    ],
};

/// FNV-1a 64 over bytes, hex. Deterministic across builds and platforms —
/// used to fingerprint prompt bytes and the system turn so two artifacts
/// from different builds are comparable without shipping the full prompt.
fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn strings(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn cmd_judge_replay(rest: &[String]) -> i32 {
    if help::wants_help(rest) {
        help::print(&HELP);
        return 0;
    }
    let mut cases_path: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut base_url = "http://localhost:9741".to_string();
    let mut model = "primary".to_string();
    let mut register_filter: Option<String> = None;
    let mut case_filter: Option<String> = None;
    let mut repeat: usize = 1;
    let mut render_only = false;

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
            "--cases" => cases_path = Some(PathBuf::from(val!("--cases"))),
            "--out" => out = Some(PathBuf::from(val!("--out"))),
            "--base-url" => base_url = val!("--base-url"),
            "--model" => model = val!("--model"),
            "--register" => register_filter = Some(val!("--register")),
            "--filter" => case_filter = Some(val!("--filter")),
            "--repeat" => match val!("--repeat").parse() {
                Ok(n) if n >= 1 => repeat = n,
                _ => {
                    eprintln!("error: --repeat must be a positive integer");
                    return 2;
                }
            },
            "--render-only" => render_only = true,
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(cases_path) = cases_path else {
        eprintln!("error: --cases is required");
        help::print(&HELP);
        return 2;
    };
    let text = match std::fs::read_to_string(&cases_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: could not read {cases_path:?}: {e}");
            return 1;
        }
    };

    let provider: Option<Arc<dyn InferenceProvider>> = if render_only {
        None
    } else {
        let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
        Some(Arc::new(RemoteApiProvider::new(
            &v1,
            None,
            &model,
            PROVIDER_CTX,
        )))
    };
    let posture = ShardingPrivacy::LocalOnly;

    // Header row: the register fingerprint of THIS build. Two verdict files
    // whose `system_turn_fnv` differ were produced by different judge
    // configurations — that comparison is the harness's whole purpose, so it
    // is stamped on the artifact rather than remembered.
    let system_turn = replay_judge_system_turn();
    let header = serde_json::json!({
        "kind": "header",
        "ts": chrono::Utc::now().to_rfc3339(),
        "cases": cases_path.display().to_string(),
        "engine": if render_only { "render-only (no model calls)" } else { "local daemon" },
        "base_url": if render_only { serde_json::Value::Null } else { base_url.clone().into() },
        "model": if render_only { serde_json::Value::Null } else { model.clone().into() },
        "repeat": repeat,
        "system_turn": system_turn,
        "system_turn_fnv": fnv1a64_hex(system_turn.as_bytes()),
        "chunk_judge_system_fnv": fnv1a64_hex(CHUNK_JUDGE_SYSTEM.as_bytes()),
    });

    let mut rows: Vec<serde_json::Value> = vec![header];
    let mut n_done = 0usize;
    let mut n_judge_failures = 0usize;

    for (li, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let case: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  [{li}] skipping unparseable case line: {e}");
                continue;
            }
        };
        let case_id = case
            .get("case_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let register = case
            .get("register")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        if let Some(f) = &register_filter {
            if &register != f {
                continue;
            }
        }
        if let Some(f) = &case_filter {
            if !case_id.contains(f.as_str()) {
                continue;
            }
        }

        // Render + fingerprint (the mechanical facet, always computed).
        let (prompt_fnv, prompt_chars, stable_prefix_len): (
            Option<String>,
            Option<usize>,
            Option<usize>,
        ) = match register.as_str() {
            "per_claim_judge" => {
                let shared = strings(case.get("shared_chunks"));
                let appended = strings(case.get("appended_chunks"));
                let claim = case.get("claim").and_then(|v| v.as_str()).unwrap_or("");
                let (prompt, boundary) = replay_render_claim_prompt(&shared, &appended, claim);
                (
                    Some(fnv1a64_hex(prompt.as_bytes())),
                    Some(prompt.chars().count()),
                    boundary,
                )
            }
            "chunk_judge" => {
                let passage = case.get("passage").and_then(|v| v.as_str()).unwrap_or("");
                let claim = case.get("claim").and_then(|v| v.as_str()).unwrap_or("");
                let prompt = chunk_judge_prompt(passage, claim);
                (
                    Some(fnv1a64_hex(prompt.as_bytes())),
                    Some(prompt.chars().count()),
                    None,
                )
            }
            "batched_support" => {
                let shared = strings(case.get("shared_chunks"));
                let claims = strings(case.get("claims"));
                let (prompt, boundary) = replay_render_batched_claims_prompt(&shared, &claims);
                (
                    Some(fnv1a64_hex(prompt.as_bytes())),
                    Some(prompt.chars().count()),
                    boundary,
                )
            }
            // The scan renders inside the production function; its inputs are
            // fingerprinted instead. Absence reported, not defaulted.
            "specifics_scan" => (None, None, None),
            other => {
                eprintln!(
                    "  [{li}] {case_id}: unknown register `{other}` — skipped, not defaulted"
                );
                continue;
            }
        };

        // Score, --repeat times. `verdicts` collects every repeat so the
        // artifact shows spread rather than a silently-averaged number.
        let mut vps: Vec<Option<f64>> = Vec::new();
        let mut scans: Vec<Option<Vec<String>>> = Vec::new();
        let mut batched: Vec<Vec<Option<bool>>> = Vec::new();
        if let Some(inference) = &provider {
            for _ in 0..repeat {
                match register.as_str() {
                    "per_claim_judge" => {
                        let shared = strings(case.get("shared_chunks"));
                        let appended = strings(case.get("appended_chunks"));
                        let claim = case.get("claim").and_then(|v| v.as_str()).unwrap_or("");
                        let mut chunks = shared.clone();
                        chunks.extend(appended);
                        let vp = replay_claim_violation_joint(
                            inference,
                            claim,
                            &chunks,
                            shared.len(),
                            posture,
                        )
                        .await;
                        if vp.is_none() {
                            n_judge_failures += 1;
                        }
                        vps.push(vp);
                    }
                    "chunk_judge" => {
                        let passage = case.get("passage").and_then(|v| v.as_str()).unwrap_or("");
                        let claim = case.get("claim").and_then(|v| v.as_str()).unwrap_or("");
                        let support = claim_chunk_support(inference, passage, claim, posture).await;
                        if support.is_none() {
                            n_judge_failures += 1;
                        }
                        // Stored as recorded: SUPPORT for this register (the
                        // bench critic's convention), never silently converted.
                        vps.push(support);
                    }
                    "batched_support" => {
                        let shared = strings(case.get("shared_chunks"));
                        let claims = strings(case.get("claims"));
                        let verdicts =
                            replay_claims_support_batched(inference, &claims, &shared, posture)
                                .await;
                        // Total failure (the register's own fallback shape) is
                        // an all-None vec — count it as a judge failure so an
                        // all-dead run exits 4 rather than green (ARCH §18.2).
                        if !verdicts.is_empty() && verdicts.iter().all(Option::is_none) {
                            n_judge_failures += 1;
                        }
                        batched.push(verdicts);
                    }
                    "specifics_scan" => {
                        let question = case.get("question").and_then(|v| v.as_str()).unwrap_or("");
                        let answer = case.get("answer").and_then(|v| v.as_str()).unwrap_or("");
                        // Cases store the two evidence tiers SPLIT — this
                        // build's scan (D3 candidate A) takes them separately:
                        // leaf window = the family prefix, summaries appended
                        // after the boundary, exactly as `gate_longform` calls
                        // it.
                        let leaves = strings(case.get("leaf_chunks"));
                        let summaries = strings(case.get("summary_chunks"));
                        let max_items = case
                            .get("max_items")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(10) as usize;
                        let items = replay_scan_unsupported_specifics(
                            inference, question, answer, &leaves, &summaries, max_items, posture,
                        )
                        .await;
                        if items.is_none() {
                            n_judge_failures += 1;
                        }
                        scans.push(items);
                    }
                    _ => unreachable!("filtered above"),
                }
            }
        }

        let row = serde_json::json!({
            "kind": "verdict",
            "case_id": case_id,
            "register": register,
            "label": case.get("label"),
            "recorded": case.get("recorded"),
            "prompt_fnv": prompt_fnv,
            "prompt_chars": prompt_chars,
            "stable_prefix_len": stable_prefix_len,
            // Every repeat, verbatim. vp for the forced-choice registers
            // (chunk_judge rows carry SUPPORT — see above); item lists for
            // the scan; per-claim bool arrays for the batched register
            // (null = parse gap for that row — production falls back to the
            // calibrated per-claim judge there). null = judge failure
            // (could-not-judge), never 0.
            "vp": vps,
            "scan_items": scans,
            "batched": batched,
        });
        eprintln!(
            "  [{:>3}] {:<28} {:<16} {}",
            n_done + 1,
            case_id.chars().take(28).collect::<String>(),
            register,
            if render_only {
                format!("rendered fnv={}", prompt_fnv.as_deref().unwrap_or("-"))
            } else if !vps.is_empty() {
                format!(
                    "vp={}",
                    vps.iter()
                        .map(|v| v.map_or("null".into(), |x| format!("{x:.4}")))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else if !batched.is_empty() {
                format!(
                    "batched={}",
                    batched
                        .iter()
                        .map(|vs| {
                            vs.iter()
                                .map(|v| match v {
                                    Some(true) => "A",
                                    Some(false) => "B",
                                    None => "-",
                                })
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else {
                format!(
                    "items={}",
                    scans
                        .iter()
                        .map(|s| s.as_ref().map_or("null".into(), |x| x.len().to_string()))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        );
        rows.push(row);
        n_done += 1;
    }

    let out = out.unwrap_or_else(|| {
        PathBuf::from(if render_only {
            "target/judge-replay/render.jsonl"
        } else {
            "target/judge-replay/verdicts.jsonl"
        })
    });
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(e) = std::fs::write(&out, body + "\n") {
        eprintln!("error: could not write {out:?}: {e}");
        return 1;
    }
    eprintln!(
        "[judge-replay] {n_done} case(s) -> {out:?}; judge failures: {n_judge_failures}; \
         engine: {}",
        if render_only {
            "render-only".to_string()
        } else {
            format!("local daemon {base_url} model={model}")
        }
    );
    // A run where every scoring call failed verified nothing (ARCH §18.2):
    // report never-ran rather than a green exit.
    if !render_only && n_done > 0 && n_judge_failures >= n_done * repeat {
        eprintln!("[judge-replay] EVERY call failed — daemon down or model absent. Exit 4.");
        return 4;
    }
    0
}
