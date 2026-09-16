//! The node's part of the daemon's state — the client token, the guest
//! grants, the process start instant, the corpus-engine handle, the
//! foreground-yield signal (last active, window, in-flight), the storage
//! budget and usage, and the activity emitter.
//!
//! DC §4.2 assigns these ten to the node, whose home is `sovereign-daemon`,
//! which `sovereign-api` may not name (`[[forbid]] sovereign-api ->
//! sovereign-*`). Until that move this part is scaffolding carried on
//! `AppStateInner`; the route shells read it directly rather than through
//! delegating accessors.

use std::sync::Arc;

use commonwealth_state::ActivityEmitter;
use corpus_engine::CorpusEngine;
use sovereign_grants::GuestGrantStore;

/// The node's ten fields, held as `AppStateInner::node`.
pub struct NodePart {
    /// Bearer token required of non-loopback callers on the client API
    /// (`:9741`). `None` (the default) means "no token configured" —
    /// the [`crate::client_auth`] layer then admits ONLY loopback
    /// callers and fails closed for any remote one. The embedded daemon
    /// installs `Some(_)` via [`crate::state::AppState::install_client_token`]
    /// at startup when it binds a routable (non-loopback) address. Stored
    /// in cleartext: the layer compares it byte-for-byte against the
    /// incoming `Authorization: Bearer`. Set-once-read-many.
    pub client_token: std::sync::RwLock<Option<Arc<str>>>,
    /// The in-process corpus engine, when this daemon hosts one. `None` on a
    /// daemon with no data directory (the knowledge routes then behave as if
    /// this node hosts no corpora).
    pub corpus_engine: Option<Arc<CorpusEngine>>,
    /// Process start instant — drives `/status`'s `process.uptime_seconds`
    /// (an uptime reset is the cheap witness that a supervised restart
    /// actually produced a fresh process).
    pub started_at: std::time::Instant,
    /// Ephemeral guest grants — short-lived bearers that are NOT mesh
    /// membership. Consulted at exactly one point, `client_auth_layer`, which
    /// asks the grant whether it permits the request's path and never inspects
    /// a `Scope` variant itself. Never persisted, never gossiped, and never
    /// touches `Mesh` — a guest is not a member and cannot become one.
    /// See `commonwealth-knowledge::guest_grant`.
    pub guest_grants: Arc<GuestGrantStore>,
    /// Unix-seconds timestamp of the last foreground inference request
    /// observed at `chat_completions`. `0` means "never touched" — the
    /// initial state at boot. Bumped via
    /// [`crate::state::AppState::bump_foreground_active`] and read by the
    /// corpus-engine `YieldHook` impl to decide whether background ingest
    /// workers should pause before the next embed batch / enrichment phase.
    /// Plain atomic — no lock contention on the hot read path.
    pub foreground_last_active_ts: std::sync::atomic::AtomicI64,
    /// Yield window in seconds. While `now - last_active < window`, the
    /// daemon's `YieldHook` returns `should_yield = true`. `0` disables
    /// the feature (tests, hosts that never want background work to
    /// pause). Configured via `~/.config/sovereign/config.toml`'s
    /// `daemon.yield_to_foreground_secs` and stuffed in here at
    /// startup; the desktop Settings tab can rewrite it at runtime
    /// without a daemon restart.
    pub yield_window_secs: std::sync::atomic::AtomicU64,
    /// Turns in flight right now. A turn holds a `ForegroundLease` on the
    /// corpus engine for its whole life, so the yield hook stays true for
    /// the entire turn regardless of the window; the window only governs
    /// the quiet after the last turn ends.
    pub foreground_inflight: std::sync::atomic::AtomicUsize,
    /// User-set ceiling on how much disk Sovereign is allowed to use
    /// for corpus storage (sum of `~/.svrnmesh/indexes/*`). Encoded
    /// as bytes; `0` is the sentinel for "no budget — use whatever
    /// disk says is free". The desktop Settings panel writes this at
    /// boot (computed from free disk on first launch, then persisted
    /// in `desktop.toml`) and via `POST /internal/storage/budget`.
    ///
    /// The enforcement point is `sovereign-mesh::capabilities::
    /// build_local_capabilities`, which clamps the gossiped
    /// `free_storage_gb` (both the static `HardwareProfile` field and
    /// the live `AvailableResources` reading) to
    /// `min(actual_free, max(0, budget − used))`. The live planner
    /// (the three `knowledge_assignment::plan_collaborative_ingestion*`
    /// variants) reads that one value to decide what to assign here
    /// (a peer at 0 is skipped outright), so clamping it
    /// at the publish boundary makes the budget self-enforcing
    /// across the whole mesh — peers won't push us shards that
    /// would breach the budget, and our own local install path
    /// already gates on the same number.
    pub storage_budget_bytes: std::sync::atomic::AtomicU64,
    /// Most recent observation of how much of the budget the corpus
    /// engine is currently using on disk. Updated each gossip tick
    /// from `CorpusEngine::installed_indexes()` (already walked once
    /// per tick to publish `hosted_corpora`, so no extra IO). Read
    /// by `GET /internal/storage/budget` to drive the desktop's
    /// "X of Y GB used" indicator without forcing the UI to re-walk
    /// the index directory.
    pub storage_used_bytes: std::sync::atomic::AtomicU64,
    /// Local Activity ledger emitter. Records this daemon's own
    /// resource work — tokens served to local clients, embeddings
    /// produced, chunks ingested/enriched, newsworthy fetches — in
    /// Sovereign's vocabulary, for the glassbox "Activity & Sharing"
    /// surface. Unlike `contribution_emitter`, its records are
    /// **local-only and never gossip** (written under the
    /// `activity-private` namespace). Cheap to clone; shares the same
    /// underlying `MeshStore`. See `commonwealth_core::activity`.
    pub activity_emitter: ActivityEmitter,
}
