//! `run_team_pipeline` — the five-stage orchestrator that drives a
//! turn through Router → Retriever → Curator → Drafter → Presenter
//! when the team-pipeline kill-switch is on
//! (per [`is_team_pipeline_enabled`]).
//!
//! Phase 4 of the situated-team plan
//! (`/Users/user/.claude/plans/there-s-a-fast-slot-delightful-peach.md`).
//! Stages 1–4 (Router/Retriever/Curator/Drafter) emit
//! [`crate::types::NarrationPhase`] stage chips for the desktop and
//! produce a Drafter draft; stage 5 (Presenter) is what user-streams
//! tokens. This split is what the plan's §4.4 streaming-subtlety
//! note describes — stage chips do the work that would otherwise
//! look like a frozen UI for the ~2–4s the pre-stages take.
//!
//! On [`Sufficiency::Insufficient`] the Drafter is skipped entirely
//! and the Presenter shapes a direct honest message — the
//! glass-box short-circuit per the plan's "epistemic-honesty
//! default" stance.
//!
//! Tool-call paths and OICP/mesh peer routing do NOT enter this
//! orchestrator (per plan §4.3): they reach `Runtime` through
//! distinct entry points (`handle_tool_invocation`, OICP-side
//! handlers) that never call into [`run_team_pipeline`].

use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures::{stream, Stream, StreamExt};

use crate::error::Result;
use crate::pipeline::curator::curate;
use crate::pipeline::judge::{should_judge, spawn_voice_judge};
use crate::pipeline::presenter::present_request;
use crate::pipeline::stages::{CuratedPackage, Sufficiency};
use crate::skills::SkillRegister;
use crate::title::strip_think_blocks;
use crate::traits::InferenceProvider;
use crate::types::{
    CompletionRequest, NarrationEvent, NarrationPhase, RouterClassification, Speed, StreamFrame,
    TurnNarration,
};

/// Default per-turn token budget for the team-pipeline. Used when
/// the caller doesn't supply a tighter cap. Sized to match the
/// situated-team plan's §Open follow-ups: bounded expression on a
/// curated package shouldn't approach this; if it does, the
/// Curator's per-section budgets are the regression signal to act
/// on.
pub const DEFAULT_TEAM_PIPELINE_MAX_TOKENS: u32 = 2048;

/// iter9 — hard cap on the Drafter's max_tokens when the register
/// is Relational. Witness replies live in the 50-200 token range;
/// the Drafter on a passthrough package (no Curator skeleton) had
/// been given the orchestrator's full 2048 budget and wrote
/// 800-1000 token analytical drafts that the Presenter then
/// mirrored as visible meta-narration. 240 tokens (~840 chars)
/// fits a 2-paragraph witness reply with comfortable headroom.
pub const DRAFTER_RELATIONAL_MAX_TOKENS: usize = 240;

/// Environment variable that gates the team-pipeline path. Set to
/// `"1"` (or `"true"` / `"on"`) to enable; anything else (including
/// unset) leaves the legacy chat path in place.
///
/// **STATUS (2026-05-03): EXPERIMENTALLY REJECTED — DEFAULT-OFF PERMANENT.**
/// The plan originally targeted "default-on with a kill-switch." The full
/// A/B (legacy vs team iter10, same daemon, same models) found the team
/// pipeline regresses 5/12 on the base voice bench, regresses 1/8 on the
/// hard set, and is 2–4× slower on the synthesis case the architecture
/// was designed to fix — which legacy now handles cleanly without
/// tangling. **Do NOT flip the default.** See
/// `sovereign/bench/voice/baseline/team-pipeline-findings.md` for the
/// full A/B numbers, the 10-iteration Presenter tuning log, and the
/// "what stays / what goes" inventory before deleting any pipeline code.
pub const TEAM_PIPELINE_ENV_VAR: &str = "SOVEREIGN_TEAM_PIPELINE";

