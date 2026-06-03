//! Tier-B *live* runner.
//!
//! Drives each scenario through a daemon-backed `Runtime`, collects
//! the response, runs deterministic checks (`checks::run_checks`),
//! and runs the voice-judge LLM rubric for per-axis scoring. Output
//! is a `ScenarioResult` (deterministic only) augmented with an
//! optional `JudgeScore`.
//!
//! Lifecycle per run:
//!
//! 1. Build a `SkillRegistry` populated from the bundled
//!    `sovereign/skills/` directory.
//! 2. Open a `ChatSession` via `build_session_with_skills` against
//!    the running daemon, with `data_dir` pointed at a fresh tempdir
//!    so user state is never touched.
//! 3. For each scenario:
//!    - Clear leftover memories from the prior scenario.
//!    - Activate ONLY the scenario's skill (so the runtime's
//!      `primary_skill_register()` resolves correctly).
//!    - Save the scenario's seed memories.
//!    - Drive `runtime.handle_message` with `scenario.turn.user`.
//!    - Run deterministic checks.
//!    - If judge mode is on, build the voice judge prompt and
//!      send it through the same inference handle.
//! 4. Aggregate into a `VoiceEvalRun` and return.
//!
//! Failure handling: any per-scenario failure is logged + the
//! scenario is recorded as failed; the run continues. A failure
//! to even open the daemon session aborts the whole run with a
//! clear remediation message.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sovereign_core::error::Result;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::Memory;
use sovereign_core::SkillRegistry;
use sovereign_inference::remote::RemoteApiProvider;

use crate::chat_cmd::bootstrap::{build_session_with_skills, ChatSession};

use super::checks::{run_checks, ScenarioResult};
use super::judge::{parse_judge_score, voice_judge_request, JudgeScore};
use super::scenarios::{parse_date, Scenario, SeedMemory};

/// Live-run options. Defaults are calibrated for "ran daemon, want
/// to score 12 scenarios" — judge on, JSON report off, fresh
/// tempdir per run.
pub struct LiveRunOptions {
    /// Daemon base URL (e.g., `http://localhost:9741`). Pulled from
    /// `SetupConfig` when `None`.
    pub daemon_base: Option<String>,
    /// Run the LLM-as-judge per scenario in addition to the
    /// deterministic checks. Costs one Fast-slot inference call per
    /// scenario; default true.
    pub judge: bool,
    /// Override location of the bundled skills dir. When `None`,
    /// `resolve_skills_dir` walks up from CWD to find
    /// `sovereign/skills/`.
    pub skills_dir: Option<PathBuf>,
    /// Override the daemon-resolved chat model id for the runtime
    /// turn. When `None`, the daemon's configured chat model is used
    /// (matches a no-flag `sovereign chat` invocation). Setting this
    /// is how a model A/B baseline is driven.
    pub chat_model: Option<String>,
    /// Override the chat model id used for the LLM-as-judge call.
    /// When `None` the judge runs on the same model as the chat
    /// turn — fine for a single-model baseline but conflates the
    /// model under test with the rater. Pin this to a stable model
    /// (typically the larger one) when comparing multiple chat
    /// models head-to-head, so judge variance doesn't get attributed
    /// to the model being scored.
    pub judge_model: Option<String>,
}

impl Default for LiveRunOptions {
    fn default() -> Self {
        Self {
            daemon_base: None,
            judge: true,
            skills_dir: None,
            chat_model: None,
            judge_model: None,
        }
    }
}

/// `ScenarioResult` augmented with the per-axis judge score, when
/// judge mode is on. The base `ScenarioResult` carries the
/// deterministic check outcomes; `judge` adds the LLM rubric.
///
/// `runtime_ms` is the wall-clock time of the runtime turn (the
/// "what does the chat model do under the witness contract" call we
/// actually care about). `judge_ms` is the judge call latency,
/// included for completeness but not the headline number — the
/// judge can run on a different model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveScenarioResult {
    #[serde(flatten)]
    pub result: ScenarioResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeScore>,
    #[serde(default)]
    pub runtime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_ms: Option<u64>,
    /// Iter5: per-stage runtime breakdown (routing, memory recall,
    /// Pass A, tensions, synthesis). `None` when the active intent
    /// didn't go through an instrumented witness path. Voice-eval
    /// uses these to compute median-per-stage waterfall in the
    /// text report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<sovereign_core::types::RuntimeMetrics>,
}

