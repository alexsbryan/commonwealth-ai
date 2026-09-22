// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! A turn that PARKS on a question (sv-surface R3; RB2's fixture too):
//! the `AskPlanner` fixture, the pause/resume round trip over the
//! wire, and the information answer that folds its search sources
//! into the conversation.

use crate::common::{self, spawn_router, TestProvider};

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{
    ConversationContext, Plan, ResolveOutcome, Step, StepKind, ToolDescriptor, TurnAnswer,
    TurnFrame, TurnMode, TurnNotice, TurnRequest,
};
use sovereign_daemon::{
    turn_http::{turn_router, turn_router_with, SocketTimers},
    EmbeddedDaemon,
};

use super::{create_conversation, engine};

// ─── A turn that PARKS on a question (sv-surface R3; RB2's fixture too) ───
//
// Hoisted out of the R3 test by RB2, which needs the same parked turn to
// CANCEL: one planner and one setup, not two that drift (ARCH §10.6).

struct AskPlanner;
#[async_trait::async_trait]
impl sovereign_contracts::traits::Planner for AskPlanner {
    async fn plan(
        &self,
        goal: &str,
        _context: &ConversationContext,
        _tools: &[ToolDescriptor],
    ) -> sovereign_contracts::error::Result<Plan> {
        Ok(Plan {
            id: "plan-ask".into(),
            goal: goal.to_string(),
            steps: vec![Step {
                id: 0,
                description: "Ask the user which corpus".into(),
                kind: StepKind::UserInput {
                    question: "Which corpus should I search?".into(),
                },
                requires_approval: false,
                inputs: Vec::new(),
                sampling: None,
                evaluation: None,
            }],
            edges: Vec::new(),
        })
    }

    /// The plan cannot fail (one step, no tools), so a replan is the
    /// same plan again — the executor never asks on this fixture.
    async fn replan(
        &self,
        original: &Plan,
        _completed: &[(usize, sovereign_contracts::types::StepOutput)],
        _failure: &sovereign_contracts::types::StepError,
        _tools: &[ToolDescriptor],
    ) -> sovereign_contracts::error::Result<Plan> {
        Ok(original.clone())
    }
}

/// A live turn, parked on its question, and the socket that owns it.
pub(crate) struct ParkedTurn {
    pub(crate) _tmp: tempfile::TempDir,
    pub(crate) _daemon: Arc<EmbeddedDaemon>,
    pub(crate) ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    /// Everything that arrived before the question.
    pub(crate) before: Vec<TurnFrame>,
    /// The id the question was asked under.
    pub(crate) prompt_id: String,
}

/// Drive a `ComplexTask` turn on a CLAIMED socket until it pauses on its
/// question.
///
/// Bounded: a hang here is the failure mode this fixture exists to catch, and
/// a test that hangs reports nothing.
pub(crate) async fn turn_parked_on_a_question(timers: SocketTimers) -> ParkedTurn {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let provider = TestProvider::new().with_complete_text("searched.");
    let services = common::desktop_services_with_planner(
        engine(tmp.path()),
        Arc::clone(&store),
        Arc::new(provider),
        Box::new(AskPlanner),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    let addr = spawn_router(turn_router_with(Arc::clone(&daemon), timers)).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream?approvals=true"
    ))
    .await
    .expect("the claimed upgrade is accepted");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "search for the adoption figure".to_string(),
            mode: TurnMode::Grounded,
            intent: Some(sovereign_contracts::types::Intent::ComplexTask),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let mut before: Vec<TurnFrame> = Vec::new();
    let prompt_id = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed before the turn asked its question");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            if let TurnFrame::Prompt { id, .. } = &frame {
                return id.clone();
            }
            before.push(frame);
        }
    })
    .await
    .expect("the turn reaches its question rather than hanging");

    ParkedTurn {
        _tmp: tmp,
        _daemon: daemon,
        ws,
        before,
        prompt_id,
    }
}

