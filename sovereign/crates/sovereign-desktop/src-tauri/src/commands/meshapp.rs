//! MeshApp bridge — the permission-gated Tauri commands a sandboxed
//! mesh-app webview reaches through `window.meshApp.*`.
//!
//! Every command's FIRST act is [`crate::meshapp::authorize`] against the
//! CALLING webview's label (Tauri injects the `WebviewWindow`; the label
//! is host-assigned at window creation and unspoofable from inside the
//! sandbox). Only after the grant check does a command touch host state.
//!
//! The numeric ops (`read_corpus`, `parcel_analytics`) are deterministic
//! and read-only — folds over typed parcel atoms, no inference — so the
//! SF-LVT "no confabulated numbers" guarantee carries onto the desktop
//! surface: a model never originates a figure here either.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::meshapp::{app_id_from_label, authorize, resolve_grant, MeshAppPermissions, Permission};
use crate::state::AppState;

use corpus_engine::enrichment::atlas::analysis::{compute_aggregates, flags, FlagKind};
use corpus_engine::enrichment::atlas::AtomEnvelope;
use corpus_engine::enrichment::pipeline::atlas::EntityType;

/// Default SF business-tax take (~$1.4B) the flat land levy must replace.
const DEFAULT_BUSINESS_TAX_TARGET: f64 = 1_400_000_000.0;
const DEFAULT_ENTITY_TYPE: &str = "parcel";

/// One parcel atom for the webview, carrying its provenance handle so the
/// per-parcel calculator can chip every number back to its source.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelDto {
    pub atom_id: String,
    pub parcel_number: String,
    pub source_chunk: Option<String>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// The deterministic city-wide aggregate + its derivation. Scalars only
/// (NOT the ~208k `atom_ids`): the macro model multiplies
/// `land_value_total` by the slider rate in JS, and provenance is
/// "computed over `parcel_count` parcel atoms in `corpus_id`", surfaced
/// via `derivation`.
#[derive(Debug, Clone, Serialize)]
pub struct ParcelAnalyticsDto {
    pub corpus_id: String,
    pub parcel_count: usize,
    pub land_value_total: f64,
    pub improvement_value_total: f64,
    pub business_tax_target: f64,
    pub neutral_rate: f64,
    pub high_land_share_count: usize,
    pub underused_count: usize,
    pub derivation: Vec<String>,
}

/// Load a corpus's atlas atoms, propagating errors (the bridge surfaces a
/// reason rather than silently returning empty). Mirrors the read path in
/// `commands::reading`.
async fn load_atoms(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
) -> Result<Vec<AtomEnvelope>, String> {
    let engine = state
        .corpus_engine
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "corpus engine not initialized".to_string())?;
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes: {e}"))?;
    let entry = installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .ok_or_else(|| format!("corpus `{corpus_id}` is not installed"))?;
    let atlas_dir = entry.path.join("atlas");
    let file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms for `{corpus_id}`: {e}"))?;
    Ok(file.atoms)
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
/// can look it up without deriving the host-side hash.
#[tauri::command]
pub async fn meshapp_read_corpus(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_ids: Vec<String>,
) -> Result<Vec<ParcelDto>, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let want: HashSet<&str> = atom_ids.iter().map(String::as_str).collect();
    let atoms = load_atoms(&state, &corpus_id).await?;
    let out = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e)
                if want.contains(e.id.as_str())
                    || want.contains(e.canonical_name.as_str()) =>
            {
                Some(ParcelDto {
                    atom_id: e.id.as_str().to_string(),
                    parcel_number: e.canonical_name.clone(),
                    source_chunk: e.provenance.source_chunk_id.clone(),
                    attributes: e.attributes.clone(),
                })
            }
            _ => None,
        })
        .collect();
    Ok(out)
}

/// `window.meshApp.searchParcels(corpusId, query, limit?)` — gated on
/// `mesh_store_read`. Substring/number search over parcel atoms so a UI
/// (a homeowner) can find their parcel by street name or number without
/// knowing the atom-id. Matches the parcel number (exact, case-folded) OR
/// `property_location` (substring, case-folded); capped at `limit` (≤100).
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

    let q = query.trim().to_uppercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let cap = limit.unwrap_or(25).min(100);
    let atoms = load_atoms(&state, &corpus_id).await?;
    let mut out: Vec<ParcelDto> = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => {
                let num_match = e.canonical_name.to_uppercase() == q;
                let addr_match = e
                    .attributes
                    .get("property_location")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_uppercase().contains(&q))
                    .unwrap_or(false);
                if num_match || addr_match {
                    Some(ParcelDto {
                        atom_id: e.id.as_str().to_string(),
                        parcel_number: e.canonical_name.clone(),
                        source_chunk: e.provenance.source_chunk_id.clone(),
                        attributes: e.attributes.clone(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    out.truncate(cap);
    Ok(out)
}

/// `window.meshApp.parcelAnalytics(corpusId, businessTaxTarget?)` — gated
/// on `mesh_store_read` (it reads corpus atoms). Deterministic: folds the
/// parcel atoms into the revenue-neutral land-levy aggregate via
/// corpus-engine's pure lib. No inference; the macro model's headline
/// figures are computed here, never originated by a model.
#[tauri::command]
pub async fn meshapp_parcel_analytics(
    webview: WebviewWindow,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    business_tax_target: Option<f64>,
) -> Result<ParcelAnalyticsDto, String> {
    let installs = state.config.read().await.meshapp_installs.clone();
    authorize(&installs, webview.label(), Permission::MeshStoreRead)?;

    let target = business_tax_target.unwrap_or(DEFAULT_BUSINESS_TAX_TARGET);
    let atoms = load_atoms(&state, &corpus_id).await?;
    let parcels: Vec<_> = atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => match &e.entity_type {
                EntityType::Other(t) if t.as_str() == DEFAULT_ENTITY_TYPE => Some(e),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if parcels.is_empty() {
        return Err(format!(
            "corpus `{corpus_id}` has no `{DEFAULT_ENTITY_TYPE}` atoms"
        ));
    }

    let agg = compute_aggregates(&parcels, &corpus_id, target);
    let fs = flags(&parcels);
    let high = fs.iter().filter(|f| f.kind == FlagKind::HighLandShare).count();
    let under = fs.iter().filter(|f| f.kind == FlagKind::Underused).count();

    let n = fmt_int(agg.parcel_count as f64);
    let derivation = vec![
        format!(
            "land_value_total = Σ assessed_land_value over {n} parcel atoms ({corpus_id}) = {}",
            fmt_usd(agg.land_value_total)
        ),
        format!(
            "neutral_rate = business_tax_target ÷ land_value_total = {} ÷ {} = {}",
            fmt_usd(agg.business_tax_target),
            fmt_usd(agg.land_value_total),
            fmt_pct(agg.neutral_rate)
        ),
    ];

    Ok(ParcelAnalyticsDto {
        corpus_id: agg.corpus_id,
        parcel_count: agg.parcel_count,
        land_value_total: agg.land_value_total,
        improvement_value_total: agg.improvement_value_total,
        business_tax_target: agg.business_tax_target,
        neutral_rate: agg.neutral_rate,
        high_land_share_count: high,
        underused_count: under,
        derivation,
    })
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

/// `$174,097,946,887.00` — full-precision, comma-grouped USD for the
/// derivation trace (matches the chat/tool surface so the two agree).
fn fmt_usd(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let dollars = (cents / 100) as f64;
    format!("${}.{:02}", fmt_int(dollars), (cents % 100).abs())
}

fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

fn fmt_int(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}