/// Read [`TEAM_PIPELINE_ENV_VAR`] per-turn (NOT cached at boot, per
/// plan §4.2) so flipping the env var on a running daemon is
/// immediate. Returns `false` by default — the team pipeline was
/// experimentally rejected on 2026-05-03; see the doc comment on
/// [`TEAM_PIPELINE_ENV_VAR`] and
/// `sovereign/bench/voice/baseline/team-pipeline-findings.md`.
pub fn is_team_pipeline_enabled() -> bool {
    match std::env::var(TEAM_PIPELINE_ENV_VAR) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Inputs the orchestrator needs from the caller. Wraps the
/// per-turn handles the legacy [`crate::runtime::Runtime`] keeps as
/// `Arc<dyn …>` fields, plus the message + classification + the
/// retriever's candidate chunks (already produced by the runtime
/// before this function is called, so the orchestrator stays
/// decoupled from the retrieval pipeline).
pub struct TeamPipelineInputs<'a> {
    pub provider: Arc<dyn InferenceProvider>,
    pub message: &'a str,
    pub classification: &'a RouterClassification,
    pub register: SkillRegister,
    pub candidates: Vec<crate::pipeline::stages::RetrievedChunk>,
    /// Caller's max-tokens budget. The orchestrator clamps the
    /// Drafter and Presenter caps from this single number so the
    /// caller doesn't have to know about per-stage subdivision.
    pub max_tokens: u32,
    /// When true, the async voice judge fires after the Presenter
    /// (Relational register only). Plan §3.2 — defaults to true on
    /// Relational; set to false in test harness paths that don't
    /// want extra Fast-slot calls.
    pub judge_enabled: bool,
    /// Pre-formatted witness grounding block — memories, working
    /// memory, temporal tensions, etc. — that the Drafter needs to
    /// answer Relational/Expressive turns. Empty for corpus-only
    /// intents (KnowledgeQuery / ComparisonQuery) where the
    /// CuratedPackage carries everything. The runtime builds this
    /// from `context` at the call site so the orchestrator stays
    /// decoupled from runtime's prompt-assembly internals.
    pub witness_grounding: String,
}

/// Sink for [`TurnNarration`] events. Decoupled from the runtime's
/// `routing_events` so the orchestrator can run inside a unit test
/// without instantiating the full event-bus machinery.
#[async_trait::async_trait]
pub trait NarrationSink: Send + Sync {
    async fn emit(&self, narration: TurnNarration);
}

/// No-op sink for tests / contexts that don't want narration.
pub struct NoopNarrationSink;

#[async_trait::async_trait]
impl NarrationSink for NoopNarrationSink {
    async fn emit(&self, _: TurnNarration) {}
}

/// Adapter that routes [`NarrationSink`] emissions into the
/// runtime's broader [`crate::traits::RoutingEventSink`]. The
/// orchestrator stays trait-decoupled (so it can run under a
/// no-op sink in unit tests); the runtime wraps its
/// `routing_events` field in this adapter at the wire-up site.
pub struct RoutingEventNarrationSink {
    pub inner: Arc<dyn crate::traits::RoutingEventSink>,
}

#[async_trait::async_trait]
impl NarrationSink for RoutingEventNarrationSink {
    async fn emit(&self, narration: TurnNarration) {
        self.inner.emit_turn_narration(narration).await;
    }
}

/// Output of a team-pipeline run. The `stream` is the Presenter's
/// streamed tokens; the `presented_text` future resolves to the
/// full presented text once the stream completes (used by the
/// runtime to persist the assistant message + fire the async
/// voice judge).
pub struct TeamPipelineOutput {
    pub stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
    pub draft: String,
    pub package: CuratedPackage,
}

