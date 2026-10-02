// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attach-mode conformance: wire tests against a loopback daemon. A sibling of
//! `lib.rs` so that file stays under its arch-gate slack (ARCH §3.1).

use super::*;

// ═══════════════════════════════════════════════════════════════
// ATTACH-MODE CONFORMANCE
//
// `SplitInferenceProvider` is the provider the SHIPPED desktop
// runs: the supervisor spawns a child daemon and the app rewrites
// its own boot mode to Attach, so this wrapper — not the embedded
// engine — serves virtually every real user.
//
// The recurring defect on it is never a wrong implementation. It is
// a MISSING one. `InferenceProvider` defaults 21 of its 25 methods,
// and 16 of those defaults return `Ok(())` / `None` / `vec![]` /
// `"unknown"` — plausible answers that actually mean "I don't
// know". Inherit one by accident and the wrapper reports success
// while doing nothing, forwarding to no one, with the correct
// implementation sitting one field away on `self.chat`. Rust emits
// no diagnostic: the code compiles exactly as written. Three
// shipped bugs came from this — `primary_slot_status`,
// `warmup_primary`, `complete_stream_with_finish`.
//
// So these assert WIRE BEHAVIOUR, which is the only thing a silent
// no-op cannot fake: a method that falls through to the default
// never opens a socket, and a synthesised terminal frame never
// carries the wire's own values. Asserting the URL-building helper
// instead is what let `warmup_primary` ship broken — that test
// passed on the one sound link of a three-link chain.
// ═══════════════════════════════════════════════════════════════

/// A one-shot loopback daemon: serves `responses` in order, then
/// hands back every request line it saw.
///
/// Raw TCP on purpose — `oicp-client` is a contract crate with no
/// dev-dependencies, and pulling an HTTP mock framework in to
/// exercise four verbs is a worse trade than this much `std`.
struct MockDaemon {
    port: u16,
    server: std::thread::JoinHandle<Vec<String>>,
}

impl MockDaemon {
    fn serving(responses: Vec<String>) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for body in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                // The WHOLE head, not just the request line —
                // `request_lines` narrows it back down. Headers
                // are behaviour too: `X-Node-Id` decides whether
                // the receiving daemon treats a request as peer
                // traffic, and a request line cannot show it.
                seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
            seen
        });
        Self { port, server }
    }

    /// The Attach-mode provider, pointed at this mock.
    fn attach_provider(&self) -> SplitInferenceProvider {
        SplitInferenceProvider::new(
            &format!("http://127.0.0.1:{}/v1", self.port),
            "chat-model".to_string(),
            "embed-model".to_string(),
            8192,
            String::new(),
        )
    }

    /// Request lines observed, in order. Consumes the mock.
    fn request_lines(self) -> Vec<String> {
        self.request_heads()
            .into_iter()
            .map(|head| head.lines().next().unwrap_or("").to_string())
            .collect()
    }

    /// Full request heads (request line + headers), in order.
    /// Consumes the mock.
    fn request_heads(self) -> Vec<String> {
        self.server.join().unwrap()
    }

    /// A plain `RemoteApiProvider` pointed at this mock.
    fn provider(&self) -> RemoteApiProvider {
        RemoteApiProvider::new(
            &format!("http://127.0.0.1:{}/v1", self.port),
            None,
            "chat-model",
            8192,
        )
    }
}

