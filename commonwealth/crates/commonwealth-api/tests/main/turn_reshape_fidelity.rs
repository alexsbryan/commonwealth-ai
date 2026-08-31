// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the reshape gate is allowed to do to a served turn.
//!
//! `POST /v1/chat/completions` is advertised as OpenAI-compatible, and
//! between the client's request and the model sit three passes that
//! [`turn_fidelity::reshape_enabled`] governs. Their effect is not
//! small. On gym fixture 007 the client sends twelve messages and the
//! model sees four: `apply_read_attractor_nudge_chat` deletes every
//! read-classified tool_call/result pair, drops the frontdoor's own
//! compressed-history message, REPLACES the caller's system prompt with
//! a write-mandate, and appends a system-role nudge. Each deletion was
//! cut against a named fixture and each is defensible — but a client on
//! this route is entitled to know that the conversation it sent is not
//! the conversation the model saw.
//!
//! Nothing pinned that until this module. `frontdoor.rs` is 5,820 lines
//! and the wiring lives in `routes_inference.rs`; a refactor of either
//! can add a fourth mutation, drop one, reorder them, or move one
//! outside the gate, and every existing test stays green because the
//! route still returns 200. That is this system's characteristic
//! failure (ARCH §18.3): a well-formed, exit-0 result that is wrong.
//!
//! The pack is five predicates over the nine committed `gym/fixtures`,
//! each with a falsifier in [`falsifiers`] proving it can fail for the
//! reason it claims, and a tripwire so a tenth frontdoor pass cannot be
//! wired into the inference route uncontrolled. It needs no model, no
//! daemon and no network: [`CapturesRequest`] returns `Err` after
//! recording the request, so the whole pack runs offline in ~50ms on
//! the existing `main` test binary.
//!
//! Deliberately NOT asserted here: whether a nudge helps. That is a
//! bench question, and answering it here would invite someone to loosen
//! a wire pin to move a bench number (ARCH §18.6). This module pins
//! SHAPE — what reaches the model — and nothing about quality.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use commonwealth_api::openai_types::ChatCompletionRequest;
use commonwealth_api::routes_inference::chat_completions;

use crate::openai_wire_fidelity::{chat_with_url_in_tool_result, solo_state, CapturesRequest};

/// A request-side pass the reshape gate governs, paired with the name
/// the route calls it by.
type Pass = (&'static str, fn(&mut ChatCompletionRequest));

/// The three passes inside `if reshape_enabled()` in
/// `routes_inference::chat_completions`, in the order it applies them.
///
/// Order is part of the contract, not an accident: failure-recovery is
/// the most specific trigger (one recent failure), anti-repetition is a
/// pattern, read-attractor is a whole mode, and all three share an
/// idempotency gate so only one fires per turn. Reordering them changes
/// which one wins.
const GATED_REQUEST_PASSES: [Pass; 3] = [
    (
        "apply_failure_nudge_chat",
        commonwealth_api::frontdoor::apply_failure_nudge_chat,
    ),
    (
        "apply_anti_repetition_chat",
        commonwealth_api::frontdoor::apply_anti_repetition_chat,
    ),
    (
        "apply_read_attractor_nudge_chat",
        commonwealth_api::frontdoor::apply_read_attractor_nudge_chat,
    ),
];

/// Every `crate::frontdoor::` function the inference route is known to
/// call, gated or not. The tripwire holds the route to exactly this set
/// so a tenth pass has to be declared here — and, being declared, has
/// to answer the question this module exists to ask: is it inside the
/// reshape gate, and if not, why is it exempt?
///
/// `promote_in_content_tool_call` and `gather_context_components` are
/// the two deliberate exemptions. The first RECOVERS a tool call the
/// model emitted as content; suppressing it loses the model's intent,
/// so it is less faithful, not more. The second only reads.
const DECLARED_ROUTE_CALLS: [&str; 9] = [
    "apply_anti_repetition_chat",
    "apply_evidence_id_allowlist_from_tool_results",
    "apply_failure_nudge_chat",
    "apply_read_attractor_nudge_chat",
    "apply_url_allowlist_from_tool_results",
    "canonicalize_chat_response_paths",
    "canonicalize_chat_response_tool_calls",
    "gather_context_components",
    "promote_in_content_tool_call",
];

// ── the corpus ────────────────────────────────────────────────────

/// One committed gym fixture: the name, and the request a real client
/// sent. These are the same `input.json` files the Codex/opencode gym
/// replays, so the pack is pinned to turns that actually occurred
/// rather than to turns invented to make an assertion pass.
struct Fixture {
    name: String,
    request: ChatCompletionRequest,
}

/// The nine committed gym turns.
fn gym_fixtures() -> Vec<Fixture> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gym/fixtures");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("gym/fixtures must be readable at {}: {e}", root.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let fixtures: Vec<Fixture> = dirs
        .into_iter()
        .map(|dir| {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let raw = std::fs::read_to_string(dir.join("input.json"))
                .unwrap_or_else(|e| panic!("{name}: input.json unreadable: {e}"));
            let request = serde_json::from_str(&raw).unwrap_or_else(|e| {
                panic!("{name}: input.json is not a ChatCompletionRequest: {e}")
            });
            Fixture { name, request }
        })
        .collect();

    // A pack over an empty corpus is green and proves nothing — the
    // zero-test trap the test wrapper exits 4 for (ARCH §18.1).
    assert!(
        fixtures.len() >= 9,
        "gym/fixtures held {} turns; the pack is calibrated against 9 and a shrinking \
         corpus silently weakens every predicate below",
        fixtures.len()
    );
    fixtures
}

