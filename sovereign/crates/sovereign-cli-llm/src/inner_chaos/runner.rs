// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live loop: per thread, pick a persona → seed the resident
//! memory fixtures into a fresh state store → N turns of
//! {brain proposes → `Runtime::handle_message` → judge} → journal.
//!
//! Rust sibling of `voice_eval::runner` (single-turn) extended to
//! multi-turn: each thread holds one `conv_id`, and the runtime
//! threads + rehydrates history across `handle_message` calls. Each
//! thread gets its own `ChatSession` over a tempdir data dir (user
//! state is never touched; memories never leak across threads) with
//! ONLY the `inner-work` skill activated, so the relational witness
//! register is the path under test.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::Memory;
use sovereign_core::SkillRegistry;
use sovereign_inference::remote::RemoteApiProvider;

use crate::chat_cmd::bootstrap::{build_session_with_skills, ChatSession};
use crate::chat_cmd::config::default_globals_for_voice_eval;
use crate::voice_eval::scenarios::{parse_date, SeedMemory};

use super::brain::{brain_request, parse_brain_message};
use super::journal::{Journal, TurnRecord};
use super::judge::{parse_witness_verdict, witness_judge_request};
use super::personas::{load_memories, load_personas, resolve_bench_dir, Persona};
use super::report::{build_report, ChaosReport};
use super::transcript::TranscriptTurn;

/// The witness skill under test. The whole harness is pointed at
/// the Reflect rail; other skills are out of scope by design.
pub(super) const WITNESS_SKILL: &str = "inner-work";

pub struct RunOptions {
    /// Wall-clock budget. Checked before each new thread; the run
    /// always executes at least one thread.
    pub minutes: Option<f64>,
    /// Hard cap on thread count (composes with `minutes`).
    pub max_threads: Option<usize>,
    /// Run only this persona id.
    pub persona_filter: Option<String>,
    /// Fixture directory override (default: walk up to
    /// `bench/inner_work`).
    pub bench_dir: Option<PathBuf>,
    /// Live journal path (wiped on start).
    pub journal_path: PathBuf,
    /// Optional extra JSON report path.
    pub output: Option<PathBuf>,
    /// Judge each turn (default true). With false the run only
    /// collects transcripts — no safety number is computed.
    pub judge: bool,
    pub daemon_base: Option<String>,
    /// Pin the SUT witness model.
    pub chat_model: Option<String>,
    /// Pin the adversarial brain to a different model. Default:
    /// the session's chat model (the 35B plays both roles, like the
    /// knowledge harness).
    pub brain_model: Option<String>,
    /// Pin the judge to a stable rater across SUT A/Bs.
    pub judge_model: Option<String>,
    pub skills_dir: Option<PathBuf>,
    /// SUT temperature override. Default `None` = the runtime's
    /// product default — the honest-baseline choice; the harness
    /// measures the product, not a determinised variant.
    pub temperature: Option<f32>,
}

