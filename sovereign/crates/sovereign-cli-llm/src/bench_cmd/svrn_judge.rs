// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's grounding primitives, answered by svrn: `svrn __probe` in its
//! `assess` and `judge` modes (phase-b-63, -64). Bench scores with the gate's
//! verdicts and judge registers and holds none of them, so the gate stays the
//! one decider and bench copies no threshold, prompt or parser.
//!
//! One exec per request, through eval_cmd's `run_probe`, the one path by
//! which bench asks svrn's probe anything.

use sovereign_cli_base::chat_globals::{default_globals_for_voice_eval, ChatGlobals};
use sovereign_contracts::probe::{
    AssertedValueVerdict, AssessAnswer, AssessEvidence, AssessOp, AssessProbe, JudgeAnswer,
    JudgeEvidence, JudgeOp, JudgeProbe, ProbeEvidence, ProbeMode, ProbeRequest,
};

use crate::eval_cmd::probe_score::run_probe;

/// The daemon a bench's judge calls are pinned on, and its provider's
/// context window: what the bench's own `RemoteApiProvider` carried.
#[derive(Debug, Clone)]
pub(crate) struct SvrnJudge {
    base_url: String,
    context: u32,
}

impl SvrnJudge {
    /// Judge calls against the daemon at `base_url` (no `/v1`).
    pub(crate) fn new(base_url: &str, context: u32) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            context,
        }
    }

    fn globals(&self) -> ChatGlobals {
        let mut globals = default_globals_for_voice_eval();
        globals.daemon_base = self.base_url.clone();
        globals.daemon_explicit = true;
        globals
    }

    async fn ask(&self, request: ProbeRequest) -> Result<ProbeEvidence, String> {
        let globals = self.globals();
        tokio::task::spawn_blocking(move || run_probe(&globals, &request))
            .await
            .map_err(|e| format!("probe task: {e}"))?
    }

    fn request(mode: ProbeMode) -> ProbeRequest {
        ProbeRequest {
            mode,
            questions: Vec::new(),
            corpus: String::new(),
            limit: 0,
            isolate: false,
            atlas: None,
            attached: None,
            vault: None,
            assess: None,
            judge: None,
        }
    }

    /// The judge registers, `model` pinned (`None` = svrn's Critic profile's).
    pub(crate) async fn judge(
        &self,
        model: Option<&str>,
        ops: Vec<JudgeOp>,
    ) -> Result<JudgeEvidence, String> {
        let n = ops.len();
        let request = ProbeRequest {
            judge: Some(JudgeProbe {
                model: model.map(str::to_string),
                context: self.context,
                ops,
            }),
            ..Self::request(ProbeMode::Judge)
        };
        match self.ask(request).await? {
            ProbeEvidence::Judge(ev) if ev.rows.len() == n => Ok(*ev),
            ProbeEvidence::Judge(ev) => Err(format!(
                "the judge probe answered {} of {n} ops",
                ev.rows.len()
            )),
            other => Err(format!(
                "the judge probe answered {}",
                other.mode().as_str()
            )),
        }
    }

    /// The gate's verdicts, `model` pinned (`None` = svrn's Critic profile's).
    pub(crate) async fn assess(
        &self,
        model: Option<&str>,
        ops: Vec<AssessOp>,
    ) -> Result<AssessEvidence, String> {
        let n = ops.len();
        let request = ProbeRequest {
            assess: Some(AssessProbe {
                model: model.map(str::to_string),
                context: self.context,
                ops,
            }),
            ..Self::request(ProbeMode::Assess)
        };
        match self.ask(request).await? {
            ProbeEvidence::Assess(ev) if ev.rows.len() == n => Ok(*ev),
            ProbeEvidence::Assess(ev) => Err(format!(
                "the assess probe answered {} of {n} ops",
                ev.rows.len()
            )),
            other => Err(format!(
                "the assess probe answered {}",
                other.mode().as_str()
            )),
        }
    }

    /// The critic model a run's calls pin (`model`, or svrn's Critic
    /// profile's when `None`) and the grounding-gate threshold as svrn
    /// resolves it: an assess probe with no ops.
    pub(crate) async fn critic_and_threshold(
        &self,
        model: Option<&str>,
    ) -> Result<(String, f64), String> {
        let ev = self.assess(model, Vec::new()).await?;
        Ok((ev.model, ev.gate_threshold))
    }

    /// The gate's `released_pure_decline` over `answer`; `None` when the
    /// probe could not run, named on stderr.
    pub(crate) async fn pure_decline(&self, answer: &str) -> Option<bool> {
        let op = AssessOp::PureDecline {
            answer: answer.to_string(),
        };
        match self.assess(None, vec![op]).await {
            Ok(ev) => match ev.rows.into_iter().next() {
                Some(AssessAnswer::PureDecline { pure }) => Some(pure),
                _ => None,
            },
            Err(e) => {
                eprintln!("    [assess] svrn probe failed: {e}");
                None
            }
        }
    }

    /// The gate's `extract_claim_list`: `Ok(Some(claims))`, `Ok(None)` on an
    /// inference failure (the primitive's own), `Err` when the probe could
    /// not run.
    pub(crate) async fn claim_list(
        &self,
        model: Option<&str>,
        question: &str,
        answer: &str,
        max_claims: usize,
    ) -> Result<Option<Vec<String>>, String> {
        let op = JudgeOp::ClaimList {
            question: question.to_string(),
            answer: answer.to_string(),
            max_claims,
        };
        match self.judge(model, vec![op]).await?.rows.into_iter().next() {
            Some(JudgeAnswer::ClaimList { claims }) => Ok(claims),
            other => Err(format!("the judge probe answered {other:?} to ClaimList")),
        }
    }

    /// The gate's `claim_chunk_support`: support in [0,1], or `None` on a
    /// judge failure or when the probe could not run (named on stderr).
    pub(crate) async fn claim_chunk_support(
        &self,
        model: &str,
        passage: &str,
        claim: &str,
    ) -> Option<f64> {
        let op = JudgeOp::ClaimChunkSupport {
            passage: passage.to_string(),
            claim: claim.to_string(),
        };
        match self.judge_one(model, op).await? {
            JudgeAnswer::ClaimChunkSupport { support } => support,
            _ => None,
        }
    }

    /// One judge op and its one answer. A probe that could not run is the
    /// op's own failure (`None` / `Err`), named on stderr.
    pub(crate) async fn judge_one(&self, model: &str, op: JudgeOp) -> Option<JudgeAnswer> {
        match self.judge(Some(model), vec![op]).await {
            Ok(ev) => ev.rows.into_iter().next(),
            Err(e) => {
                eprintln!("    [judge] svrn probe failed: {e}");
                None
            }
        }
    }

    /// `forced_choice_ab` in the gate's register: `(p_A, p_B)`, or `None`
    /// when the pass (or the probe) failed.
    pub(crate) async fn forced_choice(
        &self,
        model: &str,
        register: &str,
        prompt: &str,
    ) -> Option<(f64, f64)> {
        let op = JudgeOp::ForcedChoice {
            register: register.to_string(),
            prompt: prompt.to_string(),
        };
        match self.judge_one(model, op).await? {
            JudgeAnswer::ForcedChoice { a_b } => a_b,
            _ => None,
        }
    }

    /// The gate's asserted-value verdict over `chunks`, on `model`. A probe
    /// that could not run is `NoValue`, the could-not-judge bucket, named on
    /// stderr.
    pub(crate) async fn asserted_value(
        &self,
        model: &str,
        question: &str,
        answer: &str,
        chunks: &[String],
    ) -> AssertedValueVerdict {
        let op = AssessOp::AssertedValue {
            question: question.to_string(),
            answer: answer.to_string(),
            chunks: chunks.to_vec(),
        };
        match self.assess(Some(model), vec![op]).await {
            Ok(ev) => match ev.rows.into_iter().next() {
                Some(AssessAnswer::AssertedValue { verdict }) => verdict,
                _ => AssertedValueVerdict::NoValue,
            },
            Err(e) => {
                eprintln!("    [assess] svrn probe failed: {e}");
                AssertedValueVerdict::NoValue
            }
        }
    }
}
