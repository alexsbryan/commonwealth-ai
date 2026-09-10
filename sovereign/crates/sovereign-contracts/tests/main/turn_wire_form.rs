// SPDX-License-Identifier: AGPL-3.0-or-later
//! The falsifier for TOPOLOGY.md §10 phase 5b: moving the turn protocol out
//! of the `sovereign-server` binary must not have changed one byte of it.
//!
//! # What this catches that the compiler cannot
//!
//! rustc is exhaustive over types and blind to encoding
//! (`kernel_types::wire`'s standing lesson). Renaming a variant, renaming a
//! field, dropping a `skip_serializing_if`, or swapping the envelope from
//! externally-tagged to internally-tagged all compile clean and all break
//! every client mid-turn. The mobile client reads these exact strings, and
//! `sovereign-server/src/http_tests.rs::ws_streams_tokens_then_complete`
//! only covers `token` and `complete` — the other three frames and the
//! whole inbound half had no wire guard at all before this file.
//!
//! # The bytes are the assertion
//!
//! Each case pins the literal JSON rather than round-tripping through a
//! `serde_json::json!` value, because a value-to-value comparison passes
//! when both sides move together. A future edit that means to change the
//! protocol changes these strings and says so in its commit; an edit that
//! did not mean to fails here.

use sovereign_contracts::types::projection::{Citation, Provenance, ProvenanceSource};
use sovereign_contracts::types::{
    ActionPreview, ClarificationOption, ClarificationRequest, InformationRequest,
    InterpretationProposed, LessonProposedPayload, MessageRefinedPayload, NarrationPhase,
    ProposedAlternative, SearchedSourceEntry, StepStatus, TurnAnswer, TurnFrame, TurnMode,
    TurnNotice, TurnPrompt, TurnRequest,
};

/// Serialise, compare against the bytes a client actually reads, then parse
/// back and confirm the value survived. A frame that serialises correctly
/// but cannot be read back is not a protocol — that is the half `ServerEvent`
/// never had, being `Serialize`-only inside a binary nobody could import.
fn pin_frame(frame: TurnFrame, expected: &str) {
    let json = serde_json::to_string(&frame).expect("frame serialises");
    assert_eq!(json, expected, "wire form changed for {frame:?}");
    let back: TurnFrame = serde_json::from_str(&json).expect("frame parses back");
    assert_eq!(back, frame, "frame did not survive its own wire form");
}

#[test]
fn token_frame_wire_form() {
    pin_frame(
        TurnFrame::Token {
            message_id: "m1".into(),
            chunk: "Compat".into(),
        },
        r#"{"type":"token","data":{"message_id":"m1","chunk":"Compat"}}"#,
    );
}

#[test]
fn complete_frame_wire_form() {
    // The lean case: a handler that persisted no provenance. Both optional
    // fields must vanish from the envelope rather than appear as `null` —
    // `projection`'s documented graceful-degradation contract, and what the
    // client keys "no citations" off.
    pin_frame(
        TurnFrame::Complete {
            message_id: "m2".into(),
            provenance: None,
            citations: vec![],
            epistemic_state: None,
            task: None,
            metadata: None,
        },
        r#"{"type":"complete","data":{"message_id":"m2"}}"#,
    );
}

#[test]
fn complete_frame_carries_provenance_and_citations() {
    pin_frame(
        TurnFrame::Complete {
            message_id: "m3".into(),
            provenance: Some(Provenance {
                inference_backend: "Qwen3.5-9B.Q8_0 @ peer mac-peer".into(),
                routing_tier: Some("LOOKUP".into()),
                ttft_ms: None,
                total_ms: Some(1234),
                finish_reason: Some("length".into()),
                max_tokens_budget: None,
                completion_tokens: None,
                sources: vec![ProvenanceSource {
                    origin: "sep".into(),
                    count: 6,
                    from_peer: Some("mac-peer".into()),
                }],
            }),
            citations: vec![Citation {
                corpus_id: "sep".into(),
                chunk_id: "1396570".into(),
                title: Some("Free Will".into()),
                snippet: "Compatibilism holds that...".into(),
                score: 0.91,
                rank: 0,
                // Absent here on purpose: this case pins that a citation
                // WITHOUT the phase-6 additions serializes byte-for-byte as
                // it did before they existed.
                url: None,
                provenance_tier: None,
            }],
            epistemic_state: None,
            task: None,
            metadata: None,
        },
        concat!(
            r#"{"type":"complete","data":{"message_id":"m3","#,
            r#""provenance":{"inference_backend":"Qwen3.5-9B.Q8_0 @ peer mac-peer","#,
            r#""routing_tier":"LOOKUP","total_ms":1234,"finish_reason":"length","#,
            r#""sources":[{"origin":"sep","count":6,"from_peer":"mac-peer"}]},"#,
            r#""citations":[{"corpus_id":"sep","chunk_id":"1396570","title":"Free Will","#,
            r#""snippet":"Compatibilism holds that...","score":0.91,"rank":0}]}}"#,
        ),
    );
}

