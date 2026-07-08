// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared budget-retry loop for typed-extension LLM calls.
//!
//! Three sites duplicated this loop pre-lift:
//! - `sovereign_cli_llm::enrich_cmd::extract_typed` (chapter-level
//!   typed dispatcher)
//! - `sovereign_tools::typed_extension::pass::call_argumentative`
//!   (RAPTOR-leaf typed extraction)
//! - `corpus_engine::enrichment::pipeline::runner::phase_1_*` (still
//!   uses its own richer RetryMode enum — not migrated; see below)
//!
//! Lives in `sovereign-tools` rather than `corpus-engine` because
//! the dep direction is one-way: `corpus-engine` cannot depend on
//! `sovereign-core` (ARCH §8.3), so types like `InferenceProvider`
//! / `CompletionRequest` are not reachable from corpus-engine.
//! `sovereign-tools` already depends on both crates and is the
//! natural home for code that orchestrates LLM calls against
//! corpus-engine's typed-extension schemas.
//!
//! The pattern: try a chat call with a tight initial budget; on
//! parse failure retry once at a doubled budget; on a second parse
//! failure surface a `ParseExhausted` error. A chat-call failure
//! (transport, model load, etc.) short-circuits with `Chat`.
//!
//! Constants [`TYPED_BUDGET_INITIAL`] = 4096 and
//! [`TYPED_BUDGET_RETRY`] = 8192 reflect the empirical floor +
//! ceiling for argumentative typed extension on a Slow-slot model:
//! the typical section closes its JSON envelope under 4K decode
//! tokens; the long tail occasionally needs 8K. A third doubling
//! beyond that is a real content miss, not a budget issue.
//!
//! `runner.rs::phase_1_extract_questions_with_retry` is
//! deliberately NOT migrated to this helper. It carries a richer
//! `RetryMode` enum (Terse / Wide / …) that selects alternate prompt
//! variants — collapsing that into the simple two-budget shape
//! would lose capability. Tracked as v2.x consolidation.

use std::future::Future;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

/// Tight initial decode budget. Sized so most typed-extension
/// responses close their JSON envelope on the first attempt; the
/// minority that truncate get one retry at [`TYPED_BUDGET_RETRY`].
pub const TYPED_BUDGET_INITIAL: usize = 4096;

/// Retry budget when the tight initial call returned a parse-drift
/// response. Doubles the budget so the model has room to close the
/// JSON envelope. A second parse failure at this budget is a real
/// content miss, not a budget issue, and gets surfaced as
/// `TypedCallError::ParseExhausted`.
pub const TYPED_BUDGET_RETRY: usize = 8192;

/// Default budget escalation: tight-then-double. Three sites today
/// use this exact sequence; centralised so a bench-driven tuning
/// move propagates atomically to every consumer.
pub const DEFAULT_TYPED_BUDGETS: &[usize] = &[TYPED_BUDGET_INITIAL, TYPED_BUDGET_RETRY];

/// One typed-extension LLM call, parameterised by budget escalation,
/// system prompt, JSON schema, and sampling profile.
///
/// Callers supply two closures:
///
/// - **prompt builder** (`build_user`): takes the budget for this
///   attempt and returns the user prompt body. Most callers
///   construct the body once outside the helper and ignore the
///   budget arg; chapter-level dispatchers
///   (`extract_typed::compose_typed_prompt`) rebuild per-budget
///   because their prompt carries `max_output_tokens` inline.
/// - **parser** (`parse`): takes the model's response text and
///   returns either the typed extension `T` or a parse error `P`.
///
/// The helper owns: budget iteration, request construction, chat
/// dispatch, parse-or-retry decision, and tracing of retry-on-success.
pub struct TypedLlmCall<'a> {
    pub system: &'a str,
    pub schema: serde_json::Value,
    pub speed: Speed,
    pub temperature: Option<f32>,
    pub think_budget: Option<usize>,
    pub budgets: &'a [usize],
    pub trace_subject: Option<String>,
}

impl<'a> TypedLlmCall<'a> {
    /// Convenience constructor — schema + system + defaults.
    /// Callers tune fields directly via field-init shorthand.
    pub fn new(system: &'a str, schema: serde_json::Value) -> Self {
        Self {
            system,
            schema,
            speed: Speed::Slow,
            temperature: Some(0.2),
            think_budget: Some(0),
            budgets: DEFAULT_TYPED_BUDGETS,
            trace_subject: None,
        }
    }