pub async fn run(opts: &RunOptions) -> Result<ChaosReport, String> {
    let bench_dir = resolve_bench_dir(opts.bench_dir.as_ref())?;
    let mut personas = load_personas(&bench_dir.join("personas.toml"))?;
    if let Some(filter) = opts.persona_filter.as_deref() {
        personas.retain(|p| p.id == filter);
        if personas.is_empty() {
            return Err(format!("--persona `{filter}` matched no persona in the bank"));
        }
    }
    let memories = load_memories(&bench_dir.join("memories.toml"))?;
    let seed_contents: Vec<String> = memories.values().map(|m| m.content.clone()).collect();

    let skills_dir = crate::voice_eval::runner::resolve_skills_dir(opts.skills_dir.as_ref())
        .map_err(|e| e.to_string())?;
    eprintln!("inner-chaos: fixtures from {}", bench_dir.display());
    eprintln!("inner-chaos: skills from {}", skills_dir.display());

    let stamp = unix_seconds().to_string();
    let mut journal = Journal::create(&opts.journal_path)?;
    eprintln!(
        "inner-chaos: journal {} (wiped), stamp {stamp}",
        journal.path().display()
    );

    let started = Instant::now();
    let budget = opts.minutes.map(|m| Duration::from_secs_f64(m * 60.0));
    let mut records: Vec<TurnRecord> = Vec::new();
    let mut thread_idx = 0usize;

    'outer: loop {
        for persona in &personas {
            let budget_spent = budget.is_some_and(|b| started.elapsed() >= b);
            let capped = opts.max_threads.is_some_and(|max| thread_idx >= max);
            if (budget_spent || capped) && thread_idx > 0 {
                break 'outer;
            }
            eprintln!(
                "inner-chaos: thread {thread_idx} persona `{}` ({} turns{}){}",
                persona.id,
                persona.turns,
                if persona.probes.is_empty() {
                    String::new()
                } else {
                    format!(", probes {}", persona.probes.join("+"))
                },
                if persona.control { " [control]" } else { "" }
            );
            run_thread(
                thread_idx,
                persona,
                &memories,
                &seed_contents,
                &skills_dir,
                &stamp,
                opts,
                &mut journal,
                &mut records,
            )
            .await;
            thread_idx += 1;
        }
        // No time budget and no explicit cap = exactly one pass
        // through the persona bank.
        if budget.is_none() && opts.max_threads.is_none() {
            break;
        }
    }

    let report = build_report(&stamp, &records);
    let stamped = journal.stamped_copy(&stamp)?;
    eprintln!("inner-chaos: stamped journal copy at {}", stamped.display());
    let report_path = stamped.with_file_name(format!("inner-chaos-{stamp}.report.json"));
    super::report::write_json(&report_path, &report)?;
    eprintln!("inner-chaos: report JSON at {}", report_path.display());
    if let Some(extra) = &opts.output {
        super::report::write_json(extra, &report)?;
        eprintln!("inner-chaos: report JSON also at {}", extra.display());
    }
    Ok(report)
}

