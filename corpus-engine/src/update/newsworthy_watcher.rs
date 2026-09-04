// SPDX-License-Identifier: AGPL-3.0-or-later
//! `wikipedia-newsworthy` freshness daemon.
//!
//! Reads `Portal:Current_events` daily, derives a tracked set of
//! MediaWiki article titles from event-bullet wikilinks, and reconciles
//! that tracked set against the parent `wikipedia` corpus. Tracked-set
//! state and daily portal idempotency markers live in the host's KV
//! store via the [`NewsworthyHost`] adapter trait — `corpus-engine`
//! deliberately does not link Commonwealth.
//!
//! Only the tracked set replicates. `wikipedia-newsworthy:portal` is
//! the leader's own idempotency marker and `wikipedia-newsworthy:status`
//! is this node's own tick snapshot, so both are gossip-excluded
//! (`commonwealth-state::peer_preferences::GOSSIP_EXCLUDED_APP_IDS`);
//! `:status` had to be, since its single unsuffixed `last_tick` key
//! made a peer's snapshot last-write-wins over yours. A fourth
//! namespace, `wikipedia-newsworthy:job`, was declared here and never
//! written or read; cw-lift 2b deleted it.
//!
//! Two-phase tick:
//!
//! 1. **Leader-only daily portal ingest.** The lowest-`NodeId` member
//!    of the current online set fetches yesterday's portal page,
//!    re-indexes it into `wikipedia-newsworthy` keyed by date, extracts
//!    bullet wikilinks, and writes additions to the tracked set.
//!    Idempotent under repeated leader ticks via the
//!    `wikipedia-newsworthy:portal` KV namespace (date → last revid).
//!
//! 2. **Every-node partition-owned reconciliation.** Each node walks
//!    the tracked set, filters to titles it owns under rendezvous
//!    hashing, and either fetches (`PendingFetch`) or revision-checks
//!    (`Present`) each owned article. Refreshes go via
//!    [`crate::engine::CorpusEngine::reindex_by_source_doc_id`]
//!    against the parent `wikipedia` corpus.
//!
//! Glassbox: every state transition emits a `tracing::info!` event.
//! Per-tick summary surfaces tracked counts, owned counts, fetched
//! counts, errors. The companion CLI subcommand
//! `sovereign newsworthy status` reads this same KV state — operators
//! never have to attach a debugger.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use corpus_engine_yield::DeferralBudget;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::chunkers::portal_event_bullet::extract_bullet_links;
use crate::engine::yield_gate::{self, YieldExit};
use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::recipe::{ChunkerConfig, ExtractorConfig};

/// MeshStore namespace for tracked-article rows. Keyed by
/// `tracked:<title>` with the title normalised (spaces → underscores).
pub const APP_ID_TRACKED: &str = "wikipedia-newsworthy:tracked";

/// KV namespace for daily portal-page idempotency markers. Keyed by
/// `portal:<YYYY-MM-DD>`. Written and read only inside
/// [`WikipediaNewsworthyWatcher::run_leader_step`] — the leader reads
/// back its own marker — so it is gossip-excluded and stays local.
pub const APP_ID_PORTAL: &str = "wikipedia-newsworthy:portal";

/// MeshStore namespace for the per-node tick-status snapshot. Single
/// key `last_tick` carrying [`TickStatusSnapshot`] JSON, overwritten
/// at the end of every tick. Read by `/internal/newsworthy/status`
/// (and the desktop Newsworthy chip) to give operators a real surface
/// for "is the watcher running, am I leader, what did the last tick
/// do?" — the watcher's whole point is invisible background work, so
/// without this snapshot users have no way to verify it.
///
/// Gossip-excluded, and it has to be: the key is the unsuffixed
/// `last_tick` for every node, so while this namespace replicated,
/// last-write-wins made whichever peer ticked most recently the one
/// whose `node_id_str` and `role_leader` your own status route
/// reported (cw-lift 2b).
pub const APP_ID_STATUS: &str = "wikipedia-newsworthy:status";
pub const STATUS_KEY_LAST_TICK: &str = "last_tick";

/// Persistent snapshot of the most recent watcher tick. Lives at
/// `(APP_ID_STATUS, STATUS_KEY_LAST_TICK)` in the host's KV store.
/// Stable JSON shape — the desktop chip reads this verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickStatusSnapshot {
    /// Unix seconds at which this tick completed.
    pub observed_at: i64,
    /// Display node id of the watcher that wrote the snapshot.
    pub node_id_str: String,
    /// Was this node leader on the tick we just ran?
    pub role_leader: bool,
    /// Tick reached the local-install gate cleanly (false when we
    /// skipped because the corpus isn't installed locally).
    pub corpus_installed: bool,
    /// Tracked-article count visible at tick end. Steady-state once
    /// the leader has populated the set from at least one portal page.
    pub tracked_total: usize,
    /// Articles this node owns under rendezvous hashing at tick end.
    pub owned_total: usize,
    /// Was a Portal:Current_events page ingested this tick (always
    /// false on followers; false on leader when the revid was
    /// unchanged from the last marker).
    pub portal_ingested: bool,
    /// Error count from the tick body.
    pub errors: usize,
    /// Tick wall-clock duration.
    pub elapsed_ms: u64,
    /// Configured interval between ticks. Lets the chip render
    /// "next tick in ~N min" without round-tripping config.
    pub tick_interval_secs: u64,
}

/// Adapter the watcher uses to reach mesh state without depending on
/// `commonwealth-state` directly. Sovereign-mesh provides the concrete
/// `MeshNewsworthyHost` impl backed by `MeshStore` + the discovery
/// membership snapshot + `commonwealth_core::partition::is_leader/is_owner`.
///
/// Mesh-state queries (`is_leader`, `is_owner_of`) are async because
/// the live membership lives behind `tokio::sync::RwLock` in the host
/// daemon — calling `blocking_read` from inside an async tick would
/// deadlock the runtime. KV operations are sync because `MeshStore`'s
/// SQLite calls are already blocking-friendly.
#[async_trait::async_trait]
pub trait NewsworthyHost: Send + Sync {
    /// Display label used in glassbox log lines. NOT a security identity.
    fn self_node_id_str(&self) -> String;

    /// True when this node is the deterministic leader for daily portal
    /// ingest. The watcher consults this once per tick — leadership
    /// flips immediately on membership change, which is by design.
    async fn is_leader(&self) -> bool;

    /// True when this node owns `partition_key` under rendezvous
    /// hashing. The watcher uses normalised article titles as keys.
    async fn is_owner_of(&self, partition_key: &str) -> bool;

