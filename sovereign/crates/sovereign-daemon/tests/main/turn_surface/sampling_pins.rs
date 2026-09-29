// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite (pb-bench-dials-wire).
//!
//! A turn's `sampling` pins reach ITS model calls and no other turn's: the
//! daemon answers every socket from one `Runtime`, so a pin that landed on
//! the session-wide `inference_config` would leak into the next turn. Both
//! halves are asserted from the requests the provider actually received.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use sovereign_contracts::types::{
    CompletionRequest, InferenceConfig, SamplingOverrides, TurnFrame, TurnMode, TurnRequest,
};
use sovereign_daemon::turn_http::turn_router;

use super::{create_conversation, open_stream, send_request, serving_daemon};
use crate::common::{spawn_router, TestProvider};

type Log = Arc<Mutex<Vec<CompletionRequest>>>;

/// Ask one turn on a fresh conversation and return its terminal frame and
/// the temperatures of every model call it made.
async fn ask(
    addr: std::net::SocketAddr,
    log: &Log,
    sampling: Option<SamplingOverrides>,
) -> (TurnFrame, Vec<Option<f32>>) {
    log.lock().unwrap().clear();
    let conv = create_conversation(&format!("http://{addr}")).await;
    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Message {
            content: "what temperature did this turn run at?".into(),
            mode: TurnMode::Grounded,
            intent: None,
            sampling,
        },
    )
    .await;
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("a TurnFrame");
            if matches!(
                frame,
                TurnFrame::Complete { .. } | TurnFrame::StreamError { .. }
            ) {
                return frame;
            }
        }
        panic!("the socket closed before a terminal frame");
    })
    .await
    .expect("the turn ended within 60s");
    let temps = log.lock().unwrap().iter().map(|r| r.temperature).collect();
    (terminal, temps)
}

#[tokio::test]
async fn a_turns_temperature_pin_reaches_its_model_call_and_no_other_turns() {
    let log: Log = Arc::default();
    let provider = TestProvider::new()
        .with_stream_chunks(vec!["answered.".to_string()])
        .with_request_log(Arc::clone(&log));
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let default = InferenceConfig::default().temperature;
    assert_ne!(
        default, 0.0,
        "the pin must differ from the default to be seen"
    );

    let pin = SamplingOverrides {
        temperature: Some(0.0),
        ..Default::default()
    };
    let (frame, pinned) = ask(addr, &log, Some(pin)).await;
    assert!(matches!(frame, TurnFrame::Complete { .. }), "{frame:?}");
    assert!(
        pinned.contains(&Some(0.0)),
        "the pinned turn's model call ran at 0: {pinned:?}"
    );
    assert!(
        !pinned.contains(&Some(default)),
        "no call of the pinned turn ran at the daemon default: {pinned:?}"
    );

    // The next turn on the SAME daemon carries no pin: it must run at the
    // default, which it would not if the pin had landed session-wide.
    let (frame, unpinned) = ask(addr, &log, None).await;
    assert!(matches!(frame, TurnFrame::Complete { .. }), "{frame:?}");
    assert!(
        unpinned.contains(&Some(default)),
        "an unpinned turn runs at the daemon default ({default}): {unpinned:?}"
    );
}

#[tokio::test]
async fn a_top_p_pin_is_refused_by_name_before_any_model_call() {
    let log: Log = Arc::default();
    let provider = TestProvider::new()
        .with_stream_chunks(vec!["answered.".to_string()])
        .with_request_log(Arc::clone(&log));
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let pin = SamplingOverrides {
        top_p: Some(0.9),
        ..Default::default()
    };
    let (frame, calls) = ask(addr, &log, Some(pin)).await;
    match frame {
        TurnFrame::StreamError { message, .. } => {
            assert!(message.contains("top_p"), "names the pin: {message}")
        }
        other => panic!("a top_p pin is refused, not dropped: {other:?}"),
    }
    assert!(calls.is_empty(), "refused before any model call: {calls:?}");
}