#[test]
fn stream_error_frame_wire_form() {
    // The shed case. `retry_after_secs` is what makes the client mirror the
    // REST 503 "host busy" state instead of showing a generic failure, so
    // its presence is load-bearing, not decorative.
    pin_frame(
        TurnFrame::StreamError {
            message: "host busy".into(),
            retry_after_secs: Some(7),
        },
        r#"{"type":"stream_error","data":{"message":"host busy","retry_after_secs":7}}"#,
    );
    pin_frame(
        TurnFrame::StreamError {
            message: "boom".into(),
            retry_after_secs: None,
        },
        r#"{"type":"stream_error","data":{"message":"boom"}}"#,
    );
}

#[test]
fn narration_frame_wire_form_survived_becoming_typed() {
    // `phase` was a `serde_json::Value` while this enum lived in the server:
    // the server could name `NarrationPhase` but not put it in a type it
    // shared with nobody, so it re-encoded the phase at every emit. Typing
    // it is only wire-safe if the same derive produces the same bytes —
    // these two cases are that claim, one per NarrationPhase shape.
    pin_frame(
        TurnFrame::Narration {
            message_id: "m4".into(),
            phase: NarrationPhase::RoutingCommitted,
            text: "Routing committed".into(),
            elapsed_ms: 12,
        },
        concat!(
            r#"{"type":"narration","data":{"message_id":"m4","#,
            r#""phase":"routing_committed","text":"Routing committed","elapsed_ms":12}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Narration {
            message_id: String::new(),
            phase: NarrationPhase::ModelLoad {
                model_id: "qwen3.5-35b".into(),
                size_bytes: None,
            },
            text: "Loading weights".into(),
            elapsed_ms: 0,
        },
        concat!(
            r#"{"type":"narration","data":{"#,
            r#""phase":{"model_load":{"model_id":"qwen3.5-35b","size_bytes":null}},"#,
            r#""text":"Loading weights","elapsed_ms":0}}"#,
        ),
    );
}

#[test]
fn queue_position_frame_wire_form() {
    pin_frame(
        TurnFrame::QueuePosition {
            position: 3,
            estimated_wait_ms: 9000,
        },
        r#"{"type":"queue_position","data":{"position":3,"estimated_wait_ms":9000}}"#,
    );
}

/// The PROMPT half of the two-shape protocol (sv-surface R1). These
/// bytes are NEW — the fold replaced `approval_request` /
/// `user_input_request` while nothing rendered them (three ignore arms,
/// a deliberate error, and this file were the entire consumer census),
/// so there is no back-compat case to pin: only the shape going
/// forward, one case per closed-enum variant.
#[test]
fn prompt_frame_wire_form() {
    // `preview` is the whole `ActionPreview` — the same object the
    // desktop's approval card renders in-process, and the same one the
    // server's converged `ExecutorEvent::Prompt` broadcasts. Pinning it
    // here is what makes "the attached client shows the same card" a
    // byte fact rather than a hope.
    pin_frame(
        TurnFrame::Prompt {
            id: "step:2".into(),
            prompt: TurnPrompt::Approval {
                preview: ActionPreview {
                    tool_id: "shell".into(),
                    description: "Run the migration".into(),
                    params: serde_json::json!({ "cmd": "migrate" }),
                },
            },
        },
        concat!(
            r#"{"type":"prompt","data":{"id":"step:2","#,
            r#""prompt":{"approval":{"preview":{"tool_id":"shell","#,
            r#""description":"Run the migration","params":{"cmd":"migrate"}}}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Prompt {
            id: "input".into(),
            prompt: TurnPrompt::UserInput {
                question: "Which branch?".into(),
            },
        },
        r#"{"type":"prompt","data":{"id":"input","prompt":{"user_input":{"question":"Which branch?"}}}}"#,
    );
    // The one prompt kind whose hangup policy is SKIP, not cancel — and
    // the one whose payload is a full struct with `#[serde(default)]`
    // fields. Those defaults SERIALIZE (default is a read-side
    // concession), which is what this case pins: the empty-vecs and
    // empty strings are on the wire, not omitted.
    pin_frame(
        TurnFrame::Prompt {
            id: "t1:info:3".into(),
            prompt: TurnPrompt::Information {
                request: InformationRequest {
                    current_understanding: "You asked about adoption rates".into(),
                    gap: "The 2024 figure for region Y".into(),
                    relevance: "It decides whether the trend reversed".into(),
                    satisfying_source: "A statistics agency press release".into(),
                    search_hints: Vec::new(),
                    task_id: "t1".into(),
                    step_id: 3,
                    kind: Default::default(),
                    task_title: String::new(),
                    routes: Vec::new(),
                },
            },
        },
        concat!(
            r#"{"type":"prompt","data":{"id":"t1:info:3","prompt":{"information":{"request":"#,
            r#"{"current_understanding":"You asked about adoption rates","#,
            r#""gap":"The 2024 figure for region Y","#,
            r#""relevance":"It decides whether the trend reversed","#,
            r#""satisfying_source":"A statistics agency press release","#,
            r#""search_hints":[],"task_id":"t1","step_id":3,"kind":"refinement","#,
            r#""task_title":"","routes":[]}}}}}"#,
        ),
    );
}

