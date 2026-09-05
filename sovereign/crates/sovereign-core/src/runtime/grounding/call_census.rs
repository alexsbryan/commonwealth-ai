// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-call census — **the one funnel every gate model call goes
//! through**, so no call the gate issues can be anonymous.
//!
//! # Why this exists
//!
//! A gate turn's cost was reconstructable but not ATTRIBUTABLE. The census
//! script (`~/.sovereign/comaintainer/gate-census.py`) joins the grounding
//! journal against the daemon's `routing outcome` lines and produces a
//! per-call table of durations and output sizes — which is how the gate's
//! dominant cost class was found: **16-22 second calls emitting 4-745
//! characters**, i.e. almost pure prefill of the evidence window (note
//! 221b3b71, FINDING 3). What that reconstruction could not say is WHICH
//! MECHANISM paid. Two candidates fit the shape and the fix for them is
//! different, so tuning without naming the owner would have been the
//! mole-whacking ARCH §0 forbids.
//!
//! This module closes that. Every gate model call is issued through
//! [`gate_call`], which times it, names it with a
//! [`GateCallMechanism`], and appends a row to the turn's census. The rows
//! ride out on the gate's own journal line, so the census script reads
//! them from a stream it already opens — an exact join by construction,
//! not a timestamp heuristic.
//!
//! # Why this is NOT the stage ledger
//!
//! `runtime/stage_ledger.rs` records one row per STAGE and computes the
//! in-gate residual as `gate_ms - sum(rows)`. Per-call rows are a finer
//! grain of the same seconds: pushing them into that ledger would make the
//! attributed sum exceed the gate wall clock and saturate the residual to
//! zero, destroying the detector that catches an unrecorded mechanism.
//! The two instruments answer different questions and are kept separate on
//! purpose — the stage ledger says *which stage*, this says *which call*.
//!
//! # Ambient, for the same reason the stage ledger is
//!
//! Threading a census handle through `gate_answer` → `gate_answer_inner` →
//! `gate_longform` → `judge::*` → `surgical::*` is a signature change on
//! every function in the ladder for a value none of them read. The census
//! is therefore a task-local, installed once by
//! [`grounding::gate_answer_with_progress`] — the same funnel that owns the
//! gate's wall clock and writes the journal line.
//!
//! **The honest limit, stated rather than discovered.** A task-local does
//! not cross `tokio::spawn`. The gate's concurrency is
//! `futures::future::join_all`, which polls in-task, and no gate path
//! spawns (verified by inspection: no `tokio::spawn` in `grounding/`, only
//! `spawn_blocking` for the journal append, which issues no model call).
//! If a future gate stage is moved onto a spawned task, its calls silently
//! stop being recorded — and that does not vanish quietly either: the
//! arithmetic `sum(calls.ms)` vs `gate_ms` on the journal line is the
//! detector, and the wire type's docs say so.
//!
//! # Absence
//!
//! Surfaces that do not open a census (every non-gate caller, every test
//! that calls a judge directly) record into nothing and the line carries an
//! empty `calls` vec — which is why `GroundingDecisionLine::calls`
//! documents empty as ambiguous and names the arithmetic that resolves it,
//! rather than letting a reader read it as "this turn was free"
//! (ARCH §18.3).

use std::sync::{Arc, Mutex};

use sovereign_contracts::types::{
    GateCallMechanism, GateCallRow, JudgeFailure, JudgeFailureReason,
};

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, CompletionResponse};

/// Is this mechanism a JUDGEMENT, as opposed to synthesis or repair?
///
/// The `judge_failure` counts are about the gate's ability to reach a
/// verdict, so a failed rewrite or surgery must not inflate them: those
/// calls WRITE text, they do not judge it. Exhaustive on purpose — a new
/// mechanism has to be classified here before it can be counted, which is
/// the same property that makes `GateCallMechanism` itself exhaustive.
const fn is_judging(m: GateCallMechanism) -> bool {
    match m {
        GateCallMechanism::ClaimExtraction
        | GateCallMechanism::ClaimList
        | GateCallMechanism::PerClaimJudge
        | GateCallMechanism::ChunkJudge
        | GateCallMechanism::SpecificsScan
        | GateCallMechanism::BatchedSupport
        | GateCallMechanism::LocatedSpanTriage
        | GateCallMechanism::SentenceSweep => true,
        GateCallMechanism::Surgery
        | GateCallMechanism::Rewrite
        | GateCallMechanism::Retry
        | GateCallMechanism::ShortGuardRetry
        | GateCallMechanism::Citation => false,
    }
}