/// Drive a list of scenarios through the live Runtime and return
/// per-scenario results. Errors here are setup-level (couldn't
/// reach daemon, couldn't load skills); per-scenario failures are
/// surfaced as `ScenarioResult.passed = false` with a stub
/// response containing the error text.
///
/// Implementation note — per-scenario session: each scenario gets
/// its own `ChatSession` with a `SkillRegistry` that has ONLY the
/// scenario's named skill activated. This is required because
/// `SkillRegistry::activate` takes `&mut self` and the registry
/// inside an existing session is sealed behind `Arc`, so we can't
/// flip activation between scenarios on a shared session. The
/// per-scenario daemon-probe + corpus-build cost (≤500ms) is
/// negligible next to the actual model turn (1–30s).
pub async fn run_live(
    scenarios: &[Scenario],
    opts: &LiveRunOptions,
) -> Result<Vec<LiveScenarioResult>> {
    let skills_dir = resolve_skills_dir(opts.skills_dir.as_ref())?;
    eprintln!("voice eval: skills from {}", skills_dir.display());
    if let Some(model) = &opts.chat_model {
        eprintln!("voice eval: chat model pinned to {model}");
    }
    if let Some(model) = &opts.judge_model {
        eprintln!("voice eval: judge model pinned to {model}");
    }

    let mut out = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        eprintln!(
            "voice eval: running {} ({})",
            scenario.scenario.id, scenario.scenario.skill
        );
        match build_scenario_session(&skills_dir, &scenario.scenario.skill, opts).await {
            Ok((session, _tmpdir_keepalive)) => {
                // Resolve the judge inference handle. When no
                // `judge_model` is set we share the chat handle, which
                // is the historical behaviour. When pinned, build a
                // dedicated `RemoteApiProvider` for the judge so the
                // chat model under test can vary while the judge stays
                // stable across runs.
                let judge_inference: Arc<dyn InferenceProvider> = match &opts.judge_model {
                    Some(model_id) if Some(model_id) != opts.chat_model.as_ref() => {
                        let v1 = format!("{}/v1", session.daemon_base);
                        Arc::new(RemoteApiProvider::new(&v1, None, model_id, 8192))
                    }
                    _ => Arc::clone(&session.inference),
                };
                let live = run_one(&session, scenario, opts.judge, judge_inference.as_ref()).await;
                out.push(live);
                // _tmpdir_keepalive drops here, cleaning the per-scenario
                // state directory. No leak of memories or state across
                // scenarios.
            }
            Err(e) => {
                eprintln!(
                    "voice eval: scenario {} setup failed: {e}",
                    scenario.scenario.id
                );
                out.push(synthesize_failure(
                    scenario,
                    format!("session setup failed: {e}"),
                    None,
                ));
            }
        }
    }

    Ok(out)
}

/// Build a `ChatSession` for one scenario. Returns the session plus
/// the `TempDir` whose drop cleans up the per-scenario state. The
/// caller must keep the `TempDir` alive for the duration of the
/// session — dropping it early would yank the SQLite db out from
/// under the live runtime.
async fn build_scenario_session(
    skills_dir: &std::path::Path,
    scenario_skill_id: &str,
    opts: &LiveRunOptions,
) -> Result<(ChatSession, tempfile::TempDir)> {
    let tmp = tempfile::TempDir::new().map_err(|e| {
        sovereign_core::error::Error::Serialization(format!("create voice-eval tempdir: {e}"))
    })?;

    let mut globals = crate::chat_cmd::config::default_globals_for_voice_eval();
    if let Some(base) = &opts.daemon_base {
        globals.daemon_base = base.clone();
    }
    if let Some(model) = &opts.chat_model {
        globals.chat_model = Some(model.clone());
    }
    globals.data_dir = tmp.path().to_path_buf();
    globals.data_dir_explicit = true;
    // Voice eval wants deterministic-ish output (low temperature)
    // for reproducibility across runs. Operators tracking the
    // score over time get a less-jittery signal at temperature=0.2
    // than at the default 0.7.
    globals.temperature = Some(0.2);

    let mut skills = SkillRegistry::new();
    skills.load_and_register(skills_dir);
    if skills.list().is_empty() {
        return Err(sovereign_core::error::Error::Serialization(format!(
            "no skills loaded from {} — pass --skills-dir or run from the repo root",
            skills_dir.display()
        )));
    }
    if skills.skill_by_id(scenario_skill_id).is_none() {
        return Err(sovereign_core::error::Error::Serialization(format!(
            "scenario references skill `{scenario_skill_id}` which is not in {}",
            skills_dir.display()
        )));
    }
    // Activate ONLY this scenario's skill. The runtime's
    // `primary_skill_register()` then resolves to that skill's
    // register (Relational for inner-work / personal-assistant),
    // which is the load-bearing wire for the whole voice contract.
    skills.activate(scenario_skill_id);

    let session = build_session_with_skills(&globals, skills).await?;
    Ok((session, tmp))
}