/// A run of three identical successful `exec_command` calls: the
/// anti-repetition trigger (`REPETITION_THRESHOLD` = 3), with every
/// result at exit 0 so failure-recovery does not claim the turn first.
///
/// This witness exists because the gym corpus cannot reach
/// `apply_anti_repetition_chat` AT ALL. The three passes share
/// `has_recent_runtime_nudge`, so exactly one fires per turn; on 004 —
/// the only committed turn whose tail repeats — failure-recovery is
/// first and anti-repetition returns early every time. Measured while
/// building this pack: with only the gym turns in the corpus, deleting
/// `apply_anti_repetition_chat` outright left every predicate green.
/// A pass no turn can reach is a pass no test can pin.
fn chat_with_a_repeated_successful_command() -> ChatCompletionRequest {
    let call = |i: usize| {
        serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": format!("call_rep_{i}"),
                "type": "function",
                "function": {
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"ls -la target/debug\",\"justification\":\"checking build output\"}"
                }
            }]
        })
    };
    let result = |i: usize| {
        serde_json::json!({
            "role": "tool",
            "tool_call_id": format!("call_rep_{i}"),
            "content": "Chunk ID: 9f2c11\nWall time: 0.0100 seconds\nProcess exited with code 0\n\
                        Original token count: 12\nOutput:\ntotal 0\n"
        })
    };
    serde_json::from_value(serde_json::json!({
        "model": "primary",
        "messages": [
            {"role": "system", "content": "You are a coding agent with one tool: exec_command."},
            {"role": "user", "content": "build the crate"},
            call(0), result(0),
            call(1), result(1),
            call(2), result(2),
        ],
    }))
    .expect("anti-repetition witness builds")
}

/// The nine gym turns plus the witnesses that cover what they cannot.
///
/// Witnesses are built here rather than committed under `gym/fixtures`
/// on purpose: that directory is the Codex gym's replay corpus and
/// adding turns to it changes what the gym measures. A turn that exists
/// only to give a predicate a failing input belongs with the predicate.
fn corpus() -> Vec<Fixture> {
    let mut corpus = gym_fixtures();
    corpus.push(Fixture {
        name: "witness:repeated_successful_command".into(),
        request: chat_with_a_repeated_successful_command(),
    });
    corpus.push(Fixture {
        name: "witness:url_in_tool_result".into(),
        request: chat_with_url_in_tool_result(),
    });
    corpus
}

