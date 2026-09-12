// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshApp bridge — the permission-gated Tauri commands a sandboxed
//! mesh-app webview reaches through `window.meshApp.*`.
//!
//! Every command's FIRST act is [`crate::meshapp::authorize`] against the
//! CALLING webview's label (Tauri injects the `WebviewWindow`; the label
//! is host-assigned at window creation and unspoofable from inside the
//! sandbox). Only after the grant check does a command touch host state.
//!
//! The explorer graph ops are THIN wrappers: the projection logic lives in
//! the `sovereign-meshapp` library so the desktop host and the
//! `sovereign meshapp dev` CLI server share one source of truth. Each command
//! here adds only the permission gate. The numeric LVT ops (`read_corpus`,
//! `parcel_analytics`) are no exception since 2026-09-11: their folds are
//! `sovereign_meshapp::parcels`, so the SF-LVT "no confabulated numbers"
//! guarantee is the daemon's and this surface only relays it.
//!
//! # Where the atoms come from (thin-desktop order, 2026-09-11)
//!
//! The three parcel readers — `meshapp_read_corpus`, `meshapp_search_parcels`,
//! `meshapp_parcel_analytics` — used to pull EVERY atom of the corpus over
//! `GET /internal/corpus/{corpus}/atoms` and fold them here (sv-surface D3
//! moved the read, not the fold). The folds are the daemon's now,
//! `sovereign_meshapp::parcels` behind `/internal/meshapp/{corpus}/parcels…`;
//! each command is the authorization gate (which cannot cross a socket —
//! the webview label is host-assigned to THIS process's window) plus one
//! `TurnClient::meshapp_*` call, like the thirteen below.
//!
//! The other thirteen graph ops read the same way as of sv-surface D3's
//! delete half: each is one `TurnClient::meshapp_*` call against
//! `GET /internal/meshapp/{corpus}/...`, which runs the very
//! `sovereign_meshapp::*` projection this file used to call in-process —
//! over the DAEMON's index dir, so an attached boot answers at all. The
//! DTOs are unchanged and named from `sovereign_contracts::daemon_wire`
//! (their home since 2026-09-11; `sovereign-meshapp` re-exports them), so
//! the frontend sees the same bytes without this crate linking the
//! projection. The Wrapped deck is the one exception: a persisted schema
//! defined beside its folds and verifier, which nothing here reads a
//! field of, so `meshapp_wrapped_artifact` passes it through as
//! `serde_json::Value`. `resolve_index_path` is gone with them, and so
//! are the page defaults and clamps each command re-applied: those live
//! in the route, once (ARCH §10.6).

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::meshapp::{app_id_from_label, authorize, resolve_grant, MeshAppPermissions, Permission};
use crate::state::AppState;

use sovereign_contracts::daemon_wire::{
    ChunkDto, ClaimDto, CorpusStatsDto, DocumentFeedDto, FindingDto, GraphNodeDto, NodeDetailDto,
    ParcelAnalyticsDto, ParcelDto, QuestionDto, ReconciliationMergeDto, SubgraphDto, TimelineDto,
};

