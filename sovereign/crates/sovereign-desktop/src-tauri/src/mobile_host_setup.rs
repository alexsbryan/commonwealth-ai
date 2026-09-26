// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop wiring for the **Mobile access** toggle — a named absence.
//!
//! The toggle ran `sovereign-server`, the phone-facing API. That binary was
//! deleted and no replacement mobile host ships, so the Tauri commands that
//! reach this module keep their signatures (the Settings panel and
//! `commands.generated.ts` are unchanged) and answer the absence by name
//! (ARCH principle 6).

use tracing::{info, warn};

/// What the toggle and the pairing card answer.
const MOBILE_HOST_ABSENT: &str =
    "the mobile host was the sovereign-server binary, which was deleted; no mobile host ships";

/// Pairing card the Settings panel renders. Never produced now; kept because
/// it is `get_mobile_pairing`'s declared return type.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MobilePairing {
    pub address: String,
    pub tenant: String,
    pub token: String,
    pub iroh_dial: Option<String>,
}

/// Toggle-on, and the launch-time bring-up for a persisted toggle: refused,
/// by name.
pub async fn ensure_running() -> Result<(), String> {
    warn!("mobile-access: refused — {MOBILE_HOST_ABSENT}");
    Err(MOBILE_HOST_ABSENT.to_string())
}

/// Toggle-off. Succeeds: "not serving" is the state the user asked for, and
/// nothing can be serving. An error here would pin a persisted toggle ON,
/// because the Settings panel reverts the switch on failure.
pub async fn stop() -> Result<(), String> {
    info!("mobile-access: toggle-off — nothing to stop; {MOBILE_HOST_ABSENT}");
    Ok(())
}

/// The Settings card: refused, by name.
pub async fn pairing() -> Result<MobilePairing, String> {
    info!("mobile-access: no pairing card — {MOBILE_HOST_ABSENT}");
    Err(MOBILE_HOST_ABSENT.to_string())
}