fn http_ok(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// What the daemon emits when its slot queue sheds — the shape
/// `commonwealth-api::admission::shed_response` renders, header and
/// all. `shed_retry_after` reads `retry_after_secs` out of the BODY;
/// the header is here because the real response carries it and a
/// fixture that drops it would let a body-only parser pass on a
/// response no daemon sends.
fn http_shed(retry_after_secs: u64) -> String {
    let body = format!(
        r#"{{"error":"host busy","reason":"local_queue_full","retry_after_secs":{retry_after_secs}}}"#
    );
    format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
         Retry-After: {retry_after_secs}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// What a real daemon emits for a TRUNCATED generation: two tokens,
/// `finish_reason: "length"`, and a usage record. Truncation is the
/// case that matters — a synthesised terminal frame reports it as a
/// clean stop, and the caller cannot tell the difference.
fn sse_truncated_generation() -> String {
    [
        r#"data: {"choices":[{"delta":{"content":"held "},"finish_reason":null}]}"#,
        "",
        r#"data: {"choices":[{"delta":{"content":"token"},"finish_reason":"length"}],"usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}}"#,
        "",
        "data: [DONE]",
        "",
    ]
    .join("\n")
}

fn a_request() -> CompletionRequest {
    let mut request = CompletionRequest::default();
    request.prompt = "anything".into();
    request
}

#[tokio::test]
async fn attach_warmup_reaches_the_daemon() {
    let daemon = MockDaemon::serving(vec![http_ok("application/json", r#"{"latency_ms":0}"#)]);

    daemon.attach_provider().warmup_primary().await.unwrap();

    assert_eq!(
        daemon.request_lines(),
        vec!["POST /internal/inference/warmup HTTP/1.1"],
        "the trait default is a silent Ok(()) — the ONLY proof of a real \
         warm-up is that a request left the process"
    );
}

#[tokio::test]
async fn attach_slot_status_asks_the_node_that_owns_the_weights() {
    let body = r#"{"inference":{"resident":[
        {"role":"fast","model_id":"fast-4b","resident":true},
        {"role":"primary","model_id":"deep-35b","resident":false,
         "size_bytes":18525200896,"transitioning":false}]}}"#;
    let daemon = MockDaemon::serving(vec![http_ok("application/json", body)]);

    let slot = daemon
        .attach_provider()
        .primary_slot_status()
        .await
        .expect("this provider owns no weights, so it must ASK — the default reads its own empty resident_slots() and answers None forever");

    assert_eq!(slot.model_id, "deep-35b");
    assert!(
        !slot.resident,
        "a COLD primary is the whole point of the call; the cold row must \
         survive the round-trip verbatim"
    );
    assert_eq!(slot.size_bytes, Some(18_525_200_896));
    assert_eq!(daemon.request_lines(), vec!["GET /status HTTP/1.1"]);
}

#[tokio::test]
async fn attach_streaming_reports_the_wires_finish_reason_not_a_synthesised_stop() {
    let daemon = MockDaemon::serving(vec![http_ok(
        "text/event-stream",
        &sse_truncated_generation(),
    )]);
    let provider = daemon.attach_provider();

    let mut stream = provider
        .complete_stream_with_finish(&a_request())
        .await
        .unwrap();
    let (mut text, mut finish) = (String::new(), None);
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(t) => text.push_str(&t),
            StreamFrame::Finish { reason, usage } => finish = Some((reason, usage)),
            StreamFrame::Error(e) => panic!("unexpected stream error: {e}"),
        }
    }

    assert_eq!(text, "held token");
    let (reason, usage) = finish.expect("every stream must end with a terminal frame");
    assert_eq!(
        reason,
        FinishReason::Length,
        "the trait default appends Finish{{Stop}} it never observed, making a \
         max_tokens truncation indistinguishable from a clean finish"
    );
    assert_eq!(
        usage.map(|u| u.total_tokens),
        Some(9),
        "the default drops usage entirely, so token accounting reads None"
    );
}

#[tokio::test]
async fn attach_streaming_with_id_names_the_model_and_keeps_the_finish_reason() {
    // The composed default (`complete_stream_with_id_and_finish`)
    // is only as honest as the two methods it calls. It needs no
    // forward of its own — but that is a CONSEQUENCE of the other
    // two being right, so it is pinned rather than assumed.
    let daemon = MockDaemon::serving(vec![http_ok(
        "text/event-stream",
        &sse_truncated_generation(),
    )]);
    let provider = daemon.attach_provider();

    let (mut stream, model_id) = provider
        .complete_stream_with_id_and_finish(&a_request())
        .await
        .unwrap();
    assert_eq!(
        model_id, "chat-model",
        "\"unknown\" here poisons the provenance of every streamed response"
    );

    let mut finish = None;
    while let Some(frame) = stream.next().await {
        if let StreamFrame::Finish { reason, .. } = frame {
            finish = Some(reason);
        }
    }
    assert_eq!(finish, Some(FinishReason::Length));
}