/// One persona thread. Errors are journaled as turn records with
/// `error` set and the thread aborts — the run continues with the
/// next persona (per-thread failure isolation, like voice_eval's
/// per-scenario failures).
#[allow(clippy::too_many_arguments)]
async fn run_thread(
    thread_idx: usize,
    persona: &Persona,
    memories: &std::collections::BTreeMap<String, SeedMemory>,
    seed_contents: &[String],
    skills_dir: &std::path::Path,
    stamp: &str,
    opts: &RunOptions,
    journal: &mut Journal,
    records: &mut Vec<TurnRecord>,
) {
    let conv_id = format!("inner-chaos-{stamp}-t{thread_idx}-{}", persona.id);

    let (session, _tmpdir_keepalive) = match build_thread_session(
        skills_dir,
        opts.daemon_base.as_deref(),
        opts.chat_model.as_deref(),
        opts.temperature,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            let record = error_record(thread_idx, 0, persona, &conv_id, format!("session setup failed: {e}"));
            push(journal, records, record);
            return;
        }
    };
    // NOTE: seeded with `None` (General pool), which the inner-work
    // witness scope (`Scoped("inner-work")`) cannot see — so the core
    // safety personas run against an EMPTY recall pool. That is fine
    // for the LEAK-RESISTANCE personas (they test that the witness
    // does not fabricate/recite, and an empty pool can't be leaked),
    // but it means the core loop never exercised positive recall. The
    // `--recall` extension seeds with `Some("inner-work")` so its
    // memories are actually visible. Switching the core to
    // `Some("inner-work")` would let privacy_prober test a live pool —
    // a strictly better test, but it reopens the converged core safety
    // numbers, so it is left as an explicit follow-up, not a silent
    // change here.
    if let Err(e) = seed_memories(session.store.as_ref(), memories, None).await {
        let record = error_record(thread_idx, 0, persona, &conv_id, format!("memory-seed failed: {e}"));
        push(journal, records, record);
        return;
    }

    let brain_inference = pinned_or_shared(&session, opts.brain_model.as_ref(), opts.chat_model.as_ref());
    let judge_inference = pinned_or_shared(&session, opts.judge_model.as_ref(), opts.chat_model.as_ref());

    let mut transcript: Vec<TranscriptTurn> = Vec::new();
    for turn_idx in 0..persona.turns {
        let turn_no = turn_idx + 1;

        // 1. Brain proposes the next user move (one retry on
        //    unparseable output, then abort the thread — a broken
        //    turn fed to the SUT would poison every later turn).
        let brain_started = Instant::now();
        let mut user_msg = None;
        for attempt in 0..2 {
            let req = brain_request(persona, &transcript, turn_idx);
            match brain_inference.complete(&req).await {
                Ok(resp) => {
                    user_msg = parse_brain_message(&resp.text);
                    if user_msg.is_some() {
                        break;
                    }
                    eprintln!("  brain output unparseable (attempt {})", attempt + 1);
                }
                Err(e) => eprintln!("  brain inference failed (attempt {}): {e}", attempt + 1),
            }
        }
        let brain_ms = brain_started.elapsed().as_millis() as u64;
        let Some(user_msg) = user_msg else {
            let record = error_record(
                thread_idx,
                turn_no,
                persona,
                &conv_id,
                "brain produced no usable turn after retry".into(),
            );
            push(journal, records, record);
            return;
        };

        // 2. The witness turn under test.
        let runtime_started = Instant::now();
        let response_text = match session.runtime.handle_message(&user_msg, &conv_id).await {
            Ok(response) => {
                sovereign_core::title::strip_thinking_response(&response.message.content)
            }
            Err(e) => {
                let mut record = error_record(
                    thread_idx,
                    turn_no,
                    persona,
                    &conv_id,
                    format!("runtime turn failed: {e}"),
                );
                record.user = user_msg;
                record.brain_ms = brain_ms;
                push(journal, records, record);
                return;
            }
        };
        let runtime_ms = runtime_started.elapsed().as_millis() as u64;

        transcript.push(TranscriptTurn::user(user_msg.clone()));

        // 3. Judge — against the transcript INCLUDING this user
        //    turn, but excluding the reply being audited.
        let (verdict, judge_failed, judge_ms) = if opts.judge {
            let judge_started = Instant::now();
            let req = witness_judge_request(seed_contents, &transcript, &response_text);
            let verdict = match judge_inference.complete(&req).await {
                Ok(resp) => parse_witness_verdict(&resp.text),
                Err(e) => {
                    eprintln!("  judge inference failed: {e}");
                    None
                }
            };
            let failed = verdict.is_none();
            (verdict, failed, Some(judge_started.elapsed().as_millis() as u64))
        } else {
            (None, false, None)
        };

        match &verdict {
            Some(v) => eprintln!(
                "  turn {turn_no}/{}: {} red_lines={:?} ({runtime_ms}ms)",
                persona.turns,
                v.category.as_str(),
                v.red_lines
            ),
            None if judge_failed => {
                eprintln!("  turn {turn_no}/{}: JUDGE FAILED ({runtime_ms}ms)", persona.turns)
            }
            None => eprintln!("  turn {turn_no}/{}: unjudged ({runtime_ms}ms)", persona.turns),
        }

        let record = TurnRecord {
            ts_unix_ms: unix_millis(),
            thread: thread_idx,
            turn: turn_no,
            persona: persona.id.clone(),
            conv_id: conv_id.clone(),
            user: transcript.last().map(|t| t.text.clone()).unwrap_or_default(),
            response: response_text.clone(),
            verdict,
            judge_failed,
            error: None,
            brain_ms,
            runtime_ms,
            judge_ms,
        };
        push(journal, records, record);

        transcript.push(TranscriptTurn::witness(response_text));
    }
}

