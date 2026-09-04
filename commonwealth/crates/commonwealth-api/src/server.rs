// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use axum::routing::{any, delete, get, post};
use axum::Router;
use tokio::net::TcpListener;
use tracing::info;

use crate::routes_app_internal;
use crate::routes_apps;
use crate::routes_completions;
use crate::routes_edit_predictions;
use crate::routes_inference;
use crate::routes_internal;
use crate::routes_knowledge;
use crate::routes_oicp;
use crate::routes_oicp_ingest;
use crate::routes_ollama;
use crate::routes_rail;
use crate::routes_responses;
use crate::routes_status;
use crate::state::AppState;
// Re-exported, not moved twice: every call site says `server::ClientSurface`
// and the surface is only ever meaningful next to the router it binds.
pub use crate::client_surface::ClientSurface;

/// Explicit request-body ceiling for both API surfaces. Makes the bound
/// intentional and tunable instead of relying on axum's implicit ~2 MB default
/// (which a framework bump could silently change). 8 MB gives headroom for
/// long-context chat bodies and gossip/app-state snapshots on a low-double-digit
/// mesh while still hard-bounding per-request memory. Large model/index
/// distribution streams over GET *responses*, not these request bodies, so it
/// is unaffected by this cap.
///
/// PUBLIC because it is also the ceiling gossip senders push against:
/// the mesh_store snapshot POST is rejected by the receiver's body
/// limit, and the sender's payload gauge has to warn against the SAME
/// number rather than a second copy of it (§10.6 — one decider, one
/// name). `sovereign-mesh::gossip` reads it.
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Slow-loris guard: cap how long a client may take to deliver a request body.
/// Bounds a connection that dribbles bytes to hold resources open. Applies to
/// the REQUEST body only — streaming chat *responses* are unaffected, and an
/// 8 MB body uploads well within this on any real link.
const REQUEST_BODY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Build the client-facing API router (port 9741) for the daemon's own
/// listener, which trusts a loopback caller.
pub fn client_router(state: AppState) -> Router {
    client_router_for(state, ClientSurface::Operator)
}

