// SPDX-License-Identifier: AGPL-3.0-or-later
//! Command bridge — a localhost HTTP surface that lets external test
//! harnesses (the Playwright real-mode suite) drive the production
//! Tauri command dispatch and observe the app's event stream.
//!
//! Glassbox rationale: the desktop's entire UI is a thin layer over the
//! `generate_handler!` command surface. Exposing that surface over
//! loopback HTTP lets automation exercise the real backend (real
//! routing, retrieval, inference, supervisor) through the same dispatch
//! path the webview uses, while a JSONL ledger records every command
//! invoked so coverage is measurable rather than assumed.
//!
//! ## Dispatch fidelity
//!
//! `POST /invoke` synthesizes a [`tauri::webview::InvokeRequest`] and
//! feeds it to [`tauri::Webview::on_message`] — the same entry point the
//! real IPC layer uses. That means the invoke-key check, ACL resolution,
//! camelCase argument deserialization, and `State`/`AppHandle` injection
//! are all the production code path; the only layer skipped is wry's
//! postMessage wire serialization. `InvokeRequest` is documented as
//! "NOT part of the public stable API … meant for external testing /
//! fuzzing tools or custom invoke systems" — exactly this use. The
//! crate is pinned by Cargo.lock; revisit on tauri upgrades.
//!
//! ## Security posture
//!
//! Debug builds only (`cfg(debug_assertions)` at the call site in
//! main.rs), opt-in via `SOVEREIGN_COMMAND_BRIDGE=1`, bound to
//! 127.0.0.1. This is a development/test surface and must never ship
//! enabled in a release binary.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponse, InvokeResponseBody};
use tauri::webview::InvokeRequest;
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::broadcast;

pub const DEFAULT_PORT: u16 = 9745;
const MAIN_WINDOW_LABEL: &str = "main";

/// Lifecycle events whose latest payload is buffered and replayed to a
/// late-attaching listener. The real app emits these once near boot
/// (see main.rs / supervisor.rs emit sites); a Playwright page that
/// loads minutes later would otherwise wait forever for a handshake
/// that already happened. Mirrors the synthetic suite's poll-emit
/// `bootToChat` pattern, but driven by the real emission.
const STICKY_EVENTS: [&str; 4] = [
    "backend-ready",
    "setup-required",
    "backend-error",
    "supervisor-state",
];

/// Broadcast capacity. Real token streams emit thousands of
/// `message-chunk` events per turn; a slow SSE consumer past this
/// many buffered rows sees an explicit `{"lagged": n}` marker rather
/// than silent loss.
const EVENT_BUFFER: usize = 8192;

/// Gate read by main.rs before spawning the server.
pub fn enabled() -> bool {
    std::env::var("SOVEREIGN_COMMAND_BRIDGE").is_ok_and(|v| v == "1")
}

fn port() -> u16 {
    std::env::var("SOVEREIGN_COMMAND_BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// One forwarded Tauri event. `seq` is a process-global monotonic
/// counter: Tauri invokes Rust listeners synchronously on the emitting
/// thread, so for a single emitter (e.g. one streaming turn's
/// `message-chunk`s) seq order == emit order, end to end through the
/// single SSE stream.
#[derive(Clone, Serialize)]
struct EventRow {
    seq: u64,
    event: String,
    payload: Value,
}

/// Rows kept in the replay ring buffer served by `GET /events/recent`.
/// Sized for one busy turn (a long stream is a few thousand chunks).
const RECENT_RING: usize = 4096;

struct EventHub {
    seq: AtomicU64,
    tx: broadcast::Sender<EventRow>,
    /// Event names already wired through `listen_any` — Tauri has no
    /// wildcard listener and the app emits dynamic names
    /// (`local-corpus://progress/{job_id}`), so subscription is lazy,
    /// driven by the page's `plugin:event|listen` calls.
    subscribed: Mutex<HashSet<String>>,
    /// Latest payload per sticky event (see [`STICKY_EVENTS`]).
    sticky: Mutex<HashMap<String, Value>>,
    /// Last [`RECENT_RING`] published rows — lets harness code poll
    /// for events without racing the live SSE stream (the broadcast
    /// only serves consumers connected at emit time; a fast job can
    /// finish entirely inside that window).
    recent: Mutex<std::collections::VecDeque<EventRow>>,
}

impl EventHub {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            tx: broadcast::channel(EVENT_BUFFER).0,
            subscribed: Mutex::new(HashSet::new()),
            sticky: Mutex::new(HashMap::new()),
            recent: Mutex::new(std::collections::VecDeque::with_capacity(RECENT_RING)),
        }
    }

    /// Idempotently wire `event` from the Tauri event system into the
    /// broadcast. Returns the buffered payload if `event` is sticky and
    /// has already fired.
    fn ensure_subscribed(self: &Arc<Self>, app: &AppHandle, event: &str) -> Option<Value> {
        {
            let mut subscribed = self.subscribed.lock().unwrap();
            if !subscribed.contains(event) {
                subscribed.insert(event.to_string());
                let hub = Arc::clone(self);
                let name = event.to_string();
                app.listen_any(event.to_string(), move |raw| {
                    let payload: Value =
                        serde_json::from_str(raw.payload()).unwrap_or(Value::Null);
                    hub.publish(&name, payload);
                });
            }
        }
        self.sticky.lock().unwrap().get(event).cloned()
    }

    fn publish(&self, event: &str, payload: Value) {
        if STICKY_EVENTS.contains(&event) {
            self.sticky
                .lock()
                .unwrap()
                .insert(event.to_string(), payload.clone());
        }
        let row = EventRow {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            event: event.to_string(),
            payload,
        };
        {
            let mut recent = self.recent.lock().unwrap();
            if recent.len() == RECENT_RING {
                recent.pop_front();
            }
            recent.push_back(row.clone());
        }
        // No live receiver is fine — sticky events replay via /listen,
        // and the recent ring serves polling consumers.
        let _ = self.tx.send(row);
    }

    fn recent_since(&self, since_seq: u64) -> Vec<EventRow> {
        self.recent
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.seq >= since_seq)
            .cloned()
            .collect()
    }
}