/// Why a `judge_failed_open` turn shipped unverified, derived from the
/// turn's own census — never asserted, and never defaulted when the census
/// cannot say (ARCH §18.3).
///
/// The two derived reasons are arithmetic, not guesses: every judging call
/// answered and the ladder still reached no verdict is a PARSE gap, not an
/// availability failure, and it takes a different fix from a shed call.
#[must_use]
pub(crate) fn judge_failure_of(
    rows: &[GateCallRow],
    failures: &[JudgeFailureReason],
) -> JudgeFailure {
    let judged: Vec<&GateCallRow> = rows.iter().filter(|r| is_judging(r.mechanism)).collect();
    let attempted = judged.len() as u32;
    let answered = judged.iter().filter(|r| r.ok).count() as u32;
    // The FIRST recorded failure reason — the one that started the
    // cascade; later ones are usually the same host still refusing.
    let reason = match failures.first() {
        Some(r) => *r,
        None if attempted == 0 => JudgeFailureReason::NoJudgeCall,
        None => JudgeFailureReason::VerdictUnparseable,
    };
    JudgeFailure {
        reason,
        calls_attempted: attempted,
        calls_answered: answered,
    }
}

tokio::task_local! {
    /// The turn's census, installed by [`CallCensus::scope`].
    static TURN_CALLS: CallCensus;
}

/// A gate turn's accumulating call rows.
///
/// Cheap to clone (one `Arc`); cloning shares the same rows, which is what
/// makes "one census per turn" true across every mechanism in the ladder.
#[derive(Clone)]
pub(crate) struct CallCensus {
    rows: Arc<Mutex<Vec<GateCallRow>>>,
    /// The classified reason of every judging call that came back `Err`,
    /// in the order they failed. `GateCallRow::ok` says a call failed;
    /// this says WHY, which is the whole difference between "the gate
    /// fell open again" and a fix.
    failures: Arc<Mutex<Vec<JudgeFailureReason>>>,
    /// When the gate window opened — `start_offset_ms` is measured from
    /// here, so a reader can lay the calls on a timeline without joining
    /// to any other stream.
    opened: std::time::Instant,
}

impl CallCensus {
    pub(crate) fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(Vec::new())),
            failures: Arc::new(Mutex::new(Vec::new())),
            opened: std::time::Instant::now(),
        }
    }

    /// Run `fut` with this census installed as the ambient one.
    pub(crate) async fn scope<F: std::future::Future>(self, fut: F) -> F::Output {
        TURN_CALLS.scope(self, fut).await
    }

    /// Take the rows (in start order) and the failure reasons (in failure
    /// order). Consumes the census: a turn's census is read exactly once,
    /// by the funnel that journals the turn. The two travel together
    /// because the reason is only interpretable against the counts.
    pub(crate) fn take(self) -> (Vec<GateCallRow>, Vec<JudgeFailureReason>) {
        let mut rows = self.rows.lock().map(|r| r.clone()).unwrap_or_default();
        rows.sort_by_key(|r| r.start_offset_ms);
        let failures = self.failures.lock().map(|f| f.clone()).unwrap_or_default();
        (rows, failures)
    }
}

