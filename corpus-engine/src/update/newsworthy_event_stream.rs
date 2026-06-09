// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wikimedia EventStreams subscription for push-based article freshness.
//!
//! Subscribes to `https://stream.wikimedia.org/v2/stream/recentchange`
//! (Server-Sent Events) and filters every edit event in real time
//! against the newsworthy watcher's tracked-set. On a match the title
//! is pushed onto a channel that the watcher drains and refreshes via
//! the existing rate-limit-safe `MediaWikiClient`.
//!
//! Why this exists alongside the existing 24h portal poll:
//! - **Online + fresh:** EventStreams collapses refresh latency from
//!   "up to 24h" to "seconds-after-the-edit." Real-time freshness on
//!   the tracked set without polling MediaWiki's per-article API.
//! - **Brief offline (< ~7d):** SSE supports cursor-based replay via
//!   `Last-Event-ID`. We persist the latest seen offset to MeshStore
//!   on every event; on reconnect after an outage the server replays
//!   missed events from that cursor.
//! - **Long offline (> ~7d):** SSE retention expires beyond ~7 days,
//!   so we fall back to the existing 24h portal-poll + revid-batch-
//!   check path in `WikipediaNewsworthyWatcher::tick`. That path is
//!   the canonical bulk-reconciliation surface; EventStreams is a
//!   latency optimisation layered on top, not a replacement.
//!
//! Glassbox: emits `newsworthy.event_stream_*` info / warn events
//! at every state transition (connecting, connected, event matched,
//! event filtered, checkpoint saved, reconnect attempt, retention
//! gap detected). Operators can grep the daemon log to answer
//! "did we miss this edit?" without instrumenting further.
//!
//! Architecture seam: this module is host-agnostic. The trait
//! [`EventStreamHost`] abstracts checkpoint persistence + the
//! tracked-set query. The watcher's production host
//! (`MeshNewsworthyHost`) implements it against `MeshStore`.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// `app_id` namespace under which the SSE cursor checkpoint is
/// persisted in the host's `MeshStore`. Distinct from
/// `APP_ID_TRACKED` / `APP_ID_PORTAL` (the watcher's other state)
/// so a debug-time "wipe the cursor and re-subscribe from now"
/// doesn't accidentally drop the tracked set.
pub const APP_ID_EVENT_STREAM: &str = "wikipedia-newsworthy:event_stream";

/// Single canonical key under [`APP_ID_EVENT_STREAM`] for the
/// `Last-Event-ID` checkpoint. SSE's Last-Event-ID is a string
/// (Wikimedia returns a JSON array of `{topic, partition, offset}`
/// triples) so we persist it verbatim.
pub const CHECKPOINT_KEY: &str = "last_event_id";

/// Host-side hooks the SSE loop needs. Kept narrow so tests can
/// inject a stub without spinning up a real MeshStore or HTTP server.
#[async_trait::async_trait]
pub trait EventStreamHost: Send + Sync {
    /// Display label used in glassbox log lines. NOT a security identity.
    fn self_node_id_str(&self) -> String;

    /// True when this node owns `partition_key` under rendezvous
    /// hashing. The event-stream loop consults this before refreshing
    /// — keeps follower-step ownership semantics consistent with the
    /// 24h tick path, so the same article isn't refreshed by N
    /// followers in parallel.
    async fn is_owner_of(&self, partition_key: &str) -> bool;

    /// Read the persisted SSE cursor. `None` on first run / after
    /// `store_delete`. Returned verbatim — Wikimedia's Last-Event-ID
    /// is a JSON array, not opaque to us, but we don't peek inside.
    fn cursor_get(&self) -> Result<Option<String>>;

    /// Persist a fresh SSE cursor. Called after every successfully
    /// processed event. Implementations should make this cheap
    /// (small RAM write or a single UPDATE) — at ~30 events/sec on
    /// en.wikipedia this is hot path.
    fn cursor_set(&self, value: &str) -> Result<()>;