/// The turn as the model actually sees it: the conversation, plus the
/// two sampler constraints that decide which tokens it may emit.
///
/// Scoped deliberately. `request.model` is excluded because the handler
/// legitimately resolves it (slot targeting, ATOS pipeline) — folding
/// it in would make this a test of model routing wearing a fidelity
/// test's name.
fn turn_view(req: &ChatCompletionRequest) -> serde_json::Value {
    serde_json::json!({
        "messages": req.messages,
        "url_allowlist": req.url_allowlist,
        "evidence_id_allowlist": req.evidence_id_allowlist,
    })
}

fn compose(input: &ChatCompletionRequest, passes: &[Pass]) -> ChatCompletionRequest {
    let mut out = input.clone();
    for (_, f) in passes {
        f(&mut out);
    }
    out
}

/// Drives the real handler and returns the request the inference
/// service received — after every frontdoor pass has had its turn.
async fn served(input: &ChatCompletionRequest) -> ChatCompletionRequest {
    let svc = CapturesRequest::default();
    let state = solo_state(Arc::new(svc.clone()));
    let _ = chat_completions(State(state), HeaderMap::new(), None, Json(input.clone())).await;
    svc.seen()
}

// ── the predicates ────────────────────────────────────────────────

/// Names the fixtures where the served turn differs from `passes`
/// composed in order. THE predicate: shared by P1 and by its falsifier,
/// so the falsifier exercises the same code the gate trusts, not a
/// lookalike.
async fn differential_violations(passes: &[Pass]) -> Vec<String> {
    let mut out = Vec::new();
    for f in corpus() {
        if turn_view(&served(&f.request).await) != turn_view(&compose(&f.request, passes)) {
            out.push(f.name);
        }
    }
    out
}

/// P1. The served turn is the client's turn with exactly the three
/// declared passes applied, in the declared order — and nothing else.
///
/// This is the refactor guard. It holds however the passes are
/// rewritten internally, and it breaks the moment the route grows a
/// fourth mutation, loses one, reorders them, or moves one outside the
/// gate. A reviewer who cannot say what a 5,820-line module does to a
/// turn can still read this line and know the answer is "these three".
#[tokio::test]
async fn the_served_turn_is_exactly_the_three_declared_passes() {
    let violations = differential_violations(&GATED_REQUEST_PASSES).await;
    assert!(
        violations.is_empty(),
        "the inference route changed the turn in a way the declared passes do not account for, \
         on: {violations:?}. Either a mutation was added to the route, or one of the three was \
         moved, reordered, or dropped. Whichever it is, a client's conversation is now being \
         altered by something this pack does not name."
    );
}

/// P2. Every gated pass is load-bearing on the corpus: dropping any one
/// of the three changes the served turn for at least one fixture.
///
/// This is the §18.1 guard — a check with no failing input you can name
/// is not a check. It is also the negative control for P1: it proves
/// the differential would actually catch a dropped pass rather than
/// passing vacuously because no fixture triggers anything.
#[tokio::test]
async fn every_gated_pass_is_load_bearing_on_the_corpus() {
    for (i, (name, _)) in GATED_REQUEST_PASSES.iter().enumerate() {
        let without: Vec<Pass> = GATED_REQUEST_PASSES
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, p)| *p)
            .collect();
        let witnesses = differential_violations(&without).await;
        assert!(
            !witnesses.is_empty(),
            "no fixture in the corpus trips {name}, so P1 would stay green if a refactor \
             deleted it outright. Add a fixture that exercises it, or retire the pass."
        );
    }
}

