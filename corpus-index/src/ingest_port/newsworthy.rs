// SPDX-License-Identifier: AGPL-3.0-or-later
//! The newsworthy watcher's host port and status vocabulary, spoken by the
//! watcher (ingest) and by the host that implements it (svrn's daemon). Moved
//! from corpus-engine's `update::newsworthy_watcher`, which re-exports them
//! (pb-ingest-dial-daemon-ports).

use serde::{Deserialize, Serialize};

use crate::Result;

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
pub const APP_ID_STATUS: &str = "wikipedia-newsworthy-status";
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
/// membership snapshot + `kernel_types::partition::is_leader/is_owner`.
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
    /// (`sovereign-daemon::newsworthy_host::MeshNewsworthyHost`)
    /// implements this against
    /// `corpus_engine::enrichment::atlas::postinstall::rebuild_structural_atlas`.
    fn on_chunks_committed(&self, _affected: &[(String, &'static str)]) {}

    /// Move 6 P5: like `on_chunks_committed` but carries the
    /// list of doc_ids (article titles) that received writes in
    /// this tick, per (corpus_id, role) pair. This is the data
    /// flow that enables incremental atlas updates — the host's
    /// implementation can call
    /// corpus-engine's `enrichment::atlas::atoms_delta::apply_atom_delta`
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

/// MeshStore namespace for tracked-article rows. Keyed by
/// `tracked:<title>` with the title normalised (spaces → underscores).
///
/// **Colon-free since cw-lift 4, and it has to be.** This namespace is the
/// one on this list that REPLICATES (the `:status` and `:portal` siblings are
/// gossip-excluded), and a replicating `app_id` is now used verbatim as a ring
/// namespace — which names a DIRECTORY, `<root>/rings/<ns>/`. `:` is not a
/// legal path component on NTFS, and the desktop ships on Windows
/// (`scripts/build-desktop-windows.sh`) linking `sovereign-mesh` and through
/// it `commonwealth-rail`. The alternative was widening the rail's
/// `valid_namespace` charset for every future namespace to keep one spelling
/// here; renaming ONE constant is the cheaper decider to change, and a mapping
/// table would have been two names for one thing (ARCH §10.6). Every reader
/// goes through this constant, so the value is the only thing that moved.
///
/// The rows written under the old spelling are orphaned rather than migrated:
/// `run_leader_step` re-derives a `TrackedArticle` from the next daily portal
/// page, so the cost is one tick of `first_seen_at`, and in the shipped daemon
/// `MeshStore` is `in_memory()` and loses them on every restart anyway.
pub const APP_ID_TRACKED: &str = "wikipedia-newsworthy-tracked";

/// KV namespace for daily portal-page idempotency markers. Keyed by
/// `portal:<YYYY-MM-DD>`. Written and read only inside
/// the watcher's `run_leader_step` — the leader reads
/// back its own marker — so it is gossip-excluded and stays local.
pub const APP_ID_PORTAL: &str = "wikipedia-newsworthy-portal";