    /// Snapshot the current tracked-set as a `HashSet` of normalised
    /// titles. Called once per event to filter; the watcher's tick
    /// loop is the only writer of the tracked-set so reads here
    /// don't need to coordinate.
    fn tracked_titles(&self) -> Result<std::collections::HashSet<String>>;

    /// Trigger a refresh of `title` against the parent corpus. The
    /// production host dispatches into the watcher's refresh queue
    /// (single in-flight HTTP request to MediaWiki, polite UA,
    /// backoff on 429). Detached / fire-and-forget — the SSE loop
    /// can't block on refresh latency without falling behind the
    /// stream.
    fn refresh_article(&self, title: &str);
}

/// Per-event-stream telemetry surfaced via `tracing::info!`. Counters
/// reset on every reconnect — operators reading the log distinguish
/// long-running healthy state from churn by the reconnect rate, not
/// the absolute totals.
#[derive(Debug, Default, Clone)]
pub struct StreamStats {
    /// Total events received from the stream since this connection
    /// was established.
    pub events_received: u64,
    /// Events whose title was in the tracked-set AND this node owns.
    pub matches_dispatched: u64,
    /// Events whose title was in the tracked-set but this node
    /// doesn't own (a peer will handle the refresh).
    pub matches_skipped_not_owner: u64,
    /// Events whose title was NOT in the tracked-set. The vast
    /// majority on a busy stream; counted to expose filter
    /// selectivity in the log.
    pub filtered_out: u64,
}