/// The NOTICE half — owed nothing by construction, valid after the
/// terminal `Complete` for two of its variants. One case per variant,
/// including the two whose daemon-side producers do not exist yet (they
/// are gap-ledger rows, not speculation; see the module docs on
/// sovereign-contracts::types::turn).
#[test]
fn notice_frame_wire_form() {
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::TurnStarted {
                message_id: "m1".into(),
            },
        },
        r#"{"type":"notice","data":{"notice":{"turn_started":{"message_id":"m1"}}}}"#,
    );
    // `status` is TYPED on the wire — the hosts' `status: String`
    // spellings ran Display first, which turns Jump(4) into the prose
    // "jump to 4". Both shapes pinned: the unit variant and the
    // carrying one.
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::StepDone {
                task_id: "t1".into(),
                step_id: 2,
                description: "Run the migration".into(),
                status: StepStatus::Done,
            },
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"step_done":{"task_id":"t1","#,
            r#""step_id":2,"description":"Run the migration","status":"done"}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::StepDone {
                task_id: "t1".into(),
                step_id: 4,
                description: "Pick a branch".into(),
                status: StepStatus::Jump(7),
            },
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"step_done":{"task_id":"t1","#,
            r#""step_id":4,"description":"Pick a branch","status":{"jump":7}}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::MessageRefined(MessageRefinedPayload {
                conversation_id: "c1".into(),
                message_id: "m2".into(),
                new_content: "The revised answer.".into(),
            }),
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"message_refined":"#,
            r#"{"conversation_id":"c1","message_id":"m2","new_content":"The revised answer."}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::LessonProposed(LessonProposedPayload {
                id: "l1".into(),
                conversation_id: "c1".into(),
                message_id: "m3".into(),
                display: "Prefer terse answers".into(),
                prompt_form: "answer tersely".into(),
                enforcement: "prompt".into(),
                params: serde_json::json!({}),
                taught_from: "\"too long\"".into(),
            }),
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"lesson_proposed":"#,
            r#"{"id":"l1","conversation_id":"c1","message_id":"m3","display":"Prefer terse answers","#,
            r#""prompt_form":"answer tersely","enforcement":"prompt","params":{},"#,
            r#""taught_from":"\"too long\""}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::ResolveAck {
                id: "step:7".into(),
                outcome: sovereign_contracts::types::ResolveOutcome::Resolved,
            },
        },
        r#"{"type":"notice","data":{"notice":{"resolve_ack":{"id":"step:7","outcome":"resolved"}}}}"#,
    );
    // G7's two routing events — payloads that already lived in contracts;
    // these pins are their first wire form.
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::InterpretationProposed(InterpretationProposed {
                session_id: "s1".into(),
                conversation_id: "c1".into(),
                interpretation: "I'm reading this as a quick overview.".into(),
                alternatives: vec![ProposedAlternative {
                    label: "Walk me through the scoring".into(),
                    intent_hint: "deep_query".into(),
                }],
                confidence: 0.55,
            }),
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"interpretation_proposed":"#,
            r#"{"session_id":"s1","conversation_id":"c1","#,
            r#""interpretation":"I'm reading this as a quick overview.","#,
            r#""alternatives":[{"label":"Walk me through the scoring","#,
            r#""intent_hint":"deep_query"}],"confidence":0.55}}}}"#,
        ),
    );
    pin_frame(
        TurnFrame::Notice {
            notice: TurnNotice::ClarificationRequest(ClarificationRequest {
                session_id: "s1".into(),
                conversation_id: "c1".into(),
                question: "Understand it, change it, or debug it?".into(),
                options: vec![ClarificationOption {
                    label: "Understand how it works".into(),
                    follow_up: "Explain how the scheduler picks peers".into(),
                    intent_hint: "knowledge_query".into(),
                }],
            }),
        },
        concat!(
            r#"{"type":"notice","data":{"notice":{"clarification_request":"#,
            r#"{"session_id":"s1","conversation_id":"c1","#,
            r#""question":"Understand it, change it, or debug it?","#,
            r#""options":[{"label":"Understand how it works","#,
            r#""follow_up":"Explain how the scheduler picks peers","#,
            r#""intent_hint":"knowledge_query"}]}}}}"#,
        ),
    );
}

