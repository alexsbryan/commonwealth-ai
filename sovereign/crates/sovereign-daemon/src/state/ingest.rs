//! Collaborative ingest's part of the node's state — the active-ingest set,
//! per-corpus progress, the work queue and its grants, the peer pull loops and
//! their verification reports, the quiesce flag and the throttle dial, and the
//! newsworthy tick handle.
//!
//! DC §4.2 assigns these nine to collaborative ingest, whose home is `jobs`
//! (DC §3.2, in `sovereign-daemon`), which `sovereign-api` may not name
//! (`[[forbid]] sovereign-api -> sovereign-*`). Until that move this part is
//! scaffolding carried on `AppStateInner`; the route shells read it directly
//! rather than through delegating accessors.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use commonwealth_core::ids::HandoffId;
use sovereign_grants::{EphemeralGrantStore, VerifyReport, WorkQueueManager};
use tokio::sync::RwLock;

/// Collaborative ingest's nine fields, held as `AppStateInner::ingest`.
pub struct IngestPart {
    /// Corpus IDs currently being actively ingested on this node.
    /// Prevents the auto-collaborate loop from firing a second
    /// `collaborate` call while a live ingest task is writing chunks.
    pub active_ingests: RwLock<HashSet<String>>,
    /// Latest `IngestProgress` observed for each active corpus.
    /// Populated by the daemon-side ingest spawn's progress callback
    /// so the Desktop UI can poll `GET /internal/corpus/progress`
    /// instead of taking a Tauri-event-only path that dies when the
    /// app closes mid-ingest. Entries are retained until either a
    /// terminal phase (`Complete`) overwrites them or an explicit
    /// cancel wipes the corpus.
    pub corpus_progress: RwLock<HashMap<String, corpus_engine::IngestProgress>>,
    /// Operator-triggered tick channel for the `wikipedia-newsworthy`
    /// freshness watcher. Installed by the embedded daemon when (and
    /// only when) the watcher spawns; `None` in tests and on daemons
    /// without a corpus engine. The `POST /internal/newsworthy/tick`
    /// route grabs this sender to fire one tick on demand, bypassing
    /// the 24h interval — the only path operators have to recover
    /// from a stale snapshot or kick off the first portal ingest
    /// after becoming leader.
    pub newsworthy_force_tick: RwLock<Option<tokio::sync::mpsc::Sender<()>>>,
    /// Pull-based corpus ingestion work queues keyed by `HandoffId`.
    /// The coordinator's `corpus_collaborate` handler populates this with
    /// a unit list; peers pull units via `POST /internal/corpus/next_unit`.
    /// Only coordinators hold entries here — peer nodes never mutate it.
    /// See `commonwealth-knowledge::work_queue` for the full design.
    pub work_queue: Arc<WorkQueueManager>,
    /// Ephemeral, renewable ingest grants — the out-of-band capability that
    /// authorizes a one-off peer-assisted ingest of an otherwise local-only
    /// corpus. Consulted at the `corpus_collaborate` kickoff gate; never
    /// persisted, never mutates on-disk corpus metadata (so the corpus's
    /// standing `mesh_sharing = false` posture is preserved throughout).
    /// See `commonwealth-knowledge::ingest_grant`.
    pub grant_store: Arc<EphemeralGrantStore>,
    /// Handoff IDs for which this node is currently running a pull loop
    /// (as a peer). Prevents `auto_ingest` from spawning duplicate pull
    /// loops when the same open handoff is seen across multiple gossip ticks.
    pub active_pull_loops: RwLock<HashSet<HandoffId>>,
    /// Post-merge verification spot-check reports, keyed by handoff. The merge
    /// coordinator writes one after re-embedding a sample of the merged corpus
    /// locally; the collaborate-status endpoint surfaces it for the desktop's
    /// glassbox "re-checked N chunks — all matched" line.
    pub verify_reports: RwLock<HashMap<HandoffId, VerifyReport>>,
    /// Mesh quiesce flag. When `true`, the auto-collaborate loop
    /// (`sovereign-daemon::auto_ingest`) skips peer-pull discovery and
    /// dispatch on every tick — this node neither pulls work assigned
    /// by other coordinators nor dispatches its own queue to peers.
    /// Initial value is set from the `SOVEREIGN_DISABLE_AUTO_COLLAB`
    /// env var at boot (preserves the existing operator escape hatch);
    /// `POST /internal/mesh/quiesce` flips it at runtime without
    /// requiring a daemon restart. Reads on the hot path are a single
    /// relaxed atomic load.
    pub mesh_quiesced: std::sync::atomic::AtomicBool,
    /// Per-batch ingest throttle. Encoded as fixed-point ‰ (parts
    /// per thousand) so we can represent fractional levels without
    /// floats. `1000` = full speed (no post-batch sleep — the legacy
    /// behaviour and the default). `500` = duty-cycle 50% (sleep
    /// after each batch equal to the batch's wall time, halving
    /// effective throughput while leaving the GPU/CPU unblocked
    /// in between). `0` is rejected by the setter — use the pause
    /// route to fully stop a corpus.
    pub ingest_throttle_milli: std::sync::atomic::AtomicU32,
}