/// Build a per-thread `ChatSession`: fresh tempdir data dir, ONLY
/// the witness skill activated. The returned `TempDir` must stay
/// alive for the session's lifetime (dropping it yanks the SQLite
/// db out from under the runtime).
pub(super) async fn build_thread_session(
    skills_dir: &std::path::Path,
    daemon_base: Option<&str>,
    chat_model: Option<&str>,
    temperature: Option<f32>,
) -> Result<(ChatSession, tempfile::TempDir), String> {
    let tmp = tempfile::TempDir::new().map_err(|e| format!("create inner-chaos tempdir: {e}"))?;

    let mut globals = default_globals_for_voice_eval();
    if let Some(base) = daemon_base {
        globals.daemon_base = base.to_string();
    }
    if let Some(model) = chat_model {
        globals.chat_model = Some(model.to_string());
    }
    globals.data_dir = tmp.path().to_path_buf();
    globals.data_dir_explicit = true;
    // Unlike voice_eval (which pins 0.2 for reproducibility), the
    // chaos harness defaults to the product temperature — the
    // honest baseline measures what users get.
    globals.temperature = temperature;

    let mut skills = SkillRegistry::new();
    skills.load_and_register(skills_dir);
    if skills.skill_by_id(WITNESS_SKILL).is_none() {
        return Err(format!(
            "skill `{WITNESS_SKILL}` not found in {}",
            skills_dir.display()
        ));
    }
    skills.activate(WITNESS_SKILL);

    let session = build_session_with_skills(&globals, skills)
        .await
        .map_err(|e| e.to_string())?;
    Ok((session, tmp))
}

/// Resolve a role's inference handle: pinned to `role_model` when it
/// differs from the SUT chat model, else the session's shared
/// provider (same pattern as voice_eval's judge pinning).
pub(super) fn pinned_or_shared(
    session: &ChatSession,
    role_model: Option<&String>,
    chat_model: Option<&String>,
) -> Arc<dyn InferenceProvider> {
    match role_model {
        Some(model_id) if Some(model_id) != chat_model => {
            let v1 = format!("{}/v1", session.daemon_base);
            Arc::new(RemoteApiProvider::new(&v1, None, model_id, 8192))
        }
        _ => Arc::clone(&session.inference),
    }
}

/// Seed resident memories into the store.
///
/// `source_skill_id` decides which memory-recall POOL the seeds land
/// in. This is load-bearing: on the first message the runtime tags the
/// conversation with the active skill (`inner-work`) and recalls under
/// `MemoryScope::Scoped("inner-work")`, which admits ONLY memories
/// whose `source_skill_id == Some("inner-work")`. Seeding with `None`
/// puts them in the General pool where the witness can NEVER see them
/// (the inner-work memory wall is bidirectional). Pass
/// `Some("inner-work")` to replicate real journal entries the witness
/// can actually recall; pass `None` only when the pool is deliberately
/// irrelevant to the test.
pub(super) async fn seed_memories(
    store: &dyn StateStore,
    seeds: &std::collections::BTreeMap<String, SeedMemory>,
    source_skill_id: Option<&str>,
) -> sovereign_core::error::Result<()> {
    for (key, seed) in seeds {
        let created_at = seed
            .created_at
            .as_ref()
            .and_then(|d| parse_date(d))
            .unwrap_or(0);
        let memory = Memory {
            id: format!("inner-chaos-{key}"),
            content: seed.content.clone(),
            source: "inner_chaos_seed".into(),
            confidence: seed.confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: seed.source_conversation_id.clone(),
            source_skill_id: source_skill_id.map(|s| s.to_string()),
            ..Default::default()
        };
        store.save_memory(&memory).await?;
    }
    Ok(())
}

fn error_record(
    thread: usize,
    turn: usize,
    persona: &Persona,
    conv_id: &str,
    error: String,
) -> TurnRecord {
    eprintln!("  thread {thread} aborted: {error}");
    TurnRecord {
        ts_unix_ms: unix_millis(),
        thread,
        turn,
        persona: persona.id.clone(),
        conv_id: conv_id.to_string(),
        user: String::new(),
        response: String::new(),
        verdict: None,
        judge_failed: false,
        error: Some(error),
        brain_ms: 0,
        runtime_ms: 0,
        judge_ms: None,
    }
}

fn push(journal: &mut Journal, records: &mut Vec<TurnRecord>, record: TurnRecord) {
    if let Err(e) = journal.append(&record) {
        eprintln!("inner-chaos: journal write failed: {e}");
    }
    records.push(record);
}

pub(super) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
