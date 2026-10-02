// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's foreground, published to cw-rails' donor (pb-work-donor,
//! phase-b-38 fork 3).
//!
//! A donor whose offer says `yield_to_foreground` takes no new unit while the
//! operator is in a turn or inside the yield window after one. The donor runs
//! in cw-rails now, and the foreground is this daemon's, so the daemon posts
//! its deadline to `POST /v1/work/yield`: on every turn's begin and end, and
//! at most once a second while a turn is in flight. The deadline is
//! `now + seconds_until_foreground_idle`, the same window
//! `should_yield_to_foreground` reads, so the donor stands down exactly when
//! it did in-process.

use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::ingest_executor::TRACE_TARGET;
use crate::state::AppState;

/// The fastest the deadline is re-posted.
pub const POST_AT_MOST_EVERY: Duration = Duration::from_secs(1);

/// Aborts the publisher on Drop.
pub struct ForegroundPostHandle(tokio::task::JoinHandle<()>);

impl Drop for ForegroundPostHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn spawn(app: AppState, rails_base: String) -> ForegroundPostHandle {
    ForegroundPostHandle(tokio::spawn(publish_forever(app, rails_base)))
}

async fn publish_forever(app: AppState, rails_base: String) {
    let mut last_post: Option<Instant> = None;
    let mut told_absent = false;
    loop {
        // A turn's begin and end wake this at once; while one is in flight the
        // window must not lapse under it, so it also wakes every second.
        if app.foreground_inflight() > 0 {
            tokio::select! {
                _ = app.inner.node.foreground_changed.notified() => {}
                _ = tokio::time::sleep(POST_AT_MOST_EVERY) => {}
            }
        } else {
            app.inner.node.foreground_changed.notified().await;
        }
        if let Some(at) = last_post {
            let since = at.elapsed();
            if since < POST_AT_MOST_EVERY {
                tokio::time::sleep(POST_AT_MOST_EVERY - since).await;
            }
        }
        let Some(secs) = app.seconds_until_foreground_idle() else {
            debug!(target: TRACE_TARGET, "foreground: not yielding (window 0, or it passed); nothing to post");
            continue;
        };
        let until_ms = sovereign_time::unix_millis() + secs * 1_000;
        last_post = Some(Instant::now());
        match crate::rails_client::post_work_yield(&rails_base, until_ms).await {
            Ok(()) => {
                debug!(target: TRACE_TARGET, until_ms, in_flight = app.foreground_inflight(),
                       "foreground: posted the deadline cw-rails' donor yields to");
                told_absent = false;
            }
            Err(e) if !told_absent => {
                warn!(target: TRACE_TARGET, error = %e,
                      "foreground: cw-rails did not take the deadline, so its donor does not \
                       yield to this turn");
                told_absent = true;
            }
            Err(e) => debug!(target: TRACE_TARGET, error = %e,
                             "foreground: the deadline still does not reach cw-rails"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// **The yield reaches the donor.** A turn beginning posts a deadline one
    /// window past now to `/v1/work/yield`, the door cw-rails' donor reads
    /// before it takes a unit. The failing input is a daemon that tracks its
    /// foreground and never posts it: the donor, now in another process,
    /// would take units through every chat turn.
    #[tokio::test]
    async fn a_turn_posts_the_window_deadline_to_the_donors_door() {
        let seen: Arc<Mutex<Vec<u64>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let door = axum::Router::new().route(
            "/v1/work/yield",
            axum::routing::post(move |axum::Json(b): axum::Json<serde_json::Value>| {
                let sink = Arc::clone(&sink);
                async move {
                    sink.lock().unwrap().push(b["until_ms"].as_u64().unwrap());
                    axum::Json(b)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, door).await.unwrap() });

        let app = crate::state::test_app_state();
        app.set_yield_window_secs(60);
        let _handle = spawn(app.clone(), base);
        let before = sovereign_time::unix_millis();
        app.foreground_begin();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let posted = seen
            .lock()
            .unwrap()
            .first()
            .copied()
            .expect("a posted deadline");
        assert!(
            posted >= before + 59_000 && posted <= sovereign_time::unix_millis() + 60_000,
            "the deadline is one window past the turn, got {posted} (turn at {before})"
        );
        app.foreground_end();
    }
}