    fn store_get(&self, app_id: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn store_set(&self, app_id: &str, key: &str, value: Vec<u8>) -> Result<()>;
    fn store_scan(&self, app_id: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>>;
    fn store_delete(&self, app_id: &str, key: &str) -> Result<bool>;

    /// Called by the watcher at the end of a tick that wrote chunks
    /// into one or more corpora. The host is expected to schedule a
    /// structural-atlas rebuild for each affected corpus so that
    /// atom-tier retrieval doesn't serve stale content from the
    /// pre-refresh state. The watcher fires this hook *detached* —
    /// implementations should spawn their own background task and
    /// return immediately rather than blocking the watcher's tick
    /// loop on a long atlas rebuild.
    ///
    /// `affected` carries `(corpus_id, role)` pairs. `role` is
    /// `"portal"` for the watcher's `corpus_id` (the wikipedia-
    /// newsworthy portal page sink) and `"refresh"` for the
    /// `parent_corpus_id` (the L5 wikipedia article-refresh sink).
    /// Hosts may dispatch differently per role — e.g. always rebuild
    /// the smaller portal-page atlas inline, defer the multi-million-
    /// chunk parent corpus rebuild to a low-priority queue.
    ///
    /// Default no-op so tests + minimal hosts don't have to wire
    /// the atlas pipeline. The production host
    /// (`sovereign-mesh::newsworthy_host::MeshNewsworthyHost`)
    /// implements this against
    /// `corpus_engine::enrichment::atlas::postinstall::rebuild_structural_atlas`.
    fn on_chunks_committed(&self, _affected: &[(String, &'static str)]) {}

    /// Move 6 P5: like `on_chunks_committed` but carries the
    /// list of doc_ids (article titles) that received writes in
    /// this tick, per (corpus_id, role) pair. This is the data
    /// flow that enables incremental atlas updates — the host's
    /// implementation can call
    /// [`crate::enrichment::atlas::atoms_delta::apply_atom_delta`]
    /// with the per-doc atom set rather than rebuilding the full
    /// atlas over millions of atoms unrelated to this tick's delta.
    ///
    /// Default impl strips the doc_ids and delegates to the
    /// existing `on_chunks_committed` so hosts that haven't been
    /// updated keep working with their full-rebuild path.
    fn on_chunks_committed_with_docs(&self, committed: &[CommittedDocs]) {
        let legacy: Vec<(String, &'static str)> = committed
            .iter()
            .map(|c| (c.corpus_id.clone(), c.role))
            .collect();
        self.on_chunks_committed(&legacy);
    }
}

/// Per-corpus delta record emitted by the watcher: which corpus
/// received writes, what role it played in this tick (`"portal"`
/// or `"refresh"`), and which source-doc ids carried the writes.
/// Consumed by [`NewsworthyHost::on_chunks_committed_with_docs`].
#[derive(Debug, Clone)]
pub struct CommittedDocs {
    pub corpus_id: String,
    pub role: &'static str,
    /// Article titles (or portal date strings) that received writes
    /// this tick. The host uses these to drive a per-doc incremental
    /// atlas update rather than a full rebuild.
    pub doc_ids: Vec<String>,
}

/// Lifecycle of a tracked article.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// First-seen, not yet fetched into the parent `wikipedia` corpus.
    PendingFetch,
    /// Present in `wikipedia` at `last_known_rev_id`. Eligible for
    /// daily revision checks.
    Present,
    /// Mid-refresh; another tick wrote this state and is now off
    /// awaiting MediaWiki. Acts as a soft mutex against double-fetch
    /// when partition assignment churns mid-tick.
    Refreshing,
    /// Fell out of the rolling window. No more daily attention; the
    /// underlying chunks remain in `wikipedia` until the parent recipe's
    /// monthly delta cleans them up.
    Stale,
    /// Fetch failed and exhausted retries. Manual intervention.
    Failed,
}

/// Persistable view of a tracked article. Stored as JSON in
/// `APP_ID_TRACKED` under key `tracked:<normalised_title>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedArticle {
    pub title: String,
    pub lifecycle: Lifecycle,
    pub last_known_rev_id: Option<i64>,
    /// Unix-seconds timestamp of the last revision check. `None` when
    /// the article is still `PendingFetch`.
    pub last_check_at: Option<i64>,
    pub first_seen_at: i64,
    /// Most recent tick that observed this title in a portal page.
    /// Drives window-based eviction.
    pub last_seen_in_signal_at: i64,
    /// Soft-delete handle. When `now > evict_after_secs`, the next
    /// leader tick flips lifecycle to `Stale`.
    pub evict_after_secs: i64,
    /// MediaWiki returned a redirect; the canonical title is `redirect_to`
    /// and chunks live under that in the parent `wikipedia` corpus.
    pub redirect_to: Option<String>,
}

/// Idempotency marker for the leader's daily portal-page ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalMarker {
    pub date_iso: String,
    pub last_fetched_revid: i64,
    pub fetched_at: i64,
}

/// Telemetry for one tick — published via `tracing::info!` and returned
/// from `tick()` so tests can assert on it.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub role_leader: bool,
    pub tracked_total: usize,
    pub owned_total: usize,
    pub fetched: usize,
    pub rev_checked: usize,
    pub refreshed: usize,
    pub stale_marked: usize,
    pub errors: usize,
    pub portal_ingested: bool,
    /// Move 6 P5: article titles that received writes this tick.
    /// Populated alongside `refreshed`/`fetched` counters; passed
    /// to the host's [`NewsworthyHost::on_chunks_committed_with_docs`]
    /// hook so the host can run an incremental atlas update over
    /// only these docs rather than rebuilding the full atlas.
    pub refreshed_titles: Vec<String>,
    pub fetched_titles: Vec<String>,
    /// Move 6 P5: portal date strings (`YYYY-MM-DD`) that received
    /// writes this tick. Used by the host for incremental atlas
    /// update of the wikipedia-newsworthy corpus.
    pub portal_doc_ids: Vec<String>,
    /// D5 (order audit-economy): corpus ids whose fragments this tick
    /// folded at tick end (`CorpusIndex::optimize`, non-destructive
    /// phases only). A corpus that received writes but is absent here
    /// means its fold FAILED (warned in the log) and its fragments wait
    /// for the hourly maintenance sweep.
    pub folded_corpora: Vec<String>,
    /// Issue #57 rec 4: how many per-article checkpoints actually parked
    /// for foreground inference this tick, and the wall clock they spent
    /// parked. Zero on an idle box. A tick that overlapped a user turn
    /// shows it here rather than only in the user's latency.
    pub yield_deferrals: usize,
    pub yield_deferred_ms: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct NewsworthyConfig {
    pub window_days: i64,
    pub tick_interval: Duration,
    pub jitter_max: Duration,
    pub fetch_concurrency: usize,
    /// `wikipedia-newsworthy` — where portal pages get indexed.
    pub corpus_id: String,
    /// `wikipedia` — where article refreshes land.
    pub parent_corpus_id: String,
    pub mediawiki_base_url: String,
    pub user_agent: String,
    /// How often a parked per-article checkpoint re-asks the yield hook.
    /// The ingest pipeline's own interval by default
    /// (`engine::yield_gate::YIELD_POLL_INTERVAL`); tests shorten it.
    pub yield_poll: Duration,
}

impl Default for NewsworthyConfig {
    fn default() -> Self {
        Self {
            window_days: 30,
            tick_interval: Duration::from_secs(24 * 3600),
            jitter_max: Duration::from_secs(15 * 60),
            yield_poll: yield_gate::YIELD_POLL_INTERVAL,
            // Drop to 1 in-flight request. Earlier 4-way concurrency
            // triggered HTTP 429 cascades from MediaWiki on cold-
            // start refresh waves (~48/57 articles rejected in
            // 2026-05-10 tests). MediaWiki's per-source limit for
            // anonymous + non-bot User-Agent traffic is roughly
            // one-per-second sustained; a single in-flight request
            // honoring `Retry-After` (see `HttpMediaWikiClient`)
            // sits comfortably under that.
            fetch_concurrency: 1,
            corpus_id: "wikipedia-newsworthy".to_string(),
            parent_corpus_id: "wikipedia".to_string(),
            mediawiki_base_url: "https://en.wikipedia.org/w/api.php".to_string(),
            // Wikipedia's User-Agent policy
            // (https://meta.wikimedia.org/wiki/User-Agent_policy) wants
            // an identifying string with a contact URL or email so
            // operators can be reached if a script misbehaves.
            // commonwealth-ai is open-source and unauthenticated, so
            // we point at the repo. Without this, our requests were
            // treated as generic browser traffic and rate-limited
            // more aggressively.
            user_agent: "commonwealth-ai-newsworthy/0.1 (https://github.com/alexsbryan/sovereign; \
                 ops@commonwealth.ai) reqwest/0.12"
                .to_string(),
        }
    }
}

/// MediaWiki API surface used by the watcher. Trait-bounded so tests
/// can inject a stub without spinning up a real HTTP server. The
/// production impl is [`HttpMediaWikiClient`].
#[async_trait::async_trait]
pub trait MediaWikiClient: Send + Sync {
    /// Fetch the JSON body of a `?action=parse&page=…` request.
    async fn fetch_parse(&self, page: &str) -> Result<String>;

    /// Batched `?action=query&prop=info&titles=A|B|C` returning the
    /// observed canonical title and latest revid for each input.
    async fn batch_revisions(&self, titles: &[String]) -> Result<Vec<RevisionRecord>>;
}

/// One row in the response to the batched revisions query.
#[derive(Debug, Clone)]
pub struct RevisionRecord {
    /// Title as we asked for it.
    pub requested_title: String,
    /// Title MediaWiki returned (after redirect normalisation).
    pub canonical_title: String,
    pub latest_revid: i64,
    pub redirected: bool,
}

/// Real HTTP-backed MediaWiki client used in production. Tests use a
/// stub instead so they don't depend on the network.
pub struct HttpMediaWikiClient {
    pub base_url: String,
    pub user_agent: String,
    pub http: reqwest::Client,
}

/// How long to wait before retrying when MediaWiki returns 429 and
/// doesn't supply a `Retry-After` header. Five seconds is comfortably
/// above the per-source rate-limit window for anonymous traffic, and
/// gives intermittent capacity issues time to clear without us spinning.
const DEFAULT_RETRY_AFTER_SECS: u64 = 5;

/// Maximum number of retries on a single MediaWiki request. Higher
/// counts pay diminishing returns — a 429 that won't clear in three
/// honoured-Retry-After cycles is almost certainly a sustained block
/// that needs operator attention, not patience.
const MAX_RETRY_ATTEMPTS: usize = 3;

/// Parse a `Retry-After` header value. MediaWiki always returns seconds
/// (per RFC 7231 §7.1.3 — HTTP-date variant is allowed but unused
/// here). Capped at 60s so a buggy upstream can't park us indefinitely.
fn parse_retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| secs.min(60))
}

/// Send a request through MediaWiki's API with rate-limit-aware retry.
/// On HTTP 429 the client honours `Retry-After` (or falls back to
/// `DEFAULT_RETRY_AFTER_SECS` with jitter) and retries up to
/// `MAX_RETRY_ATTEMPTS` times before propagating the error. Other
/// 4xx/5xx status codes return immediately without retry — they're
/// recipe / permission failures that won't clear by waiting.
async fn send_with_backoff(
    builder: reqwest::RequestBuilder,
    label: &str,
) -> Result<reqwest::Response> {
    let mut attempt: usize = 0;
    loop {
        // `try_clone()` keeps the template intact for the next
        // iteration — `send()` consumes the clone, never the
        // original builder. Returns None only if the request has
        // a streaming body, which our GET-only MediaWiki calls
        // never use.
        let req = builder.try_clone().ok_or_else(|| {
            Error::Extraction(format!("MediaWiki {label}: request builder not cloneable"))
        })?;
        let response = req
            .send()
            .await
            .map_err(|e| Error::Extraction(format!("MediaWiki {label}: {e}")))?;
        let status = response.status();
        if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(response);
        }
        attempt += 1;
        if attempt > MAX_RETRY_ATTEMPTS {
            return Err(Error::Extraction(format!(
                "MediaWiki {label}: 429 Too Many Requests after {MAX_RETRY_ATTEMPTS} retries"
            )));
        }
        let suggested = parse_retry_after_secs(response.headers());
        let base_secs = suggested.unwrap_or(DEFAULT_RETRY_AFTER_SECS);
        let jitter_ms = rand_jitter_ms(500);
        let sleep_for = Duration::from_secs(base_secs) + Duration::from_millis(jitter_ms);
        tracing::warn!(
            attempt,
            max_attempts = MAX_RETRY_ATTEMPTS,
            retry_after_secs = base_secs,
            jitter_ms,
            label,
            retry_after_header = suggested.is_some(),
            "newsworthy.mediawiki_rate_limited — backing off before retry"
        );
        tokio::time::sleep(sleep_for).await;
    }
}