/// Issue one gate model call, named and costed.
///
/// **Every model call the grounding gate makes goes through here.** A call
/// site that reaches `inference.complete` directly is a call the census
/// cannot see, and the ladder's own cost stops adding up — which is the
/// exact failure this funnel exists to make impossible (ARCH §10.6, one
/// decider one name).
///
/// The row is recorded on BOTH arms. A failed call still burned wall time,
/// and a mechanism whose calls are erroring is a finding to surface, not an
/// absence to swallow (ARCH §18.3).
pub(crate) async fn gate_call(
    inference: &dyn InferenceProvider,
    req: &CompletionRequest,
    mechanism: GateCallMechanism,
) -> crate::error::Result<CompletionResponse> {
    let started = std::time::Instant::now();
    let out = inference.complete(req).await;
    let ms = started.elapsed().as_millis() as u64;
    let prompt_chars = req.prompt.chars().count();
    let out_chars = out.as_ref().map(|r| r.text.chars().count()).unwrap_or(0);
    let ok = out.is_ok();
    // Glassbox at debug: the same facts the journal row carries, available
    // on a live tail without waiting for the turn to close (ARCH §9).
    tracing::debug!(
        target: "grounding_gate",
        mechanism = mechanism.label(),
        ms,
        prompt_chars,
        out_chars,
        ok,
        stable_prefix_bytes = ?req.stable_prefix_len,
        "gate model call"
    );
    // WHY it failed, not just that it did — classified from the error's
    // own variant, and emitted with the provider's message so a live tail
    // carries the detail the metadata-only journal line must not (ARCH §9,
    // and the journal's "counts, never text" rule).
    if let Err(e) = &out {
        let reason = JudgeFailureReason::from_error(e);
        tracing::warn!(
            target: "grounding_gate",
            event = "gate_call_failed",
            mechanism = mechanism.label(),
            reason = reason.label(),
            judging = is_judging(mechanism),
            ms,
            detail = %e,
            "gate model call failed"
        );
        if is_judging(mechanism) {
            let _ = TURN_CALLS.try_with(|c| {
                if let Ok(mut f) = c.failures.lock() {
                    f.push(reason);
                }
            });
        }
    }
    let _ = TURN_CALLS.try_with(|c| {
        let start_offset_ms = started
            .saturating_duration_since(c.opened)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        if let Ok(mut rows) = c.rows.lock() {
            rows.push(GateCallRow {
                mechanism,
                ms,
                prompt_chars,
                out_chars,
                ok,
                stable_prefix_bytes: req.stable_prefix_len,
                start_offset_ms,
            });
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::types::{Depth, ProviderCapabilities, Speed};
    use futures::Stream;
    use std::pin::Pin;

    struct Echo {
        fail: bool,
    }

    #[async_trait::async_trait]
    impl InferenceProvider for Echo {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            if self.fail {
                return Err(crate::error::Error::Inference("nope".into()));
            }
            Ok(CompletionResponse {
                text: format!("<{}>", r.prompt.len()),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "census-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unimplemented!("no stream in census tests")
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            unimplemented!("no embed in census tests")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn req(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn outside_a_census_the_call_still_runs_and_records_nothing() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(Echo { fail: false });
        // No panic, no row, the answer still comes back — the honest state
        // for every non-gate caller.
        let out = gate_call(&*inf, &req("hello"), GateCallMechanism::SpecificsScan)
            .await
            .unwrap();
        assert_eq!(out.text, "<5>");
    }

    #[tokio::test]
    async fn every_call_lands_named_in_start_order() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(Echo { fail: false });
        let census = CallCensus::new();
        let rows = census
            .clone()
            .scope(async {
                gate_call(&*inf, &req("aaaa"), GateCallMechanism::ClaimList)
                    .await
                    .unwrap();
                gate_call(&*inf, &req("bb"), GateCallMechanism::SpecificsScan)
                    .await
                    .unwrap();
                census.clone().take().0
            })
            .await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].mechanism, GateCallMechanism::ClaimList);
        assert_eq!(rows[0].prompt_chars, 4);
        assert_eq!(rows[0].out_chars, 3); // "<4>"
        assert_eq!(rows[1].mechanism, GateCallMechanism::SpecificsScan);
        assert!(rows[1].start_offset_ms >= rows[0].start_offset_ms);
    }

    /// A failing call is time the turn paid. It gets a row, flagged — the
    /// alternative is a census that silently under-reports exactly when the
    /// gate is misbehaving.
    #[tokio::test]
    async fn a_failed_call_is_recorded_not_swallowed() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(Echo { fail: true });
        let census = CallCensus::new();
        let rows = census
            .clone()
            .scope(async {
                assert!(gate_call(&*inf, &req("x"), GateCallMechanism::Surgery)
                    .await
                    .is_err());
                census.clone().take()
            })
            .await;
        let (rows, failures) = rows;
        // Surgery WRITES text, it does not judge it — a failed rewrite
        // must not be reported as the gate failing to reach a verdict.
        assert!(
            failures.is_empty(),
            "a synthesis call is not a judge failure"
        );
        assert_eq!(rows.len(), 1, "a failed call still burned wall time");
        assert!(!rows[0].ok);
        assert_eq!(rows[0].out_chars, 0);
    }

    /// **The funnel invariant, enforced structurally rather than
    /// remembered** (ARCH §7/§10). A new gate mechanism that calls
    /// `inference.complete` directly would be invisible to the census, and
    /// the failure mode is the quiet one: the turn still works, the numbers
    /// just stop adding up. So the sources are read at compile time and the
    /// only legal `.complete(` in `grounding/` is this module's own.
    ///
    /// `include_str!` rather than a filesystem walk on purpose: it is
    /// resolved by the compiler relative to THIS file, so it cannot go
    /// stale against a moved module or pass vacuously in a different
    /// working directory.
    /// covers: GR-50
    #[test]
    fn no_gate_module_calls_inference_complete_behind_the_censuss_back() {
        const SOURCES: [(&str, &str); 6] = [
            ("mod.rs", include_str!("mod.rs")),
            ("judge.rs", include_str!("judge.rs")),
            ("surgical.rs", include_str!("surgical.rs")),
            ("citation.rs", include_str!("citation.rs")),
            (
                "citation_attribution.rs",
                include_str!("citation_attribution.rs"),
            ),
            ("pipeline.rs", include_str!("pipeline.rs")),
        ];
        for (name, src) in SOURCES {
            // Skip the test modules: mocks legitimately IMPLEMENT
            // `complete`, and a test driving a provider directly is not a
            // gate call. Production code is everything above `mod tests`.
            let prod = src.split("\nmod tests {").next().unwrap_or(src);
            let prod = prod.split("\n#[cfg(test)]").next().unwrap_or(prod);
            for (i, line) in prod.lines().enumerate() {
                let l = line.trim_start();
                if l.starts_with("//") || l.starts_with("///") {
                    continue;
                }
                assert!(
                    !line.contains("inference.complete(") && !line.contains("inference.complete_"),
                    "{name}:{} calls the provider directly, bypassing the per-call census — \
                     route it through `call_census::gate_call` with a `GateCallMechanism`: {line}",
                    i + 1
                );
            }
        }
    }

    fn row(mechanism: GateCallMechanism, ok: bool) -> GateCallRow {
        GateCallRow {
            mechanism,
            ms: 1,
            prompt_chars: 1,
            out_chars: usize::from(ok),
            ok,
            stable_prefix_bytes: None,
            start_offset_ms: 0,
        }
    }

    /// The reason is a MEASUREMENT off the turn's own census, so each of
    /// the three shapes has a failing input named beside it (ARCH §18.1):
    /// swap any arm's expectation and this test says which.
    #[test]
    fn the_fail_open_reason_is_derived_from_the_turn_and_never_defaulted() {
        // (a) the provider refused: the recorded reason wins outright.
        let jf = judge_failure_of(
            &[
                row(GateCallMechanism::ClaimList, true),
                row(GateCallMechanism::PerClaimJudge, false),
            ],
            &[JudgeFailureReason::QueueShed],
        );
        assert_eq!(jf.reason, JudgeFailureReason::QueueShed);
        assert_eq!((jf.calls_attempted, jf.calls_answered), (2, 1));

        // (b) every judging call ANSWERED and the ladder still reached no
        // verdict — a parse gap, which no retry or priority can fix. This
        // is the arm that would silently read as "shed" if the reason were
        // assumed rather than derived.
        let jf = judge_failure_of(
            &[
                row(GateCallMechanism::ClaimList, true),
                row(GateCallMechanism::PerClaimJudge, true),
            ],
            &[],
        );
        assert_eq!(jf.reason, JudgeFailureReason::VerdictUnparseable);
        assert_eq!((jf.calls_attempted, jf.calls_answered), (2, 2));

        // (c) nothing was even asked. A rewrite is not a judgement, so a
        // turn whose only call was a rewrite has attempted ZERO judges.
        let jf = judge_failure_of(&[row(GateCallMechanism::Rewrite, true)], &[]);
        assert_eq!(jf.reason, JudgeFailureReason::NoJudgeCall);
        assert_eq!((jf.calls_attempted, jf.calls_answered), (0, 0));
    }

    /// A judging call that fails records its reason; the same failure on a
    /// synthesis mechanism records none. Watched by the assertion inside
    /// `a_failed_call_is_recorded_not_swallowed` for the negative half.
    #[tokio::test]
    async fn a_failed_judge_call_records_why_it_failed() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(Echo { fail: true });
        let census = CallCensus::new();
        let (rows, failures) = census
            .clone()
            .scope(async {
                let _ = gate_call(&*inf, &req("x"), GateCallMechanism::PerClaimJudge).await;
                census.clone().take()
            })
            .await;
        assert_eq!(rows.len(), 1);
        // `Echo` fails with `Error::Inference`, and the classifier reads
        // the VARIANT — not the message "nope".
        assert_eq!(failures, vec![JudgeFailureReason::Inference]);
    }

    /// The census must see calls made from inside a `join_all` fan-out —
    /// that is how the per-claim judges run, and it is the whole reason a
    /// task-local (rather than a threaded parameter) is sound here.
    #[tokio::test]
    async fn a_join_all_fan_out_is_visible_to_the_census() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(Echo { fail: false });
        let census = CallCensus::new();
        let rows = census
            .clone()
            .scope(async {
                let futs = (0..4).map(|i| {
                    let inf = inf.clone();
                    async move {
                        gate_call(
                            &*inf,
                            &req(&"c".repeat(i + 1)),
                            GateCallMechanism::PerClaimJudge,
                        )
                        .await
                    }
                });
                let _ = futures::future::join_all(futs).await;
                census.clone().take().0
            })
            .await;
        assert_eq!(rows.len(), 4, "in-task fan-out must not lose rows");
        assert!(rows
            .iter()
            .all(|r| r.mechanism == GateCallMechanism::PerClaimJudge));
    }
}