/// [`client_router`] for a named principal class. See [`ClientSurface`].
pub fn client_router_for(state: AppState, surface: ClientSurface) -> Router {
    let auth_policy = surface.auth_policy();
    // Per-route admission gate applied to peer-reachable inference
    // endpoints. Local requests (no `X-Node-Id`) pass through; peer
    // requests are checked against pause / foreground-yield / ceiling
    // and 503 with structured body + Retry-After when gated. See
    // `crate::admission`.
    let admission = || {
        axum::middleware::from_fn_with_state(state.clone(), crate::admission::peer_admission_layer)
    };

    // Per-principal equal share for CLIENT (non-peer) callers — the other
    // half of the same gate. `peer_admission_layer` rations traffic that
    // names a node; this rations traffic that does not, keyed by
    // `crate::principal`. The two are disjoint by construction (the client
    // layer returns early on `X-Node-Id`), so a request meets exactly one of
    // them and is never double-gated. Applied to the same route set as the
    // peer gate, for the same reason: it is the surface that consumes the
    // decode permit. See `MESH_SCALE_100_USERS_1000_CORPORA.md` §9.3.
    let fair_share = || {
        axum::middleware::from_fn_with_state(state.clone(), crate::admission::client_fairness_layer)
    };

    // Eagerly load the primary chat slot so the first turn after an idle
    // unload doesn't pay the 10-90s lazy-load tax; and the guest-grant
    // lifecycle. Both live on the client port rather than
    // `internal_router` (`:9742`) because every legitimate caller is
    // LOCAL — the desktop's window-focus warm, and the attach-mode
    // provider in `oicp-client`, which derives this URL from its `/v1`
    // endpoint. On `:9742` the warm POST simply 404'd and was swallowed
    // as a best-effort no-op, so Attach mode never warmed its model
    // while looking fully wired.
    //
    // Their cost if a peer could pull them is an 18.5 GB disk load and a
    // forged credential for an outsider, so which LISTENER carries them
    // is the whole guard: `ClientSurface::Operator` and nothing else.
    // Until 2026-08-28 the guard was the `client_auth` layer below, and
    // that was a false premise — a member reaching this router through
    // the iroh acceptor is admitted by the loopback arm before any
    // credential is read.
    let operator_routes: Router<AppState> = if surface.serves_operator_routes() {
        Router::new()
            .route(
                "/internal/inference/warmup",
                post(routes_internal::inference_warmup),
            )
            .route(
                "/internal/guest/grant",
                post(routes_internal::guest_grant_issue),
            )
            .route(
                "/internal/guest/grant/revoke",
                post(routes_internal::guest_grant_revoke),
            )
            .route(
                "/internal/guest/grant/list",
                get(routes_internal::guest_grant_list),
            )
    } else {
        Router::new()
    };

    // The general client surface — inference, knowledge, status, OICP, the
    // Ollama shim, app management. Present on `Operator`, `Peer` and
    // `Guest`; absent entirely on `Rail`.
    //
    // A ring app's reach is the route set mounted on the listener it can
    // reach, not a predicate it has to fail (§7.1). Note the consequence
    // for `AUTH_EXEMPT_PATHS`: `/status` and `/oicp/v1/capabilities` are
    // not mounted here either, so a probe against a rail listener 404s
    // rather than 401s. That is deliberate — a ring app has no business
    // probing federation health, and an exempt path that answered would be
    // the one route it could reach without a credential.
    let general: Router<AppState> = if surface.serves_general_client_routes() {
        Router::new()
            // OpenAI-compatible inference endpoints.
            .route(
                "/v1/chat/completions",
                post(routes_inference::chat_completions)
                    .layer(admission())
                    .layer(fair_share()),
            )
            // OpenAI Responses API — adapter over /v1/chat/completions.
            // Required by `codex` and the OpenAI agents libraries since
            // their dropping `wire_api="chat"` (2026-05). See
            // `routes_responses` module docs for the translation contract.
            .route("/v1/responses", post(routes_responses::responses))
            // FIM inline completion (INLINE_COMPLETION.md). Loopback-tokenless
            // like the rest of :9741 — the extension talks to its own daemon.
            .route("/v1/completions", post(routes_completions::completions))
            // Next-edit prediction, both lanes (NEXT_EDIT.md §3). The rule
            // lane is pure string work, but the model lane consults the
            // resident FIM slot, so this endpoint carries the same
            // admission gate as every other inference route — a peer must
            // not drive local inference through it while the operator has
            // contribution paused. Local requests (no `X-Node-Id`) are
            // always admitted, so the editor path is untouched. The tighter
            // body limit overrides the router-wide 8 MB frontdoor: the
            // handler's documented caps (512 KiB text, 32 units) are a
            // contract check, and the transport should refuse a body that
            // could never satisfy them before serde allocates it.
            .route(
                "/v1/edit_predictions",
                post(routes_edit_predictions::edit_predictions)
                    .layer(axum::extract::DefaultBodyLimit::max(
                        routes_edit_predictions::MAX_BODY_BYTES,
                    ))
                    .layer(admission()),
            )
            // What the developer did with a suggestion. Deliberately NOT
            // behind `admission()`: it is a local editor reporting on a
            // prediction this daemon already served, it costs one appended
            // line, and a refusal here would be an invisible telemetry
            // failure rather than protection (decision note `09599af1`).
            .route(
                "/v1/edit_predictions/outcome",
                post(crate::next_edit_journal::edit_prediction_outcome),
            )
            // Behind `admission()` for the same reason `/v1/edit_predictions`
            // is: it drives local inference on this box. It was the ONE
            // inference route without the gate, and the omission carried no
            // note explaining it — unlike `/v1/edit_predictions/outcome`,
            // whose exemption is argued directly above. It was an oversight,
            // and it was reachable: measured 2026-08-31 against a live daemon,
            // one node id, one moment — `/v1/chat/completions` answered 503
            // and `/v1/embeddings` answered 200 and SERVED. A peer could drive
            // this host's embed slot past the ceiling, through a foreground
            // yield, and while contribution was paused, appearing in no tally
            // (`peer_requests` never moved). Found because a terminal node's
            // embeddings reached their entry node and the entry node's own
            // counters showed nothing.
            //
            // Safe for the caller this most affects: a `terminal` holds no
            // embed model, and its provider is built `waiting_out_sheds()`
            // precisely because "the node on the far end is the only holder
            // there is" (`oicp-client`), so a 503 here is waited out, not
            // fatal.
            .route(
                "/v1/embeddings",
                post(routes_inference::embeddings).layer(admission()),
            )
            .route("/v1/models", get(routes_inference::list_models))
            // Knowledge search endpoint.
            .route(
                "/v1/knowledge/search",
                post(routes_knowledge::knowledge_search),
            )
            // Status endpoint.
            .route("/status", get(routes_status::status))
            // The operator-only surface, mounted on the `Operator` bind and
            // NOWHERE else. Empty on `Peer` and `Guest`, so those listeners
            // 404 these paths rather than gating them — the distinction
            // matters, because the gate they would otherwise carry
            // ("is the caller loopback") is inert on a listener the iroh
            // acceptor feeds. See [`ClientSurface`].
            .merge(operator_routes)
            // OICP capability manifest.
            .route("/oicp/v1/capabilities", get(routes_oicp::capabilities))
            // OICP v0.4 §5 ingest extension: install a corpus by recipe id,
            // poll coarse progress, and dry-run a recipe. Protocol DTOs only
            // (no `corpus_engine` types on the wire); advertised in the
            // manifest's `knowledge.ingest` when a corpus engine is wired.
            // Covered by the outer `client_auth` layer like the rest of :9741.
            .route(
                "/oicp/v1/corpus/install",
                post(routes_oicp_ingest::corpus_install),
            )
            .route(
                "/oicp/v1/corpus/progress",
                get(routes_oicp_ingest::corpus_progress),
            )
            .route(
                "/oicp/v1/recipe/test",
                post(routes_oicp_ingest::recipe_test),
            )
            // Ollama-native /api/* compatibility shim. Pure translation over the
            // OpenAI handlers above — no new inference/routing logic. Chat +
            // generate carry the same peer-admission gate as /v1/chat/completions
            // (a no-op for local Ollama clients, which don't send X-Node-Id).
            // Same unauthenticated posture as the rest of :9741 — see
            // `routes_ollama` module docs for the trust/CORS rationale.
            .route("/api/version", get(routes_ollama::version))
            .route("/api/tags", get(routes_ollama::tags))
            .route("/api/ps", get(routes_ollama::ps))
            .route("/api/show", post(routes_ollama::show))
            .route("/api/chat", post(routes_ollama::chat).layer(admission()))
            .route(
                "/api/generate",
                post(routes_ollama::generate).layer(admission()),
            )
            .route("/api/embed", post(routes_ollama::embed))
            .route("/api/embeddings", post(routes_ollama::embeddings))
            // App management endpoints.
            .route("/v1/apps", get(routes_apps::list_apps))
            .route("/v1/apps/{app_id}/install", post(routes_apps::install_app))
            .route("/v1/apps/{app_id}/status", get(routes_apps::app_status))
            .route("/v1/apps/{app_id}", delete(routes_apps::uninstall_app))
            // Reverse proxy to locally running apps.
            .route("/app/{app_id}/{*path}", any(routes_apps::proxy_app))
    } else {
        Router::new()
    };

    // The ring-app rail. Present on `Rail` (where it is the ONLY thing
    // served) and on `Operator` (a local caller already reaches everything,
    // and `svrn ring` has to be able to read its own ledger). Absent on
    // `Peer` and `Guest`: a ring rail is loopback-only in M0.
    let rail: Router<AppState> = if surface.serves_rail_routes() {
        Router::new()
            .route("/v1/rail/append", post(routes_rail::append))
            .route("/v1/rail/log", get(routes_rail::log))
    } else {
        Router::new()
    };

    general
        .merge(rail)
        // OUTERMOST layer: bearer-token auth for non-loopback callers.
        // Wraps the whole client surface (including the per-route
        // admission gates), so authentication runs BEFORE load-shedding
        // and before any handler work. Loopback callers and the
        // `AUTH_EXEMPT_PATHS` (federation/health) pass through. See
        // `crate::client_auth`.
        .layer(axum::middleware::from_fn_with_state(
            crate::client_auth::ClientAuthState::new(state.clone(), auth_policy),
            crate::client_auth::client_auth_layer,
        ))
        // Outermost frontdoor: bound request-body size + slow-dribble time
        // before any handler or auth work runs.
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            REQUEST_BODY_READ_TIMEOUT,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

/// Build the internal mesh API router (port 9742).
pub fn internal_router(state: AppState) -> Router {
    // Same admission gate as the client router — applied to peer-
    // fan-out routes so a busy operator's machine 503s knowledge
    // searches from peers rather than starving local chat. See
    // `crate::admission`.
    let admission = || {
        axum::middleware::from_fn_with_state(state.clone(), crate::admission::peer_admission_layer)
    };

    Router::new()
        .route("/internal/gossip", post(routes_internal::gossip))
        .route("/internal/join", post(routes_internal::join))
        .route(
            "/internal/scheduling/intent",
            post(routes_internal::scheduling_intent),
        )
        .route(
            "/internal/scheduling/plan",
            post(routes_internal::scheduling_plan),
        )
        .route(
            "/internal/model/transfer",
            post(routes_internal::model_transfer),
        )
        // Peer-to-peer GGUF distribution. See routes_internal::model_files.
        .route(
            commonwealth_core::model::MODELS_LIST_PATH,
            get(routes_internal::list_model_files),
        )
        .route(
            commonwealth_core::model::MODEL_FILE_ROUTE,
            get(routes_internal::serve_model_file),
        )
        // Distributed-inference auto-warm: a host asks this worker to seed its
        // RPC tensor cache with its shard before a distributed load. The worker
        // fetches the GGUF (or its byte ranges) from the model-file route above.
        .route("/internal/rpc-warm", post(routes_internal::rpc_warm))
        .route(
            "/internal/index/transfer",
            post(routes_internal::index_transfer),
        )
        .route("/internal/index/serve", get(routes_internal::index_serve))
        .route(
            "/internal/knowledge/search",
            post(routes_internal::knowledge_search).layer(admission()),
        )
        .route("/internal/atlas/status", get(routes_internal::atlas_status))
        .route(
            "/internal/latency/probe",
            get(routes_internal::latency_probe),
        )
        .route(
            "/internal/corpus/collaborate",
            post(routes_internal::corpus_collaborate),
        )
        // Which mesh peers can help with a peer-assisted ingest (drives the
        // desktop peer picker: eligible peers + reasons for ineligible ones).
        .route(
            "/internal/corpus/collaborate/eligible_peers",
            post(routes_internal::corpus_eligible_peers),
        )
        // Ephemeral ingest-grant lifecycle: authorize a one-off, revocable
        // peer-assisted ingest of an otherwise local-only corpus. The
        // `collaborate` gate above consults the grant issued here.
        .route(
            "/internal/corpus/grant",
            post(routes_internal::corpus_grant_issue),
        )
        .route(
            "/internal/corpus/grant/revoke",
            post(routes_internal::corpus_grant_revoke),
        )
        // Ephemeral teardown: a peer wipes its own working partition dir when
        // the coordinator has pulled its shard (no peer retention).
        .route(
            "/internal/corpus/partition_evict",
            post(routes_internal::corpus_partition_evict),
        )
        // Glassbox progress for a (possibly peer-assisted) collaborative
        // ingest — per-peer unit tallies + the grant's remaining window.
        .route(
            "/internal/corpus/collaborate/status",
            post(routes_internal::corpus_collaborate_status),
        )
        .route(
            "/internal/corpus/ingest_partition",
            post(routes_internal::corpus_ingest_partition),
        )
        // Pull-based work queue (new path; coexists with ingest_partition
        // while `SOVEREIGN_USE_WORK_QUEUE` gates the coordinator).
        .route(
            "/internal/corpus/next_unit",
            post(routes_internal::corpus_next_unit),
        )
        .route(
            "/internal/corpus/heartbeat",
            post(routes_internal::corpus_heartbeat),
        )
        .route(
            "/internal/corpus/complete_unit",
            post(routes_internal::corpus_complete_unit),
        )
        .route(
            "/internal/corpus/cancel",
            post(routes_internal::corpus_cancel),
        )
        .route(
            "/internal/corpus/pause",
            post(routes_internal::corpus_pause),
        )
        .route(
            // Mesh-aware pause of `sovereign pipeline run` drivers.
            // Local CLI sets `fanout: true`; the receiving daemon
            // forwards to each online peer with `fanout: false` so
            // peers run only their own /proc walk and the message
            // can't loop. See routes_internal/pipeline_pause.rs.
            "/internal/pipeline/pause",
            post(routes_internal::pipeline_pause),
        )
        .route(
            "/internal/corpus/install",
            post(routes_internal::corpus_install),
        )
        .route(
            "/internal/corpus/expand",
            post(routes_internal::corpus_expand),
        )
        .route(
            "/internal/corpus/progress",
            get(routes_internal::corpus_progress),
        )
        .route(
            "/internal/corpus/status",
            get(routes_internal::corpus_status),
        )
        // Watcher liveness for the `wikipedia-newsworthy` freshness
        // daemon. Read-only; surfaces the most recent tick + the
        // current leader so the desktop chip can answer "is this
        // working?" without operators tailing daemon logs.
        // Generic per-corpus enrichment progress. Reads
        // `_enrichment_state.json` written by any pipeline that
        // adopts EnrichmentProgressSink — folder tiered, structural
        // atlas postinstall, conversation RAPTOR, future pipelines.
        .route(
            "/internal/enrichment/status",
            get(routes_internal::enrichment_status),
        )
        .route(
            "/internal/newsworthy/status",
            get(routes_internal::newsworthy_status),
        )
        // Operator-triggered tick — fires one watcher pass immediately
        // so users don't have to wait up to 24h to see a snapshot
        // refresh after install/leader-election state changes.
        .route(
            "/internal/newsworthy/tick",
            axum::routing::post(routes_internal::newsworthy_tick),
        )
        // Phase 6 canonical-sync: peers fetch this node's canonical
        // index for `<corpus_id>` as a streaming tar.zst. Loopback-
        // gated like the other internal routes; the auth path is the
        // same one peers already use for `/internal/knowledge/search`
        // and friends.
        .route(
            "/internal/corpus/canonical/{corpus_id}",
            get(routes_internal::corpus_canonical_stream),
        )
        .route(
            "/internal/node/activity",
            post(routes_internal::node_activity),
        )
        // App gossip endpoints.
        .route(
            "/internal/app/state",
            post(routes_app_internal::recv_app_state),
        )
        // Ring-ledger anti-entropy. Its OWN route on its own cadence rather
        // than a namespace riding `/internal/app/state`: that push ships a
        // full snapshot to every online peer every 10s, which for a ledger
        // that only grows is a bandwidth bill that never stops climbing.
        .route("/internal/ring/sync", post(routes_internal::ring_sync))
        .route(
            "/internal/app/registry",
            post(routes_app_internal::recv_app_registry),
        )
        // Runtime slot management — load/unload extras chat slots
        // without daemon restart. Complements the static
        // `[models.extra]` config table (loaded at startup) by
        // letting operators swap models mid-session.
        .route("/internal/models/load", post(routes_internal::models_load))
        .route(
            "/internal/models/unload",
            post(routes_internal::models_unload),
        )
        .route(
            "/internal/models/inventory",
            get(routes_internal::models_inventory),
        )
        // NOTE: `/internal/inference/warmup` is NOT here — it is
        // mounted on `client_router` (`:9741`), and only on its
        // `ClientSurface::Operator` bind. See the rationale at that
        // route; in short, its callers are local and its cost is an
        // 18.5 GB disk load, so it belongs on the one listener no mesh
        // principal is ever forwarded to.
        //
        // Contribution controls (W2). Read by the Settings panel and
        // the tray status chip; mutated by the pause/ceiling controls.
        // Loopback-only — the same guard that protects /internal/*.
        .route(
            "/internal/contribution/status",
            get(routes_internal::contribution_status),
        )
        .route(
            "/internal/contribution/ceiling",
            post(routes_internal::contribution_ceiling_set),
        )
        .route(
            "/internal/contribution/pause",
            post(routes_internal::contribution_pause),
        )
        .route(
            "/internal/contribution/resume",
            post(routes_internal::contribution_resume),
        )
        .route(
            "/internal/contribution/recent",
            get(routes_internal::contribution_recent),
        )
        // Dimensional per-node ledger view (Mesh Health Members panel).
        // Read-only aggregation over the default 30-day window. Attach-
        // mode desktops hit this so they can render Members without an
        // in-process AppState.
        .route(
            "/internal/contribution/view",
            get(routes_internal::contribution_view),
        )
        // Local Activity ledger — the glassbox "what is my daemon
        // doing?" surface. Local-only namespace; loopback-only like
        // the rest of /internal/*. `summary` is the totals card;
        // `recent` is the unified feed. Read by Settings → Activity &
        // Sharing.
        .route(
            "/internal/activity/summary",
            get(routes_internal::activity_summary),
        )
        .route(
            "/internal/activity/recent",
            get(routes_internal::activity_recent),
        )
        // Foreground-yield introspection — read-only snapshot of the
        // atomics that decide whether ingest workers are pausing for
        // chat. No POST: the window is configured at startup via
        // `daemon.yield_to_foreground_secs`, not pushed at runtime.
        .route(
            "/internal/daemon/foreground_state",
            get(routes_internal::foreground_state),
        )
        // Mesh-quiesce control. GET reports current state; POST flips
        // it at runtime so an operator can stop participating in
        // shared ingests on this node without a daemon restart. Used
        // by the desktop's "Stop participating" affordance and the
        // peer-pause workflow when foreground inference is being
        // crushed by background ingest activity from another node.
        .route(
            "/internal/mesh/quiesce",
            get(routes_internal::mesh_quiesce_get).post(routes_internal::mesh_quiesce_set),
        )
        // Per-batch ingest throttle. GET reports the current factor;
        // POST sets it. `1.0` = full speed (default), `0.5` =
        // duty-cycle 50% (sleep after each embed batch equal to its
        // wall time). Use the pause route to fully stop a corpus —
        // `0.0` is rejected here.
        .route(
            "/internal/ingest/budget",
            get(routes_internal::ingest_budget_get).post(routes_internal::ingest_budget_set),
        )
        // Storage budget. GET reports the current ceiling, observed
        // usage, raw free disk, and a recommended baseline; POST
        // accepts `{ "budget_bytes": <≥1 GiB | null> }`. The
        // enforcement point is the gossip-tick capabilities builder
        // (`sovereign-mesh::capabilities::build_local_capabilities`)
        // which clamps the published `free_storage_gb` to budget
        // remaining — every existing scheduler picks up the cap
        // automatically.
        .route(
            "/internal/storage/budget",
            get(routes_internal::storage_budget_get).post(routes_internal::storage_budget_set),
        )
        // Frontdoor bound on the perimeter-trusted internal port too — its
        // routes (gossip, app-state, knowledge) carry no auth gate, so this is
        // their resource ceiling.
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            REQUEST_BODY_READ_TIMEOUT,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

/// Start both API servers. Returns when both are shut down.
pub async fn serve(
    state: AppState,
    client_addr: SocketAddr,
    internal_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // CRITICAL: the client router's `client_auth` layer extracts
    // `ConnectInfo<SocketAddr>` to decide loopback-vs-remote (and fails
    // closed if it's absent). Bare `axum::serve` does NOT attach
    // ConnectInfo, so the client listener MUST use
    // `into_make_service_with_connect_info` or every request — even
    // loopback — 500s. (The sovereign-mesh daemon already does this on
    // its own listener; this is the standalone-daemon / test-harness
    // path. Same requirement the loopback guard documents.)
    let client_app =
        client_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let internal_app = internal_router(state);

    let client_listener = TcpListener::bind(client_addr).await?;
    let internal_listener = TcpListener::bind(internal_addr).await?;

    info!(
        client = %client_addr,
        internal = %internal_addr,
        "API servers starting"
    );

    tokio::select! {
        result = axum::serve(client_listener, client_app) => {
            result?;
        }
        result = axum::serve(internal_listener, internal_app) => {
            result?;
        }
    }

    Ok(())
}

/// Test-only: `client_router` plus a `MockConnectInfo` layer supplying
/// a loopback peer address, so `tower::oneshot` requests carry the
/// `ConnectInfo<SocketAddr>` the `client_auth` layer requires (which a
/// bare `oneshot` does not attach). Loopback ⇒ the auth layer admits
/// the request, leaving the test to exercise the handler. Shared with
/// `routes_ollama` / `routes_oicp` test modules via `crate::server::`.
#[cfg(test)]
pub(crate) fn mock_router(state: AppState) -> Router {
    // NB: NOT `axum::extract::connect_info::MockConnectInfo` — that
    // inserts a `MockConnectInfo<T>` extension that only axum's
    // `ConnectInfo` *extractor* falls back to. `client_auth` reads the
    // real `ConnectInfo<SocketAddr>` extension directly (for the
    // fail-closed path), so we insert the real thing via an outer
    // layer — runs before `client_auth`, mirroring how the production
    // `into_make_service_with_connect_info` populates it.
    use axum::extract::{ConnectInfo, Request};
    use axum::middleware::{from_fn, Next};
    async fn inject(mut req: Request, next: Next) -> axum::response::Response {
        req.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        next.run(req).await
    }
    client_router(state).layer(from_fn(inject))
}

/// [`mock_router`] for a named surface. Same loopback `ConnectInfo`
/// injection; the surface decides both the auth posture and the route set,
/// so a test can ask "is this route MOUNTED here" separately from "would
/// auth admit me" — 404 and 401 are different answers and a test that
/// cannot tell them apart proves nothing.
#[cfg(test)]
pub(crate) fn mock_router_for(state: AppState, surface: ClientSurface) -> Router {
    use axum::extract::{ConnectInfo, Request};
    use axum::middleware::{from_fn, Next};
    async fn inject(mut req: Request, next: Next) -> axum::response::Response {
        req.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        next.run(req).await
    }
    client_router_for(state, surface).layer(from_fn(inject))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_app_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// THE GAP. `/v1/embeddings` was the one inference route with no
    /// admission layer, so a peer could drive this host's embed slot while
    /// contribution was paused — past the ceiling, past the foreground yield,
    /// and tallied nowhere.
    ///
    /// Measured against a live daemon 2026-08-31 before the fix: with one node
    /// id at one moment, `/v1/chat/completions` answered **503** and
    /// `/v1/embeddings` answered **200 and served**. Surfaced by a two-machine
    /// terminal run whose embeddings reached their entry node while the entry
    /// node's `peer_requests` stayed empty.
    ///
    /// Asserted as a PAIR: chat is the control. A test that only checked
    /// embeddings would pass just as well if the whole gate stopped working.
    #[tokio::test]
    async fn a_paused_host_refuses_peer_embeddings_exactly_as_it_refuses_peer_chat() {
        let state = test_app_state();
        // Paused far enough ahead that the window cannot lapse mid-test.
        state.set_contribution_paused_until(sovereign_core::time::unix_now() + 3600);
        let peer = commonwealth_core::ids::NodeId::from_u128(0xBEEF).to_hex();

        for path in ["/v1/chat/completions", "/v1/embeddings"] {
            let resp = mock_router(state.clone())
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/json")
                        .header("x-node-id", &peer)
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .expect("the gate must answer, not hang");
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}: a paused host must refuse a PEER request before the handler runs"
            );
        }
    }

    /// The other half, so the fix cannot be "503 everything": a LOCAL caller
    /// carries no `X-Node-Id` and is never a peer, so a pause must not touch
    /// the operator's own embeddings. Asserted as "not 503" rather than a
    /// specific code — the handler's own outcome on a stub state is not this
    /// test's business.
    #[tokio::test]
    async fn a_paused_host_still_serves_its_own_embeddings() {
        let state = test_app_state();
        state.set_contribution_paused_until(sovereign_core::time::unix_now() + 3600);
        let resp = mock_router(state)
            .oneshot(
                Request::post("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("a local request must reach the handler");
        assert_ne!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a pause rations PEERS; the operator's own machine is never gated"
        );
    }

    #[tokio::test]
    async fn status_endpoint() {
        let app = mock_router(test_app_state());

        let response = app
            .oneshot(Request::get("/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("node_id").is_some());
        assert!(json.get("mesh").is_some());
    }

    #[tokio::test]
    async fn models_endpoint_empty() {
        let app = mock_router(test_app_state());

        let response = app
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn oicp_capabilities_endpoint() {
        let app = mock_router(test_app_state());

        let response = app
            .oneshot(
                Request::get("/oicp/v1/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["oicp_version"],
            commonwealth_inference::oicp::OICP_VERSION
        );
        assert_eq!(json["provider"]["type"], "mesh");
    }

    #[tokio::test]
    async fn chat_completions_no_model_loaded() {
        let app = mock_router(test_app_state());

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should fail because no models are loaded.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn responses_endpoint_rejects_previous_response_id() {
        // The /v1/responses adapter doesn't implement server-side
        // conversation state. A request that carries
        // `previous_response_id` must 400 so codex falls back to
        // resending full history.
        let app = mock_router(test_app_state());
        let body = serde_json::json!({
            "model": "x",
            "input": "hi",
            "previous_response_id": "resp_old"
        });
        let response = app
            .oneshot(
                Request::post("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("previous_response_id"));
    }

    #[tokio::test]
    async fn responses_endpoint_no_model_loaded_returns_503() {
        // With no local_inference and no loaded models, the inner
        // chat_completions handler returns 503. The adapter forwards
        // it as-is.
        let app = mock_router(test_app_state());
        let body = serde_json::json!({
            "model": "x",
            "input": "hello"
        });
        let response = app
            .oneshot(
                Request::post("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn responses_endpoint_accepts_codex_shape_request() {
        // Pin the wire shape codex actually sends so we know the
        // adapter parses the canonical request format. We don't drive
        // it through to a successful inference here — there's no model
        // loaded — but the request must at least deserialise and
        // reach the inner handler (i.e. 503, not 400).
        let app = mock_router(test_app_state());
        let body = serde_json::json!({
            "model": "primary",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                }
            ],
            "instructions": "you are terse",
            "tools": [{
                "type": "function",
                "name": "shell",
                "description": "run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {"cmd": {"type": "string"}},
                    "required": ["cmd"]
                }
            }],
            "tool_choice": "auto",
            "stream": false,
            "max_output_tokens": 1024,
            "store": false,
            "parallel_tool_calls": true,
            "reasoning": {"effort": "medium"}
        });
        let response = app
            .oneshot(
                Request::post("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Reached the inner handler => translation succeeded.
        // The inner handler 503s with no model loaded.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn chat_completions_rejects_local_only() {
        let app = mock_router(test_app_state());

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "oicp": {
                "oicp_version": "0.1.0",
                "privacy": { "sharding": "local_only" }
            }
        });

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("local_only"));
    }

    #[tokio::test]
    async fn internal_gossip_endpoint_rejects_wrong_mesh() {
        // After the gossip handler was wired for real (replacing the
        // accept-any-JSON stub), the minimal shape it accepts is a
        // full `MeshWire` payload. A test AppState has mesh_id=1 and
        // an all-zero invite_key_hash; posting a body with a different
        // mesh_id proves the auth guard fires. The full "merges
        // incoming delta" happy path is covered by the dedicated
        // tests/gossip_route.rs integration file.
        let app = internal_router(test_app_state());

        // MeshId serializes as a 16-byte array; hash as a 32-byte
        // array. Both built as vecs so `serde_json::json!` is happy.
        let mesh_id_bytes = vec![0u8; 16];
        let hash_bytes = vec![0u8; 32];
        // Flip one byte in the id to differ from test_app_state()'s
        // default, so the handler's mesh-id check fires.
        let mut foreign_id = mesh_id_bytes.clone();
        foreign_id[0] = 42;
        let body = serde_json::json!({
            "mesh": {
                "id": foreign_id,
                "name": "Other",
                "join_key_hash": hash_bytes,
                "members": [],
                "peers": []
            }
        });
        let response = app
            .oneshot(
                Request::post("/internal/gossip")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn internal_latency_probe_endpoint() {
        let app = internal_router(test_app_state());

        let response = app
            .oneshot(
                Request::get("/internal/latency/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The warm route must be reachable on the CLIENT port and absent
    /// from the peer-reachable one. It sat only on `internal_router`
    /// (`:9742`) until 2026-07-27: the desktop and `oicp-client` derive
    /// the URL from their `/v1` endpoint — `:9741` — so every warm-up
    /// POST 404'd and was swallowed as a best-effort no-op, which is
    /// why Attach mode silently never warmed its model while looking
    /// fully wired. Both directions are pinned, because moving it back
    /// would re-disable warm-up in the shipped app without failing
    /// anything else.
    #[tokio::test]
    async fn warmup_route_is_on_the_client_port_not_the_peer_port() {
        let response = mock_router(test_app_state())
            .oneshot(
                Request::post("/internal/inference/warmup")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "warmup must be routable on the client port the desktop actually calls"
        );

        let response = internal_router(test_app_state())
            .oneshot(
                Request::post("/internal/inference/warmup")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "an 18.5 GB disk load must not be a lever any mesh peer can pull"
        );
    }

    /// The other half of that sentence, and the one that was false until
    /// 2026-08-28. Keeping warmup off `:9742` was never enough: a MEMBER
    /// dialling `CLIENT_ALPN` is forwarded to a bind of THIS router, and
    /// arrives wearing the acceptor's loopback address, so `client_auth`
    /// admits it before reading anything. The peer bind serves a router
    /// where the route does not exist.
    ///
    /// Each surface is driven with a credential it ACCEPTS, so the only
    /// thing left to observe is whether the route is mounted. Asserting
    /// 404 through a refusal would prove nothing — a 401 also is not 200.
    #[tokio::test]
    async fn the_peer_and_guest_surfaces_do_not_serve_the_operator_only_routes() {
        const OPERATOR_ONLY: &[&str] = &[
            "/internal/inference/warmup",
            "/internal/guest/grant",
            "/internal/guest/grant/revoke",
        ];
        const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

        // `Peer` trusts a loopback caller — a member's key was already proved
        // at the QUIC handshake — so the injected ConnectInfo admits us.
        for path in OPERATOR_ONLY {
            let response = mock_router_for(test_app_state(), ClientSurface::Peer)
                .oneshot(
                    Request::post(*path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "peer surface must not serve {path}"
            );
        }

        // `Guest` and `Rail` do not trust loopback, so they need the daemon
        // token to get past auth. Once past it, the same routes are simply
        // absent.
        for surface in [ClientSurface::Guest, ClientSurface::Rail] {
            for path in OPERATOR_ONLY {
                let state = test_app_state();
                state.install_client_token(Some(TOKEN.into()));
                let response = mock_router_for(state, surface)
                    .oneshot(
                        Request::post(*path)
                            .header("content-type", "application/json")
                            .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                            .body(Body::from("{}"))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{surface:?} surface must not serve {path}"
                );
            }
        }

        // And the control: the SAME request on the operator surface is served,
        // so the 404s above are the route set changing and not a broken probe.
        let response = mock_router_for(test_app_state(), ClientSurface::Operator)
            .oneshot(
                Request::get("/internal/guest/grant/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the operator surface must still serve what the others refuse"
        );
    }

    /// The `Rail` surface serves the ring-app rail and NOTHING else.
    ///
    /// A deployed ring app is a guest that happens to run on this machine.
    /// It must not be able to drive inference, search the operator's
    /// corpora, or manage apps — and the guarantee is that those routes are
    /// absent from the listener it can reach, not that a predicate refuses
    /// them (§7.1). A 404 here is the route set, not a credential.
    ///
    /// Driven with a credential the surface ACCEPTS, so the only variable
    /// left is whether the route is mounted; the control at the end proves
    /// the probe itself is not simply broken (§18.1).
    #[tokio::test]
    async fn the_rail_surface_does_not_serve_the_general_client_routes() {
        const GENERAL: &[(&str, &str)] = &[
            ("POST", "/v1/chat/completions"),
            ("POST", "/v1/knowledge/search"),
            ("GET", "/v1/models"),
            ("GET", "/v1/apps"),
            ("POST", "/api/chat"),
        ];
        const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

        let probe = |surface: ClientSurface, method: &str, path: &str| {
            let state = test_app_state();
            state.install_client_token(Some(TOKEN.into()));
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::from("{}"))
                .unwrap();
            async move { mock_router_for(state, surface).oneshot(req).await.unwrap() }
        };

        for (method, path) in GENERAL {
            let response = probe(ClientSurface::Rail, method, path).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "rail surface must not serve {method} {path}"
            );
        }

        // Control: the same requests on the operator surface are routed —
        // so the 404s above are the route set changing, not a probe that
        // 404s everything. Any status but NOT_FOUND proves the route exists;
        // these handlers legitimately 4xx/5xx on an empty body.
        for (method, path) in GENERAL {
            let response = probe(ClientSurface::Operator, method, path).await;
            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "operator surface must still serve {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn models_endpoint_with_registered_model() {
        let state = test_app_state();

        // Register a model.
        use commonwealth_inference::model::{ModelArchitecture, ModelInfo};
        use commonwealth_inference::oicp::{Capability, CapabilityProfile};
        use std::collections::HashMap;

        let mut caps = CapabilityProfile::default();
        caps.insert(Capability::Code, 4);

        let model = ModelInfo {
            id: commonwealth_core::ModelId::from_u128(1),
            name: "test-coder".into(),
            repo: "test/model".into(),
            file: "model.gguf".into(),
            size_bytes: 17_000_000_000,
            total_layers: 64,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: caps,
            quantization: "Q4_K_M".into(),
            // Fields added after the adaptive-mesh-scheduler change —
            // all have `#[serde(default)]` on the struct, so
            // defaults here are fine. Keeping them explicit documents
            // the shape the test expects.
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        };
        state.register_model(model);

        let app = mock_router(state);
        let response = app
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"].as_array().unwrap().len(), 1);
        assert_eq!(json["data"][0]["id"], "test-coder");
    }
}