/// sv-surface R3 + the C1 OPEN VERIFICATION, closed: a REAL turn that
/// PAUSES on a question over the wire. Until this, no test anywhere drove
/// a turn that parks the executor through `serve_turn` +
/// `capabilities::scope` + a real WebSocket — the unit tests exercised
/// the channel in isolation, and the stub fixture's `NoOpPlanner` made
/// the ComplexTask path unreachable. The planner knob
/// (`desktop_services_with_planner`) exists for exactly this.
#[tokio::test]
async fn a_turn_pauses_on_a_question_over_the_wire_and_its_answer_resumes_it() {
    let ParkedTurn {
        _tmp,
        _daemon,
        mut ws,
        before: frames,
        prompt_id: id,
    } = turn_parked_on_a_question(SocketTimers::default()).await;

    // On THIS path the pause beats every other frame: ComplexTask is
    // non-streamable, so the whole turn — pause included — runs inside
    // `handle_message` before `serve_non_streaming_turn` has a response
    // to emit. Nothing but the Prompt could have arrived, and nothing
    // did.
    assert!(
        frames.is_empty(),
        "the pause is the wire's first frame; got {frames:?}"
    );
    assert!(
        id.ends_with(":input") && id.len() > ":input".len(),
        "ask_user parks under the one id it mints, PREFIXED by the turn's own nonce — a bare \
         `input` is spelled the same on every turn of this socket, so a late answer would \
         resolve the next turn's question (sv-surface C2); got {id:?}"
    );

    // The answer goes up the same socket; the parked executor resumes.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: id.clone(),
            answer: TurnAnswer::Text("sep".into()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // And the turn completes. The tail frames are the fallback path's
    // fixed order — TurnStarted (late by nature on this path; the
    // comment in `serve_non_streaming_turn` says so), the whole answer
    // as one Token, then Complete.
    let tail = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut tail: Vec<TurnFrame> = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            let done = matches!(frame, TurnFrame::Complete { .. });
            tail.push(frame);
            if done {
                return tail;
            }
        }
        panic!("socket closed without a Complete after the answer");
    })
    .await
    .expect("the answer resumed the turn");

    let position_of = |pred: &dyn Fn(&TurnFrame) -> bool| {
        tail.iter()
            .position(pred)
            .unwrap_or_else(|| panic!("no matching frame in the tail: {tail:?}"))
    };
    let started = position_of(&|f| {
        matches!(
            f,
            TurnFrame::Notice {
                notice: TurnNotice::TurnStarted { .. }
            }
        )
    });
    let token = position_of(&|f| matches!(f, TurnFrame::Token { .. }));
    let complete = position_of(&|f| matches!(f, TurnFrame::Complete { .. }));
    assert!(
        started < token && token < complete,
        "the tail is TurnStarted, Token, Complete in order; got {tail:?}"
    );

    // The pause and the resume left nothing parked: a SECOND answer with
    // the same id resolves nothing and the refusal says so by name.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: id.clone(),
            answer: TurnAnswer::Text("stale".into()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let answered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            match frame {
                TurnFrame::Notice {
                    notice: TurnNotice::ResolveAck { outcome, .. },
                } => return outcome,
                // sv-surface RB4, the whole point: the double-answer is a
                // recoverable refusal about one question. Riding the
                // terminal frame made a client throw away a turn that had
                // already completed perfectly well.
                TurnFrame::StreamError { message, .. } => {
                    panic!("a stale reply must not be reported as a dead turn; got {message:?}")
                }
                _ => continue,
            }
        }
        panic!("no frame answered the stale reply");
    })
    .await
    .expect("the stale reply is answered, not swallowed");
    assert_eq!(
        answered,
        ResolveOutcome::NoSuchPending,
        "the ack names what the reply found: nothing, because it was already answered"
    );
}