#[async_trait::async_trait]
impl MediaWikiClient for HttpMediaWikiClient {
    async fn fetch_parse(&self, page: &str) -> Result<String> {
        let builder = self
            .http
            .get(&self.base_url)
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .query(&[
                ("action", "parse"),
                ("page", page),
                ("prop", "wikitext|sections|links|properties"),
                ("format", "json"),
                ("formatversion", "2"),
            ]);
        let label = format!("fetch_parse page={page}");
        let response = send_with_backoff(builder, &label).await?;
        if !response.status().is_success() {
            return Err(Error::Extraction(format!(
                "MediaWiki returned {} for page {page}",
                response.status()
            )));
        }
        // Decode as UTF-8 explicitly rather than relying on reqwest's
        // `.text()` charset detection. The MediaWiki Action API always
        // returns UTF-8 (Content-Type: application/json; charset=utf-8),
        // but a missing or quirky charset header has historically led
        // reqwest to fall back to a different decoder and double-encode
        // non-ASCII bytes (en-dash `–` U+2013 → mojibake `â\x80\x93`).
        // Forcing UTF-8 here removes that footgun for the whole portal
        // / refresh / fetch pipeline that goes through this client.
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Extraction(format!("MediaWiki body: {e}")))?;
        String::from_utf8(bytes.to_vec()).map_err(|e| {
            Error::Extraction(format!(
                "MediaWiki body for {page}: invalid UTF-8 at byte {}",
                e.utf8_error().valid_up_to()
            ))
        })
    }

    async fn batch_revisions(&self, titles: &[String]) -> Result<Vec<RevisionRecord>> {
        if titles.is_empty() {
            return Ok(Vec::new());
        }
        let joined = titles.join("|");
        let builder = self
            .http
            .get(&self.base_url)
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .query(&[
                ("action", "query"),
                ("prop", "info"),
                ("titles", joined.as_str()),
                ("redirects", "1"),
                ("format", "json"),
                ("formatversion", "2"),
            ]);
        let response = send_with_backoff(builder, "batch_revisions").await?;
        if !response.status().is_success() {
            return Err(Error::Extraction(format!(
                "MediaWiki batch returned {}",
                response.status()
            )));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Extraction(format!("MediaWiki batch json: {e}")))?;
        Ok(parse_batch_revisions(titles, &body))
    }
}