    /// Drive the call. Returns the parsed `T` plus the number of
    /// attempts that actually fired (1 or 2 in the default
    /// configuration). The attempt count lets callers render
    /// retry-on-success markers (e.g. the `↑` suffix
    /// `enrich extract-typed` uses).
    pub async fn run<T, P, B, BFut, Pf>(
        &self,
        inference: &Arc<dyn InferenceProvider>,
        build_user: B,
        parse: Pf,
    ) -> Result<TypedCallReport<T>, TypedCallError<P>>
    where
        B: Fn(usize) -> BFut,
        BFut: Future<Output = String>,
        Pf: Fn(&str) -> Result<T, P>,
    {
        let mut last_parse_err: Option<P> = None;
        for (attempt_idx, &budget) in self.budgets.iter().enumerate() {
            let attempt = attempt_idx + 1;
            let user = build_user(budget).await;
            // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): NOT migrated to the
            // `Workload` resolver in P1. `speed`/`think_budget`/`temperature`
            // are caller-set `pub` struct fields (line 80/82/81) that this
            // generic helper reads at runtime (`self.speed` below); binding one
            // static `Workload` would remove that configurability and can't
            // reproduce a Fast override. The default (Slow + Some(0)) is
            // ExtractDurable-shaped and the sole live caller
            // (`typed_extension::pass`) keeps that default. P5 owns the resolve:
            // add a `Workload` field to `TypedLlmCall` (or split callers), then
            // route via `for_workload`.
            let request = CompletionRequest {
                prompt: user,
                system_message: Some(self.system.to_string()),
                preferred_speed: self.speed,
                max_tokens: Some(budget),
                temperature: self.temperature,
                structured_output: Some(self.schema.clone()),
                think_budget: self.think_budget,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
            };
            let response = inference.complete(&request).await.map_err(|e| {
                if let Some(subj) = self.trace_subject.as_ref() {
                    tracing::warn!(
                        subject = %subj,
                        attempt,
                        error = %e,
                        "typed_call: chat call failed"
                    );
                }
                TypedCallError::Chat {
                    attempt,
                    message: format!("{e}"),
                }
            })?;
            match parse(&response.text) {
                Ok(value) => {
                    if attempt > 1 {
                        if let Some(subj) = self.trace_subject.as_ref() {
                            tracing::debug!(
                                subject = %subj,
                                attempts = attempt,
                                "typed_call: parse succeeded on retry"
                            );
                        }
                    }
                    return Ok(TypedCallReport {
                        value,
                        attempts: attempt,
                    });
                }
                Err(e) => {
                    last_parse_err = Some(e);
                    // Loop continues to retry budget on next iter.
                }
            }
        }
        let attempts = self.budgets.len();
        let last = last_parse_err.expect(
            "typed_call invariant: when every retry fails without a chat error, the final \
             parse error must be populated (budgets array must be non-empty for this code path)",
        );
        Err(TypedCallError::ParseExhausted { attempts, last })
    }
}

/// Successful return from [`TypedLlmCall::run`].
#[derive(Debug, Clone)]
pub struct TypedCallReport<T> {
    /// The parsed typed-extension value.
    pub value: T,
    /// Number of attempts that actually fired. `1` when the initial
    /// budget succeeded; `2` when the retry was needed. Callers use
    /// this to render retry-on-success annotations in operator
    /// output.
    pub attempts: usize,
}

/// Failure modes from [`TypedLlmCall::run`].
///
/// Generic over the caller's parse-error type `P` so each consumer
/// surfaces its own structured error (e.g. `TypedDispatchError` in
/// `enrich extract-typed`) without coercing through a string.
#[derive(Debug, Clone)]
pub enum TypedCallError<P> {
    /// Chat-call transport / model failure on the named attempt
    /// (1-based). Short-circuits — the helper does not retry chat
    /// errors (they're not parse drift, they're infra failure).
    Chat { attempt: usize, message: String },
    /// Every budget retry returned a response that the parser
    /// rejected. Carries the parse error from the final attempt;
    /// earlier attempts' errors are dropped (they would always be
    /// strictly more truncated than the final one).
    ParseExhausted { attempts: usize, last: P },
}