/// Run the five-stage team pipeline. Pre-stages (Router → Retriever
/// → Curator → Drafter) execute synchronously and emit narration
/// chips between stages; the Presenter is invoked as the streaming
/// stage and yields tokens to the caller.
///
/// `session_id` / `conversation_id` are pulled from the caller's
/// turn so the emitted narration frames reach the right open
/// session in the desktop. Pass empty strings only when running in
/// a no-narration test path.
pub async fn run_team_pipeline<'a>(
    inputs: TeamPipelineInputs<'a>,
    narration: Arc<dyn NarrationSink>,
    session_id: String,
    conversation_id: String,
) -> Result<TeamPipelineOutput> {
    let started = Instant::now();
    tracing::info!(
        intent = ?inputs.classification.primary.intent,
        register = ?inputs.register,
        candidates = inputs.candidates.len(),
        max_tokens = inputs.max_tokens,
        "team-pipeline: turn begin"
    );

    let candidate_count = inputs.candidates.len();

    // Stage 1 (Router) already happened — the caller passes the
    // classification in. We mirror it as a narration chip here so
    // the desktop sees the stage chip series for chat continuity.
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::RoutingComplete {
            intent: format!("{:?}", inputs.classification.primary.intent),
            register: format!("{:?}", inputs.register),
            confidence: inputs.classification.primary.confidence,
        },
        format!(
            "Routed as {:?} — proceeding.",
            inputs.classification.primary.intent
        ),
    )
    .await;

    // Stage 2 (Retrieval) already happened too — `candidates` is
    // the result. Surface as a chip with shape.
    let corpora: Vec<String> = inputs
        .candidates
        .iter()
        .map(|c| c.corpus_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::RetrievalComplete {
            chunks_in: candidate_count,
            corpora: corpora.clone(),
        },
        if candidate_count == 0 {
            "No retrieval — answering from general knowledge.".to_string()
        } else {
            format!(
                "Retrieved {chunks} chunks across {n_corpora} sources.",
                chunks = candidate_count,
                n_corpora = corpora.len(),
            )
        },
    )
    .await;

    // Stage 3 — Curator.
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::CurationStart,
        "Selecting the most relevant material.".to_string(),
    )
    .await;
    let package = curate(
        Arc::clone(&inputs.provider),
        inputs.classification,
        inputs.register,
        inputs.message,
        inputs.candidates,
        inputs.max_tokens,
    )
    .await?;
    let kept = package.kept_chunks.len();
    let skeleton_labels: Vec<String> = package
        .skeleton
        .iter()
        .map(|s| s.label.clone())
        .filter(|l| !l.is_empty())
        .collect();
    let sufficient = matches!(package.sufficiency, Sufficiency::Sufficient);
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::CurationComplete {
            chunks_kept: kept,
            skeleton: skeleton_labels.clone(),
            sufficient,
        },
        match &package.sufficiency {
            Sufficiency::Sufficient => format!("Curated {kept} chunks; drafting now."),
            Sufficiency::Partial { gaps } => {
                format!("Curated {kept} chunks; gaps: {}.", gaps.join(", "))
            }
            Sufficiency::Insufficient { reason, .. } => {
                format!("Insufficient grounding: {reason}.")
            }
        },
    )
    .await;

    // Glass-box honesty short-circuit: skip Drafter on
    // `Insufficient`, route Presenter to an honest message.
    if let Sufficiency::Insufficient {
        reason,
        suggested_action,
    } = package.sufficiency.clone()
    {
        let honest = format!(
            "I don't have grounding for this from the corpora I checked — {reason}. \
             Would you like me to {suggested_action}?"
        );
        let presented = run_presenter_streaming(
            Arc::clone(&inputs.provider),
            inputs.message,
            &honest,
            inputs.register,
            inputs.max_tokens,
            narration.clone(),
            session_id.clone(),
            conversation_id.clone(),
        )
        .await?;
        spawn_judge_if_appropriate(
            Arc::clone(&inputs.provider),
            inputs.message,
            &honest,
            inputs.register,
            inputs.judge_enabled,
        );
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "team-pipeline: insufficient short-circuit"
        );
        return Ok(TeamPipelineOutput {
            stream: presented.stream,
            draft: honest,
            package,
        });
    }

    // Stage 4 — Drafter (Primary slot, non-streaming so the full
    // draft lands as Presenter input).
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::DraftingStart,
        "Drafting the response.".to_string(),
    )
    .await;
    let drafter_request = build_drafter_request(
        inputs.message,
        inputs.classification,
        inputs.register,
        &package,
        &inputs.witness_grounding,
    );
    let draft_response = inputs.provider.complete(&drafter_request).await?;
    let draft = strip_think_blocks(&draft_response.text).trim().to_string();
    let drafter_finish_reason =
        if draft_response.tokens_used >= drafter_request.max_tokens.unwrap_or(usize::MAX) {
            "length"
        } else {
            "stop"
        };
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::DraftingComplete {
            tokens: (draft_response
                .tokens_used
                .saturating_sub(draft_response.prompt_tokens)) as u32,
            finish_reason: drafter_finish_reason.to_string(),
        },
        format!(
            "Draft complete ({} tokens) — refining for voice.",
            draft_response
                .tokens_used
                .saturating_sub(draft_response.prompt_tokens),
        ),
    )
    .await;

    // Stage 5 — Presenter (Primary slot since iter3, streaming).
    let presented = run_presenter_streaming(
        Arc::clone(&inputs.provider),
        inputs.message,
        &draft,
        inputs.register,
        inputs.max_tokens,
        narration.clone(),
        session_id.clone(),
        conversation_id.clone(),
    )
    .await?;

    // Async voice judge — fire-and-forget. Per plan §3.2 the
    // result records as a delayed
    // `NarrationPhase::PresentationComplete` frame; for Phase 4 v1
    // we just spawn the judge and log — wiring the delayed frame
    // into the narration sink is a follow-up so the orchestrator
    // stays slim.
    spawn_judge_if_appropriate(
        Arc::clone(&inputs.provider),
        inputs.message,
        &draft,
        inputs.register,
        inputs.judge_enabled,
    );

    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        kept_chunks = kept,
        skeleton_sections = skeleton_labels.len(),
        drafter_tokens = draft_response
            .tokens_used
            .saturating_sub(draft_response.prompt_tokens),
        "team-pipeline: complete"
    );

    Ok(TeamPipelineOutput {
        stream: presented.stream,
        draft,
        package,
    })
}

