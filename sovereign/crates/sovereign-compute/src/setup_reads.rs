// SPDX-License-Identifier: AGPL-3.0-or-later
//! The setup UI's reads of the SERVING machine: what hardware it has, the
//! primary catalog for its tier, and the single-pick slots. Moved from the
//! daemon's assets_http.rs (pb-svrn-dials-serve) so the process that serves
//! answers them: `serve` mounts [`bundle`], and the daemon forwards to serve
//! (pb-serve-distributes).

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use host_kit::shell::RouteBundle;
use serde::Deserialize;
use sovereign_contracts::daemon_wire::{HardwareProfile, ProfileName, SlotConfig};
use sovereign_inference::hardware;
use sovereign_inference::setup_planner;

/// The three reads as `serve`'s named bundle, at the paths the daemon serves
/// them on.
pub fn bundle() -> RouteBundle {
    RouteBundle::new("setup_reads")
        .route("/v1/admin/hardware", get(hardware))
        .route(
            "/v1/admin/setup/catalog",
            get(|Query(q): Query<ProfileQuery>| catalog(q)),
        )
        .route(
            "/v1/admin/setup/slot",
            get(|Query(q): Query<SlotQuery>| slot(q)),
        )
}

/// `{"error": "<message>"}`, the daemon's refusal shape
/// (sovereign-daemon http_response::json_error), so a read answers alike
/// from either process.
pub(crate) fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Answer of `GET /v1/admin/hardware`. Flat rather than nested so the
/// profile travels with the probe that selected it — a caller that read one
/// without the other would be reasoning about a tier it cannot explain.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HardwareView {
    /// What the probe found on the machine the daemon runs on.
    pub hardware: HardwareProfile,
    /// The tier that follows from it.
    pub profile: ProfileName,
}

/// `GET /v1/admin/hardware` — what the SERVING machine can run.
///
/// Detection walks `/proc` (or the llama.cpp backend device list), so it goes
/// to a blocking thread rather than the reactor — the same call
/// `setup_cmd` makes, through the same free function.
pub async fn hardware() -> Response {
    let Ok(hardware) = tokio::task::spawn_blocking(hardware::detect_hardware).await else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "hardware detection panicked",
        );
    };
    let profile = hardware::select_profile(&hardware);
    Json(HardwareView { hardware, profile }).into_response()
}

/// Query of the two setup reads. `profile` absent means "detect it".
#[derive(Debug, Deserialize)]
pub struct ProfileQuery {
    /// A [`ProfileName::as_str`] spelling. An unrecognised one is refused by
    /// name rather than bucketed into `default` (ARCH principle 6).
    #[serde(default)]
    pub profile: Option<String>,
}

async fn resolve_profile(q: &ProfileQuery) -> Result<ProfileName, Response> {
    match q.profile.as_deref() {
        Some(s) => ProfileName::from_wire(s).ok_or_else(|| {
            json_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "unknown profile `{s}` (expected one of {})",
                    ProfileName::ALL
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        }),
        None => match tokio::task::spawn_blocking(hardware::detect_hardware).await {
            Ok(hw) => Ok(hardware::select_profile(&hw)),
            Err(_) => Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "hardware detection panicked",
            )),
        },
    }
}

/// `GET /v1/admin/setup/catalog?profile=` — the curated primary catalog for a
/// tier. The same `setup_planner::build_primary_catalog` the wizard calls.
pub async fn catalog(q: ProfileQuery) -> Response {
    let profile = match resolve_profile(&q).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    Json(setup_planner::build_primary_catalog(&profile)).into_response()
}

/// Query of `GET /v1/admin/setup/slot`.
#[derive(Debug, Deserialize)]
pub struct SlotQuery {
    /// `fast` or `embed`. The thoughtful slot has the catalog route above;
    /// `fim` has its own onboarding (`svrn setup --fim`) and is not offered
    /// here, so an unknown kind is refused rather than resolved to something.
    pub kind: String,
    #[serde(default)]
    pub profile: Option<String>,
}

/// `GET /v1/admin/setup/slot?kind=fast|embed&profile=` — the single-pick slot
/// for a tier. `null` when the bundled manifest defines none: absent, not a
/// substituted default (ARCH principle 6).
pub async fn slot(q: SlotQuery) -> Response {
    let kind = match q.kind.as_str() {
        "fast" => setup_planner::SlotKind::Fast,
        "embed" => setup_planner::SlotKind::Embed,
        other => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown slot kind `{other}` (expected fast or embed)"),
            )
        }
    };
    let profile = match resolve_profile(&ProfileQuery {
        profile: q.profile.clone(),
    })
    .await
    {
        Ok(p) => p,
        Err(r) => return r,
    };
    let slot: Option<SlotConfig> = setup_planner::resolve_slot(&profile, kind);
    Json(slot).into_response()
}

#[cfg(test)]
#[path = "setup_reads/tests.rs"]
mod tests;