/// sv-surface G3b, end to end: a turn pauses on an INFORMATION request,
/// the client's answer carries its web-search registry rows, and the
/// daemon folds them into the conversation AS PART OF the resolve — one
/// user action (picking a search result), one atomic effect. The store
/// assertion is the point: the registry write landed daemon-side, where
/// the turn's synthesis reads it.
#[tokio::test]
async fn a_search_built_information_answer_folds_its_sources_into_the_conversation() {
    use sovereign_contracts::types::{InformationRequest, SearchedSourceEntry};

    struct AskInfoPlanner;
    #[async_trait::async_trait]
    impl sovereign_contracts::traits::Planner for AskInfoPlanner {
        async fn plan(
            &self,
            goal: &str,
            _context: &sovereign_contracts::types::ConversationContext,
            _tools: &[sovereign_contracts::types::ToolDescriptor],
        ) -> sovereign_contracts::error::Result<sovereign_contracts::types::Plan> {
            Ok(sovereign_contracts::types::Plan {
                id: "plan-info".into(),
                goal: goal.to_string(),
                steps: vec![sovereign_contracts::types::Step {
                    id: 0,
                    description: "Ask for the missing figure".into(),
                    kind: sovereign_contracts::types::StepKind::AwaitUserInfo {
                        request: InformationRequest {
                            current_understanding: "The answer needs one figure".into(),
                            gap: "The 2024 adoption rate".into(),
                            relevance: "It decides the trend".into(),
                            satisfying_source: "A statistics agency".into(),
                            search_hints: Vec::new(),
                            task_id: String::new(),
                            step_id: 0,
                            kind: Default::default(),
                            task_title: String::new(),
                            routes: Vec::new(),
                        },
                    },
                    requires_approval: false,
                    inputs: Vec::new(),
                    sampling: None,
                    evaluation: None,
                }],
                edges: Vec::new(),
            })
        }

        async fn replan(
            &self,
            original: &sovereign_contracts::types::Plan,
            _completed: &[(usize, sovereign_contracts::types::StepOutput)],
            _failure: &sovereign_contracts::types::StepError,
            _tools: &[sovereign_contracts::types::ToolDescriptor],
        ) -> sovereign_contracts::error::Result<sovereign_contracts::types::Plan> {
            Ok(original.clone())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let provider = TestProvider::new().with_complete_text("answered.");
    let services = common::desktop_services_with_planner(
        engine(tmp.path()),
        Arc::clone(&store),
        Arc::new(provider),
        Box::new(AskInfoPlanner),
    );
    let daemon = Arc::new(EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    ));
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream?approvals=true"
    ))
    .await
    .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "what is the adoption rate?".to_string(),
            mode: TurnMode::Grounded,
            intent: Some(sovereign_contracts::types::Intent::ComplexTask),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Read to the information prompt (bounded — a hang is the failure).
    let prompt_id = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed before the information request");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).unwrap();
            if let TurnFrame::Prompt { id, .. } = frame {
                return id;
            }
        }
    })
    .await
    .expect("the turn parks on its information request");
    assert!(
        prompt_id.ends_with(":info:0") && prompt_id.len() > ":info:0".len(),
        "the information request parks under the step's id, PREFIXED by the turn's nonce \
         (sv-surface C2); got {prompt_id:?}"
    );

    // The client's search-produced answer, carrying its registry rows.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: prompt_id,
            answer: TurnAnswer::Information {
                content: Some(
                    "Web search results for \"adoption rate\" (via duckduckgo):\n[1] 12.4%".into(),
                ),
                sources: vec![SearchedSourceEntry {
                    url: "https://example.org/adoption".into(),
                    title: "Adoption rates 2024".into(),
                    first_seen_turn: 0,
                    last_referenced_turn: 0,
                    search_query: "adoption rate".into(),
                }],
            },
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // ResolveAck arrives, then the turn completes.
    let saw_ack = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        use sovereign_contracts::types::TurnNotice;
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed early");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).unwrap();
            match frame {
                TurnFrame::Notice {
                    notice: TurnNotice::ResolveAck { outcome, .. },
                } => return outcome,
                TurnFrame::Complete { .. } => panic!("Complete before the ResolveAck"),
                _ => {}
            }
        }
    })
    .await
    .expect("the answer is acknowledged");
    assert_eq!(
        saw_ack,
        sovereign_contracts::types::ResolveOutcome::Resolved,
        "the acknowledgement says the answer landed"
    );

    // THE assertion: the registry row landed in the daemon's store, part
    // of the same effect that resolved the answer.
    let conv_row = store
        .get_conversation(&conv)
        .await
        .expect("the conversation");
    let sources = conv_row.searched_sources.expect("the registry was written");
    assert_eq!(sources.len(), 1, "one row folded; got {sources:?}");
    assert_eq!(sources[0].url, "https://example.org/adoption");
    assert!(
        sources[0].first_seen_turn >= 1,
        "the daemon stamped the conversation's real turn count ({}), not the client's placeholder",
        sources[0].first_seen_turn
    );
}