/// P3. A turn that trips no pass reaches the model unmodified.
///
/// The promise `turn_fidelity` makes to everyone who is not running
/// Codex: all three passes key on that tool vocabulary, so a client
/// speaking a different one is untouched. Four of the nine fixtures
/// trip nothing, and this is what says so in a way a refactor cannot
/// quietly take away — without needing to mutate process env under
/// every other test in the binary.
#[tokio::test]
async fn a_turn_that_trips_no_pass_is_served_unmodified() {
    let mut untouched = 0usize;
    for f in corpus() {
        let composed = compose(&f.request, &GATED_REQUEST_PASSES);
        if turn_view(&composed) != turn_view(&f.request) {
            continue; // this fixture does trip a pass; P1 covers it
        }
        untouched += 1;
        assert_eq!(
            turn_view(&served(&f.request).await),
            turn_view(&f.request),
            "{}: no declared pass alters this turn, so the route must serve it through \
             unmodified — anything else is an unaccounted mutation",
            f.name
        );
    }
    assert!(
        untouched >= 4,
        "only {untouched} fixtures passed through untouched; the pack is calibrated against 4 \
         and a drop means the passes now fire on turns they used to leave alone"
    );
}

/// P4. No fixture gets a synthesised sampler constraint.
///
/// The corpus-wide form of the 2026-08-29 regression: an allowlist
/// invented from `role: tool` messages became the only URL set the
/// model could reach, so a request to emit a Stripe endpoint came back
/// holding a rust-lang URL — 200 OK, no warning, wrong bytes. One
/// fixture proved the gate; nine prove it across every committed turn.
#[tokio::test]
async fn no_fixture_gets_a_synthesised_sampler_constraint() {
    for f in corpus() {
        let seen = served(&f.request).await;
        assert_eq!(
            seen.url_allowlist, None,
            "{}: the daemon invented a URL allowlist for a caller that never asked for one",
            f.name
        );
        assert_eq!(
            seen.evidence_id_allowlist, None,
            "{}: the daemon invented an evidence-id allowlist for a caller that never asked \
             for one",
            f.name
        );
    }
}

