// SPDX-License-Identifier: AGPL-3.0-or-later
//! `KnowledgeViewManager` — lifecycle façade for the three
//! KnowledgeView corpora (`personal-knowledge`,
//! `conversation-history`, `institutional-notes`).
//!
//! After the Phase-B split this module is a thin orchestrator:
//!
//!   - **Bookkeeping** — registers the `SqliteAcquirer`, builds a
//!     `ViewEntry` per view, holds the mpsc trigger channel.
//!   - **Observer** — implements `StateStoreObserver`, translating
//!     memory/message/conversation writes into `ViewEvent`s that
//!     the `debouncer` module consumes.
//!   - **Digest splice** — implements `LandscapeDigestProvider`,
//!     formatting each view through `digest::format_landscape` and
//!     building the cross-view resonance block via
//!     `cross_view::build_cross_view_digest`.
//!
//! Policy constants (debounce thresholds, default budgets) live
//! alongside the mechanisms that consume them: `debouncer.rs` owns
//! the timing window; `view_kind.rs` owns per-view budgets.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::engine::CorpusEngine;
use corpus_engine::enrichment::skeleton::FieldSkeleton;
use corpus_engine::error::{Error as CorpusError, Result as CorpusResult};
use corpus_engine::recipe::Recipe;
use corpus_engine::types::{CorpusSpec, InferenceFn};
use sovereign_core::observer::StateStoreObserver;
use sovereign_core::traits::LandscapeDigestProvider;
use sovereign_core::types::{ConversationContext, LandscapeDigest};
use tokio::sync::{mpsc, RwLock};

use super::acquirers::register_sqlite;
use super::atlas_digest;
use super::cross_view;
use super::debouncer::{spawn_debouncer, ViewEntry, ViewEvent};
use super::digest::format_landscape;
use super::recipes::{
    conversation_history_recipe, institutional_notes_recipe, personal_knowledge_recipe,
};
use super::tokens::estimate_tokens;
use super::view_kind::ViewKind;

#[cfg(feature = "treesitter")]
use super::relational::{format_relational, RelationalNote};
#[cfg(feature = "treesitter")]
use super::splice_extension::{
    load_chunk_timestamps, relational_notes_for_entity, strategic_goals_for_entity,
    ConversationCorpus,
};
#[cfg(all(feature = "treesitter", feature = "atos"))]
use super::splice_extension::AtosSnapshot;
#[cfg(feature = "treesitter")]
use super::strategic::{format_strategic, StrategicGoal};
#[cfg(feature = "treesitter")]
use super::timeline::assemble_timelines_from_atlas;
#[cfg(all(feature = "treesitter", feature = "atos"))]
use corpus_engine_atos::features::FeatureStore;
#[cfg(feature = "treesitter")]
use corpus_engine_notes::notes::NoteStore;

// ── Backwards-compatible string-id constants ────────────────────
//
// The public API historically exposed the view ids as plain `&'static
// str` constants, and downstream tests + tools still import them.
// Keep them as named aliases over `ViewKind::*.id()` so the single
// source of truth lives on the enum.

/// Canonical id for the personal-knowledge view. Prefer `ViewKind::Personal.id()` in new code.
pub const VIEW_PERSONAL_KNOWLEDGE: &str = ViewKind::Personal.id();
/// Canonical id for the conversation-history view.
pub const VIEW_CONVERSATION_HISTORY: &str = ViewKind::Conversational.id();
/// Canonical id for the institutional-notes view.
pub const VIEW_INSTITUTIONAL_NOTES: &str = ViewKind::Institutional.id();
/// Synthetic id for the cross-view resonance digest.
pub const VIEW_CROSS_VIEW: &str = ViewKind::CrossView.id();

/// Token budget for the cross-view resonance digest. Third-tier
/// context, meant to carry 2–4 tentative connections; a larger
/// budget would drift toward "surveillance" rather than
/// "invitation to connect".
const CROSS_VIEW_BUDGET_TOKENS: usize = ViewKind::CrossView.default_budget_tokens();

/// One manager per running Sovereign instance. Cheap to clone via `Arc`.
///
/// The inference function passed to `KnowledgeViewManager::new` is
/// captured by the debouncer task — it is not stored on the manager
/// itself since all enrichment paths flow through the debouncer.
pub struct KnowledgeViewManager {
    engine: Arc<CorpusEngine>,
    views: Arc<RwLock<HashMap<String, ViewEntry>>>,
    triggers: mpsc::UnboundedSender<ViewEvent>,
    /// Skill ids whose declared `privacy = "local_only"` means the
    /// conversational + institutional digests must be OMITTED when
    /// one of them is the active skill. Sourced from
    /// `SkillRegistry::local_only_skill_ids()` at construction.
    local_only_skill_ids: Vec<String>,
    /// Formatted-digest cache, keyed by `(view_id, budget_tokens)`.
    /// The cached entry's mtime is compared against
    /// `field_skeleton.json`'s mtime at read time — stale entries
    /// are thrown away and the digest is re-formatted. This avoids
    /// the per-turn JSON parse + formatter cost when the skeleton
    /// hasn't changed (which is the common case: reading happens
    /// on every message, enrichment happens every few minutes).
    digest_cache: Arc<tokio::sync::RwLock<HashMap<(String, usize), CachedDigest>>>,
    /// Sovereign state DB path. Held so the splice path can resolve
    /// chunk_ids → memory.last_used / conversation.updated_at.
    db_path: PathBuf,
    /// Working-notes DB path. Held so the splice path can attach
    /// commitment / follow_up / goal notes by `related_entity` to
    /// the relational and strategic blocks.
    notes_db_path: PathBuf,
    /// ATOS feature DB path (typically `~/.sovereign/features.db`).
    /// `None` = no feature lookup; the strategic block falls back to
    /// initiative names without phase / drift annotation. Only read when
    /// the `atos` feature is on (the strategic ATOS splice).
    #[cfg_attr(not(feature = "atos"), allow(dead_code))]
    features_db_path: Option<PathBuf>,
    /// `.sovereign/project.toml` path. `None` = no project name in
    /// scope; initiatives can still surface from atoms but they
    /// won't link to a local project.
    project_toml_path: Option<PathBuf>,
    /// Lazy NoteStore handle, opened on first splice. The store
    /// itself is sharable across threads, so we hold an Arc.
    #[cfg(feature = "treesitter")]
    notes_handle: Arc<tokio::sync::Mutex<Option<Arc<NoteStore>>>>,
    /// Lazy FeatureStore handle, opened on first splice when
    /// `features_db_path` is set.
    #[cfg(all(feature = "treesitter", feature = "atos"))]
    features_handle: Arc<tokio::sync::Mutex<Option<Arc<FeatureStore>>>>,
}