#[derive(Clone)]
struct BridgeState {
    app: AppHandle,
    events: Arc<EventHub>,
}

pub async fn serve(app: AppHandle) {
    let events = Arc::new(EventHub::new());
    // Sticky lifecycle events are subscribed eagerly so a payload
    // emitted during boot is buffered even before any client attaches.
    for name in STICKY_EVENTS {
        events.ensure_subscribed(&app, name);
    }

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port()));
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/invoke", post(invoke))
        .route("/listen", post(listen))
        .route("/events", get(events_stream))
        .route("/events/recent", get(events_recent))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(BridgeState { app, events });

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("command-bridge: failed to bind {addr}: {e}");
            return;
        }
    };
    tracing::info!("command-bridge: listening on http://{addr}");
    if let Err(e) = axum::serve(listener, router).await {
        tracing::error!("command-bridge: server exited: {e}");
    }
}

async fn healthz() -> Json<Value> {
    Json(json!({ "ok": true, "service": "sovereign-command-bridge" }))
}

#[derive(Deserialize)]
struct ListenPayload {
    event: String,
}

/// Wire one event name into the SSE stream (idempotent). When a sticky
/// event already fired, `replayed` is true and `replay` carries its
/// buffered payload — the page-side shim emits it locally to its own
/// listeners, killing the boot race without generic replay machinery.
/// `replayed` is a separate flag because lifecycle events emit `()`,
/// which serializes to JSON null — `replay` alone can't distinguish
/// "nothing buffered" from "buffered null payload".
async fn listen(
    State(state): State<BridgeState>,
    Json(payload): Json<ListenPayload>,
) -> Json<Value> {
    let replay = state.events.ensure_subscribed(&state.app, &payload.event);
    Json(json!({
        "ok": true,
        "event": payload.event,
        "replayed": replay.is_some(),
        "replay": replay,
    }))
}

#[derive(Deserialize)]
struct RecentQuery {
    #[serde(default)]
    since_seq: u64,
}

/// Snapshot of the replay ring (last [`RECENT_RING`] published rows,
/// optionally filtered to `?since_seq=N`). Poll this instead of the
/// live SSE stream when the events of interest may fire before a
/// consumer can connect — e.g. a fast ingest job's progress channel.
async fn events_recent(
    State(state): State<BridgeState>,
    axum::extract::Query(q): axum::extract::Query<RecentQuery>,
) -> Json<Value> {
    let rows = state.events.recent_since(q.since_seq);
    Json(json!({ "ok": true, "rows": rows }))
}

/// Single SSE stream carrying every subscribed event in `seq` order.
/// A consumer that falls more than EVENT_BUFFER rows behind receives an
/// explicit `{"lagged": n}` row instead of silent loss.
async fn events_stream(
    State(state): State<BridgeState>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.events.tx.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(row) => {
                    let data = serde_json::to_string(&row)
                        .unwrap_or_else(|_| "{\"event\":\"__serialize_error__\"}".into());
                    return Some((Ok(SseEvent::default().data(data)), rx));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    return Some((
                        Ok(SseEvent::default().data(format!("{{\"lagged\":{n}}}"))),
                        rx,
                    ));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct InvokePayload {
    cmd: String,
    #[serde(default)]
    args: Option<Value>,
}

/// Dispatch one command through the production invoke path and return
/// `{ok: true, result}` or `{ok: false, error}` — mirroring the
/// resolve/reject semantics the frontend `invoke()` sees.
async fn invoke(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Json(payload): Json<InvokePayload>,
) -> (StatusCode, Json<Value>) {
    let started = std::time::Instant::now();
    let spec = headers
        .get("x-sovereign-spec")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let Some(window) = state.app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "error": "main window not available" })),
        );
    };
    let webview = window.as_ref().clone();
    let url = match webview.url() {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false, "error": format!("webview url: {e}") })),
            );
        }
    };

    let request = InvokeRequest {
        cmd: payload.cmd.clone(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url,
        body: InvokeBody::Json(payload.args.unwrap_or_else(|| json!({}))),
        headers: HeaderMap::default(),
        invoke_key: state.app.invoke_key().to_string(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<InvokeResponse>();
    webview.on_message(
        request,
        Box::new(move |_webview, _cmd, response, _callback, _error| {
            let _ = tx.send(response);
        }),
    );

    let response = match rx.await {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": "responder dropped without a response" })),
            );
        }
    };

    let (ok, body) = match response {
        InvokeResponse::Ok(InvokeResponseBody::Json(raw)) => {
            let result: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            (true, json!({ "ok": true, "result": result }))
        }
        InvokeResponse::Ok(InvokeResponseBody::Raw(bytes)) => {
            (true, json!({ "ok": true, "result": bytes }))
        }
        InvokeResponse::Err(e) => (false, json!({ "ok": false, "error": e.0 })),
    };
    ledger::record(&payload.cmd, ok, started.elapsed().as_millis() as u64, &spec);
    (StatusCode::OK, Json(body))
}