/// Names any `crate::frontdoor::` call in the inference route that
/// `declared` does not list. Shared with the falsifier below.
fn undeclared_route_calls(source: &str, declared: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = source
        .match_indices("crate::frontdoor::")
        .map(|(i, m)| {
            source[i + m.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty() && !declared.contains(&name.as_str()))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// P5, the tripwire. The inference route calls exactly the frontdoor
/// passes this module declares.
///
/// Without it the pack decays silently: pass number ten gets wired into
/// the route, P1 starts failing, and the cheapest way to make it green
/// again is to add the new pass to `GATED_REQUEST_PASSES` without ever
/// asking whether it belongs inside the reshape gate. This makes that
/// an explicit edit to a list whose doc comment asks the question.
#[test]
fn the_inference_route_calls_exactly_the_declared_frontdoor_passes() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes_inference.rs"),
    )
    .expect("routes_inference.rs is readable");

    let undeclared = undeclared_route_calls(&source, &DECLARED_ROUTE_CALLS);
    assert!(
        undeclared.is_empty(),
        "the inference route calls frontdoor passes this pack does not know about: {undeclared:?}. \
         Add each to DECLARED_ROUTE_CALLS, and decide the question that list asks: does it belong \
         inside the reshape gate? If it mutates the turn, it does."
    );

    for name in DECLARED_ROUTE_CALLS {
        assert!(
            source.contains(&format!("crate::frontdoor::{name}")),
            "{name} is declared here but the route no longer calls it — a pass was removed \
             without updating the pack, so the pack now over-states what the daemon does"
        );
    }
}

// ── falsifiers ────────────────────────────────────────────────────
//
// A gate nobody has watched fail is not a gate. Each test below drives
// the predicate above it against a deliberately broken input and
// asserts it reports the failure — so the green above is evidence, not
// an absence of evidence.

mod falsifiers {
    use super::*;

    /// Falsifies P1 and P2. With a pass removed from the declared set,
    /// the differential must name the fixtures that pass accounts for.
    /// If this ever goes quiet, P1 has stopped watching the route.
    #[tokio::test]
    async fn the_differential_catches_a_dropped_pass() {
        for (i, (name, _)) in GATED_REQUEST_PASSES.iter().enumerate() {
            let without: Vec<Pass> = GATED_REQUEST_PASSES
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, p)| *p)
                .collect();
            assert!(
                !differential_violations(&without).await.is_empty(),
                "dropping {name} left the differential green — P1 cannot see this pass at all"
            );
        }
    }

    /// Falsifies P3. A turn that trips no pass is only interesting if
    /// the comparison would notice a change to it.
    #[tokio::test]
    async fn the_passthrough_check_catches_an_altered_turn() {
        let untouched = corpus()
            .into_iter()
            .find(|f| turn_view(&compose(&f.request, &GATED_REQUEST_PASSES)) == turn_view(&f.request))
            .expect("at least one fixture trips no pass");

        let mut altered = untouched.request.clone();
        let last = altered
            .messages
            .last()
            .cloned()
            .expect("fixture has messages");
        altered.messages.push(last);

        assert_ne!(
            turn_view(&altered),
            turn_view(&untouched.request),
            "appending a message left turn_view identical, so P3 would not notice a route that \
             injected one"
        );
    }

    /// Falsifies P4. The allowlist fields are not always `None` — the
    /// synthesis genuinely populates them — so asserting `None` after
    /// the gate is a real check and not a tautology over a field
    /// nothing ever sets.
    #[test]
    fn a_synthesised_allowlist_is_something_the_corpus_can_actually_produce() {
        let witness = corpus().into_iter().find_map(|f| {
            let mut req = f.request.clone();
            commonwealth_api::frontdoor::apply_url_allowlist_from_tool_results(&mut req);
            req.url_allowlist.is_some().then_some(f.name)
        });
        assert!(
            witness.is_some(),
            "no fixture produces a URL allowlist even with the synthesis applied directly, so \
             P4's `== None` proves nothing about the gate. Add a fixture whose tool results \
             carry a URL."
        );
    }

    /// Falsifies P5. The tripwire must see a call that is not declared;
    /// a scanner that silently matches nothing would keep the pack green
    /// through exactly the change it exists to catch.
    #[test]
    fn the_tripwire_sees_an_undeclared_pass() {
        let forged = "    crate::frontdoor::apply_some_new_reshape(&mut request);";
        assert_eq!(
            undeclared_route_calls(forged, &DECLARED_ROUTE_CALLS),
            vec!["apply_some_new_reshape".to_string()],
            "the tripwire did not report an undeclared frontdoor call"
        );
        assert!(
            undeclared_route_calls(forged, &["apply_some_new_reshape"]).is_empty(),
            "the tripwire reported a call that WAS declared"
        );
    }
}

// ── the response side of the same gate ────────────────────────────
//
// `reshape_enabled` governs five passes, not three. The two below act
// on the model's OUTPUT — they rewrite arguments of a tool call it
// already emitted — and they are as invisible to a 200-assertion as
// the request-side three. A client that gets back a repaired heredoc
// cannot tell the model emitted a broken one; a refactor that drops
// the repair looks identical from outside.

use commonwealth_api::openai_types::{ChatCompletionResponse, ChatMessage, ToolCall};
use commonwealth_api::state::{LocalInferenceError, LocalInferenceService};
use commonwealth_api::openai_types::StreamFrame;
use futures::Stream;
use http_body_util::BodyExt;
use std::pin::Pin;

/// A response-side pass the gate governs, paired with the name the
/// route calls it by. Both take the choice's tool calls; the path
/// canonicalizer additionally needs the context component map the
/// route builds from the post-reshape request.
type ResponsePass = (
    &'static str,
    fn(&mut Vec<ToolCall>, &std::collections::HashMap<String, usize>),
);