async fn run_one(
    session: &ChatSession,
    scenario: &Scenario,
    judge_enabled: bool,
    judge_inference: &dyn InferenceProvider,
) -> LiveScenarioResult {
    // Seed memories.
    if let Err(e) = seed_memories(session.store.as_ref(), &scenario.seed_memories).await {
        return synthesize_failure(scenario, format!("memory-seed failed: {e}"), None);
    }

    // Conversation id — unique per scenario so the streaming
    // runtime path can attach messages without cross-contamination.
    let conv_id = format!("voice-eval-{}", scenario.scenario.id);

    // Drive one turn through the runtime. Time it — the headline
    // wall-clock for the run, fed into the report's latency
    // aggregates so an operator can see the small/large model
    // latency gap alongside the quality gap.
    let runtime_started = Instant::now();
    let (response_text, metrics) =
        match drive_turn(&session.runtime, &scenario.turn.user, &conv_id).await {
            Ok(t) => t,
            Err(e) => {
                return synthesize_failure(scenario, format!("runtime turn failed: {e}"), None);
            }
        };
    let runtime_ms = runtime_started.elapsed().as_millis() as u64;

    let result = run_checks(scenario, &response_text);

    let (judge, judge_ms) = if judge_enabled {
        let started = Instant::now();
        let score = run_judge(judge_inference, &scenario.turn.user, &response_text).await;
        let elapsed = started.elapsed().as_millis() as u64;
        (score, Some(elapsed))
    } else {
        (None, None)
    };

    LiveScenarioResult {
        result,
        judge,
        runtime_ms,
        judge_ms,
        metrics,
    }
}

async fn drive_turn(
    runtime: &Runtime,
    user_message: &str,
    conv_id: &str,
) -> Result<(String, Option<sovereign_core::types::RuntimeMetrics>)> {
    let response = runtime.handle_message(user_message, conv_id).await?;
    // The relational-Expressive synthesis path now flips
    // `enable_thinking: true` and strips the trace before the
    // response leaves the runtime — this `strip_thinking_response`
    // call is defensive: it covers any response that still carries
    // a think trace (other intents in flux, off-by-default-thinking
    // models that nonetheless emit `</think>`) and is a no-op when
    // there's nothing to strip. Same helper the runtime uses, so
    // eval and production see the same shape.
    let text = sovereign_core::title::strip_thinking_response(&response.message.content);
    Ok((text, response.metrics))
}

async fn run_judge(
    inference: &dyn InferenceProvider,
    user_message: &str,
    candidate: &str,
) -> Option<JudgeScore> {
    let request = voice_judge_request(user_message, candidate);
    match inference.complete(&request).await {
        Ok(resp) => Some(parse_judge_score(&resp.text)),
        Err(e) => {
            tracing::warn!(error = %e, "voice eval: judge inference failed; continuing without score");
            None
        }
    }
}

async fn seed_memories(
    store: &dyn StateStore,
    seeds: &std::collections::BTreeMap<String, SeedMemory>,
) -> Result<()> {
    for (key, seed) in seeds {
        let created_at = seed
            .created_at
            .as_ref()
            .and_then(|d| parse_date(d))
            .unwrap_or(0);
        let memory = Memory {
            id: format!("voice-eval-{key}"),
            content: seed.content.clone(),
            source: "voice_eval_seed".into(),
            confidence: seed.confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: seed.source_conversation_id.clone(),
            source_skill_id: None,
            ..Default::default()
        };
        store.save_memory(&memory).await?;
    }
    Ok(())
}

fn synthesize_failure(
    scenario: &Scenario,
    error: String,
    judge: Option<JudgeScore>,
) -> LiveScenarioResult {
    let stub_response = format!("[voice-eval setup error] {error}");
    let mut result = run_checks(scenario, &stub_response);
    // Force overall fail; the deterministic checks will already
    // fail required-content / banned-phrase checks for most
    // scenarios but be explicit.
    result.passed = false;
    LiveScenarioResult {
        result,
        judge,
        runtime_ms: 0,
        judge_ms: None,
        metrics: None,
    }
}

fn resolve_skills_dir(explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_dir() {
            return Ok(p.clone());
        }
        return Err(sovereign_core::error::Error::Serialization(format!(
            "voice eval: --skills-dir `{}` is not a directory",
            p.display()
        )));
    }
    let mut here: PathBuf = std::env::current_dir().map_err(|e| {
        sovereign_core::error::Error::Serialization(format!("cannot resolve current dir: {e}"))
    })?;
    loop {
        // Prefer the new `modes/` directory; fall back to `skills/`
        // for back-compat with checkouts that haven't pulled the
        // skills-as-menu retirement yet.
        for sub in ["modes", "skills"] {
            let candidate = here.join("sovereign").join(sub);
            if candidate.is_dir() && candidate.join("inner-work").is_dir() {
                return Ok(candidate);
            }
            let alt = here.join(sub);
            if alt.is_dir() && alt.join("inner-work").is_dir() {
                return Ok(alt);
            }
        }
        if !here.pop() {
            break;
        }
    }
    Err(sovereign_core::error::Error::Serialization(
        "voice eval: could not find `sovereign/modes/` (or legacy `sovereign/skills/`) walking up from CWD. Pass --skills-dir.".into(),
    ))
}