/// The daemon this process talks to — in-process on a Local boot, over
/// the socket on an attached one. ONE construction for the whole file, so
/// the base url is resolved in one place (ARCH §10.6).
fn wire(state: &State<'_, Arc<AppState>>) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

/// `window.meshApp.capabilities()` — ungated. Returns the permission
/// subset the calling app was granted (all-false when not installed), so
/// the UI can hide affordances it isn't allowed to use.
#[tauri::command]
pub async fn meshapp_capabilities(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<MeshAppPermissions, String> {
    let app_id = app_id_from_label(webview.label())
        .ok_or_else(|| "caller is not a mesh-app window".to_string())?;
    let installs = state.config.read().await.meshapp_installs.clone();
    Ok(resolve_grant(&installs, &app_id)
        .map(|i| i.granted)
        .unwrap_or_default())
}

/// `window.meshApp.readCorpus(corpusId, ids)` — gated on `mesh_store_read`.
/// Returns the requested parcel atoms with provenance. Each id matches by
/// EITHER the atom id (content-hash) OR the parcel number (canonical
/// name) — so a UI that knows only a human parcel number (e.g. a blklot)
/// can look it up without deriving the host-side hash. The fold is the
/// daemon's (`sovereign_meshapp::parcels::parcels_by_id`, over
/// `GET /internal/meshapp/{corpus}/parcels`).
#[tauri::command]
pub async fn meshapp_read_corpus(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_ids: Vec<String>,
) -> Result<Vec<ParcelDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_parcels::<Vec<ParcelDto>>(&corpus_id, &atom_ids)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.searchParcels(corpusId, query, limit?)` — gated on
/// `mesh_store_read`. Substring/number search over parcel atoms so a UI
/// (a homeowner) can find their parcel by street name or number without
/// knowing the atom-id. Matches the parcel number (exact, case-folded) OR
/// `property_location` (substring, case-folded); the cap (≤100) is the
/// route's clamp now, not a `.min()` here.
#[tauri::command]
pub async fn meshapp_search_parcels(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ParcelDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_search_parcels::<Vec<ParcelDto>>(&corpus_id, &query, limit)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.parcelAnalytics(corpusId, businessTaxTarget?)` — gated
/// on `mesh_store_read` (it reads corpus atoms). Deterministic: the daemon
/// folds the parcel atoms into the revenue-neutral land-levy aggregate via
/// corpus-engine's pure lib (`sovereign_meshapp::parcels::parcel_analytics`).
/// No inference; the macro model's headline figures are computed there,
/// never originated by a model, and the SF defaults (business-tax target,
/// property-tax rate) are the fold's, so the chat tool and this surface
/// cannot disagree.
#[tauri::command]
pub async fn meshapp_parcel_analytics(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    business_tax_target: Option<f64>,
) -> Result<ParcelAnalyticsDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_parcel_analytics::<ParcelAnalyticsDto>(&corpus_id, business_tax_target)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

// ─── Explorer graph ops — thin wrappers over the meshapp routes ──────
// The projection logic (atlas/investigation dispatch, degree ranking, edge
// resolution, the subgraph/timeline/stats/reconciliation reads) lives in the
// `sovereign-meshapp` lib, which the DAEMON now calls: each command below is
// the `mesh_store_read` gate plus one `TurnClient::meshapp_*` call. The DTOs
// are the contract layer's (`daemon_wire::meshapp`) so the wire contract is
// identical, and the defaults/clamps each command used to apply belong to
// the route.

/// `window.meshApp.graph(corpusId, nodeType?, limit?)` — gated on
/// `mesh_store_read`. Degree-ranked entities, highest-degree first.
#[tauri::command]
pub async fn meshapp_graph(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    node_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<GraphNodeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_graph::<Vec<GraphNodeDto>>(&corpus_id, node_type.as_deref(), limit)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.node(corpusId, id)` — gated on `mesh_store_read`. One
/// entity's full detail + every incident edge, each quoting its evidence.
#[tauri::command]
pub async fn meshapp_node(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    id: String,
) -> Result<NodeDetailDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_node::<NodeDetailDto>(&corpus_id, &id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.findings(corpusId, pattern?)` — gated on `mesh_store_read`.
#[tauri::command]
pub async fn meshapp_findings(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    pattern: Option<String>,
) -> Result<Vec<FindingDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_findings::<Vec<FindingDto>>(&corpus_id, pattern.as_deref())
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.searchEntities(corpusId, query, nodeType?, limit?)` —
/// gated on `mesh_store_read`. Case-folded substring over name/aliases/attrs.
/// A blank query answers `[]` at the HOST without loading the graph, so a
/// cleared search box costs one round-trip and no disk — the short-circuit
/// this command used to make locally, moved to where the decision is made.
#[tauri::command]
pub async fn meshapp_search_entities(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    node_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<GraphNodeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_search_entities::<Vec<GraphNodeDto>>(
            &corpus_id,
            &query,
            node_type.as_deref(),
            limit,
        )
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.claims(corpusId, limit?)` — gated on `mesh_store_read`.
/// Claim atoms (the corpus's arguments) with attribution + cited evidence.
/// The entity-graph ops don't surface claims; this does.
#[tauri::command]
pub async fn meshapp_claims(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit: Option<usize>,
) -> Result<Vec<ClaimDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_claims::<Vec<ClaimDto>>(&corpus_id, limit)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.questions(corpusId, limit?)` — gated on `mesh_store_read`.
/// Question atoms (open inquiries the corpus raises).
#[tauri::command]
pub async fn meshapp_questions(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit: Option<usize>,
) -> Result<Vec<QuestionDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_questions::<Vec<QuestionDto>>(&corpus_id, limit)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.reconciliation(corpusId)` — gated on `mesh_store_read`.
/// The atlas cross-origin identity merges, richest first.
#[tauri::command]
pub async fn meshapp_reconciliation(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<ReconciliationMergeDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_reconciliation::<Vec<ReconciliationMergeDto>>(&corpus_id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.subgraph(corpusId, nodeType?, limit?)` — gated on
/// `mesh_store_read`. Top-degree nodes + induced edges, for a node-link map.
#[tauri::command]
pub async fn meshapp_subgraph(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    node_type: Option<String>,
    limit: Option<usize>,
) -> Result<SubgraphDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_subgraph::<SubgraphDto>(&corpus_id, node_type.as_deref(), limit)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.corpusStats(corpusId)` — gated on `mesh_store_read`.
/// Headline scale/provenance counts for a banner.
#[tauri::command]
pub async fn meshapp_corpus_stats(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<CorpusStatsDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_corpus_stats::<CorpusStatsDto>(&corpus_id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.timeline(corpusId)` — gated on `mesh_store_read`.
/// Documents bucketed by month, parsed from each email chunk's `Date:` header.
#[tauri::command]
pub async fn meshapp_timeline(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<TimelineDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_timeline::<TimelineDto>(&corpus_id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.readChunk(corpusId, chunkId)` — gated on `mesh_store_read`.
/// One chunk's full text by its numeric id (the id an edge carries).
#[tauri::command]
pub async fn meshapp_read_chunk(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: String,
) -> Result<ChunkDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    let id: u64 = chunk_id
        .trim()
        .parse()
        .map_err(|_| format!("chunk id `{chunk_id}` is not a numeric id"))?;
    wire(&state)
        .meshapp_read_chunk::<ChunkDto>(&corpus_id, id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.documentFeed(corpusId, limitDocs)` — gated on
/// `mesh_store_read`. The latest N source documents with their chunks
/// and metadata-derived `outbound_links`, newest `source_doc_id` first —
/// the read primitive for feed-shaped apps (the "Today" current-events
/// app renders portal days; an inbox app would render threads).
#[tauri::command]
pub async fn meshapp_document_feed(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit_docs: Option<u32>,
) -> Result<DocumentFeedDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_document_feed::<DocumentFeedDto>(&corpus_id, limit_docs.map(|n| n as usize))
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.wrappedArtifact(corpusId)` — gated on `mesh_store_read`.
/// The precomputed Wrapped story-card artifact: the host serves the cached
/// `wrapped/all-time.json` under the corpus index dir when fresh and
/// rebuilds on demand otherwise (a pure Rust fold — no inference; every
/// quote audited verbatim before serving), so this call can be slow on a
/// cold corpus. The GLiNER entity cards read the daemon's OWN state db —
/// this command used to name `svrnmesh_root()/sovereign.db` itself, which
/// on an attached boot is not necessarily the file the daemon writes; when
/// it is absent those cards are simply absent from the deck.
#[tauri::command]
pub async fn meshapp_wrapped_artifact(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<serde_json::Value, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    wire(&state)
        .meshapp_wrapped_artifact::<serde_json::Value>(&corpus_id)
        .await
        .map_err(|e| format!("`{corpus_id}`: {e}"))
}

/// `window.meshApp.openOuterWork(corpusId)` — gated on `mesh_store_read`
/// (only an installed, granted app may ask; no data crosses — it's a
/// navigation request). The Wrapped Door card's funnel: focus the MAIN
/// window and ask it (via the `meshapp-open-outer-work` event) to open
/// Outer Work on a fresh conversation whose retrieval is scoped to this
/// corpus — "ask your past self anything", literally.
#[tauri::command]
pub async fn meshapp_open_outer_work(
    app: AppHandle,
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let _ = main.set_focus();
    app.emit_to(
        "main",
        "meshapp-open-outer-work",
        serde_json::json!({ "corpus_id": corpus_id }),
    )
    .map_err(|e| format!("emit meshapp-open-outer-work: {e}"))
}

// ─── Host-side install management ────────────────────────────────────
// These are called from the MAIN (host) window's UI, not the sandbox
// bridge. They mutate the grant store, so each guards against being
// called FROM a mesh-app window — otherwise (since Tauri v2 lets any
// webview invoke any app command) a hostile bundle could grant itself
// permissions. The check: the caller's label must NOT be a meshapp-*
// window. (Trusted-first-party model; this is belt-and-suspenders.)

/// `meshapp_list_installs()` — installed mesh apps + their granted
/// permission subsets, for the host's manage-apps UI.
#[tauri::command]
pub async fn meshapp_list_installs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::meshapp::MeshAppInstall>, String> {
    Ok(state.config.read().await.meshapp_installs.clone())
}

/// `meshapp_record_install(appId, name, granted)` — record (or replace)
/// an install with the GRANTED permission subset from the consent sheet.
/// Persist-first so the grant survives a restart; the granted set, not
/// the manifest's request, is what the bridge enforces.
#[tauri::command]
pub async fn meshapp_record_install(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    app_id: String,
    name: String,
    granted: MeshAppPermissions,
) -> Result<crate::meshapp::MeshAppInstall, String> {
    if app_id_from_label(webview.label()).is_some() {
        return Err("install management is host-only".into());
    }
    let recorded_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let install = crate::meshapp::MeshAppInstall {
        app_id: app_id.clone(),
        name,
        granted,
        trust: crate::meshapp::MeshAppTrust::Unsigned,
        recorded_at_unix,
    };
    let mut cfg = state.config.write().await;
    cfg.meshapp_installs.retain(|i| i.app_id != app_id);
    cfg.meshapp_installs.push(install.clone());
    cfg.save()
        .map_err(|e| format!("save desktop config: {e}"))?;
    Ok(install)
}

/// `meshapp_uninstall(appId)` — remove an install, revoking every grant.
#[tauri::command]
pub async fn meshapp_uninstall(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    app_id: String,
) -> Result<(), String> {
    if app_id_from_label(webview.label()).is_some() {
        return Err("install management is host-only".into());
    }
    let mut cfg = state.config.write().await;
    let before = cfg.meshapp_installs.len();
    cfg.meshapp_installs.retain(|i| i.app_id != app_id);
    if cfg.meshapp_installs.len() != before {
        cfg.save()
            .map_err(|e| format!("save desktop config: {e}"))?;
    }
    Ok(())
}

/// `meshapp_stage_corpus_recipe(corpusId, recipeToml)` — host-only. A mesh app
/// declares a `corpus` dependency and ships that corpus's recipe (with its
/// `[prebuilt]` HuggingFace-snapshot block) in its bundle. This writes the
/// recipe to the local-override recipes dir (`~/.svrnmesh/recipes/<id>.toml`),
/// which the daemon checks FIRST when resolving a corpus to install — so "Get
/// data" works even though the corpus isn't in the shipped registry. The
/// prebuilt fast-path then restores the index from HF in seconds. Idempotent;
/// the `corpus_id` is slug-validated to keep the write inside the recipes dir.
#[tauri::command]
pub async fn meshapp_stage_corpus_recipe(
    webview: WebviewWindow,
    corpus_id: String,
    recipe_toml: String,
) -> Result<(), String> {
    if app_id_from_label(webview.label()).is_some() {
        return Err("staging a corpus recipe is host-only".into());
    }
    if corpus_id.is_empty()
        || !corpus_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid corpus id `{corpus_id}`"));
    }
    let dir = sovereign_contracts::rebrand::svrnmesh_root().join("recipes");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{corpus_id}.toml"));
    std::fs::write(&path, recipe_toml).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Read the manifests of apps installed via `sovereign meshapp install`
/// (under `~/.svrnmesh/meshapps/<id>/meshapp.json`). Skips the shared `_sdk/`
/// and the `artifacts/` cache. Pure (takes the dir) so it's unit-testable.
fn scan_installed_apps(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name == "artifacts" {
            continue;
        }
        if let Ok(bytes) = std::fs::read(e.path().join("meshapp.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                out.push(v);
            }
        }
    }
    out
}

/// `meshapp_installed_apps()` — the manifests of registry-installed apps, for
/// the host to merge into its catalog alongside the bundled first-party apps.
/// (Opening an installed app in a sandboxed window — serving its bundle from
/// the install dir — is the remaining integration; today installed apps run
/// via `sovereign meshapp dev <id>`. See docs/MESHAPP_AUTHORING.md.)
#[tauri::command]
pub async fn meshapp_installed_apps() -> Result<Vec<serde_json::Value>, String> {
    let dir = sovereign_contracts::rebrand::svrnmesh_root().join("meshapps");
    Ok(scan_installed_apps(&dir))
}

// ─── Window creation + sandbox ───────────────────────────────────────

/// The `window.meshApp` shim injected into every mesh-app window before
/// its own scripts run. The bundle calls these instead of touching
/// `invoke` directly. (Trusted-first-party model: a hostile bundle could
/// still reach `window.__TAURI__` since Tauri v2 doesn't gate app
/// commands per-window — tauri#9227 — so true isolation for untrusted
/// apps is the deferred no-IPC bridge milestone. For first-party apps
/// this shim is the clean, intended surface.)
// Embedded from a shared `.js` file so the Playwright wiring test injects
// the EXACT same source (single source of truth) — the mocked-`meshApp`
// specs don't exercise this shim→IPC path, which is where the
// `withGlobalTauri`-off bug hid. See `meshapp_shim.js` for the rationale.
const MESHAPP_SHIM: &str = include_str!("../meshapp_shim.js");

/// Strict CSP for a mesh-app window: scripts/styles from the bundle only
/// (no inline/eval scripts), NO external network egress — `connect-src`
/// is limited to the Tauri IPC scheme so `window.meshApp` still works but
/// the bundle cannot `fetch`/WebSocket anywhere. The only path to the
/// host is the gated bridge.
const MESHAPP_CSP: &str = "default-src 'self'; script-src 'self'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
     connect-src ipc: http://ipc.localhost; object-src 'none'; \
     base-uri 'self'; form-action 'none'";

/// `meshapp_open(appId, entry?)` — host command (main-window UI) that
/// opens the sandboxed window for an INSTALLED app. The window label is
/// `meshapp-<appId>`, which the bridge resolves the calling app from and
/// which `capabilities/meshapp.json` scopes to. Loads the bundled assets
/// at `meshapp/<appId>/<entry>`, injects the `window.meshApp` shim, and
/// clamps the window to the strict CSP. Async per Tauri's
/// WebviewWindowBuilder guidance (sync commands can deadlock on Windows).
#[tauri::command]
pub async fn meshapp_open(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    app_id: String,
    entry: Option<String>,
) -> Result<(), String> {
    // Only open INSTALLED apps — the consent/grant must exist first, so a
    // window never loads for an app with no recorded permissions.
    let installed = state
        .config
        .read()
        .await
        .meshapp_installs
        .iter()
        .any(|i| i.app_id == app_id);
    if !installed {
        return Err(format!(
            "app `{app_id}` is not installed — record install consent first"
        ));
    }

    let label = format!("{}{app_id}", crate::meshapp::MESHAPP_LABEL_PREFIX);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }

    let entry = entry.unwrap_or_else(|| "index.html".to_string());
    let url = format!("meshapp/{app_id}/{entry}");
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(format!("Mesh App — {app_id}"))
        .inner_size(1024.0, 760.0)
        .initialization_script(MESHAPP_SHIM)
        .on_web_resource_request(|_req, res| {
            res.headers_mut().insert(
                tauri::http::header::CONTENT_SECURITY_POLICY,
                tauri::http::HeaderValue::from_static(MESHAPP_CSP),
            );
        })
        .build()
        .map_err(|e| format!("open mesh-app window `{label}`: {e}"))?;
    Ok(())
}

/// `open_corpus_explorer(corpusId)` — host command (recipe-author + the demo
/// tutorial) that opens the generic Atlas Explorer mesh app bound to a corpus
/// chosen at RUNTIME. Ensures the explorer's one-time `mesh_store_read` install
/// grant exists, then opens the sandboxed window at
/// `meshapp/explorer/index.html?corpus=<id>` (the bundle reads `?corpus=`). The
/// corpus must already be built/installed — the explorer only reads its atlas.
/// One install record unlocks the explorer for every corpus the user authors.
#[tauri::command]
pub async fn open_corpus_explorer(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    const EXPLORER_ID: &str = "explorer";
    // The corpus id is interpolated into the window URL's query string —
    // slug-guard it (same shape `meshapp_stage_corpus_recipe` enforces).
    if corpus_id.is_empty()
        || !corpus_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid corpus id: `{corpus_id}`"));
    }

    // Ensure the explorer's install record (one-time, read-only). Scoped so the
    // write guard drops before `meshapp_open` takes its own read guard.
    {
        let mut cfg = state.config.write().await;
        if !cfg.meshapp_installs.iter().any(|i| i.app_id == EXPLORER_ID) {
            let recorded_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            cfg.meshapp_installs.push(crate::meshapp::MeshAppInstall {
                app_id: EXPLORER_ID.to_string(),
                name: "Atlas Explorer".to_string(),
                granted: MeshAppPermissions {
                    mesh_store_read: true,
                    mesh_store_write: false,
                    inference_access: false,
                    knowledge_access: false,
                },
                trust: crate::meshapp::MeshAppTrust::Unsigned,
                recorded_at_unix,
            });
            cfg.save()
                .map_err(|e| format!("save desktop config: {e}"))?;
        }
    }

    meshapp_open(
        app,
        state,
        EXPLORER_ID.to_string(),
        Some(format!("index.html?corpus={corpus_id}")),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_installed_apps_reads_manifests_skips_sdk_and_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::create_dir_all(d.join("enron")).unwrap();
        std::fs::write(
            d.join("enron").join("meshapp.json"),
            r#"{"id":"enron","name":"Enron"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(d.join("_sdk")).unwrap(); // shared SDK — skipped
        std::fs::create_dir_all(d.join("artifacts")).unwrap(); // publish cache — skipped
        std::fs::create_dir_all(d.join("nomanifest")).unwrap(); // no meshapp.json — skipped

        let apps = scan_installed_apps(d);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0]["id"], "enron");
    }
}
