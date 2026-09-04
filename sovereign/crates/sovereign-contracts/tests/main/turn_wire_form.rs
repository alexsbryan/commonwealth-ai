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
use sovereign_contracts::types::NarrationPhase;
use sovereign_contracts::types::{TurnFrame, TurnMode, TurnRequest};

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
            r#"{"type":"approve","data":{"task_id":"t1","step_id":2,"approved":true}}"#,
            TurnRequest::Approve {
                task_id: "t1".into(),
                step_id: 2,
                approved: true,
            },
        ),
        (
            r#"{"type":"user_reply","data":{"task_id":"t1","content":"yes"}}"#,
            TurnRequest::UserReply {
                task_id: "t1".into(),
                content: "yes".into(),
            },
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