/// The methods answered from the provider's own fields. No daemon:
/// reaching the network here would itself be the bug.
#[test]
fn attach_answers_from_its_own_state_without_the_unknown_sentinels() {
    let provider = SplitInferenceProvider::new(
        "http://127.0.0.1:1/v1",
        "chat-model".to_string(),
        "embed-model".to_string(),
        8192,
        String::new(),
    );

    assert_eq!(provider.model_id_for(Speed::Slow), "chat-model");
    assert_eq!(provider.model_id_for(Speed::Fast), "chat-model");
    assert_eq!(
        provider.embed_model_id(),
        "embed-model",
        "\"unknown\" is the documented 'cannot verify' sentinel — returning it \
         here would silently disable the persisted-embedding staleness guard"
    );
    assert_eq!(provider.effective_context_size(), Some(8192));
}

/// A LEDGER of the defaults this provider still inherits — not an
/// endorsement of them. Each line is a known gap in Attach mode,
/// recorded so it is countable instead of invisible.
///
/// If you implement one of these, this test FAILS. That failure is
/// the point: delete the line, and the gap is gone from the ledger
/// too. A silent gap is what produced the three bugs above.
#[tokio::test]
async fn attach_remaining_gaps_are_recorded_not_forgotten() {
    let provider = SplitInferenceProvider::new(
        "http://127.0.0.1:1/v1",
        "chat-model".to_string(),
        "embed-model".to_string(),
        8192,
        String::new(),
    );

    // Heuristic, not the daemon's real BPE vocab: Attach and Local
    // therefore budget context differently for the same text.
    assert_eq!(provider.count_tokens("12345678"), 2);
    // The Settings "you can raise ctx to N" ceiling is absent.
    assert_eq!(provider.n_ctx_train_for_primary(), None);
    // Extras loaded on the daemon are invisible to the desktop.
    assert!(provider.extras_inventory().is_empty());
    // Honest here, unlike the others: this provider genuinely holds
    // no slots. It is `primary_slot_status` that must not be
    // derived from it — see the dedicated test above.
    assert!(provider.resident_slots().is_empty());
}

#[tokio::test]
async fn capabilities_url_resolves_at_daemon_root_for_both_endpoint_shapes() {
    // `/oicp/v1/capabilities` is mounted at the daemon root, so a `/v1`
    // endpoint (chat-bootstrap shape) must strip it — otherwise the fetch
    // hits `…/v1/oicp/v1/capabilities` (404) and manifest-driven context
    // silently falls back to the hardcoded default.
    let with_v1 = RemoteApiProvider::new("http://host:9741/v1", None, "m", 4096);
    let bare = RemoteApiProvider::new("http://host:9741", None, "m", 4096);
    assert_eq!(with_v1.daemon_root().await.unwrap(), "http://host:9741");
    assert_eq!(bare.daemon_root().await.unwrap(), "http://host:9741");
    assert_eq!(
        format!(
            "{}/oicp/v1/capabilities",
            with_v1.daemon_root().await.unwrap()
        ),
        "http://host:9741/oicp/v1/capabilities"
    );
}

// ── M5 piece 3: the peer identity stamp ────────────────────
//
// Wire assertions, per this module's own doctrine above: the
// failure being guarded is a header that is silently ABSENT, and
// an absent header changes nothing a caller can observe. The
// request still succeeds. It is simply admitted on the far side
// as local traffic, bypassing the operator's pause, the
// foreground yield and the `max_peer_inflight` ceiling. Only the
// bytes on the socket can tell the two apart.