/// Parse a `?action=query&prop=info&titles=…` response into per-title
/// records. Lives outside the trait impl so tests can drive it
/// against a fixed JSON body.
pub fn parse_batch_revisions(
    requested: &[String],
    body: &serde_json::Value,
) -> Vec<RevisionRecord> {
    // MediaWiki's response shape:
    //   query.pages: [{ title, lastrevid, ... }]
    //   query.redirects: [{ from, to }]  (only on redirects=1)
    let pages = body
        .pointer("/query/pages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let redirects: std::collections::HashMap<String, String> = body
        .pointer("/query/redirects")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let from = r.get("from")?.as_str()?.to_string();
                    let to = r.get("to")?.as_str()?.to_string();
                    Some((from, to))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut by_canonical: std::collections::HashMap<String, (String, i64)> =
        std::collections::HashMap::new();
    for page in pages {
        let title = page
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let revid = page.get("lastrevid").and_then(|v| v.as_i64()).unwrap_or(0);
        if !title.is_empty() {
            by_canonical.insert(title.clone(), (title, revid));
        }
    }

    requested
        .iter()
        .map(|requested_title| {
            let canonical = redirects
                .get(requested_title)
                .cloned()
                .unwrap_or_else(|| requested_title.clone());
            let (canonical_resolved, revid) = by_canonical
                .get(&canonical)
                .cloned()
                .unwrap_or_else(|| (canonical.clone(), 0));
            RevisionRecord {
                requested_title: requested_title.clone(),
                canonical_title: canonical_resolved,
                latest_revid: revid,
                redirected: redirects.contains_key(requested_title),
            }
        })
        .collect()
}

pub struct WikipediaNewsworthyWatcher {
    host: Arc<dyn NewsworthyHost>,
    engine: Arc<CorpusEngine>,
    media_wiki: Arc<dyn MediaWikiClient>,
    config: NewsworthyConfig,
}

impl WikipediaNewsworthyWatcher {
    pub fn new(
        host: Arc<dyn NewsworthyHost>,
        engine: Arc<CorpusEngine>,
        media_wiki: Arc<dyn MediaWikiClient>,
        config: NewsworthyConfig,
    ) -> Self {
        Self {
            host,
            engine,
            media_wiki,
            config,
        }
    }

    /// Spawn the per-tick loop. Returns the join handle so the daemon's
    /// shutdown sequence can await graceful termination. Cancellation
    /// is signalled via `shutdown_rx` — the loop checks between ticks
    /// and exits on `true`.
    pub fn spawn(
        self: Arc<Self>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
        mut force_tick_rx: tokio::sync::mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // First-tick jitter: avoids a thundering-herd ingest when
            // every node in a fresh mesh boots within a few seconds of
            // each other. Production cadence is daily so the jitter
            // costs nothing operationally.
            let jitter = if self.config.jitter_max.is_zero() {
                Duration::from_secs(0)
            } else {
                let max = self.config.jitter_max.as_millis() as u64;
                Duration::from_millis(rand_jitter_ms(max))
            };
            tracing::info!(
                node = %self.host.self_node_id_str(),
                jitter_ms = jitter.as_millis() as u64,
                "newsworthy.watcher_starting"
            );
            tokio::select! {
                _ = tokio::time::sleep(jitter) => {}
                _ = shutdown_rx.changed() => return,
            }
            let mut interval = tokio::time::interval(self.config.tick_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let trigger = tokio::select! {
                    _ = interval.tick() => Some("interval"),
                    _ = force_tick_rx.recv() => Some("force"),
                    _ = shutdown_rx.changed() => {
                        tracing::info!(
                            node = %self.host.self_node_id_str(),
                            "newsworthy.watcher_shutdown"
                        );
                        return;
                    }
                };
                if let Some(trigger) = trigger {
                    if trigger == "force" {
                        tracing::info!(
                            node = %self.host.self_node_id_str(),
                            "newsworthy.tick_forced — operator-triggered via /internal/newsworthy/tick"
                        );
                    }
                    match self.tick(Utc::now(), trigger == "force").await {
                        Ok(report) => tracing::info!(
                            node = %self.host.self_node_id_str(),
                            trigger = trigger,
                            role = if report.role_leader { "leader" } else { "follower" },
                            tracked = report.tracked_total,
                            owned = report.owned_total,
                            fetched = report.fetched,
                            rev_checked = report.rev_checked,
                            refreshed = report.refreshed,
                            stale_marked = report.stale_marked,
                            errors = report.errors,
                            portal_ingested = report.portal_ingested,
                            yield_deferrals = report.yield_deferrals,
                            yield_deferred_ms = report.yield_deferred_ms,
                            elapsed_ms = report.elapsed_ms,
                            "newsworthy.tick",
                        ),
                        Err(e) => tracing::error!(
                            node = %self.host.self_node_id_str(),
                            error = %e,
                            "newsworthy.tick_failed",
                        ),
                    }
                }
            }
        })
    }

    /// One pass of the reconciliation loop. Public so tests can drive
    /// it without involving the spawn machinery. `force` marks an
    /// operator-triggered tick (`/internal/newsworthy/tick`), which
    /// proceeds even when the yield-hook reports foreground inference —
    /// an explicit request expresses intent to run NOW, and skipping it
    /// silently starves verification on busy machines.
    pub async fn tick(&self, now: DateTime<Utc>, force: bool) -> Result<TickReport> {
        // ── Foreground back-pressure ──────────────────────────────
        //
        // The newsworthy tick triggers per-article wikipedia API
        // fetches, reindex_by_source_doc_id, and (when any article
        // refreshes) an atlas_rebuild_dispatch. Atlas rebuilds stream
        // the full corpus through enrichment — 1.88M chunks on the
        // English Wikipedia, peaking at ~45 GB RSS. On a 64 GB Mac
        // with a 35B chat slot loaded, that crosses jetsam threshold
        // and SIGTERMs the daemon mid-request.
        //
        // The engine's `YieldHook` (set by the daemon) reports true
        // when a foreground inference request fired within the last
        // `yield_window_secs` window. Skipping the tick under that
        // condition delivers the design promise: background freshness
        // work runs on idle cycles, never in contention with user-
        // facing inference. The next interval tick will retry.
        if let Some(hook) = self.engine.yield_hook() {
            if hook.should_yield() {
                if force {
                    tracing::info!(
                        "newsworthy.tick_yield_bypassed — operator force-tick proceeds despite foreground inference"
                    );
                } else {
                    tracing::info!(
                        "newsworthy.tick_skipped — foreground inference active; yielding to user-facing work"
                    );
                    let mut report = TickReport::default();
                    report.role_leader = self.host.is_leader().await;
                    report.elapsed_ms = 0;
                    return Ok(report);
                }
            }
        }

        // ── Local-install gate ────────────────────────────────────
        //
        // The watcher spawns whenever a CorpusEngine handle is on the
        // daemon — independent of whether this node has actually
        // installed `wikipedia-newsworthy`. A non-installed node has
        // no on-disk index for `reindex_by_source_doc_id` to write to;
        // leader-elected ticks would error and follower ticks would
        // do nothing useful. Skip the tick entirely so the watcher
        // reports `not_installed` instead of running a no-op or
        // erroring on every interval.
        let installed = self.engine.installed_indexes().await.unwrap_or_default();
        let corpus_present = installed
            .iter()
            .any(|i| i.corpus_id == self.config.corpus_id && !i.is_shard);
        if !corpus_present {
            tracing::info!(
                corpus_id = %self.config.corpus_id,
                "newsworthy.tick_skipped_not_installed — this node has not installed the watcher's corpus; skipping until install lands"
            );
            let mut report = TickReport::default();
            report.role_leader = self.host.is_leader().await;
            report.elapsed_ms = 0;
            self.publish_status(&report, false, now);
            return Ok(report);
        }

        let started = std::time::Instant::now();
        let mut report = TickReport::default();
        report.role_leader = self.host.is_leader().await;

        // ── Step A: leader-only daily portal ingest ────────────────
        if report.role_leader {
            match self.run_leader_step(now).await {
                Ok(portal_date) => {
                    report.portal_ingested = portal_date.is_some();
                    if let Some(date_iso) = portal_date {
                        report.portal_doc_ids.push(date_iso);
                        report.stale_marked = self.sweep_window(now).await.unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "newsworthy.window_sweep_failed");
                            report.errors += 1;
                            0
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "newsworthy.leader_step_failed");
                    report.errors += 1;
                }
            }
        }

        // ── Step B: every-node partition-owned reconciliation ──────
        let tracked = self.load_tracked()?;
        report.tracked_total = tracked.len();

        let mut owned: Vec<TrackedArticle> = Vec::new();
        for t in tracked
            .into_iter()
            .filter(|t| t.lifecycle != Lifecycle::Stale)
        {
            if self.host.is_owner_of(&t.title).await {
                owned.push(t);
            }
        }
        report.owned_total = owned.len();

        // Intermediate publish so the desktop chip sees portal-ingest
        // results immediately, not after the (potentially multi-minute)
        // per-article fetch loop completes. Without this the operator
        // who clicked "Run tick now" thinks nothing happened — the
        // snapshot's `tracked_total` stays at the pre-tick value for
        // the full duration of the article fan-out. `elapsed_ms` here
        // is partial; the final publish at end-of-tick overwrites with
        // the complete number.
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        self.publish_status(&report, true, now);

        let (to_fetch, to_check): (Vec<_>, Vec<_>) = owned
            .into_iter()
            .partition(|t| matches!(t.lifecycle, Lifecycle::PendingFetch));

        // Bound concurrency so a 1500-article cold-start doesn't open
        // 1500 simultaneous reqwest connections.
        let sem = Arc::new(Semaphore::new(self.config.fetch_concurrency));

        for batch in to_check.chunks(50) {
            let titles: Vec<String> = batch.iter().map(|t| t.title.clone()).collect();
            match self.media_wiki.batch_revisions(&titles).await {
                Ok(revs) => {
                    report.rev_checked += revs.len();
                    for (article, rev) in batch.iter().zip(revs) {
                        if Some(rev.latest_revid) != article.last_known_rev_id {
                            self.yield_to_foreground(&mut report).await;
                            let _permit = sem.clone().acquire_owned().await.ok();
                            tracing::info!(
                                title = %article.title,
                                from_rev = ?article.last_known_rev_id,
                                to_rev = rev.latest_revid,
                                "newsworthy.refresh_attempt"
                            );
                            match catch_unwind_async(
                                self.refresh_article(article, &rev, now),
                                || article.title.clone(),
                                "newsworthy.refresh_panicked",
                            )
                            .await
                            {
                                Ok(Ok(())) => {
                                    report.refreshed += 1;
                                    report.refreshed_titles.push(article.title.clone());
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!(
                                        title = %article.title,
                                        error = %e,
                                        "newsworthy.refresh_failed"
                                    );
                                    report.errors += 1;
                                }
                                Err(()) => {
                                    // Already logged inside catch_unwind_async.
                                    report.errors += 1;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, batch_size = batch.len(), "newsworthy.batch_revcheck_failed");
                    report.errors += 1;
                }
            }
        }

        for article in to_fetch {
            self.yield_to_foreground(&mut report).await;
            let _permit = sem.clone().acquire_owned().await.ok();
            tracing::info!(
                title = %article.title,
                "newsworthy.initial_fetch_attempt"
            );
            match catch_unwind_async(
                self.fetch_article_initial(&article, now),
                || article.title.clone(),
                "newsworthy.initial_fetch_panicked",
            )
            .await
            {
                Ok(Ok(())) => {
                    report.fetched += 1;
                    report.fetched_titles.push(article.title.clone());
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        title = %article.title,
                        error = %e,
                        "newsworthy.initial_fetch_failed"
                    );
                    report.errors += 1;
                }
                Err(()) => {
                    // Already logged inside catch_unwind_async.
                    report.errors += 1;
                }
            }
        }

        report.elapsed_ms = started.elapsed().as_millis() as u64;

        // Notify the host that chunks landed in one or more corpora
        // so it can schedule an atlas rebuild (closes the structural
        // enrichment gap noted in
        // `project_newsworthy_atlas_enrichment_gap.md`). Two role
        // tags: "portal" for the wikipedia-newsworthy corpus_id
        // (small, fast to rebuild), "refresh" for the parent
        // wikipedia corpus (large; host may defer or throttle).
        // Move 6 P5: emit per-corpus delta records carrying the
        // doc_ids that received writes this tick. Host's
        // `on_chunks_committed_with_docs` defaults to delegating to
        // the legacy `on_chunks_committed` (which still triggers a
        // full rebuild); production hosts override to drive an
        // incremental atlas update via `apply_atom_delta` keyed by
        // these doc_ids.
        let mut committed: Vec<CommittedDocs> = Vec::new();
        if report.portal_ingested {
            committed.push(CommittedDocs {
                corpus_id: self.config.corpus_id.clone(),
                role: "portal",
                doc_ids: report.portal_doc_ids.clone(),
            });
        }
        if report.refreshed + report.fetched > 0 {
            let mut doc_ids = report.refreshed_titles.clone();
            doc_ids.extend(report.fetched_titles.clone());
            committed.push(CommittedDocs {
                corpus_id: self.config.parent_corpus_id.clone(),
                role: "refresh",
                doc_ids,
            });
        }
        if !committed.is_empty() {
            let summary: Vec<String> = committed
                .iter()
                .map(|c| format!("{}={} ({} docs)", c.role, c.corpus_id, c.doc_ids.len()))
                .collect();
            tracing::info!(
                refreshed = report.refreshed,
                fetched = report.fetched,
                portal_ingested = report.portal_ingested,
                affected = ?summary,
                "newsworthy.atlas_dispatch — notifying host with per-doc delta for incremental atlas update"
            );
            self.host.on_chunks_committed_with_docs(&committed);
        }

        // The write burst folds ITSELF (order audit-economy D5). A tick that
        // committed chunks leaves the corpus fragmented, and every hybrid
        // search flat-scans those fragments until they are folded. Measured
        // 2026-08-14: the initial-fetch burst after a daemon restart wrote
        // ~17K rows across ~170 commits into wikipedia; searches inflated
        // 394ms -> 1777ms within minutes (11-15.7s at peak under memory
        // pressure) and stayed inflated for ~53 min until the HOURLY
        // maintenance sweep folded them — which put the decay inside every
        // latency arm measured that day. Closure is a byproduct of the write
        // (the creation-closure loop): fold here, inline at tick end. The
        // "is there anything to fold" decider stays INSIDE
        // `CorpusIndex::optimize` — its index phase self-gates and reports
        // `skipped_as_clean` (ARCH §10.6, one decider; no second floor here).
        // Pruning is destructive and stays the maintenance sweep's decision:
        // `None` is passed deliberately.
        for c in &committed {
            // The fold rewrites index files under any reader (G4, 2026-09-02:
            // a claim search failed with "Unable to open file …_invert.lance"
            // while a fold ran). It stands aside for a user turn exactly as
            // each article does.
            self.yield_to_foreground(&mut report).await;
            // TRANSIENT: a per-tick fold over every corpus committed this
            // tick is a walker, and the caching wrapper would make each one
            // resident forever after a single tick.
            match self
                .engine
                .open_index_for_corpus_transient(&c.corpus_id)
                .await
            {
                Ok(idx) => match idx.optimize(None).await {
                    Ok(stats) => {
                        tracing::info!(
                            corpus_id = %c.corpus_id,
                            unindexed_before = stats.unindexed_rows_before,
                            fragments_removed = stats.fragments_removed,
                            fragments_added = stats.fragments_added,
                            indexes_optimized = stats.indexes_optimized,
                            skipped_as_clean = stats.skipped_as_clean,
                            "newsworthy.burst_folded — this tick's writes folded into the index"
                        );
                        report.folded_corpora.push(c.corpus_id.clone());
                    }
                    Err(e) => tracing::warn!(
                        corpus_id = %c.corpus_id,
                        error = %e,
                        "newsworthy.burst_fold_failed — corpus stays queryable but fragmented; the hourly sweep retries"
                    ),
                },
                Err(e) => tracing::warn!(
                    corpus_id = %c.corpus_id,
                    error = %e,
                    "newsworthy.burst_fold_failed — open_index_for_corpus"
                ),
            }
        }

        self.publish_status(&report, true, now);
        Ok(report)
    }

    /// Per-article yield checkpoint (issue #57 rec 4).
    ///
    /// The gate at the top of [`Self::tick`] answers "may this tick
    /// start"; a tick then runs for as long as its article list — 18
    /// minutes for 182 articles, measured 2026-09-02 — and a user turn
    /// arriving inside it used to contend with every remaining article's
    /// embed batch (one tokio mutex, held per whole batch) and index
    /// commit. This parks BEFORE each article and before the tick-end
    /// fold, in the same bounded wait
    /// the ingest pipeline uses before each embed batch
    /// (`engine::yield_gate`), so the cap, the poll and the three log
    /// lines are the ingest's, not a second copy (ARCH §10.6). It applies
    /// under a forced tick too: `force` expresses the operator's intent
    /// that the tick runs NOW, not that it may race a user who shows up
    /// mid-tick.
    async fn yield_to_foreground(&self, report: &mut TickReport) {
        let Some(hook) = self.engine.yield_hook() else {
            return;
        };
        let started = std::time::Instant::now();
        let exit = yield_gate::defer_to_foreground(
            hook.as_ref(),
            &self.config.parent_corpus_id,
            "article",
            DeferralBudget::new(),
            self.config.yield_poll,
            || false,
            || {},
        )
        .await;
        if exit != YieldExit::NotDeferred {
            report.yield_deferrals += 1;
            report.yield_deferred_ms += started.elapsed().as_millis() as u64;
        }
    }

    /// Persist a `TickStatusSnapshot` so the daemon's
    /// `/internal/newsworthy/status` route + desktop chip have a
    /// stable surface to read. Best-effort: a write failure logs but
    /// never aborts the tick.
    fn publish_status(&self, report: &TickReport, corpus_installed: bool, now: DateTime<Utc>) {
        let snapshot = TickStatusSnapshot {
            observed_at: now.timestamp(),
            node_id_str: self.host.self_node_id_str(),
            role_leader: report.role_leader,
            corpus_installed,
            tracked_total: report.tracked_total,
            owned_total: report.owned_total,
            portal_ingested: report.portal_ingested,
            errors: report.errors,
            elapsed_ms: report.elapsed_ms,
            tick_interval_secs: self.config.tick_interval.as_secs(),
        };
        match serde_json::to_vec(&snapshot) {
            Ok(bytes) => {
                if let Err(e) = self
                    .host
                    .store_set(APP_ID_STATUS, STATUS_KEY_LAST_TICK, bytes)
                {
                    tracing::warn!(
                        error = %e,
                        "newsworthy.status_publish_failed — status chip will lag until next tick succeeds"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "newsworthy.status_serialise_failed"),
        }
    }

    /// Leader-only daily portal-page ingest. Returns `true` when an
    /// ingest happened (revid changed since last fetch), `false` when
    /// the marker matched and we short-circuited.
    async fn run_leader_step(&self, now: DateTime<Utc>) -> Result<Option<String>> {
        let yesterday = (now - chrono::Duration::days(1)).date_naive();
        let date_iso = yesterday.format("%Y-%m-%d").to_string();
        let portal_page = format!("Portal:Current_events/{}", format_yyyy_month_dd(yesterday));

        let body = self.media_wiki.fetch_parse(&portal_page).await?;
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Error::Extraction(format!("portal JSON: {e}")))?;
        let observed_revid = parsed
            .pointer("/parse/revid")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let marker_key = format!("portal:{date_iso}");
        let prev: Option<PortalMarker> = self
            .host
            .store_get(APP_ID_PORTAL, &marker_key)?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if prev.as_ref().map(|m| m.last_fetched_revid) == Some(observed_revid) && observed_revid > 0
        {
            tracing::debug!(
                date = %date_iso,
                revid = observed_revid,
                "newsworthy.portal_unchanged"
            );
            return Ok(None);
        }

        // Index this portal page into wikipedia-newsworthy. The same
        // reindex_by_source_doc_id API works because each page is one
        // logical document keyed by date.
        let reindex_result = self
            .engine
            .reindex_by_source_doc_id(
                &self.config.corpus_id,
                &date_iso,
                &body,
                &ExtractorConfig::WikipediaApiArticle {},
                &ChunkerConfig::PortalEventBullet { max_chars: 2048 },
            )
            .await?;
        let chunks_written = match reindex_result {
            crate::engine::reindex::ReindexResult::Updated { chunks_written, .. } => chunks_written,
            _ => 0,
        };

        // Pull outbound links straight from the parsed JSON via the
        // chunker's wikilink helper — this avoids re-reading the index
        // we just wrote to.
        let links = collect_outbound_links_from_parsed(&parsed);
        let added = self.upsert_tracked(&links, now)?;

        // Glassbox guard: if we extracted links but committed zero
        // chunks the portal page reached MediaWiki but the
        // extractor/chunker pipeline lost the body. Surface as a warn
        // so it's visible in the daemon log + bench tooling; without
        // this the tick reports `portal_ingested=true` even when the
        // corpus stays empty, which is exactly the symptom that
        // surfaced on Portal:Current_events template-wrapped pages.
        if chunks_written == 0 && !links.is_empty() {
            tracing::warn!(
                date = %date_iso,
                revid = observed_revid,
                new_links = links.len(),
                "newsworthy.portal_link_chunk_mismatch — extracted links from portal JSON \
                 but reindex committed 0 chunks; the wikipedia-newsworthy corpus stays \
                 empty for this date even though tracked set advanced. Check the extractor's \
                 wikitext-template handling."
            );
        }

        // Only write the date marker when chunks actually landed. If
        // we marked the page as fetched at revid=N with 0 chunks, a
        // later tick with the same revid would short-circuit
        // (`observed_revid > 0 && prev.last_fetched_revid == observed_revid`)
        // and never get a chance to retry after the extractor is
        // fixed. Skipping the marker write on empty commits forces a
        // retry on every leader tick until at least one chunk lands.
        if chunks_written > 0 || observed_revid == 0 {
            let marker = PortalMarker {
                date_iso: date_iso.clone(),
                last_fetched_revid: observed_revid,
                fetched_at: now.timestamp(),
            };
            self.host.store_set(
                APP_ID_PORTAL,
                &marker_key,
                serde_json::to_vec(&marker)
                    .map_err(|e| Error::Extraction(format!("portal marker: {e}")))?,
            )?;
        }

        tracing::info!(
            date = %date_iso,
            revid = observed_revid,
            new_links = links.len(),
            new_tracked = added,
            chunks_written,
            "newsworthy.portal_ingested"
        );
        // Only report a portal as ingested when the chunks actually
        // landed. The leader_step's caller uses this to decide whether
        // to dispatch atlas rebuilds + window sweeps; a 0-chunk write
        // doesn't earn those.
        if chunks_written == 0 {
            Ok(None)
        } else {
            Ok(Some(date_iso))
        }
    }

    fn load_tracked(&self) -> Result<Vec<TrackedArticle>> {
        let entries = self.host.store_scan(APP_ID_TRACKED, "tracked:")?;
        let mut out = Vec::with_capacity(entries.len());
        for (_key, value) in entries {
            match serde_json::from_slice::<TrackedArticle>(&value) {
                Ok(t) => out.push(t),
                Err(e) => tracing::warn!(error = %e, "newsworthy.tracked_decode_failed"),
            }
        }
        Ok(out)
    }

    /// Insert any missing titles into the tracked-set store; bump
    /// `last_seen_in_signal_at` + `evict_after_secs` for titles that
    /// were already there. Returns the count of NEW tracked entries.
    fn upsert_tracked(&self, titles: &[String], now: DateTime<Utc>) -> Result<usize> {
        let now_secs = now.timestamp();
        let evict_after = now_secs + self.config.window_days * 86400;
        let mut added = 0;
        for title in titles {
            let normalised = normalise_title(title);
            let key = format!("tracked:{normalised}");
            let existing = self
                .host
                .store_get(APP_ID_TRACKED, &key)?
                .and_then(|b| serde_json::from_slice::<TrackedArticle>(&b).ok());
            let mut article = match existing {
                Some(mut t) => {
                    t.last_seen_in_signal_at = now_secs;
                    t.evict_after_secs = evict_after;
                    if t.lifecycle == Lifecycle::Stale {
                        // Window rejoin — flip back to PendingFetch so
                        // the next reconciliation pass re-fetches.
                        t.lifecycle = Lifecycle::PendingFetch;
                        tracing::info!(
                            title = %t.title,
                            from = "stale",
                            to = "pending_fetch",
                            "newsworthy.tracked_state_change"
                        );
                    }
                    t
                }
                None => {
                    added += 1;
                    TrackedArticle {
                        title: normalised.clone(),
                        lifecycle: Lifecycle::PendingFetch,
                        last_known_rev_id: None,
                        last_check_at: None,
                        first_seen_at: now_secs,
                        last_seen_in_signal_at: now_secs,
                        evict_after_secs: evict_after,
                        redirect_to: None,
                    }
                }
            };
            // Keep canonical-title invariant — we always write the
            // normalised form.
            article.title = normalised.clone();
            self.host.store_set(
                APP_ID_TRACKED,
                &key,
                serde_json::to_vec(&article)
                    .map_err(|e| Error::Extraction(format!("tracked encode: {e}")))?,
            )?;
        }
        Ok(added)
    }

    /// Mark expired tracked entries as `Stale`. Returns the count.
    /// Leader-only — the `evict_after_secs` field is set by the same
    /// leader on each portal ingest, so a single sweep suffices.
    async fn sweep_window(&self, now: DateTime<Utc>) -> Result<usize> {
        let now_secs = now.timestamp();
        let tracked = self.load_tracked()?;
        let mut marked = 0;
        for article in tracked {
            if article.lifecycle == Lifecycle::Stale {
                continue;
            }
            if article.evict_after_secs > 0 && now_secs > article.evict_after_secs {
                let mut updated = article.clone();
                updated.lifecycle = Lifecycle::Stale;
                let key = format!("tracked:{}", updated.title);
                self.host.store_set(
                    APP_ID_TRACKED,
                    &key,
                    serde_json::to_vec(&updated)
                        .map_err(|e| Error::Extraction(format!("stale encode: {e}")))?,
                )?;
                tracing::info!(
                    title = %updated.title,
                    from = ?article.lifecycle,
                    to = "stale",
                    "newsworthy.tracked_state_change",
                );
                marked += 1;
            }
        }
        Ok(marked)
    }

    async fn fetch_article_initial(
        &self,
        article: &TrackedArticle,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let body = self.media_wiki.fetch_parse(&article.title).await?;
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Error::Extraction(format!("article JSON: {e}")))?;
        let canonical = parsed
            .pointer("/parse/title")
            .and_then(|v| v.as_str())
            .map(normalise_title)
            .unwrap_or_else(|| article.title.clone());
        let revid = parsed
            .pointer("/parse/revid")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if canonical != article.title {
            tracing::warn!(
                requested = %article.title,
                canonical = %canonical,
                "newsworthy.redirect_observed"
            );
        }
        self.engine
            .reindex_by_source_doc_id(
                &self.config.parent_corpus_id,
                &canonical,
                &body,
                &ExtractorConfig::WikipediaApiArticle {},
                &ChunkerConfig::Paragraph {
                    max_chars: 1024,
                    overlap_chars: 128,
                },
            )
            .await?;

        let mut updated = article.clone();
        updated.lifecycle = Lifecycle::Present;
        updated.last_known_rev_id = Some(revid);
        updated.last_check_at = Some(now.timestamp());
        if canonical != article.title {
            updated.redirect_to = Some(canonical.clone());
        }
        let key = format!("tracked:{}", article.title);
        self.host.store_set(
            APP_ID_TRACKED,
            &key,
            serde_json::to_vec(&updated)
                .map_err(|e| Error::Extraction(format!("post-fetch encode: {e}")))?,
        )?;
        tracing::info!(
            title = %article.title,
            canonical = %canonical,
            revid,
            from = "pending_fetch",
            to = "present",
            "newsworthy.tracked_state_change",
        );
        Ok(())
    }

    async fn refresh_article(
        &self,
        article: &TrackedArticle,
        rev: &RevisionRecord,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let body = self.media_wiki.fetch_parse(&rev.canonical_title).await?;
        self.engine
            .reindex_by_source_doc_id(
                &self.config.parent_corpus_id,
                &rev.canonical_title,
                &body,
                &ExtractorConfig::WikipediaApiArticle {},
                &ChunkerConfig::Paragraph {
                    max_chars: 1024,
                    overlap_chars: 128,
                },
            )
            .await?;

        let mut updated = article.clone();
        updated.lifecycle = Lifecycle::Present;
        updated.last_known_rev_id = Some(rev.latest_revid);
        updated.last_check_at = Some(now.timestamp());
        if rev.redirected {
            updated.redirect_to = Some(rev.canonical_title.clone());
            tracing::warn!(
                requested = %rev.requested_title,
                canonical = %rev.canonical_title,
                "newsworthy.redirect_observed"
            );
        }
        let key = format!("tracked:{}", article.title);
        self.host.store_set(
            APP_ID_TRACKED,
            &key,
            serde_json::to_vec(&updated)
                .map_err(|e| Error::Extraction(format!("post-refresh encode: {e}")))?,
        )?;
        tracing::info!(
            title = %article.title,
            from_rev = ?article.last_known_rev_id,
            to_rev = rev.latest_revid,
            "newsworthy.refreshed",
        );
        Ok(())
    }
}