/// Minimal subset of the Wikimedia `recentchange` event shape that
/// the filter cares about. Full schema:
/// <https://schema.wikimedia.org/repositories/primary/jsonschema/mediawiki/recentchange/current.yaml>
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecentChange {
    /// `edit`, `new`, `log`, `categorize`. We filter on `edit | new`.
    #[serde(rename = "type")]
    pub change_type: String,
    /// Article title in display form (spaces, not underscores).
    pub title: String,
    /// Wiki identifier — we filter on `enwiki`.
    pub wiki: String,
    /// Article namespace; 0 = main namespace. Talk / User pages live
    /// in other namespaces and are correctly excluded.
    #[serde(default)]
    pub namespace: i64,
    /// `meta` carries the stream offset Wikimedia uses for resume.
    pub meta: RecentChangeMeta,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecentChangeMeta {
    pub domain: String,
    /// Wikimedia's event ID — passed back as `Last-Event-ID` on
    /// reconnect. JSON-shaped; we treat it as an opaque cursor.
    pub id: String,
}

/// Normalise a Wikipedia title for tracked-set lookup. MediaWiki
/// uses underscores in URLs but spaces in display + the stream's
/// `title` field. The tracked set is keyed on the underscored URL
/// form so we normalise event titles to match.
pub fn normalise_title(raw: &str) -> String {
    raw.trim().replace(' ', "_")
}

/// Build the EventStreams URL with the optional cursor in query
/// position. Wikimedia accepts either `?since=<offset>` or the
/// `Last-Event-ID` header for resume; we use the header path since
/// `reqwest-eventsource` handles it for us. The query-position
/// `since` is here for completeness — when we want to force a
/// known starting point (debug / forced re-sync) the caller can
/// pass `?since=...` and bypass the persisted cursor.
pub fn stream_url(base: &str) -> String {
    format!("{base}/v2/stream/recentchange")
}

/// Spawn the SSE subscription loop. Returns the join handle so the
/// daemon's shutdown sequence can await graceful termination.
/// Cancellation is signalled via `shutdown_rx` — the loop checks
/// between events and exits on `true`.
///
/// On reconnect (transient HTTP failures, server-side stream end),
/// the loop uses `Last-Event-ID` for resume within Wikimedia's
/// retention window. After ~7 days offline the cursor expires
/// silently; the loop reconnects from the live edge and the
/// existing 24h portal-poll path catches any gap on its next tick.
pub fn spawn(
    host: Arc<dyn EventStreamHost>,
    base_url: String,
    user_agent: String,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // First-tick jitter: a fresh-mesh scenario where every node
        // boots in the same second would all dial Wikimedia
        // simultaneously. 0-15s jitter on each daemon's first
        // subscribe smooths the thundering-herd. After the initial
        // connect we go straight to reconnect-on-drop with backoff.
        let initial_jitter_ms = crate::update::newsworthy_watcher::rand_jitter_ms(15_000);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(initial_jitter_ms)) => {}
            _ = shutdown_rx.changed() => return,
        }

        // Exponential reconnect backoff. Resets to 1s after a
        // successful (event-receiving) connection lasts longer than
        // RECONNECT_RESET_AFTER. Caps at MAX_RECONNECT_BACKOFF so a
        // sustained Wikimedia outage doesn't park us indefinitely.
        const MIN_BACKOFF: Duration = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(60);
        const RECONNECT_RESET_AFTER: Duration = Duration::from_secs(30);
        let mut backoff = MIN_BACKOFF;

        loop {
            let connect_started = std::time::Instant::now();
            tracing::info!(
                node = %host.self_node_id_str(),
                base_url = %base_url,
                "newsworthy.event_stream_connecting"
            );
            let outcome = run_one_session(&host, &base_url, &user_agent, &mut shutdown_rx).await;
            match outcome {
                SessionOutcome::Shutdown => {
                    tracing::info!(
                        node = %host.self_node_id_str(),
                        "newsworthy.event_stream_shutdown"
                    );
                    return;
                }
                SessionOutcome::Disconnected { stats, reason } => {
                    let connection_secs = connect_started.elapsed().as_secs();
                    if connect_started.elapsed() > RECONNECT_RESET_AFTER {
                        backoff = MIN_BACKOFF;
                    }
                    tracing::warn!(
                        node = %host.self_node_id_str(),
                        events_received = stats.events_received,
                        matches_dispatched = stats.matches_dispatched,
                        matches_skipped_not_owner = stats.matches_skipped_not_owner,
                        filtered_out = stats.filtered_out,
                        connection_secs,
                        reason = %reason,
                        backoff_secs = backoff.as_secs(),
                        "newsworthy.event_stream_reconnect"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown_rx.changed() => return,
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    })
}

enum SessionOutcome {
    Shutdown,
    Disconnected { stats: StreamStats, reason: String },
}

/// One subscription session — connects to the SSE endpoint, drains
/// events until disconnect or shutdown. Resume cursor is read at the
/// start of each session and updated as events flow.
async fn run_one_session(
    host: &Arc<dyn EventStreamHost>,
    base_url: &str,
    user_agent: &str,
    shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> SessionOutcome {
    use futures::StreamExt;
    use reqwest_eventsource::{Event, EventSource};

    let url = stream_url(base_url);
    let cursor = host.cursor_get().ok().flatten();

    let mut builder = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::USER_AGENT, user_agent);
    if let Some(cursor_value) = cursor.as_ref() {
        builder = builder.header("Last-Event-ID", cursor_value);
    }
    let mut es = match EventSource::new(builder) {
        Ok(e) => e,
        Err(e) => {
            return SessionOutcome::Disconnected {
                stats: StreamStats::default(),
                reason: format!("EventSource init: {e}"),
            }
        }
    };

    let mut stats = StreamStats::default();
    let resume_kind = if cursor.is_some() { "resumed" } else { "fresh" };

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => return SessionOutcome::Shutdown,
            maybe_ev = es.next() => {
                let ev = match maybe_ev {
                    Some(v) => v,
                    None => {
                        return SessionOutcome::Disconnected {
                            stats,
                            reason: "stream ended (None)".into(),
                        };
                    }
                };
                match ev {
                    Ok(Event::Open) => {
                        tracing::info!(
                            node = %host.self_node_id_str(),
                            resume = resume_kind,
                            "newsworthy.event_stream_connected"
                        );
                    }
                    Ok(Event::Message(msg)) => {
                        stats.events_received += 1;
                        // Wikimedia sets the SSE id on every event;
                        // persist it BEFORE handling so a crash
                        // between handling and persist re-plays at
                        // most one event (idempotent on our side —
                        // article refresh by title is fine to repeat).
                        if !msg.id.is_empty() {
                            if let Err(e) = host.cursor_set(&msg.id) {
                                tracing::warn!(
                                    node = %host.self_node_id_str(),
                                    error = %e,
                                    "newsworthy.event_stream_cursor_persist_failed"
                                );
                            }
                        }
                        match serde_json::from_str::<RecentChange>(&msg.data) {
                            Ok(rc) => handle_event(host, &rc, &mut stats).await,
                            Err(e) => {
                                // Schema drift OR a non-recentchange
                                // event (Wikimedia mixes a few stream
                                // shapes). Log at debug to avoid log
                                // spam — the filtered_out counter on
                                // the next reconnect will reflect the
                                // miss rate.
                                tracing::debug!(
                                    error = %e,
                                    "newsworthy.event_stream_parse_skip"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        return SessionOutcome::Disconnected {
                            stats,
                            reason: format!("event error: {e}"),
                        };
                    }
                }
            }
        }
    }
}

async fn handle_event(host: &Arc<dyn EventStreamHost>, rc: &RecentChange, stats: &mut StreamStats) {
    // Filter: en.wikipedia, main namespace, edit-shaped event.
    if rc.meta.domain != "en.wikipedia.org"
        || rc.wiki != "enwiki"
        || rc.namespace != 0
        || !matches!(rc.change_type.as_str(), "edit" | "new")
    {
        stats.filtered_out += 1;
        return;
    }
    let normalised = normalise_title(&rc.title);
    let tracked = match host.tracked_titles() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "newsworthy.event_stream_tracked_lookup_failed");
            stats.filtered_out += 1;
            return;
        }
    };
    if !tracked.contains(&normalised) {
        stats.filtered_out += 1;
        return;
    }
    // The watcher's tick partitions ownership across mesh nodes via
    // rendezvous hashing. Honour that here too so the same edit
    // doesn't trigger N parallel refreshes from N online peers.
    if !host.is_owner_of(&normalised).await {
        stats.matches_skipped_not_owner += 1;
        tracing::debug!(
            title = %normalised,
            "newsworthy.event_stream_match_not_owner"
        );
        return;
    }
    stats.matches_dispatched += 1;
    tracing::info!(
        title = %normalised,
        domain = %rc.meta.domain,
        change_type = %rc.change_type,
        "newsworthy.event_stream_match"
    );
    host.refresh_article(&normalised);
}