#[test]
fn turn_request_wire_form() {
    // Pinned as PARSES, not as serialises: this half is what a client sends,
    // so the guarantee owed is that the bytes a client already emits still
    // land. `http_tests::ws_streams_tokens_then_complete` sends the first of
    // these literally.
    let cases = [
        (
            // NO `mode` key — the bytes every client emitted before phase 6
            // added one. This case is the compatibility guarantee: it must
            // keep landing, and it must land as `Grounded`.
            r#"{"type":"message","data":{"content":"hello"}}"#,
            TurnRequest::Message {
                content: "hello".into(),
                mode: TurnMode::Grounded,
                intent: None,
            },
        ),
        (
            // Raw model over the wire. Before phase 6 this was reachable only
            // by a host holding its own `Runtime`, which is what kept
            // `svrn chat --naked` from becoming a surface.
            r#"{"type":"message","data":{"content":"hello","mode":"naked"}}"#,
            TurnRequest::Message {
                content: "hello".into(),
                mode: TurnMode::Naked,
                intent: None,
            },
        ),
        (
            // The ANSWER half of the two-shape protocol — one case per
            // kind, replacing the folded `approve` / `user_reply`. The
            // ids echo whatever the Prompt minted (here, the daemon
            // socket's and the server's spellings).
            r#"{"type":"answer","data":{"id":"step:2","answer":{"approved":true}}}"#,
            TurnRequest::Answer {
                id: "step:2".into(),
                answer: TurnAnswer::Approved(true),
            },
        ),
        (
            r#"{"type":"answer","data":{"id":"input","answer":{"text":"yes"}}}"#,
            TurnRequest::Answer {
                id: "input".into(),
                answer: TurnAnswer::Text("yes".into()),
            },
        ),
        (
            // The skip: `content: null` IS the answer, not an absent
            // one — the executor resumes corpus-only, same as a pressed
            // skip. `sources` is omitted when empty (G3b).
            r#"{"type":"answer","data":{"id":"t1:info:3","answer":{"information":{"content":null}}}}"#,
            TurnRequest::Answer {
                id: "t1:info:3".into(),
                answer: TurnAnswer::Information {
                    content: None,
                    sources: Vec::new(),
                },
            },
        ),
        (
            // G3b: a search-built answer carries its registry rows, and
            // the host folds them into the conversation's cumulative
            // searched_sources as part of the resolve — one user action,
            // one atomic effect.
            concat!(
                r#"{"type":"answer","data":{"id":"info:0","answer":{"information":"#,
                r#"{"content":"Web search results for \"q\" (via duckduckgo):","#,
                r#""sources":[{"url":"https://example.org/a","title":"A","#,
                r#""first_seen_turn":4,"last_referenced_turn":4,"#,
                r#""search_query":"q"}]}}}}"#,
            ),
            TurnRequest::Answer {
                id: "info:0".into(),
                answer: TurnAnswer::Information {
                    content: Some("Web search results for \"q\" (via duckduckgo):".into()),
                    sources: vec![SearchedSourceEntry {
                        url: "https://example.org/a".into(),
                        title: "A".into(),
                        first_seen_turn: 4,
                        last_referenced_turn: 4,
                        search_query: "q".into(),
                    }],
                },
            },
        ),
        (
            // Session continuation — a ClarificationCard option click. FLAT,
            // like every other variant: three strings, not a nested `resume`
            // object. `ResumeSession` is the in-process shape and stays there;
            // nesting it here would have made the client's bytes follow a Rust
            // struct boundary.
            r#"{"type":"resume","data":{"content":"the second one","session_id":"s1","intent_hint":"DeepQuery"}}"#,
            TurnRequest::Resume {
                content: "the second one".into(),
                session_id: "s1".into(),
                intent_hint: "DeepQuery".into(),
            },
        ),
        (
            // NO `content` key, and that absence is the contract: redirect
            // re-answers the message the session already holds. A `content`
            // added here later would let a client disagree with what was
            // asked, which is the whole reason the field is missing.
            r#"{"type":"redirect","data":{"session_id":"s1","intent_hint":"Comparison"}}"#,
            TurnRequest::Redirect {
                session_id: "s1".into(),
                intent_hint: "Comparison".into(),
            },
        ),
        (
            // G2: the cancel carries NOTHING — the host aborts whatever is
            // in flight on this socket, and the turn's own terminal frame
            // says how it ended.
            r#"{"type":"cancel","data":{}}"#,
            TurnRequest::Cancel {},
        ),
    ];
    for (wire, expected) in cases {
        let parsed: TurnRequest = serde_json::from_str(wire).expect("client message parses");
        assert_eq!(parsed, expected, "inbound wire form changed for {wire}");
        assert_eq!(
            serde_json::to_string(&parsed).expect("re-serialises"),
            wire,
            "inbound frame is not symmetric"
        );
    }
}