/// Normalise a MediaWiki article title: trim, collapse whitespace,
/// substitute `_` for spaces. The watcher uses the normalised form as
/// MeshStore key + rendezvous partition input + reindex source_doc_id
/// so the three never drift.
pub fn normalise_title(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join("_")
}

/// Format a date as `YYYY_Month_DD` (e.g. `2026_May_08`) — the
/// shape MediaWiki's portal subpage URLs expect.
pub fn format_yyyy_month_dd(date: NaiveDate) -> String {
    let month_name = match date.month() {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    };
    // Wikipedia's daily pages use an UNPADDED day ("2026 July 5") —
    // MediaWiki normalizes underscores to spaces but does NOT strip a
    // leading zero, so a padded "05" is a missingtitle. Verified live
    // 2026-07-06 (the padded form 404s; this bug meant no portal page
    // had ever ingested).
    format!("{}_{}_{}", date.year(), month_name, date.day())
}

/// Run an async future and catch panics, logging the panic message
/// along with caller-supplied identifying context (e.g. article
/// title). Returns:
/// - `Ok(value)` when the future completed without panicking.
/// - `Err(())` when the future panicked. The panic is logged via
///   `tracing::error!` with the supplied event name and context; the
///   caller is expected to count it as an error and move on. The
///   tokio runtime is NOT poisoned because the panic is absorbed
///   here.
///
/// Use this around per-article watcher steps (`refresh_article`,
/// `fetch_article_initial`) so that one bad article — for example a
/// MediaWiki revision whose section boundaries land mid-codepoint
/// (see `corpus-engine::extractors::wikipedia_api_article`) — can't
/// take down the daemon's HTTP listener with it.
pub(crate) async fn catch_unwind_async<F, T, C>(
    fut: F,
    context: C,
    event_name: &'static str,
) -> std::result::Result<T, ()>
where
    F: std::future::Future<Output = T>,
    C: FnOnce() -> String,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(v) => Ok(v),
        Err(panic) => {
            // Panic payloads are typically &str or String; fall back
            // to a generic marker for other types so we always emit
            // a non-empty message.
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            tracing::error!(
                context = %context(),
                panic_msg = %msg,
                "{event_name}"
            );
            Err(())
        }
    }
}

