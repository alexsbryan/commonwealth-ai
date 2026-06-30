// SPDX-License-Identifier: AGPL-3.0-or-later
//! System tray menu + 5s status poller.
//!
//! Surfaces W2/W3 contribution state to the user without making them
//! open a Settings panel:
//!
//! ```text
//! ┌────────────────────────────────┐
//! │ Status: Serving 2 peer requests│  ← live, refreshed every 5s
//! ├────────────────────────────────┤
//! │ ▸ Pause sharing                │
//! │     15 minutes                 │  ← POST /internal/contribution/pause
//! │     1 hour                     │
//! │     Until I resume             │
//! │ Resume sharing                 │  ← enabled only when paused
//! ├────────────────────────────────┤
//! │ Open svrnmesh                 │
//! │ Quit                           │
//! └────────────────────────────────┘
//! ```
//!
//! Icon-color swapping (green/yellow/red) is deferred — needs three
//! bundled asset files and platform-specific tint rules. The text
//! line is the load-bearing visibility surface for v1.

use std::sync::Arc;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Wry,
};

use crate::commands::{
    self, get_contribution_status, pause_contributions, resume_contributions, ContributionStatus,
};

/// Items the 5s poller mutates. Held in an `Arc` so the poller task
/// can read them without contending with the menu builder.
struct TrayItems {
    status: MenuItem<Wry>,
    resume: MenuItem<Wry>,
}

pub fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let status = MenuItem::with_id(app, "status", "Status: Starting…", false, None::<&str>)?;
    let pause_15m = MenuItem::with_id(app, "pause:900", "15 minutes", true, None::<&str>)?;
    let pause_1h = MenuItem::with_id(app, "pause:3600", "1 hour", true, None::<&str>)?;
    // 0 encodes "until I resume" — handled in the event handler by
    // stamping a far-future expiry. Keeping the wire field a single
    // `duration_secs` u64 means the daemon route doesn't need a
    // separate "indefinite" code path.
    let pause_indef = MenuItem::with_id(app, "pause:0", "Until I resume", true, None::<&str>)?;
    let pause_submenu = Submenu::with_items(
        app,
        "Pause sharing",
        true,
        &[&pause_15m, &pause_1h, &pause_indef],
    )?;

    // Disabled by default; the poller flips this on whenever the
    // daemon reports an active pause.
    let resume = MenuItem::with_id(app, "resume", "Resume sharing", false, None::<&str>)?;

    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let open = MenuItem::with_id(app, "open", "Open svrnmesh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[&status, &sep1, &pause_submenu, &resume, &sep2, &open, &quit],
    )?;

    let items = Arc::new(TrayItems {
        status: status.clone(),
        resume: resume.clone(),
    });

    TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("svrnmesh")
        .on_menu_event(move |app: &AppHandle, event| handle_event(app, event))
        .build(app)?;

    // Kick off the 5s status poller. The first tick fires immediately
    // (interval default) so the menu doesn't sit on "Starting…" for
    // longer than one HTTP round-trip.
    spawn_poller(app.handle().clone(), Arc::clone(&items));

    Ok(())
}

fn handle_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "quit" => app.exit(0),
        "open" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        id if id.starts_with("pause:") => {
            let secs: u64 = id
                .strip_prefix("pause:")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            // 0 ("Until I resume") maps to a far-future expiry. One
            // year is well past any "I forgot I paused" timeframe and
            // shorter than i64::MAX seconds (which would risk weird
            // arithmetic at the daemon).
            let duration = if secs == 0 { 365 * 24 * 3600 } else { secs };
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match pause_contributions(duration).await {
                    Ok(_) => {
                        let _ = app_handle.emit("tray-status-refresh", ());
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            duration_secs = duration,
                            "tray: pause_contributions failed"
                        );
                    }
                }
            });
        }
        "resume" => {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match resume_contributions().await {
                    Ok(_) => {
                        let _ = app_handle.emit("tray-status-refresh", ());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tray: resume_contributions failed");
                    }
                }
            });
        }
        _ => {}
    }
}