/// One row of the digest cache. Stored body is the exact string
/// returned by `format_landscape`; the mtime is the last-modified
/// time of `field_skeleton.json` at the moment we read it.
struct CachedDigest {
    skeleton_mtime: std::time::SystemTime,
    body: String,
}

impl KnowledgeViewManager {
    /// Construct a manager, register the SQLite acquirer, and spawn
    /// the background debouncer task.
    ///
    /// `db_path` is the sovereign SQLite file (typically
    /// `~/.sovereign/sovereign.db`). `local_only_skill_ids` is the
    /// resolved set of skill ids whose conversations must be excluded
    /// from the conversational view.
    pub async fn new(
        engine: Arc<CorpusEngine>,
        inference: InferenceFn,
        db_path: PathBuf,
        local_only_skill_ids: Vec<String>,
    ) -> Self {
        let notes_db_path = db_path
            .parent()
            .map(|p| p.join("notes.db"))
            .unwrap_or_else(|| PathBuf::from("notes.db"));
        Self::new_with_notes_path(
            engine,
            inference,
            db_path,
            notes_db_path,
            local_only_skill_ids,
        )
        .await
    }

    /// Construct with an explicit path for the agent's working-notes
    /// DB (where `NoteStore` writes). Used by the desktop bootstrap
    /// when the notes file lives in a project-scoped directory rather
    /// than `~/.sovereign/`.
    pub async fn new_with_notes_path(
        engine: Arc<CorpusEngine>,
        inference: InferenceFn,
        db_path: PathBuf,
        notes_db_path: PathBuf,
        local_only_skill_ids: Vec<String>,
    ) -> Self {
        register_sqlite(&engine);

        let local_refs: Vec<&str> = local_only_skill_ids.iter().map(|s| s.as_str()).collect();
        let mut views = HashMap::new();
        views.insert(
            ViewKind::Personal.id().to_string(),
            ViewEntry {
                recipe: personal_knowledge_recipe(&db_path),
                lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        views.insert(
            ViewKind::Conversational.id().to_string(),
            ViewEntry {
                recipe: conversation_history_recipe(&db_path, &local_refs),
                lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        // The institutional-notes view reads from the NoteStore DB,
        // not the main sovereign.db. If the notes DB doesn't exist
        // yet (fresh install, no notes written), the acquirer will
        // return a Recipe error at ingest time and the manager's
        // soft-fail path will simply leave the view empty.
        views.insert(
            ViewKind::Institutional.id().to_string(),
            ViewEntry {
                recipe: institutional_notes_recipe(&notes_db_path),
                lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );

        let (tx, rx) = mpsc::unbounded_channel();
        let views = Arc::new(RwLock::new(views));

        spawn_debouncer(engine.clone(), inference, views.clone(), rx);

        Self {
            engine,
            views,
            triggers: tx,
            local_only_skill_ids,
            digest_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            db_path,
            notes_db_path,
            features_db_path: None,
            project_toml_path: None,
            #[cfg(feature = "treesitter")]
            notes_handle: Arc::new(tokio::sync::Mutex::new(None)),
            #[cfg(all(feature = "treesitter", feature = "atos"))]
            features_handle: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Set the ATOS feature DB path. When set + `with_project_toml_path`
    /// is also set, the strategic digest will surface phase + drift
    /// annotations on initiatives that match a local project or
    /// feature. Caller chains this on the constructor:
    ///
    /// ```ignore
    /// let mgr = KnowledgeViewManager::new(...).await
    ///     .with_features_db_path(features_db_path)
    ///     .with_project_toml_path(project_toml_path);
    /// ```
    pub fn with_features_db_path(mut self, path: PathBuf) -> Self {
        self.features_db_path = Some(path);
        self
    }

    /// Set the `.sovereign/project.toml` path. Used by the strategic
    /// digest to match initiative entity names against the local
    /// project name (with parent-dir fallback for v1 files).
    pub fn with_project_toml_path(mut self, path: PathBuf) -> Self {
        self.project_toml_path = Some(path);
        self
    }

    /// Ingest each view. Safe to call repeatedly —
    /// `CorpusEngine::ingest` is checkpoint-based, so a second
    /// invocation over an already-populated index is a no-op beyond a
    /// manifest check. Per-view mutex ensures a concurrent
    /// background-spawned init + manual enrich() serialise rather
    /// than race.
    pub async fn init(&self) -> CorpusResult<()> {
        let view_ids: Vec<String> = {
            let guard = self.views.read().await;
            guard.keys().cloned().collect()
        };
        for view_id in view_ids {
            tracing::info!(view_id, "KnowledgeViewManager: ensuring view is ingested");
            if let Err(e) = self.ingest_view(&view_id).await {
                tracing::warn!(view_id, error = %e, "ingest failed");
            }
        }
        Ok(())
    }

    /// Spawn `init()` on a detached tokio task. Use from process
    /// bootstraps (server, CLI, desktop) so the listener bind isn't
    /// blocked by first-run ingest, which can take tens of seconds on
    /// a populated SQLite DB.
    pub fn spawn_init(self: std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        self.spawn_init_after(std::time::Duration::ZERO)
    }

    /// Variant of `spawn_init` that delays the actual ingest by
    /// `delay` before kicking off. Used by the desktop bootstrap to
    /// keep the fast slot free for the user's first interaction —
    /// otherwise enrichment Phase 2 fans out parallel calls onto the
    /// fast slot at the exact moment the user wants to chat.
    ///
    /// The delay is silent: the init log line still fires when the
    /// real work starts, so operators can see when enrichment begins
    /// rather than when it was queued. Use `Duration::ZERO` (the
    /// `spawn_init` shortcut) for processes where delay is unwanted
    /// (CLI / server cold-starts where there's no UI to protect).
    ///
    /// Cancellation: the `JoinHandle` aborts cleanly mid-sleep —
    /// dropping it is the supported way to cancel a deferred init
    /// when the process is shutting down before the timer fires.
    /// Cooperative cancellation of the *running* enrichment is a
    /// separate piece of work (see RELEASING.md / project notes).
    pub fn spawn_init_after(
        self: std::sync::Arc<Self>,
        delay: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !delay.is_zero() {
                tracing::info!(
                    delay_secs = delay.as_secs(),
                    "knowledge_view: deferring background init"
                );
                tokio::time::sleep(delay).await;
            }
            tracing::info!("knowledge_view: starting background init");
            match self.init().await {
                Ok(()) => tracing::info!("knowledge_view: init complete"),
                Err(e) => tracing::warn!(
                    error = %e,
                    "knowledge_view: init failed; landscape digests will be missing \
                     until a later write triggers the debouncer"
                ),
            }
        })
    }

    /// Manually enrich a view. Used for CLI triggers and tests.
    pub async fn enrich(&self, view_id: &str) -> CorpusResult<()> {
        let _ = self.triggers.send(ViewEvent::Manual {
            view_id: view_id.to_string(),
        });
        Ok(())
    }

    /// Assemble a landscape digest for the given view, bounded by
    /// `budget_tokens`. Output is a plain markdown string.
    ///
    /// `active_skill` is currently advisory — v1 does not filter
    /// digest content by skill beyond what the acquirer already
    /// filtered out at ingest. Reserved for the v2 skill-tiered
    /// digest work.
    pub async fn landscape_digest(
        &self,
        view_id: &str,
        _active_skill: Option<&str>,
        budget_tokens: usize,
    ) -> CorpusResult<String> {
        // Fast path: return the cached formatted body when the
        // skeleton (v1) or atoms.json (v2 atlas) file hasn't been
        // modified since we last formatted it. splice_into fires on
        // every turn; an enrichment run happens every few minutes at
        // most — so the cache hit rate is effectively 100% on hot
        // paths.
        let index = self.engine.open_index_for_corpus(view_id).await?;
        let index_dir = index.path();
        // `conversation-history` was migrated to the v2 atlas pipeline
        // (`conversation_atlas`) in the conversation-imports landing;
        // its digest source is `atlas/atoms.json` rather than
        // `field_skeleton.json`. The other two views still run v1
        // `field_model` enrichment.
        let use_atlas = view_id == ViewKind::Conversational.id();
        let source_path = if use_atlas {
            index_dir.join("atlas").join("atoms.json")
        } else {
            index_dir.join("field_skeleton.json")
        };
        let source_mtime = std::fs::metadata(&source_path)
            .and_then(|m| m.modified())
            .ok();

        if let Some(mtime) = source_mtime {
            let cache = self.digest_cache.read().await;
            if let Some(entry) = cache.get(&(view_id.to_string(), budget_tokens)) {
                if entry.skeleton_mtime == mtime {
                    return Ok(entry.body.clone());
                }
            }
        }

        let body = if use_atlas {
            let atlas_dir = index_dir.join("atlas");
            let rendered = atlas_digest::render_atlas_digest(&atlas_dir, budget_tokens);
            if rendered.is_empty() {
                let title = ViewKind::from_id(view_id)
                    .map(|k| k.title())
                    .unwrap_or("Knowledge view");
                return Ok(format!("{title}: not yet enriched."));
            }
            rendered
        } else {
            let Some(skeleton) = index.load_field_skeleton()? else {
                let title = ViewKind::from_id(view_id)
                    .map(|k| k.title())
                    .unwrap_or("Knowledge view");
                return Ok(format!("{title}: not yet enriched."));
            };
            format_landscape(&skeleton, view_id, budget_tokens)
        };

        // Insert into cache only if we managed to read the mtime —
        // without it we can't detect staleness, so falling through to
        // re-format next time is safer than caching blind.
        if let Some(mtime) = source_mtime {
            let mut cache = self.digest_cache.write().await;
            cache.insert(
                (view_id.to_string(), budget_tokens),
                CachedDigest {
                    skeleton_mtime: mtime,
                    body: body.clone(),
                },
            );
        }
        Ok(body)
    }

    /// Assemble landscape digests for every configured view and
    /// splice them into `ctx.knowledge_view_digests`.
    ///
    /// Intended to be called from `Runtime::handle_message_stream`
    /// after skill routing so `active_skill` is available. Soft-fails
    /// per view: a missing index or unenriched skeleton is silently
    /// omitted. The field is always set to `Some(_)` on return, so
    /// callers downstream can rely on the invariant "post-routing
    /// context has a non-`None` digests field".
    pub async fn splice_into(&self, ctx: &mut ConversationContext, active_skill: Option<&str>) {
        // Extract conversation messages (the only piece of `ctx` the
        // digest computation actually reads — the relational +
        // strategic blocks build an in-conversation predicate from
        // them). Pulling a borrow-free snapshot here keeps
        // `compute_digests` reusable from the daemon's HTTP handler,
        // which doesn't have a `ConversationContext` to lend.
        let messages: Vec<String> = ctx
            .conversation
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect();
        // Resolve `active_is_local_only` against the manager's own
        // registered set — the in-process splice path has the skill
        // ids on hand and shouldn't pay an HTTP-style indirection.
        let active_is_local_only = active_skill
            .map(|s| self.local_only_skill_ids.iter().any(|id| id == s))
            .unwrap_or(false);
        let digests = self
            .compute_digests(active_skill, active_is_local_only, &messages)
            .await;
        ctx.set_landscape_digests(digests);
    }

    /// Compute the full landscape-digest set for the given
    /// `active_skill` and conversation transcript. Identical
    /// orchestration to `splice_into` but returns the vector
    /// instead of writing to a `ConversationContext`, so the daemon
    /// HTTP surface (`POST /v1/knowledge/landscape_digest`) can
    /// serve attached desktops a ready-to-splice payload.
    ///
    /// `active_is_local_only` is caller-determined — the in-process
    /// path resolves it against `self.local_only_skill_ids`, the
    /// HTTP handler reads it from the request body. Lifting it out
    /// of the manager's state lets a daemon serve digests for a
    /// caller whose skill registry it doesn't share (the desktop
    /// has its own copy of `local_only_skill_ids`).
    ///
    /// `conversation_messages` is the in-conversation message
    /// content used by the relational/strategic blocks for the
    /// "name appears in this conversation already" predicate. Pass
    /// an empty slice when no in-conversation context is available
    /// (e.g. a digest-warm cache request) — the predicate degrades
    /// to "no conversational matches" without affecting the rest of
    /// the digest.
    pub async fn compute_digests(
        &self,
        active_skill: Option<&str>,
        active_is_local_only: bool,
        conversation_messages: &[String],
    ) -> Vec<LandscapeDigest> {
        // Skill-aware digest selection (spec: "when the active skill
        // is inner-work, the technical knowledge digest is absent
        // entirely"). Acquirer-level filtering already keeps
        // local_only conversations out of the corpus; this branch
        // suppresses the REMAINING digests so cross-session context
        // can't leak into a private session. The caller has
        // already resolved `active_is_local_only` from its own skill
        // registry — see signature docstring.

        let mut view_budgets: Vec<(ViewKind, usize)> = Vec::with_capacity(3);
        view_budgets.push((
            ViewKind::Personal,
            ViewKind::Personal.default_budget_tokens(),
        ));
        if !active_is_local_only {
            view_budgets.push((
                ViewKind::Conversational,
                ViewKind::Conversational.default_budget_tokens(),
            ));
            view_budgets.push((
                ViewKind::Institutional,
                ViewKind::Institutional.default_budget_tokens(),
            ));
        } else {
            tracing::debug!(
                active_skill = ?active_skill,
                "compute_digests: omitting conversation-history + institutional \
                 digests for privacy=local_only active skill"
            );
        }

        let mut digests = Vec::with_capacity(view_budgets.len() + 1);
        // Track the view_ids whose digests actually landed — these
        // are the only views that can contribute to cross-view
        // matching. Suppressed views (conversational + institutional
        // for a local_only active skill) must NOT leak through a
        // cross-view match.
        let mut included_view_ids: Vec<String> = Vec::new();
        for (kind, budget) in view_budgets {
            let view_id = kind.id();
            match self.landscape_digest(view_id, active_skill, budget).await {
                Ok(body) if !body.trim().is_empty() => {
                    included_view_ids.push(view_id.to_string());
                    digests.push(LandscapeDigest {
                        view_id: view_id.to_string(),
                        body,
                    });
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(
                    view_id,
                    error = %e,
                    "landscape_digest skipped — view not yet available"
                ),
            }
        }

        // Cross-view resonance — runs only when ≥ 2 included views
        // have enriched skeletons. See `cross_view` module docs for
        // the design rationale + privacy guarantee.
        if included_view_ids.len() >= 2 {
            match self
                .cross_view_digest(&included_view_ids, CROSS_VIEW_BUDGET_TOKENS)
                .await
            {
                Ok(Some(body)) => digests.push(LandscapeDigest {
                    view_id: ViewKind::CrossView.id().to_string(),
                    body,
                }),
                Ok(None) => {
                    tracing::debug!(
                        views = ?included_view_ids,
                        "cross_view: no matches surfaced — skipping digest"
                    );
                }
                Err(e) => tracing::debug!(
                    error = %e,
                    "cross_view_digest failed — proceeding without it"
                ),
            }
        }

        // Relational + Strategic blocks — entity-aware digests
        // composed from the personal-knowledge + conversation-history
        // atlases. Suppressed for `local_only` skills on the same
        // gate as the conversational + institutional blocks: the
        // entity graph is built from those corpora, so a private
        // skill must not see them surface here either.
        #[cfg(feature = "treesitter")]
        if !active_is_local_only {
            self.append_relational_strategic_blocks(conversation_messages, &mut digests)
                .await;
        }

        digests
    }

    /// Compose the Relational + Strategic digest blocks (Phase 4.B).
    /// Reads atoms / edges from the personal-knowledge and
    /// conversation-history atlases, joins NoteStore records by
    /// `related_entity`, and composes ATOS phase / drift via
    /// `AtosSnapshot`. Soft-fails: any I/O error per source falls
    /// through with a debug log; the splice continues without the
    /// affected block.
    #[cfg(feature = "treesitter")]
    async fn append_relational_strategic_blocks(
        &self,
        conversation_messages: &[String],
        digests: &mut Vec<LandscapeDigest>,
    ) {
        // 1. Resolve every atlas chunk_id once. Cheap (a few
        //    thousand rows) compared to per-edge lookup.
        let timestamps = load_chunk_timestamps(&self.db_path);
        let chunk_ts = |id: &str| timestamps.get(id).copied();

        // 2. Build the ATOS snapshot (feature `atos`, off by default). When
        //    on, timelines get phase/drift annotation from the FeatureStore +
        //    project.toml. When off, a `NoAtosLookup` means no annotation —
        //    the strategic block still renders from NoteStore-backed goals.
        #[cfg(feature = "atos")]
        let atos = {
            let features = self.features_store_handle().await;
            AtosSnapshot::build(features.as_ref(), self.project_toml_path.as_deref()).await
        };
        #[cfg(not(feature = "atos"))]
        let atos = super::timeline::NoAtosLookup;

        // 3. Pull timelines from both atlas sources. We compute the
        //    per-corpus index directory directly (engine.index_dir +
        //    view_id) — calling `open_index_for_corpus` would attempt
        //    to open the LanceDB tables, which fails for a brand-new
        //    install where no ingest has run yet. The atlas reader
        //    only needs the directory layout and is happy when the
        //    file is missing (it returns an empty vec).
        let mut timelines = Vec::new();
        for view_id in [ViewKind::Personal.id(), ViewKind::Conversational.id()] {
            let corpus_dir = self.engine.index_dir().join(view_id);
            match assemble_timelines_from_atlas(&corpus_dir, &chunk_ts, &atos) {
                Ok(mut tls) => timelines.append(&mut tls),
                Err(e) => {
                    tracing::debug!(
                        view_id,
                        error = %e,
                        "splice: timeline assembly failed; skipping atlas"
                    );
                }
            }
        }

        if timelines.is_empty() {
            return;
        }

        // 4. NoteStore-backed `related_entity` resolver. We collect
        //    all needed (entity_name, kind-bucket) pairs and pre-load
        //    them into HashMaps so the formatters can stay sync.
        let notes_handle = self.notes_store_handle().await;
        let mut relational_index: HashMap<String, Vec<RelationalNote>> = HashMap::new();
        let mut strategic_index: HashMap<String, Vec<StrategicGoal>> = HashMap::new();
        if let Some(notes) = notes_handle.as_ref() {
            for tl in &timelines {
                let key = tl.entity_name.clone();
                if !relational_index.contains_key(&key) {
                    let r = relational_notes_for_entity(notes, &tl.entity_name).await;
                    relational_index.insert(key.clone(), r);
                }
                if let std::collections::hash_map::Entry::Vacant(e) = strategic_index.entry(key) {
                    let g = strategic_goals_for_entity(notes, &tl.entity_name).await;
                    e.insert(g);
                }
            }
        }

        // 5. In-conversation predicate from the current message thread.
        let corpus = ConversationCorpus::from_messages(conversation_messages.iter().cloned());
        let in_conv = |name: &str| corpus.contains_entity(name);

        let now = chrono::Utc::now().timestamp();

        // 6. Render Relational.
        let relational_lookup = |name: &str| -> Vec<RelationalNote> {
            relational_index.get(name).cloned().unwrap_or_default()
        };
        let (relational_body, _) = format_relational(
            &timelines,
            &relational_lookup,
            &in_conv,
            now,
            ViewKind::Relational.default_budget_tokens(),
        );
        if !relational_body.trim().is_empty() {
            digests.push(LandscapeDigest {
                view_id: ViewKind::Relational.id().to_string(),
                body: relational_body,
            });
        }

        // 7. Render Strategic.
        let strategic_lookup = |name: &str| -> Vec<StrategicGoal> {
            strategic_index.get(name).cloned().unwrap_or_default()
        };
        let (strategic_body, _) = format_strategic(
            &timelines,
            &strategic_lookup,
            &in_conv,
            now,
            ViewKind::Strategic.default_budget_tokens(),
        );
        if !strategic_body.trim().is_empty() {
            digests.push(LandscapeDigest {
                view_id: ViewKind::Strategic.id().to_string(),
                body: strategic_body,
            });
        }
    }

    /// Load an [`EntityInventory`](sovereign_core::memory::EntityInventory)
    /// from the on-disk atlases of every primary view that has one.
    /// Returns lowercased canonical names + aliases — the input shape
    /// `apply_confidence_decay_with_rate_and_inventory` expects for
    /// the relationship-weighted decay path (Phase 7).
    ///
    /// Returns an empty set when no atlas files are present yet
    /// (fresh install pre-enrichment). Callers can pass `None` to
    /// the decay path in that case to fall back to uniform decay.
    #[cfg(feature = "treesitter")]
    pub fn entity_inventory_from_atlases(&self) -> sovereign_core::memory::EntityInventory {
        use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
        use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};

        let mut names: Vec<String> = Vec::new();
        for kind in [ViewKind::Personal, ViewKind::Conversational] {
            let atlas_dir = self.engine.index_dir().join(kind.id()).join(ATLAS_DIRNAME);
            let Ok(file) = read_atlas_atoms(&atlas_dir) else {
                continue;
            };
            for atom in &file.atoms {
                if let AtomEnvelope::Entity(e) = atom {
                    names.push(e.canonical_name.clone());
                    for alias in &e.aliases {
                        names.push(alias.clone());
                    }
                }
            }
        }
        sovereign_core::memory::entity_inventory_from_names(names)
    }

    /// Lazy NoteStore opener. Returns `None` when the underlying
    /// file can't be opened — the splice path then renders without
    /// `related_entity` annotations.
    #[cfg(feature = "treesitter")]
    async fn notes_store_handle(&self) -> Option<Arc<NoteStore>> {
        let mut guard = self.notes_handle.lock().await;
        if let Some(h) = guard.as_ref() {
            return Some(h.clone());
        }
        match NoteStore::open(&self.notes_db_path) {
            Ok(store) => {
                let arc = Arc::new(store);
                *guard = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::debug!(
                    path = %self.notes_db_path.display(),
                    error = %e,
                    "splice: NoteStore::open failed; relational annotations skipped"
                );
                None
            }
        }
    }

    /// Lazy FeatureStore opener. Returns `None` when no
    /// `features_db_path` is configured or the file can't be opened.
    #[cfg(all(feature = "treesitter", feature = "atos"))]
    async fn features_store_handle(&self) -> Option<Arc<FeatureStore>> {
        let path = self.features_db_path.as_ref()?;
        let mut guard = self.features_handle.lock().await;
        if let Some(h) = guard.as_ref() {
            return Some(h.clone());
        }
        match FeatureStore::open(path) {
            Ok(store) => {
                let arc = Arc::new(store);
                *guard = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "splice: FeatureStore::open failed; ATOS phases skipped"
                );
                None
            }
        }
    }

    /// Build a cross-view resonance digest across `view_ids` at the
    /// given token budget. Returns `None` when fewer than two views
    /// have enriched skeletons or when no matches clear the
    /// similarity threshold.
    async fn cross_view_digest(
        &self,
        view_ids: &[String],
        budget_tokens: usize,
    ) -> CorpusResult<Option<String>> {
        let mut skeletons: Vec<(String, FieldSkeleton)> = Vec::new();
        let mut max_mtime: Option<std::time::SystemTime> = None;
        for view_id in view_ids {
            let index = match self.engine.open_index_for_corpus(view_id).await {
                Ok(i) => i,
                Err(_) => continue,
            };
            let mtime = std::fs::metadata(index.path().join("field_skeleton.json"))
                .and_then(|m| m.modified())
                .ok();
            match index.load_field_skeleton()? {
                Some(sk) => {
                    skeletons.push((view_id.clone(), sk));
                    if let Some(mt) = mtime {
                        max_mtime = Some(match max_mtime {
                            Some(cur) if cur >= mt => cur,
                            _ => mt,
                        });
                    }
                }
                None => continue,
            }
        }
        if skeletons.len() < 2 {
            return Ok(None);
        }

        // Cache lookup — cross-view entries live in the same
        // digest_cache, keyed by VIEW_CROSS_VIEW. Stored mtime is the
        // composite max across source skeletons.
        if let Some(mt) = max_mtime {
            let cache = self.digest_cache.read().await;
            if let Some(entry) = cache.get(&(VIEW_CROSS_VIEW.to_string(), budget_tokens)) {
                if entry.skeleton_mtime == mt {
                    return Ok(Some(entry.body.clone()));
                }
            }
        }

        let embed = self.engine.embed_fn();
        let body = cross_view::build_cross_view_digest(
            &skeletons,
            &embed,
            budget_tokens,
            cross_view::DEFAULT_MATCH_THRESHOLD,
            estimate_tokens,
        )
        .await?;

        if let (Some(b), Some(mt)) = (body.as_ref(), max_mtime) {
            let mut cache = self.digest_cache.write().await;
            cache.insert(
                (VIEW_CROSS_VIEW.to_string(), budget_tokens),
                CachedDigest {
                    skeleton_mtime: mt,
                    body: b.clone(),
                },
            );
        }
        Ok(body)
    }

    /// Search the enriched index for `query`, returning up to `k`
    /// scored chunks. v1 stub — full wiring deferred until consumer
    /// tools are written.
    pub async fn search(
        &self,
        _view_id: &str,
        _query: &str,
        _k: usize,
    ) -> CorpusResult<Vec<String>> {
        Ok(Vec::new())
    }

    // ── Internals ───────────────────────────────────────────

    async fn ingest_view(&self, view_id: &str) -> CorpusResult<()> {
        let (recipe, lock) = {
            let guard = self.views.read().await;
            let entry = guard
                .get(view_id)
                .ok_or_else(|| CorpusError::Recipe(format!("Unknown view: {view_id}")))?;
            (entry.recipe.clone(), entry.lock.clone())
        };
        // Serialise with any concurrent enrichment for this view.
        let _guard = lock.lock().await;
        let path = recipe_to_tempfile(&recipe)?;
        let spec = CorpusSpec::RecipePath(path);
        let result = self.engine.ingest(&spec, None).await.map(|_| ());
        // Ingest can change the index (and eventually the skeleton
        // via downstream enrich) — invalidate digest entries for
        // this view so the next splice re-reads. The Phase B
        // incremental NER hook (PROGRESSIVE_ENRICHMENT.md §B) fires
        // inside `engine.ingest` itself when the recipe declares
        // `display.category = "conversation"` — no fan-out here.
        self.invalidate_digest_cache(view_id).await;
        result
    }

    async fn invalidate_digest_cache(&self, view_id: &str) {
        let mut cache = self.digest_cache.write().await;
        cache.retain(|(v, _), _| v != view_id);
    }
}

impl StateStoreObserver for KnowledgeViewManager {
    fn on_memory_written(&self, _memory_id: &str) {
        let _ = self.triggers.send(ViewEvent::MemoryTouched);
    }

    fn on_message_written(&self, _conversation_id: &str) {
        let _ = self.triggers.send(ViewEvent::ConversationTouched);
    }

    fn on_conversation_deleted(&self, _conversation_id: &str) {
        let _ = self.triggers.send(ViewEvent::ConversationDeleted);
    }
}

#[async_trait]
impl LandscapeDigestProvider for KnowledgeViewManager {
    async fn splice_landscape_digests(
        &self,
        ctx: &mut ConversationContext,
        active_skill: Option<&str>,
    ) {
        self.splice_into(ctx, active_skill).await;
    }

    #[cfg(feature = "treesitter")]
    async fn entity_inventory(&self) -> Option<sovereign_core::memory::EntityInventory> {
        let inv = self.entity_inventory_from_atlases();
        if inv.is_empty() {
            None
        } else {
            Some(inv)
        }
    }
}

// ── Recipe → temp TOML (for CorpusSpec::RecipePath) ─────────

fn recipe_to_tempfile(recipe: &Recipe) -> CorpusResult<PathBuf> {
    let toml_text = toml::to_string(recipe)
        .map_err(|e| CorpusError::Recipe(format!("serialize recipe: {e}")))?;
    let dir = std::env::temp_dir().join("sovereign-knowledge-view-recipes");
    std::fs::create_dir_all(&dir).map_err(CorpusError::Io)?;
    let path = dir.join(format!("{}.toml", recipe.corpus.id));
    std::fs::write(&path, toml_text).map_err(CorpusError::Io)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    //! Manager-level integration tests.
    //!
    //! Pure-function tests for the extracted modules live alongside
    //! their implementation (`digest.rs`, `tokens.rs`, `debouncer.rs`).

    use super::*;
    use corpus_engine::enrichment::clustering::FieldModelStats;
    use corpus_engine::enrichment::skeleton::{
        CanonicalQuestion, FieldSkeleton, SkeletonFaultLine, SkeletonOpenQuestion, SkeletonPosition,
    };

    fn fixture_skeleton() -> FieldSkeleton {
        FieldSkeleton {
            schema_version: 1,
            corpus_id: "personal-knowledge".into(),
            generated_at: "2026-04-20T00:00:00Z".into(),
            extraction_method: "fixture".into(),
            prompt_version: "v1".into(),
            domain_id: "personal".into(),
            canonical_questions: vec![CanonicalQuestion {
                id: "q1".into(),
                question: "What does meaningful work look like for me?".into(),
                status: "contested".into(),
                question_type: "normative".into(),
                primary_entries: vec![],
                positions: vec![SkeletonPosition {
                    id: "p1".into(),
                    name: "Purpose-driven".into(),
                    claim: "meaningful work serves others".into(),
                    status: "held".into(),
                    proponents: vec![],
                    source: "skeleton".into(),
                    cluster_ids: vec![],
                    centroid_chunk_ids: vec![],
                    discovery_confidence: None,
                }],
                fault_lines: vec![SkeletonFaultLine {
                    id: "f1".into(),
                    between_positions: vec!["p1".into(), "p2".into()],
                    crux: "stability vs. autonomy".into(),
                    key_chunk_ids: vec![],
                    confidence: 0.8,
                    source: "detected".into(),
                    resolution_condition: None,
                }],
            }],
            open_questions: vec![SkeletonOpenQuestion {
                id: "oq1".into(),
                question: "what kind of life do I actually want".into(),
                status: "open".into(),
                related_question_id: Some("q1".into()),
                representative_chunk_ids: vec![],
            }],
            field_stats: FieldModelStats::default(),
        }
    }

    async fn bare_manager_with_local_only(local_only: Vec<String>) -> KnowledgeViewManager {
        let tmp = tempfile::TempDir::new().unwrap();
        let indexes_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        let db_path = tmp.path().join("sovereign.db");
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();
        let _ = std::fs::File::create(&db_path).unwrap();
        let embed: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) })
        });
        let infer: corpus_engine::InferenceFn =
            std::sync::Arc::new(|_, _: Option<&serde_json::Value>| {
                Box::pin(async { Ok::<String, corpus_engine::Error>("{}".into()) })
            });
        let engine = std::sync::Arc::new(corpus_engine::CorpusEngine::new(
            recipes_dir,
            indexes_dir,
            embed,
        ));
        KnowledgeViewManager::new(engine, infer, db_path, local_only).await
    }

    fn tmp_context() -> sovereign_core::types::ConversationContext {
        sovereign_core::types::ConversationContext {
            conversation: sovereign_core::types::Conversation {
                id: "c".into(),
                title: None,
                messages: vec![],
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
                enabled_corpora: None,
                searched_sources: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            corpus_ceiling: None,
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_omits_conversation_history_when_active_skill_is_local_only() {
        let mgr = bare_manager_with_local_only(vec!["inner-work".to_string()]).await;
        let mut ctx = tmp_context();
        mgr.splice_into(&mut ctx, Some("inner-work")).await;
        let digests = ctx.knowledge_view_digests.unwrap();
        assert!(
            !digests
                .iter()
                .any(|d| d.view_id == VIEW_CONVERSATION_HISTORY),
            "conversation-history must be omitted for local_only active skill: {digests:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_includes_conversation_history_for_non_local_skill() {
        let mgr = bare_manager_with_local_only(vec!["inner-work".to_string()]).await;
        let mut ctx = tmp_context();
        mgr.splice_into(&mut ctx, Some("research-analyst")).await;
        assert!(ctx.knowledge_view_digests.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_view_mutex_serialises_ingest_and_enrich() {
        // Each view gets its own mutex so an enrichment on personal
        // does NOT block a concurrent one on conversational.
        let mgr = bare_manager_with_local_only(vec![]).await;
        let views = mgr.views.read().await;
        let personal = views
            .get(VIEW_PERSONAL_KNOWLEDGE)
            .expect("personal view present")
            .lock
            .clone();
        let conversation = views
            .get(VIEW_CONVERSATION_HISTORY)
            .expect("conversation view present")
            .lock
            .clone();
        let institutional = views
            .get(VIEW_INSTITUTIONAL_NOTES)
            .expect("institutional view present")
            .lock
            .clone();
        assert!(!Arc::ptr_eq(&personal, &conversation));
        assert!(!Arc::ptr_eq(&personal, &institutional));
        assert!(!Arc::ptr_eq(&conversation, &institutional));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn digest_cache_hit_skips_refetch_when_mtime_unchanged() {
        // Two back-to-back calls must produce string-equal output.
        // That's necessary for correctness regardless of cache, but
        // it's the minimal property the cache must preserve.
        let mgr = bare_manager_with_local_only(vec![]).await;
        let a = mgr
            .landscape_digest(VIEW_PERSONAL_KNOWLEDGE, None, 300)
            .await;
        let b = mgr
            .landscape_digest(VIEW_PERSONAL_KNOWLEDGE, None, 300)
            .await;
        match (a, b) {
            (Ok(x), Ok(y)) => assert_eq!(x, y, "repeat call must be stable"),
            (Err(_), Err(_)) => { /* both fail identically — fine for unenriched view */ }
            _ => panic!("one call succeeded and the other didn't — cache inconsistency"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalidate_digest_cache_drops_entries_for_view() {
        let mgr = bare_manager_with_local_only(vec![]).await;
        {
            let mut cache = mgr.digest_cache.write().await;
            cache.insert(
                (VIEW_PERSONAL_KNOWLEDGE.to_string(), 300),
                CachedDigest {
                    skeleton_mtime: std::time::SystemTime::now(),
                    body: "cached".into(),
                },
            );
            cache.insert(
                (VIEW_CONVERSATION_HISTORY.to_string(), 200),
                CachedDigest {
                    skeleton_mtime: std::time::SystemTime::now(),
                    body: "cached-conv".into(),
                },
            );
            assert_eq!(cache.len(), 2);
        }
        mgr.invalidate_digest_cache(VIEW_PERSONAL_KNOWLEDGE).await;
        let cache = mgr.digest_cache.read().await;
        assert_eq!(cache.len(), 1);
        assert!(
            cache.contains_key(&(VIEW_CONVERSATION_HISTORY.to_string(), 200)),
            "sibling view's cache entry must survive targeted invalidation"
        );
        assert!(
            !cache.contains_key(&(VIEW_PERSONAL_KNOWLEDGE.to_string(), 300)),
            "target view's cache entry must be dropped"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_starts_from_none_and_ends_with_some() {
        let mgr = bare_manager_with_local_only(vec![]).await;
        let mut ctx = tmp_context();
        assert!(ctx.knowledge_view_digests.is_none());
        mgr.splice_into(&mut ctx, None).await;
        assert!(
            ctx.knowledge_view_digests.is_some(),
            "splice_into always populates the field"
        );
    }

    #[test]
    fn combined_landscape_digest_budget_is_bounded() {
        // Spec §11 budget ceiling: personal (300) + conversational
        // (200) stays within 500 tokens combined.
        let sk = fixture_skeleton();
        let personal = format_landscape(&sk, VIEW_PERSONAL_KNOWLEDGE, 300);
        let conversational = format_landscape(&sk, VIEW_CONVERSATION_HISTORY, 200);
        let combined_tokens = estimate_tokens(&personal) + estimate_tokens(&conversational);
        assert!(
            combined_tokens <= 500,
            "combined digest exceeds 500-token budget: {combined_tokens} tokens"
        );
    }

    #[test]
    fn legacy_view_id_constants_match_view_kind() {
        // Guard rail against accidentally decoupling the legacy
        // string constants from the ViewKind source of truth.
        assert_eq!(VIEW_PERSONAL_KNOWLEDGE, ViewKind::Personal.id());
        assert_eq!(VIEW_CONVERSATION_HISTORY, ViewKind::Conversational.id());
        assert_eq!(VIEW_INSTITUTIONAL_NOTES, ViewKind::Institutional.id());
        assert_eq!(VIEW_CROSS_VIEW, ViewKind::CrossView.id());
    }

    // ── Phase 4.B splice integration ────────────────────────────────────
    //
    // Verifies that when a personal-knowledge atlas is on disk and a
    // memories table backs the chunk_ids, the splice path produces
    // both Relational and Strategic digest blocks, and that they're
    // suppressed on a local_only active skill.

    #[cfg(feature = "treesitter")]
    fn seed_personal_atlas_with_entities(indexes_dir: &std::path::Path) {
        use corpus_engine::enrichment::atlas::atoms::{
            AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity,
        };
        use corpus_engine::enrichment::atlas::edges::{
            Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile,
        };
        use corpus_engine::enrichment::atlas::writer::ATLAS_DIRNAME;
        use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

        let atlas_dir = indexes_dir.join("personal-knowledge").join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();

        let sarah = Entity {
            id: AtomId::entity(1),
            canonical_name: "Sarah Chen".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("mem-1".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: Some("Acme Corp".into()),
            role: Some("VP Eng".into()),
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        let api_migration = Entity {
            id: AtomId::entity(2),
            canonical_name: "API migration".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Initiative,
            first_appearance: ChunkRef::new("mem-1".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: vec![AtomId::entity(1)],
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };

        let edge_sarah = Edge {
            id: EdgeId::new(1),
            edge_type: EdgeType::Involves,
            source: AtomId::from_raw("chunk-mem-1".to_string()),
            target: sarah.id.clone(),
            evidence: vec![ChunkRef::new("mem-1".to_string(), None)],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        let edge_init = Edge {
            id: EdgeId::new(2),
            edge_type: EdgeType::Involves,
            source: AtomId::from_raw("chunk-mem-1".to_string()),
            target: api_migration.id.clone(),
            evidence: vec![ChunkRef::new("mem-1".to_string(), None)],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };

        let atoms_file = AtomsFile::new(vec![
            AtomEnvelope::Entity(sarah),
            AtomEnvelope::Entity(api_migration),
        ]);
        let edges_file = EdgesFile::new(vec![edge_sarah, edge_init]);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_string(&atoms_file).unwrap(),
        )
        .unwrap();
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_string(&edges_file).unwrap(),
        )
        .unwrap();
    }

    #[cfg(feature = "treesitter")]
    fn seed_memories_table(db_path: &std::path::Path) {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                last_used INTEGER NOT NULL,
                deleted_at INTEGER
            );
            CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER
            );
            INSERT INTO memories VALUES ('mem-1', 'note about Sarah', 1700000000, NULL);
            ",
        )
        .unwrap();
    }

    #[cfg(feature = "treesitter")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_emits_relational_and_strategic_blocks_when_atlas_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let indexes_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        let db_path = tmp.path().join("sovereign.db");
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();
        seed_memories_table(&db_path);
        seed_personal_atlas_with_entities(&indexes_dir);

        let embed: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) })
        });
        let infer: corpus_engine::InferenceFn =
            std::sync::Arc::new(|_, _: Option<&serde_json::Value>| {
                Box::pin(async { Ok::<String, corpus_engine::Error>("{}".into()) })
            });
        let engine = std::sync::Arc::new(corpus_engine::CorpusEngine::new(
            recipes_dir,
            indexes_dir.clone(),
            embed,
        ));
        let mgr =
            KnowledgeViewManager::new(engine, infer, db_path, vec!["inner-work".into()]).await;

        let mut ctx = tmp_context();
        mgr.splice_into(&mut ctx, Some("research-analyst")).await;
        let digests = ctx.knowledge_view_digests.unwrap();
        let by_id: std::collections::HashMap<&str, &str> = digests
            .iter()
            .map(|d| (d.view_id.as_str(), d.body.as_str()))
            .collect();

        let relational = by_id
            .get(ViewKind::Relational.id())
            .expect("relational block expected");
        assert!(
            relational.contains("Sarah Chen"),
            "relational digest should name the Person atom: {relational}"
        );

        let strategic = by_id
            .get(ViewKind::Strategic.id())
            .expect("strategic block expected");
        assert!(
            strategic.contains("API migration"),
            "strategic digest should name the Initiative atom: {strategic}"
        );
    }

    #[cfg(feature = "treesitter")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_suppresses_relational_and_strategic_for_local_only_skill() {
        // Same atlas + DB seeding as above, but the active skill is
        // marked local_only. Both blocks must drop alongside
        // conversational + institutional.
        let tmp = tempfile::TempDir::new().unwrap();
        let indexes_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        let db_path = tmp.path().join("sovereign.db");
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();
        seed_memories_table(&db_path);
        seed_personal_atlas_with_entities(&indexes_dir);

        let embed: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) })
        });
        let infer: corpus_engine::InferenceFn =
            std::sync::Arc::new(|_, _: Option<&serde_json::Value>| {
                Box::pin(async { Ok::<String, corpus_engine::Error>("{}".into()) })
            });
        let engine = std::sync::Arc::new(corpus_engine::CorpusEngine::new(
            recipes_dir,
            indexes_dir,
            embed,
        ));
        let mgr =
            KnowledgeViewManager::new(engine, infer, db_path, vec!["inner-work".into()]).await;

        let mut ctx = tmp_context();
        mgr.splice_into(&mut ctx, Some("inner-work")).await;
        let digests = ctx.knowledge_view_digests.unwrap();
        assert!(
            !digests
                .iter()
                .any(|d| d.view_id == ViewKind::Relational.id()),
            "relational digest must be suppressed for local_only active skill"
        );
        assert!(
            !digests
                .iter()
                .any(|d| d.view_id == ViewKind::Strategic.id()),
            "strategic digest must be suppressed for local_only active skill"
        );
    }
}