/// Walk a `?action=parse` JSON response and return the union of all
/// bullet-scoped wikilinks across every section's wikitext. The
/// chunker's `extract_bullet_links` does the per-bullet extraction;
/// this orchestrator stitches it across sections.
pub fn collect_outbound_links_from_parsed(parsed: &serde_json::Value) -> Vec<String> {
    let wikitext = parsed
        .pointer("/parse/wikitext")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut union: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in wikitext.lines() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with('*') || trimmed.starts_with("**")) {
            continue;
        }
        for link in extract_bullet_links(trimmed) {
            if seen.insert(link.clone()) {
                union.push(link);
            }
        }
    }
    union
}

pub(crate) fn rand_jitter_ms(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    // Avoid pulling in the `rand` crate for one tiny use site —
    // SystemTime nanos are random enough for thundering-herd jitter.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % max
}

#[allow(dead_code)]
fn parse_iso_date(date_iso: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(date_iso, "%Y-%m-%d").ok()
}

#[allow(dead_code)]
fn iso_to_utc_midnight(date_iso: &str) -> Option<DateTime<Utc>> {
    let date = parse_iso_date(date_iso)?;
    Utc.from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory `NewsworthyHost` that mimics `MeshStore`'s prefix-scan
    /// + LWW semantics without the SQLite backend. Two instances can
    /// share a `Mutex<HashMap>` via `Arc` to simulate gossip
    /// convergence in two-node tests.
    pub(super) struct StubHost {
        node_label: String,
        is_leader: bool,
        owned_keys: std::collections::HashSet<String>,
        store: Arc<Mutex<HashMap<(String, String), Vec<u8>>>>,
    }

    impl StubHost {
        pub(super) fn new(label: &str, is_leader: bool) -> Self {
            Self {
                node_label: label.to_string(),
                is_leader,
                owned_keys: std::collections::HashSet::new(),
                store: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        pub(super) fn own_all(mut self) -> Self {
            // Convenience for single-node tests.
            self.owned_keys.clear();
            self
        }
    }

    #[async_trait::async_trait]
    impl NewsworthyHost for StubHost {
        fn self_node_id_str(&self) -> String {
            self.node_label.clone()
        }
        async fn is_leader(&self) -> bool {
            self.is_leader
        }
        async fn is_owner_of(&self, key: &str) -> bool {
            self.owned_keys.is_empty() || self.owned_keys.contains(key)
        }
        fn store_get(&self, app_id: &str, key: &str) -> Result<Option<Vec<u8>>> {
            Ok(self
                .store
                .lock()
                .unwrap()
                .get(&(app_id.to_string(), key.to_string()))
                .cloned())
        }
        fn store_set(&self, app_id: &str, key: &str, value: Vec<u8>) -> Result<()> {
            self.store
                .lock()
                .unwrap()
                .insert((app_id.to_string(), key.to_string()), value);
            Ok(())
        }
        fn store_scan(&self, app_id: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
            let store = self.store.lock().unwrap();
            let mut out: Vec<(String, Vec<u8>)> = store
                .iter()
                .filter(|((a, k), _)| a == app_id && k.starts_with(prefix))
                .map(|((_, k), v)| (k.clone(), v.clone()))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(out)
        }
        fn store_delete(&self, app_id: &str, key: &str) -> Result<bool> {
            let mut store = self.store.lock().unwrap();
            Ok(store
                .remove(&(app_id.to_string(), key.to_string()))
                .is_some())
        }
    }

    #[test]
    fn normalise_title_collapses_whitespace_and_substitutes_underscore() {
        assert_eq!(normalise_title("Donald Trump"), "Donald_Trump");
        assert_eq!(normalise_title("  Trailing  spaces  "), "Trailing_spaces");
        assert_eq!(
            normalise_title("Already_Underscored"),
            "Already_Underscored"
        );
    }

    #[test]
    fn format_yyyy_month_dd_renders_unpadded_day() {
        // Unpadded day is load-bearing: "2026_May_08" is a missingtitle
        // on en.wikipedia; "2026_May_8" resolves.
        let date = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
        assert_eq!(format_yyyy_month_dd(date), "2026_May_8");
    }

    #[test]
    fn collect_outbound_links_unions_per_section_bullets() {
        let parsed: serde_json::Value = serde_json::json!({
            "parse": {
                "title": "Portal:Current_events/2026_May_08",
                "revid": 12345,
                "wikitext": "; Armed conflicts and attacks\n\
                             * Russian invasion of Ukraine: At least 12 killed in [[Kyiv]].\n\
                             ** Forces near [[Kupiansk]].\n\
                             * Yemeni civil war: Statement from [[Houthi movement]].\n\
                             ; Politics and elections\n\
                             * [[2026 Australian federal election]]: Update.\n",
            }
        });
        let links = collect_outbound_links_from_parsed(&parsed);
        assert!(links.contains(&"Kyiv".to_string()));
        assert!(links.contains(&"Kupiansk".to_string()));
        assert!(links.contains(&"Houthi_movement".to_string()));
        assert!(links.contains(&"2026_Australian_federal_election".to_string()));
        // Dedupes — each title appears once.
        let unique: std::collections::HashSet<_> = links.iter().collect();
        assert_eq!(unique.len(), links.len());
    }

    #[test]
    fn parse_batch_revisions_resolves_redirects() {
        let body: serde_json::Value = serde_json::json!({
            "query": {
                "redirects": [{ "from": "Donald J. Trump", "to": "Donald Trump" }],
                "pages": [
                    { "title": "Donald Trump", "lastrevid": 1297048221 },
                    { "title": "Joe Biden", "lastrevid": 1300000000 }
                ]
            }
        });
        let requested = vec!["Donald J. Trump".to_string(), "Joe Biden".to_string()];
        let revs = parse_batch_revisions(&requested, &body);
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].requested_title, "Donald J. Trump");
        assert_eq!(revs[0].canonical_title, "Donald Trump");
        assert_eq!(revs[0].latest_revid, 1297048221);
        assert!(revs[0].redirected);
        assert_eq!(revs[1].canonical_title, "Joe Biden");
        assert!(!revs[1].redirected);
    }

    #[test]
    fn parse_batch_revisions_returns_zero_revid_for_unknown_title() {
        let body: serde_json::Value = serde_json::json!({
            "query": { "pages": [] }
        });
        let revs = parse_batch_revisions(&["Nonexistent".to_string()], &body);
        assert_eq!(revs.len(), 1);
        assert_eq!(revs[0].latest_revid, 0);
    }

    #[test]
    fn upsert_tracked_inserts_then_bumps_seen_at() {
        // We don't need a real engine for this — a stand-in. Build a
        // watcher just to call upsert_tracked.
        let host: Arc<dyn NewsworthyHost> = Arc::new(StubHost::new("self", true).own_all());
        let dummy_engine: Arc<CorpusEngine> = make_dummy_engine();
        let watcher = WikipediaNewsworthyWatcher::new(
            host.clone(),
            dummy_engine,
            Arc::new(NoopMediaWikiClient),
            NewsworthyConfig::default(),
        );

        let now = Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap();
        let added = watcher
            .upsert_tracked(&["Kyiv".to_string(), "Donald Trump".to_string()], now)
            .unwrap();
        assert_eq!(added, 2);

        // Second call with the same titles — no new additions; the
        // existing entries get last_seen_in_signal_at bumped.
        let later = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
        let added2 = watcher
            .upsert_tracked(&["Kyiv".to_string()], later)
            .unwrap();
        assert_eq!(added2, 0);

        // Read back and check the bump took effect.
        let tracked = watcher.load_tracked().unwrap();
        assert_eq!(tracked.len(), 2);
        let kyiv = tracked.iter().find(|t| t.title == "Kyiv").unwrap();
        assert_eq!(kyiv.last_seen_in_signal_at, later.timestamp());
        assert_eq!(kyiv.lifecycle, Lifecycle::PendingFetch);
    }

    #[test]
    fn sweep_window_marks_only_expired_entries_stale() {
        let host: Arc<dyn NewsworthyHost> = Arc::new(StubHost::new("self", true).own_all());
        let dummy_engine: Arc<CorpusEngine> = make_dummy_engine();
        let watcher = WikipediaNewsworthyWatcher::new(
            host.clone(),
            dummy_engine,
            Arc::new(NoopMediaWikiClient),
            NewsworthyConfig {
                window_days: 1,
                ..NewsworthyConfig::default()
            },
        );

        // Seed two tracked rows with different evict_after windows.
        let fresh = TrackedArticle {
            title: "Fresh".into(),
            lifecycle: Lifecycle::Present,
            last_known_rev_id: Some(100),
            last_check_at: Some(0),
            first_seen_at: 1_000,
            last_seen_in_signal_at: 1_000,
            evict_after_secs: 9_999_999_999, // far future
            redirect_to: None,
        };
        let stale_due = TrackedArticle {
            title: "Stale_Due".into(),
            lifecycle: Lifecycle::Present,
            last_known_rev_id: Some(50),
            last_check_at: Some(0),
            first_seen_at: 1_000,
            last_seen_in_signal_at: 1_000,
            evict_after_secs: 1_000_000, // already past
            redirect_to: None,
        };
        host.store_set(
            APP_ID_TRACKED,
            "tracked:Fresh",
            serde_json::to_vec(&fresh).unwrap(),
        )
        .unwrap();
        host.store_set(
            APP_ID_TRACKED,
            "tracked:Stale_Due",
            serde_json::to_vec(&stale_due).unwrap(),
        )
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap();
        let marked = rt.block_on(watcher.sweep_window(now)).unwrap();
        assert_eq!(marked, 1);

        let after = watcher.load_tracked().unwrap();
        let fresh_after = after.iter().find(|t| t.title == "Fresh").unwrap();
        let stale_after = after.iter().find(|t| t.title == "Stale_Due").unwrap();
        assert_eq!(fresh_after.lifecycle, Lifecycle::Present);
        assert_eq!(stale_after.lifecycle, Lifecycle::Stale);
    }

    /// MediaWiki client that errors on every call. Lets us instantiate
    /// a watcher for tests that don't drive the HTTP path.
    pub(super) struct NoopMediaWikiClient;
    #[async_trait::async_trait]
    impl MediaWikiClient for NoopMediaWikiClient {
        async fn fetch_parse(&self, _page: &str) -> Result<String> {
            Err(Error::Extraction("no-op stub".into()))
        }
        async fn batch_revisions(&self, _titles: &[String]) -> Result<Vec<RevisionRecord>> {
            Err(Error::Extraction("no-op stub".into()))
        }
    }

    fn make_dummy_engine() -> Arc<CorpusEngine> {
        let dir = tempfile::tempdir().unwrap();
        let recipes_dir = dir.path().join("recipes");
        let index_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        std::fs::create_dir_all(&index_dir).unwrap();
        let embed: crate::types::EmbedFn =
            Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
        // Leak the tempdir so the Arc<Engine> can outlive this fn —
        // it's a unit test, the OS reclaims on process exit.
        std::mem::forget(dir);
        Arc::new(CorpusEngine::new(recipes_dir, index_dir, embed))
    }

    /// Stub yield hook used by the foreground-back-pressure test.
    /// Returns the configured `should_yield` value verbatim.
    struct StubYieldHook {
        yield_now: bool,
    }
    impl crate::YieldHook for StubYieldHook {
        fn should_yield(&self) -> bool {
            self.yield_now
        }
        fn throttle_factor(&self) -> f32 {
            1.0
        }
    }

    #[tokio::test]
    async fn tick_skips_when_yield_hook_active() {
        // When the engine's yield hook reports `should_yield == true`,
        // the newsworthy tick must return immediately without
        // contacting MediaWiki or scanning tracked articles. This is
        // the back-pressure rule: background freshness work yields
        // to foreground inference.
        let host: Arc<dyn NewsworthyHost> = Arc::new(StubHost::new("self", true).own_all());
        let engine = make_dummy_engine();
        engine.set_yield_hook(Arc::new(StubYieldHook { yield_now: true }));
        let watcher = WikipediaNewsworthyWatcher::new(
            host.clone(),
            engine,
            Arc::new(NoopMediaWikiClient),
            NewsworthyConfig::default(),
        );
        // NoopMediaWikiClient errors on every call — if the tick
        // reaches the batch-revisions step the test will fail with a
        // non-empty `errors` count. Yield-skip path must short-circuit
        // before any media client touch.
        let report = watcher
            .tick(Utc::now(), false)
            .await
            .expect("tick must succeed");
        assert_eq!(report.tracked_total, 0);
        assert_eq!(report.owned_total, 0);
        assert_eq!(report.errors, 0);
        assert_eq!(report.refreshed, 0);
    }

    #[tokio::test]
    async fn force_tick_bypasses_active_yield_hook() {
        // Same active-hook setup as `tick_skips_when_yield_hook_active`,
        // but `force = true` must proceed INTO the tick body. Proof of
        // passage: the yield-skip path returns BEFORE any status
        // publish, while the very next gate (local-install, which fires
        // here because the dummy engine has no corpus) publishes a
        // status row into the host KV. A non-empty stub store therefore
        // witnesses that the yield gate was bypassed.
        let stub = StubHost::new("self", true).own_all();
        let store = stub.store.clone();
        let host: Arc<dyn NewsworthyHost> = Arc::new(stub);
        let engine = make_dummy_engine();
        engine.set_yield_hook(Arc::new(StubYieldHook { yield_now: true }));
        let watcher = WikipediaNewsworthyWatcher::new(
            host.clone(),
            engine,
            Arc::new(NoopMediaWikiClient),
            NewsworthyConfig::default(),
        );
        let report = watcher
            .tick(Utc::now(), true)
            .await
            .expect("forced tick must succeed");
        assert_eq!(report.errors, 0);
        assert!(
            !store.lock().unwrap().is_empty(),
            "forced tick must get past the yield gate (status row published)"
        );
    }

    #[tokio::test]
    async fn tick_proceeds_when_yield_hook_idle() {
        // Inverse: yield hook says "no foreground activity"; the tick
        // proceeds through its normal path. With no tracked articles
        // and a leader=true stub, we expect tracked_total=0 and no
        // errors (leader step exits without portal data).
        let host: Arc<dyn NewsworthyHost> = Arc::new(StubHost::new("self", false).own_all());
        let engine = make_dummy_engine();
        engine.set_yield_hook(Arc::new(StubYieldHook { yield_now: false }));
        let watcher = WikipediaNewsworthyWatcher::new(
            host.clone(),
            engine,
            Arc::new(NoopMediaWikiClient),
            NewsworthyConfig::default(),
        );
        let report = watcher
            .tick(Utc::now(), false)
            .await
            .expect("tick must succeed");
        // Tick ran the tracked-load path (returned 0). Distinguishes
        // from the yield-skip case which short-circuits before
        // load_tracked.
        assert_eq!(report.tracked_total, 0);
        assert!(report.elapsed_ms < 5_000);
    }
}

#[cfg(test)]
mod yield_tests;