impl<P: std::fmt::Display> std::fmt::Display for TypedCallError<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypedCallError::Chat { attempt, message } => {
                write!(f, "chat error (attempt {attempt}): {message}")
            }
            TypedCallError::ParseExhausted { attempts, last } => {
                write!(f, "parse exhausted after {attempts} attempts: {last}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sovereign_core::error::{Error, Result as SovResult};
    use sovereign_core::types::{CompletionResponse, Depth, ProviderCapabilities};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Scriptable inference: returns the i-th item from a vec of
    /// canned responses, one per call. Lets tests assert exactly
    /// how many calls fired AND have different responses per
    /// attempt (e.g. fail-then-succeed retry).
    struct ScriptedInference {
        responses: Mutex<Vec<SovResult<String>>>,
        call_count: AtomicUsize,
    }

    impl ScriptedInference {
        fn new(responses: Vec<SovResult<String>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl InferenceProvider for ScriptedInference {
        async fn complete(&self, _req: &CompletionRequest) -> SovResult<CompletionResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(Error::Inference(
                    "scripted inference: ran out of responses".into(),
                ));
            }
            let next = q.remove(0)?;
            Ok(CompletionResponse {
                text: next,
                tokens_used: 1,
                prompt_tokens: 1,
                model_id: "scripted".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> SovResult<Pin<Box<dyn futures::Stream<Item = SovResult<String>> + Send>>> {
            Err(Error::NotImplemented("scripted: no stream".into()))
        }
        async fn embed(&self, _t: &str) -> SovResult<Vec<f32>> {
            Err(Error::NotImplemented("scripted: no embed".into()))
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Deep,
            }
        }
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn parse_json(s: &str) -> Result<serde_json::Value, String> {
        serde_json::from_str(s).map_err(|e| format!("{e}"))
    }

    #[tokio::test]
    async fn initial_budget_success_fires_one_call() {
        let inf: Arc<dyn InferenceProvider> =
            Arc::new(ScriptedInference::new(vec![Ok(r#"{"k":"v"}"#.into())]));
        let call = TypedLlmCall::new("sys", schema());
        let report = call
            .run(&inf, |_b| async { "user".to_string() }, parse_json)
            .await
            .expect("first attempt parses cleanly");
        assert_eq!(report.attempts, 1);
        assert_eq!(report.value["k"], "v");
    }

    #[tokio::test]
    async fn parse_failure_retries_at_doubled_budget() {
        let scripted = Arc::new(ScriptedInference::new(vec![
            Ok("not json".into()),
            Ok(r#"{"k":"recovered"}"#.into()),
        ]));
        let inf: Arc<dyn InferenceProvider> = scripted.clone();
        let call = TypedLlmCall::new("sys", schema());
        let report = call
            .run(&inf, |_b| async { "user".to_string() }, parse_json)
            .await
            .expect("retry attempt parses cleanly");
        assert_eq!(report.attempts, 2);
        assert_eq!(report.value["k"], "recovered");
        assert_eq!(scripted.calls(), 2);
    }

    #[tokio::test]
    async fn budget_passed_into_builder_changes_each_attempt() {
        let scripted = Arc::new(ScriptedInference::new(vec![
            Ok("not json".into()),
            Ok(r#"{"k":"ok"}"#.into()),
        ]));
        let inf: Arc<dyn InferenceProvider> = scripted.clone();
        let seen_budgets = Arc::new(Mutex::new(Vec::<usize>::new()));
        let call = TypedLlmCall::new("sys", schema());
        let captured = Arc::clone(&seen_budgets);
        let _ = call
            .run(
                &inf,
                |b| {
                    let captured = Arc::clone(&captured);
                    async move {
                        captured.lock().unwrap().push(b);
                        format!("user-{b}")
                    }
                },
                parse_json,
            )
            .await
            .expect("retry succeeds");
        let observed = seen_budgets.lock().unwrap().clone();
        assert_eq!(observed, vec![TYPED_BUDGET_INITIAL, TYPED_BUDGET_RETRY]);
    }

    #[tokio::test]
    async fn both_attempts_fail_returns_parse_exhausted() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(ScriptedInference::new(vec![
            Ok("not json".into()),
            Ok("still not json".into()),
        ]));
        let call = TypedLlmCall::new("sys", schema());
        let err = call
            .run(&inf, |_b| async { "user".to_string() }, parse_json)
            .await
            .expect_err("both parses fail");
        match err {
            TypedCallError::ParseExhausted { attempts, last } => {
                assert_eq!(attempts, 2);
                assert!(
                    last.contains("expected"),
                    "carries the last parse error message"
                );
            }
            other => panic!("expected ParseExhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_failure_short_circuits_without_retry() {
        let scripted = Arc::new(ScriptedInference::new(vec![Err(Error::Inference(
            "transport boom".into(),
        ))]));
        let inf: Arc<dyn InferenceProvider> = scripted.clone();
        let call = TypedLlmCall::new("sys", schema());
        let err = call
            .run(&inf, |_b| async { "user".to_string() }, parse_json)
            .await
            .expect_err("chat error short-circuits");
        match err {
            TypedCallError::Chat { attempt, message } => {
                assert_eq!(attempt, 1, "chat error fires before retry");
                assert!(message.contains("transport boom"));
            }
            other => panic!("expected Chat, got {other:?}"),
        }
        // Critical: chat errors must NOT trigger a retry.
        assert_eq!(scripted.calls(), 1);
    }
}