/// **The absent key is the contract** (scope item 5 of order
/// quality-check-lean; ARCH §18.3).
///
/// `svrn chat ask --format json` was a host that read the message row it had
/// just written; phase 6 made it a surface and the three how-it-was-served
/// facts stopped crossing the boundary, leaving SQL as the only reader. They
/// ride `Complete` now — and a turn that opened NO ledger must produce no
/// `stage_attribution` key at all. `null` and `{}` are both readings a
/// consumer would have to guess at: `{}` says "measured, nothing to report"
/// about a turn that was never measured, which is the flattering direction
/// and the one `TurnStageLedger`'s own doc forbids.
#[test]
fn a_turn_that_opened_no_ledger_has_no_stage_attribution_key() {
    use sovereign_contracts::types::projection::{project_turn_metadata, TurnMetadata};
    use sovereign_contracts::types::{ServedBy, StackOwner, StageId, StageRow, TurnStageLedger};

    // 1. No ledger at all — the projection reports absence, not an empty one.
    let no_ledger = serde_json::json!({"routed_intent": "DeepQuery"});
    let projected = project_turn_metadata(&Some(no_ledger)).expect("routed_intent is a fact");
    assert!(projected.stage_attribution.is_none());
    let wire = serde_json::to_value(&projected).unwrap();
    assert!(
        wire.get("stage_attribution").is_none(),
        "an unmeasured turn must carry no stage_attribution key, got {wire}"
    );
    assert_eq!(
        wire.get("routed_intent").and_then(|v| v.as_str()),
        Some("DeepQuery")
    );

    // 2. An explicit `null` in the blob is the same absence, not a value.
    let nulled = serde_json::json!({
        "routed_intent": "DeepQuery",
        "stage_attribution": serde_json::Value::Null,
        "grounding_gate": serde_json::Value::Null,
    });
    let projected = project_turn_metadata(&Some(nulled)).unwrap();
    assert!(projected.stage_attribution.is_none());
    assert!(projected.grounding_gate.is_none());

    // 3. A turn that DID open one round-trips it.
    let ledger = TurnStageLedger::seal(
        26_605,
        vec![StageRow {
            stage: StageId::Retrieval,
            owner: StackOwner::Shared,
            ms: 410,
            mechanism: None,
            cause: None,
            calls: Some(1),
        }],
    );
    let blob = serde_json::json!({
        "routed_intent": "DeepQuery",
        "grounding_gate": {"action": "citation_grounded", "mode": "citation", "located": 0},
        "stage_attribution": serde_json::to_value(&ledger).unwrap(),
    });
    let projected = project_turn_metadata(&Some(blob)).unwrap();
    let back = projected
        .stage_attribution
        .as_ref()
        .expect("the ledger crossed");
    assert_eq!(back.total_ms, 26_605);
    assert_eq!(back.rows[0].stage, StageId::Retrieval);
    assert_ne!(back.served_by, ServedBy::NativeOnly);
    assert_eq!(
        projected
            .grounding_gate
            .as_ref()
            .and_then(|g| g.get("action"))
            .and_then(|v| v.as_str()),
        Some("citation_grounded")
    );

    // 4. Nothing at all in the blob is `None`, never an empty TurnMetadata —
    //    so "the host does not send this" stays tellable from "the turn had
    //    nothing to report".
    assert!(project_turn_metadata(&Some(serde_json::json!({"streamed": true}))).is_none());
    assert!(project_turn_metadata(&None).is_none());
    assert!(TurnMetadata::default().is_empty());
}