/// The two passes inside the response-side `if !reshape_enabled()
/// { continue; }` guard, in the order `routes_inference` applies them.
///
/// `promote_in_content_tool_call` is deliberately NOT here: it runs
/// before the guard and is exempt on purpose, because suppressing it
/// loses a tool call the model actually made. That exemption is pinned
/// by `the_ungated_promotion_runs_even_though_the_gate_governs_the_rest`.
const GATED_RESPONSE_PASSES: [ResponsePass; 2] = [
    ("canonicalize_chat_response_tool_calls", |tcs, _ctx| {
        commonwealth_api::frontdoor::canonicalize_chat_response_tool_calls(tcs);
    }),
    ("canonicalize_chat_response_paths", |tcs, ctx| {
        commonwealth_api::frontdoor::canonicalize_chat_response_paths(tcs, ctx);
    }),
];

/// Returns a fixed response, so the response-side passes run against a
/// model output the test controls. The request-side capture is not
/// enough here: those passes only run when the service SUCCEEDS.
#[derive(Clone)]
struct RespondsWith(ChatCompletionResponse);

#[async_trait::async_trait]
impl LocalInferenceService for RespondsWith {
    async fn chat_completion(
        &self,
        _r: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LocalInferenceError> {
        Ok(self.0.clone())
    }

    async fn chat_completion_stream(
        &self,
        _r: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
        Err(LocalInferenceError::Other("not the streaming path".into()))
    }

    fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
        None
    }

    async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
        unimplemented!("embedding is not on this path")
    }
}

fn response_with_exec_command(cmd: &str) -> ChatCompletionResponse {
    serde_json::from_value(serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "primary",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_0",
                    "type": "function",
                    "function": {
                        "name": "exec_command",
                        "arguments": serde_json::to_string(&serde_json::json!({
                            "cmd": cmd, "justification": "t"
                        })).unwrap()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .expect("canned response builds")
}

/// Drives the real route with a service that returns `canned`, and
/// returns the response body the client actually receives.
async fn served_response(
    request: &ChatCompletionRequest,
    canned: &ChatCompletionResponse,
) -> ChatCompletionResponse {
    let state = solo_state(Arc::new(RespondsWith(canned.clone())));
    let resp = chat_completions(
        State(state),
        HeaderMap::new(),
        None,
        Json(request.clone()),
    )
    .await;
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("response body collects")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "route did not return a ChatCompletionResponse: {e}\nbody: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// The response the route SHOULD emit: the model's output with the
/// ungated promotion, then `passes`, applied exactly as the route does.
fn compose_response(
    request: &ChatCompletionRequest,
    canned: &ChatCompletionResponse,
    passes: &[ResponsePass],
) -> ChatCompletionResponse {
    // The route builds the component map from the request as it stands
    // AFTER the request-side reshape, not from the client's original.
    let post_reshape = compose(request, &GATED_REQUEST_PASSES);
    let ctx = commonwealth_api::frontdoor::gather_context_components(&post_reshape.messages);

    let mut out = canned.clone();
    for choice in out.choices.iter_mut() {
        commonwealth_api::frontdoor::promote_in_content_tool_call(&mut choice.message);
        if let Some(tcs) = choice.message.tool_calls.as_mut() {
            for (_, f) in passes {
                f(tcs, &ctx);
            }
        }
    }
    out
}

fn response_view(r: &ChatCompletionResponse) -> serde_json::Value {
    serde_json::json!(r
        .choices
        .iter()
        .map(|c| &c.message)
        .collect::<Vec<&ChatMessage>>())
}

/// A turn whose context names the canonical path often enough for the
/// path canonicalizer to have something to rewrite TOWARD, paired with
/// a model emission that drops the leading `a` — the tokenizer-drift
/// typo the pass was cut against.
fn turn_naming_a_canonical_path() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "primary",
        "messages": [
            {"role": "system", "content":
             "Work in /Users/alexsbryan/dev/atos-experiment-oicp-types."},
            {"role": "user", "content":
             "read /Users/alexsbryan/dev/atos-experiment-oicp-types/oicp-v0.3.md and \
              summarise /Users/alexsbryan/dev/atos-experiment-oicp-types/README.md"},
        ],
    }))
    .expect("path-context turn builds")
}