struct PresenterStreamBundle {
    stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
}

/// Stream the Presenter's rewrite. Internally calls
/// `complete_stream_with_finish` and adapts the typed
/// [`StreamFrame`] back to the legacy `Result<String>` shape so the
/// runtime's existing chat-stream consumers don't need to know
/// about the typed surface yet (Phase 1.1's typed shape is bounded
/// to the `LocalInferenceService` HTTP path; the runtime's
/// in-process channel keeps the legacy shape for now).
///
/// Emits `PresentationStart` before the stream begins; the runtime
/// emits `PresentationComplete` after the stream is exhausted.
async fn run_presenter_streaming(
    provider: Arc<dyn InferenceProvider>,
    user_message: &str,
    draft: &str,
    register: SkillRegister,
    max_tokens: u32,
    narration: Arc<dyn NarrationSink>,
    session_id: String,
    conversation_id: String,
) -> Result<PresenterStreamBundle> {
    emit_phase(
        &narration,
        &session_id,
        &conversation_id,
        NarrationPhase::PresentationStart,
        "Refining the response for voice.".to_string(),
    )
    .await;

    let request = present_request(
        user_message,
        draft,
        register,
        max_tokens.min(u16::MAX as u32),
    );
    let raw = provider.complete_stream_with_finish(&request).await?;

    // Adapt typed StreamFrame → Result<String> for the legacy
    // consumer surface. Token frames pass through verbatim; Finish
    // closes the stream; Error becomes a single Err.
    let adapted = raw.flat_map(|frame| {
        let items: Vec<Result<String>> = match frame {
            StreamFrame::Token(text) => vec![Ok(text)],
            StreamFrame::Finish { .. } => Vec::new(),
            StreamFrame::Error(msg) => vec![Err(crate::error::Error::Inference(msg))],
        };
        stream::iter(items)
    });

    Ok(PresenterStreamBundle {
        stream: Box::pin(adapted),
    })
}

fn spawn_judge_if_appropriate(
    provider: Arc<dyn InferenceProvider>,
    user_message: &str,
    candidate: &str,
    register: SkillRegister,
    judge_enabled: bool,
) {
    if !should_judge(register, judge_enabled) {
        return;
    }
    let user_message = user_message.to_string();
    let candidate = candidate.to_string();
    let handle = spawn_voice_judge(provider, user_message, candidate);
    tokio::spawn(async move {
        match handle.await {
            Ok(score) => {
                tracing::info!(
                    fold_total = score.fold_total(),
                    avoid_list_penalty = score.avoid_list_penalty,
                    rationale = %score.rationale,
                    "team-pipeline: voice judge result"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "team-pipeline: judge task panicked");
            }
        }
    });
}

