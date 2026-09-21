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
use sovereign_grants::{GuestGrantStore, GuestSessionBinding, GuestSessionStore};

use crate::internal_gate::InternalAuth;

/// Everything the node's part is constructed with (DC §4.2 "Construction is
/// staged, and parts are total"): the values that exist before the part is
/// built. The daemon resolves the client-token posture before the listeners
/// bind and passes it here; a test takes `Default`.
#[derive(Default)]
pub struct NodeSeed {
    /// The client-API bearer token, resolved by the daemon's
    /// `resolve_client_bind_posture` before it builds the state, so it is known
    /// before the listeners exist. `None` means "no token configured" — a
    /// loopback-only bind, or a non-loopback bind whose token could not be
    /// resolved, which the [`crate::client_auth`] layer treats as fail-closed.
    pub client_token: Option<Arc<str>>,
    /// What a guest's claimed name is recognised under, resolved from
    /// `[daemon] guest_sessions` before the guest-session store exists. The
    /// default ([`GuestSessionBinding::Door`]) is a name that holds across
    /// this wall — see `sovereign_grants::guest_session`.
    pub guest_sessions: GuestSessionBinding,
    /// Which apps this wall's owner declared open to guests, resolved from
    /// `[daemon] guest_page_dir` and `[daemon.guest_pages]`. The door serves
    /// its pages from this and the rail route scopes a wall grant by it — the
    /// two cannot disagree, because there is one registry and it is decided
    /// before either exists.
    pub guest_pages: crate::guest_door::GuestPages,
    /// What the internal port (`:9742`) requires of a caller, resolved from
    /// `[daemon] internal_auth` before the listeners bind. The default
    /// ([`InternalAuth::Member`]) refuses a caller that is neither a member of
    /// this mesh nor a local process — see `crate::internal_gate`.
    pub internal_auth: InternalAuth,
}

impl NodeSeed {
    /// The seed as this daemon's configuration declares it.
    ///
    /// **THE one reader of `[daemon] guest_sessions`.** The store is built
    /// with the binding already decided, so no request path reads config. An
    /// unparseable value is refused rather than quietly read as the default —
    /// an operator who asked for the strict binding and silently got the
    /// permissive one would never find out (ARCH 6).
    ///
    /// The guest-page registry is resolved here for the same reason and by
    /// its own one reader ([`crate::guest_door::GuestPages::from_config`]),
    /// which refuses a declaration this daemon's own rings would have to
    /// honour. Either refusal reaches the caller as a config error and the
    /// daemon declines to start; `Box<dyn Error>` is what lets the two keep
    /// their own types rather than being flattened into a shared one.
    pub async fn resolved(
        client_token: Option<String>,
        config: &tokio::sync::RwLock<sovereign_core::setup_config::SetupConfig>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let daemon = config.read().await.daemon.clone();
        let guest_sessions = match daemon.guest_sessions.as_deref() {
            None => GuestSessionBinding::default(),
            Some(raw) => GuestSessionBinding::parse(raw)?,
        };
        let guest_pages = crate::guest_door::GuestPages::from_config(&daemon)?;
        let internal_auth = match daemon.internal_auth.as_deref() {
            None => InternalAuth::default(),
            Some(raw) => InternalAuth::parse(raw)?,
        };
        if internal_auth == InternalAuth::Perimeter {
            // ONCE, here, and never per request: the weaker posture is
            // disclosed at the moment it is chosen. Under it the internal port
            // serves any caller that can route to it, which is what every
            // build before `internal_auth` existed did.
            tracing::warn!(
                "node seed: [daemon] internal_auth = \"perimeter\" — the internal \
                 mesh API serves EVERY caller that can route to it, including one \
                 holding no mesh credential. Correct only where something else \
                 scopes reachability (a firewall, an `internal_bind` on a private \
                 NIC, or `require_encryption`, which binds it loopback-only)."
            );
        }
        tracing::debug!(
            guest_sessions = guest_sessions.as_str(),
            guest_pages = ?guest_pages,
            internal_auth = internal_auth.as_str(),
            "node seed: guest session binding, page registry and internal-port posture resolved"
        );
        Ok(Self {
            client_token: client_token.map(Into::into),
            guest_sessions,
            guest_pages,
            internal_auth,
        })
    }
}

/// The node's ten fields, held as `AppStateInner::node`.
pub struct NodePart {
    /// Bearer token required of non-loopback callers on the client API
    /// (`:9741`). `None` means "no token configured" — the
    /// [`crate::client_auth`] layer then admits ONLY loopback callers and fails
    /// closed for any remote one. A construction argument
    /// ([`NodeSeed::client_token`]), not an install: the daemon resolves the
    /// token before the listeners bind, so the set-once `RwLock<Option<_>>`
    /// slot it used to arrive through is gone. Stored in cleartext: the layer
    /// compares it byte-for-byte against the incoming `Authorization: Bearer`.
    pub client_token: Option<Arc<str>>,
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
    /// What the internal port requires of a caller — the ONE reader is
    /// `crate::internal_gate::internal_gate_layer`. A construction argument
    /// ([`NodeSeed::internal_auth`]), not an install: the posture is decided
    /// before the listeners bind, so no request path reads config.
    pub internal_auth: InternalAuth,
    /// The NAMES claimed at this door — one QR serves a room, so the grant
    /// cannot say which phone is asking and the session does. A session is not
    /// a second credential: it names no scope, `GuestGrant::permits_path` on
    /// the bearer presented stays the only decider, and it cannot outlive the
    /// grants it is recognised under. Which grants those are is
    /// [`NodeSeed::guest_sessions`]. See `sovereign_grants::guest_session`.
    pub guest_sessions: Arc<GuestSessionStore>,
    /// The apps this wall's owner DECLARED open to guests, and what guests may
    /// do on each. Read by the door's page routes and by
    /// `routes_rail::namespace_for`, which is what scopes a wall grant — the
    /// resource declares, the credential identifies.
    pub guest_pages: Arc<crate::guest_door::GuestPages>,
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