#[tokio::test]
async fn a_node_stamped_provider_identifies_itself_on_every_request() {
    let daemon = MockDaemon::serving(vec![http_ok(
        "application/json",
        r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
    )]);
    let provider = daemon.provider().with_node_id("00c0ffee");

    let _ = provider.complete(&a_request()).await;

    let head = daemon.request_heads().remove(0).to_ascii_lowercase();
    assert!(
        head.contains("x-node-id: 00c0ffee"),
        "the chat completion must carry the node id; head was:\n{head}"
    );
}

/// A resolver pointed at a fixed base — the mock stand-in for
/// `EntryNodeEndpoint`, which resolves a bound node's identity through the
/// mesh on every call.
#[derive(Debug)]
struct FixedResolver(String);

#[async_trait::async_trait]
impl EndpointResolver for FixedResolver {
    async fn base_url(&self) -> Option<String> {
        Some(self.0.clone())
    }
    fn describe(&self) -> String {
        "test-binding".to_string()
    }
}

fn terminal_provider(port: u16, node_id: Option<String>) -> SplitInferenceProvider {
    SplitInferenceProvider::resolved(
        std::sync::Arc::new(FixedResolver(format!("http://127.0.0.1:{port}/v1"))),
        sovereign_contracts::traits::ServingLocus::ForwardsOffBox,
        "primary".to_string(),
        "embed-model".to_string(),
        8192,
        String::new(),
        node_id,
    )
}