/// Every response-side witness: a turn, a model output, and the pass
/// the pair exists to exercise.
fn response_witnesses() -> Vec<(&'static str, ChatCompletionRequest, ChatCompletionResponse)> {
    vec![
        (
            // Real codex emission (gym 008): the `*** End Patch` marker
            // is missing before the EOF closer.
            "heredoc_missing_end_patch",
            chat_with_a_repeated_successful_command(),
            response_with_exec_command(
                "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: a.rs\n+pub fn x() {}\nEOF",
            ),
        ),
        (
            "path_typo_drops_leading_char",
            turn_naming_a_canonical_path(),
            response_with_exec_command(
                "cat /Users/alexsbryan/dev/tos-experiment-oicp-types/oicp-v0.3.md",
            ),
        ),
    ]
}

/// P6. What the client gets back is the model's output with exactly
/// the ungated promotion plus the two declared canonicalizers — and
/// nothing else.
///
/// The response-side twin of P1. A refactor that adds a third rewrite,
/// or moves one out from behind the gate, changes bytes the client
/// reads as the model's own words.
#[tokio::test]
async fn the_served_response_is_exactly_the_declared_response_passes() {
    for (name, request, canned) in response_witnesses() {
        assert_eq!(
            response_view(&served_response(&request, &canned).await),
            response_view(&compose_response(&request, &canned, &GATED_RESPONSE_PASSES)),
            "{name}: the route rewrote the model's output in a way the declared response \
             passes do not account for"
        );
    }
}

/// P7. Each response-side pass is load-bearing: dropping it changes
/// what the client receives for at least one witness.
///
/// Same §18.1 guard as P2. Without it, P6 could pass because no
/// witness produces output either canonicalizer touches, and deleting
/// both would go unnoticed.
#[tokio::test]
async fn every_response_pass_is_load_bearing_on_its_witness() {
    for (i, (name, _)) in GATED_RESPONSE_PASSES.iter().enumerate() {
        let without: Vec<ResponsePass> = GATED_RESPONSE_PASSES
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, p)| *p)
            .collect();

        let mut witnessed = false;
        for (_, request, canned) in response_witnesses() {
            let full = compose_response(&request, &canned, &GATED_RESPONSE_PASSES);
            let partial = compose_response(&request, &canned, &without);
            if response_view(&full) != response_view(&partial) {
                witnessed = true;
                break;
            }
        }
        assert!(
            witnessed,
            "no witness exercises {name}, so P6 would stay green if a refactor deleted it. \
             Add a model emission it repairs, or retire the pass."
        );
    }
}

/// The exemption, pinned. `promote_in_content_tool_call` sits OUTSIDE
/// the reshape gate on purpose: it lifts a tool call the model emitted
/// as content into the structured field, and suppressing it loses the
/// model's intent rather than preserving it.
///
/// Stated as a test because the reasoning is the kind that gets
/// refactored away. Someone tidying the response loop sees one pass
/// outside the guard and two inside, assumes an oversight, and moves
/// it in — at which case an opencode client talking to a Qwen3 model
/// silently stops seeing tool calls.
#[tokio::test]
async fn the_ungated_promotion_runs_even_though_the_gate_governs_the_rest() {
    let canned: ChatCompletionResponse = serde_json::from_value(serde_json::json!({
        "id": "chatcmpl-test", "object": "chat.completion", "created": 0, "model": "primary",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                // The Qwen3 shape: the call is in `content`, and
                // `tool_calls` is empty.
                "content": "{\"name\": \"exec_command\", \"parameters\": {\"cmd\": \"ls\"}}"
            },
            "finish_reason": "stop"
        }]
    }))
    .expect("in-content tool call response builds");

    let served = served_response(&chat_with_a_repeated_successful_command(), &canned).await;
    let calls = served.choices[0]
        .message
        .tool_calls
        .as_ref()
        .map(|t| t.len())
        .unwrap_or(0);
    assert_eq!(
        calls, 1,
        "the model emitted a tool call as content and the client received none — the \
         promotion is exempt from the reshape gate precisely so this cannot happen"
    );
}