/// Coverage ledger — one JSONL row per dispatched command, appended to
/// `$SOVEREIGN_COMMAND_BRIDGE_LEDGER` when set. Joined against the
/// generate_handler! manifest by tests/e2e/scripts/coverage-report.mjs.
mod ledger {
    use std::io::Write;
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path() -> Option<&'static str> {
        static PATH: OnceLock<Option<String>> = OnceLock::new();
        PATH.get_or_init(|| std::env::var("SOVEREIGN_COMMAND_BRIDGE_LEDGER").ok())
            .as_deref()
    }

    pub fn record(cmd: &str, ok: bool, dur_ms: u64, spec: &str) {
        let Some(path) = path() else { return };
        if let Err(e) = append_row(std::path::Path::new(path), cmd, ok, dur_ms, spec) {
            tracing::warn!("command-bridge: ledger append failed: {e}");
        }
    }

    /// One JSONL row, O_APPEND. Single-line writes at our row sizes are
    /// atomic, so parallel invokes can share the file; a ledger write
    /// must never fail the invoke it describes (caller only logs).
    pub fn append_row(
        path: &std::path::Path,
        cmd: &str,
        ok: bool,
        dur_ms: u64,
        spec: &str,
    ) -> std::io::Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let row = serde_json::json!({
            "ts": ts, "cmd": cmd, "ok": ok, "dur_ms": dur_ms, "spec": spec,
        });
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(format!("{row}\n").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sticky events buffer their latest payload; non-sticky don't.
    /// Late subscribers read the buffer via the sticky map (what
    /// `ensure_subscribed` returns as `replay`).
    #[test]
    fn sticky_payload_buffered_and_overwritten() {
        let hub = EventHub::new();
        hub.publish("backend-ready", json!({ "boot": 1 }));
        hub.publish("message-chunk", json!({ "chunk": "hi" }));
        hub.publish("backend-ready", json!({ "boot": 2 }));

        let sticky = hub.sticky.lock().unwrap();
        assert_eq!(sticky.get("backend-ready"), Some(&json!({ "boot": 2 })));
        assert!(!sticky.contains_key("message-chunk"));
    }

    /// Rows reach a live subscriber in publish order with strictly
    /// increasing seq — the ordering contract real-mode specs rely on
    /// for concat(chunks) == full_text.
    #[tokio::test]
    async fn events_delivered_in_seq_order() {
        let hub = EventHub::new();
        let mut rx = hub.tx.subscribe();
        for i in 0..100u32 {
            hub.publish("message-chunk", json!({ "i": i }));
        }
        let mut last_seq = None;
        for i in 0..100u32 {
            let row = rx.recv().await.expect("row");
            assert_eq!(row.event, "message-chunk");
            assert_eq!(row.payload, json!({ "i": i }));
            if let Some(prev) = last_seq {
                assert!(row.seq > prev, "seq must be strictly increasing");
            }
            last_seq = Some(row.seq);
        }
    }

    /// The recent ring serves rows published with no live consumer —
    /// the race /events/recent exists to close — and honors since_seq.
    #[test]
    fn recent_ring_replays_unconsumed_rows() {
        let hub = EventHub::new();
        for i in 0..5u32 {
            hub.publish("local-corpus://progress/job-1", json!({ "i": i }));
        }
        let all = hub.recent_since(0);
        assert_eq!(all.len(), 5);
        assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
        let tail = hub.recent_since(all[3].seq);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].payload, json!({ "i": 3 }));
    }

    /// Ledger rows are one JSON object per line with the fields the
    /// coverage report joins on.
    #[test]
    fn ledger_rows_are_parseable_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.jsonl");
        ledger::append_row(&path, "send_message_stream", true, 12, "spec-a").unwrap();
        ledger::append_row(&path, "cancel_stream", false, 3, "spec-b").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["cmd"], "send_message_stream");
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[0]["spec"], "spec-a");
        assert_eq!(rows[1]["ok"], false);
        assert_eq!(rows[1]["dur_ms"], 3);
    }
}