fn spawn_poller(_app: AppHandle, items: Arc<TrayItems>) {
    tauri::async_runtime::spawn(async move {
        // Tight initial cadence so the menu reflects reality within
        // the first ~5s after launch; settles to a relaxed loop.
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match get_contribution_status().await {
                Ok(status) => {
                    let text = render_status_text(&status);
                    if let Err(e) = items.status.set_text(&text) {
                        tracing::debug!(error = %e, "tray: set status text failed");
                    }
                    let paused = status.paused_until.is_some();
                    if let Err(e) = items.resume.set_enabled(paused) {
                        tracing::debug!(error = %e, "tray: set resume enabled failed");
                    }
                }
                Err(e) => {
                    // Daemon may not be up yet during early boot, or
                    // the user may have hit a transient network error.
                    // Don't spam — debug level only.
                    tracing::debug!(error = %e, "tray: contribution status poll failed");
                }
            }
        }
    });
}

fn render_status_text(s: &ContributionStatus) -> String {
    // Priority order matches the user's mental model: an explicit
    // pause overrides everything; a yield window is the next
    // visible state; in-flight count is the default.
    if let Some(remaining) = s.pause_remaining_secs {
        if remaining > 365 * 24 * 3600 - 600 {
            return "Status: Paused (until I resume)".into();
        }
        let mins = remaining.div_ceil(60).max(1);
        return format!("Status: Paused — {mins} min remaining");
    }
    if s.yielding_secs_remaining.is_some() {
        return "Status: Yielding to local chat".into();
    }
    match s.in_flight {
        0 => "Status: Idle".into(),
        1 => "Status: Serving 1 peer request".into(),
        n => format!("Status: Serving {n} peer requests"),
    }
}

// `commands` re-export to silence the `unused import` lint if no
// other path hits these names directly. The poller uses them via the
// crate-qualified calls above.
#[allow(dead_code)]
fn _keep_commands_used() {
    let _ = commands::get_contribution_status;
    let _ = commands::pause_contributions;
    let _ = commands::resume_contributions;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ContributionStatus;

    fn status(
        in_flight: usize,
        paused_remaining: Option<u64>,
        yielding: Option<u64>,
    ) -> ContributionStatus {
        ContributionStatus {
            ceiling: usize::MAX,
            in_flight,
            paused_until: paused_remaining.map(|_| 0),
            pause_remaining_secs: paused_remaining,
            yield_peers_to_foreground: true,
            yielding_secs_remaining: yielding,
        }
    }

    #[test]
    fn renders_idle_when_nothing_in_flight() {
        let s = status(0, None, None);
        assert_eq!(render_status_text(&s), "Status: Idle");
    }

    #[test]
    fn renders_singular_when_one_peer_request() {
        let s = status(1, None, None);
        assert_eq!(render_status_text(&s), "Status: Serving 1 peer request");
    }

    #[test]
    fn renders_plural_when_multiple_peer_requests() {
        let s = status(3, None, None);
        assert_eq!(render_status_text(&s), "Status: Serving 3 peer requests");
    }

    #[test]
    fn pause_overrides_in_flight_count() {
        let s = status(5, Some(600), None);
        assert!(render_status_text(&s).starts_with("Status: Paused"));
    }

    #[test]
    fn indefinite_pause_renders_distinctively() {
        // Just under 1 year — what handle_event encodes for the
        // "Until I resume" menu item.
        let s = status(0, Some(365 * 24 * 3600), None);
        assert_eq!(render_status_text(&s), "Status: Paused (until I resume)");
    }

    #[test]
    fn yield_overrides_idle() {
        let s = status(0, None, Some(30));
        assert_eq!(render_status_text(&s), "Status: Yielding to local chat");
    }
}