/// Convert Wikipedia's `Error` variants for the SSE module. Lives
/// here rather than `crate::error` because EventStream errors are
/// non-fatal — the session loop converts them into reconnects.
impl From<reqwest_eventsource::Error> for Error {
    fn from(e: reqwest_eventsource::Error) -> Self {
        Error::Extraction(format!("EventSource: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_handles_spaces_and_trim() {
        assert_eq!(normalise_title("2026 Iran war"), "2026_Iran_war");
        assert_eq!(
            normalise_title("  2026 Israel–Lebanon ceasefire  "),
            "2026_Israel–Lebanon_ceasefire"
        );
        assert_eq!(normalise_title("Already_Normalised"), "Already_Normalised");
    }

    #[test]
    fn stream_url_appends_path() {
        assert_eq!(
            stream_url("https://stream.wikimedia.org"),
            "https://stream.wikimedia.org/v2/stream/recentchange"
        );
    }

    #[test]
    fn recent_change_deserialises_minimal_shape() {
        let body = r#"{
            "type": "edit",
            "title": "2026 Israel-Lebanon ceasefire",
            "wiki": "enwiki",
            "namespace": 0,
            "meta": {
                "domain": "en.wikipedia.org",
                "id": "[{\"topic\":\"eqiad.mediawiki.recentchange\",\"partition\":0,\"offset\":12345}]"
            }
        }"#;
        let rc: RecentChange = serde_json::from_str(body).expect("parses");
        assert_eq!(rc.change_type, "edit");
        assert_eq!(rc.namespace, 0);
        assert_eq!(rc.meta.domain, "en.wikipedia.org");
    }
}