fn build_drafter_request(
    user_message: &str,
    classification: &RouterClassification,
    register: SkillRegister,
    package: &CuratedPackage,
    witness_grounding: &str,
) -> CompletionRequest {
    let formatted = package.format_for_drafter();
    let grounding_block = if witness_grounding.trim().is_empty() {
        String::new()
    } else {
        format!("<grounding>\n{witness_grounding}\n</grounding>\n\n")
    };
    let prompt = format!(
        "Intent: {intent:?}\nRegister: {register:?}\n\n\
         User question:\n{user_message}\n\n\
         {grounding_block}\
         You have a curated package below: a list of chunks the \
         retriever surfaced and a section skeleton with per-section \
         token budgets. Generate the response section-by-section, \
         in the order given. Cite chunks by `[chunk N]` (the chunk \
         id from `<chunks>`) where appropriate. Stay within each \
         section's `budget`. Do not invent content the chunks don't \
         support; if a chunk is referenced and the relevant detail \
         isn't there, say so briefly rather than fabricating.\n\
         When a `<grounding>` block is present, treat it as the \
         authoritative record of what the system actually knows about \
         the user — names, prior turns, working context, temporal \
         contradictions to acknowledge. If the user asks about \
         something the grounding doesn't cover, say so honestly \
         rather than inferring.\n\n\
         <package>\n{formatted}</package>\n\n\
         Begin the response now.",
        intent = classification.primary.intent,
    );

    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Slow);
    // iter8: the Drafter carries the witness contract again
    // (RELATIONAL_BASE_SYSTEM_PROMPT for Relational) so it
    // produces witness-voice prose, not retrieval-analytical
    // prose. iter7 showed the Presenter mirrored the Drafter's
    // analytical structure ("1. The user asks... 2. The records
    // show...") regardless of how the Presenter prompt was framed.
    // With the witness contract on the Drafter, the Presenter
    // sees clean witness prose and its few-shot polish has clean
    // input to work on. For Factual we leave the Drafter's general
    // prompt — Factual responses don't need the witness frame.
    if matches!(register, SkillRegister::Relational) {
        req.system_message = Some(crate::runtime::epistemic_contract_for(register).to_string());
    }

    // iter9: clamp Drafter max_tokens for Relational. iter8 gave
    // the Drafter the orchestrator's full 2048 cap on passthrough
    // packages (no Curator skeleton); the Drafter then wrote
    // 800-1000 token analytical drafts that the Presenter mirrored
    // as visible meta-narration ("Let me analyze this carefully: 1.
    // The user is sharing..."). Witness replies live in the
    // 50-200 token range; forcing the Drafter to fit there cuts
    // the analytical bloat at the source. Factual untouched —
    // factual answers can be longer.
    if matches!(register, SkillRegister::Relational) {
        let cap = req
            .max_tokens
            .unwrap_or(usize::MAX)
            .min(DRAFTER_RELATIONAL_MAX_TOKENS);
        req.max_tokens = Some(cap);
    }
    // Cap to the SUM of per-section target_tokens (the Curator's
    // composed budget), not the ceiling. The ceiling is the caller's
    // hard maximum (typically the orchestrator's full 2048); the
    // target is what the Curator actually budgeted across its
    // skeleton. Iter0 used the ceiling and produced 1.3-2.2x length
    // blowouts on 5/20 scenarios (02-rich, 04-load-bearing,
    // 12-avoid-list, H06, H07) because the Drafter had no upstream
    // signal to stop. Passthrough packages have target == ceiling so
    // the bypass path is unaffected.
    req.max_tokens = Some(package.draft_budget.target_tokens as usize);
    req.temperature = Some(0.5);
    // Suppress thinking on the Drafter — Presenter will do voice
    // shaping; thinking-mode CoT here would just need stripping.
    req.enable_thinking = Some(false);
    req
}

async fn emit_phase(
    sink: &Arc<dyn NarrationSink>,
    session_id: &str,
    conversation_id: &str,
    phase: NarrationPhase,
    text: String,
) {
    let event = NarrationEvent {
        phase,
        text,
        elapsed_ms: 0,
    };
    sink.emit(TurnNarration {
        session_id: session_id.to_string(),
        conversation_id: conversation_id.to_string(),
        event,
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests are process-global and race when cargo test runs
    // them in parallel — one test removes the var while another
    // expects it set. Serialise via a shared mutex so the cases
    // execute sequentially even under parallel test runners.
    static KILL_SWITCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn kill_switch_off_when_env_var_absent() {
        let _g = KILL_SWITCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(TEAM_PIPELINE_ENV_VAR);
        assert!(!is_team_pipeline_enabled());
    }

    #[test]
    fn kill_switch_on_when_env_var_truthy() {
        let _g = KILL_SWITCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for v in ["1", "true", "on", "yes", "TRUE", "On"] {
            std::env::set_var(TEAM_PIPELINE_ENV_VAR, v);
            assert!(
                is_team_pipeline_enabled(),
                "expected truthy for env value {v:?}"
            );
        }
        std::env::remove_var(TEAM_PIPELINE_ENV_VAR);
    }

    #[test]
    fn kill_switch_off_for_explicit_false_values() {
        let _g = KILL_SWITCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for v in ["0", "false", "off", "no", ""] {
            std::env::set_var(TEAM_PIPELINE_ENV_VAR, v);
            assert!(
                !is_team_pipeline_enabled(),
                "expected falsy for env value {v:?}"
            );
        }
        std::env::remove_var(TEAM_PIPELINE_ENV_VAR);
    }
}