/// **A terminal identifies itself to its entry node — on BOTH slots.**
///
/// `with_node_id` had exactly one call site in the tree
/// (`provider_for_peer`), and `SplitInferenceProvider::resolved` — the
/// terminal's own provider — never called it. So every request a terminal
/// sent its entry node arrived unstamped and was admitted as the entry
/// node's OWN LOCAL traffic: unrationable (the ceiling, the foreground
/// yield and the contribution pause are all keyed on the header) and
/// unaccounted (invisible on `/status.inference.peer_requests`).
///
/// Measured 2026-08-31: RuggedFox's embeddings reached MAC and left no
/// trace there beyond the terminal's own log line, which is why the
/// obvious two-machine corroboration could not pass.
///
/// Both slots asserted in ONE test because they are one decision. The
/// embed slot is the one that matters most here — a terminal's ingest is
/// embeddings, so a chat-only stamp would leave the bulk of its cost
/// invisible.
#[tokio::test]
async fn a_terminal_stamps_its_identity_on_chat_and_embeddings_alike() {
    let daemon = MockDaemon::serving(vec![
        http_ok(
            "application/json",
            r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
        ),
        http_ok("application/json", r#"{"data":[{"embedding":[0.5]}]}"#),
    ]);
    let provider = terminal_provider(daemon.port, Some("f1f2589f".to_string()));

    let _ = provider.complete(&a_request()).await;
    let _ = provider.embed("some text").await;

    let heads = daemon.request_heads();
    assert_eq!(
        heads.len(),
        2,
        "both slots must have dialled the entry node"
    );
    for (slot, head) in ["chat", "embed"].iter().zip(heads.iter()) {
        assert!(
            head.to_ascii_lowercase().contains("x-node-id: f1f2589f"),
            "the {slot} slot must identify this node to its entry node, or the \
             entry node admits the turn as its own local traffic; head was:\n{head}"
        );
    }
}

/// The control, and the reason the test above is a gate rather than a
/// tautology: `None` really does send nothing.
///
/// `None` is the honest "this node's identity is unknown" — a terminal
/// booting before it has ever joined a mesh has no persisted node id yet.
/// Synthesising a placeholder instead would be worse than unstamped:
/// `parse_x_node_id` buckets an unparseable value under the zero node and
/// STILL gates it, so every terminal in a fleet would ration as one peer.
#[tokio::test]
async fn a_terminal_that_cannot_name_itself_sends_no_identity() {
    let daemon = MockDaemon::serving(vec![http_ok(
        "application/json",
        r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
    )]);

    let _ = terminal_provider(daemon.port, None)
        .complete(&a_request())
        .await;

    let head = daemon.request_heads().remove(0).to_ascii_lowercase();
    assert!(
        !head.contains("x-node-id"),
        "an unknown identity must be reported as absent, never defaulted; \
         head was:\n{head}"
    );
}

#[tokio::test]
async fn an_unstamped_provider_sends_no_identity_at_all() {
    // The control, and the reason the test above is a gate: this
    // provider is what a bench, an Ollama user or an OpenAI
    // endpoint gets, and none of them are mesh peers. Stamping
    // unconditionally would tell every third-party endpoint a
    // node identity it has no business holding.
    let daemon = MockDaemon::serving(vec![http_ok(
        "application/json",
        r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
    )]);

    let _ = daemon.provider().complete(&a_request()).await;

    let head = daemon.request_heads().remove(0).to_ascii_lowercase();
    assert!(
        !head.contains("x-node-id"),
        "an unstamped provider must present as local traffic; head was:\n{head}"
    );
}

/// The stamp lives in ONE body (`stamped`) precisely so a new
/// outbound method cannot quietly ship without it. This pins the
/// streaming path, which is the one the product's chat actually
/// uses — non-streaming passing tells you nothing about it.
#[tokio::test]
async fn the_streaming_path_is_stamped_too() {
    let daemon = MockDaemon::serving(vec![http_ok("text/event-stream", "data: [DONE]\n\n")]);
    let provider = daemon.provider().with_node_id("00c0ffee");

    let _ = provider.complete_stream(&a_request()).await;

    let head = daemon.request_heads().remove(0).to_ascii_lowercase();
    assert!(
        head.contains("x-node-id: 00c0ffee"),
        "the streaming completion must carry the node id; head was:\n{head}"
    );
}

/// The two halves of the shed policy, in one test, because they are one
/// decision seen from two sides — and shipping the retry ON by default on
/// 2026-08-26 proved that half of it alone is a mesh regression.
///
/// A WIRE assertion for the same reason the stamp tests above are: a
/// provider that quietly gave up and one that quietly waited both return
/// an `Err` to the caller. Only the socket count separates them.
///
/// Named failing input (ARCH §18.1): drop `.waiting_out_sheds()` from
/// either slot in `SplitInferenceProvider::new` and half A fails with one
/// request line instead of two; add it to `provider_for_peer` and half B
/// hangs the mock's second accept, which is `chat_completion_e2e`'s
/// `a_yielding_peer_is_asked_once_not_once_per_turn` restated at this
/// layer.
#[tokio::test]
async fn the_daemon_backed_slot_waits_out_a_shed_and_a_bare_provider_reports_it() {
    // A. The last resort. Scripted shed-then-answer: the client only ever
    //    sees the answer if it comes back for it.
    let daemon = MockDaemon::serving(vec![
        http_shed(1),
        http_ok(
            "application/json",
            r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#,
        ),
    ]);
    let served = daemon.attach_provider().complete(&a_request()).await;
    assert!(
        served.is_ok(),
        "the daemon-backed slot owns no weights and has nowhere to route: a shed \
         is a wait, not a verdict — got {served:?}"
    );
    assert_eq!(
        daemon.request_lines().len(),
        2,
        "the wait has to reach the socket; one request line means the hint was \
         computed, transported and dropped (note `bf432b4d`)"
    );

    // B. The control, and the invariant the default protects. ONE response
    //    scripted, so a provider that re-dialled would block the mock's
    //    second `accept` — the failure is a hang, which is louder than a
    //    wrong count and is the point.
    let peer = MockDaemon::serving(vec![http_shed(1)]);
    let refused = peer.provider().complete(&a_request()).await;
    let err = refused.expect_err("a bare provider must surface the shed, not absorb it");
    assert!(
        err.to_string().contains("503"),
        "a peer shed is a ROUTING signal and must arrive intact so the cascade \
         tries elsewhere — got {err}"
    );
    assert_eq!(
        peer.request_lines().len(),
        1,
        "a peer is asked ONCE; re-dialling inside its own retry window is the \
         failed-hop tax MESH_SCALE §9.1.1 measures"
    );
}
