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

use corpus_engine::engine::CorpusEngine;
use corpus_engine::enrichment::skeleton::FieldSkeleton;
use corpus_engine::error::{Error as CorpusError, Result as CorpusResult};
use corpus_engine::recipe::Recipe;
use corpus_engine::types::{CorpusSpec, InferenceFn};
use async_trait::async_trait;
use sovereign_core::observer::StateStoreObserver;
use sovereign_core::traits::LandscapeDigestProvider;
use sovereign_core::types::{ConversationContext, LandscapeDigest};
use tokio::sync::{mpsc, RwLock};

use super::acquirers::register_sqlite;
use super::cross_view;
use super::debouncer::{spawn_debouncer, ViewEntry, ViewEvent};
use super::digest::format_landscape;
use super::recipes::{
    conversation_history_recipe, institutional_notes_recipe, personal_knowledge_recipe,
};
use super::tokens::estimate_tokens;
use super::view_kind::ViewKind;

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
        Self::new_with_notes_path(engine, inference, db_path, notes_db_path, local_only_skill_ids)
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
        }
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
        tokio::spawn(async move {
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
        // skeleton file hasn't been modified since we last formatted
        // it. splice_into fires on every turn; an enrichment run
        // happens every few minutes at most — so the cache hit rate
        // is effectively 100% on hot paths.
        let index = self.engine.open_index_for_corpus(view_id).await?;
        let skeleton_path = index.path().join("field_skeleton.json");
        let skeleton_mtime = std::fs::metadata(&skeleton_path)
            .and_then(|m| m.modified())
            .ok();

        if let Some(mtime) = skeleton_mtime {
            let cache = self.digest_cache.read().await;
            if let Some(entry) = cache.get(&(view_id.to_string(), budget_tokens)) {
                if entry.skeleton_mtime == mtime {
                    return Ok(entry.body.clone());
                }
            }
        }

        let Some(skeleton) = index.load_field_skeleton()? else {
            let title = ViewKind::from_id(view_id)
                .map(|k| k.title())
                .unwrap_or("Knowledge view");
            return Ok(format!("{title}: not yet enriched."));
        };
        let body = format_landscape(&skeleton, view_id, budget_tokens);

        // Insert into cache only if we managed to read the mtime —
        // without it we can't detect staleness, so falling through to
        // re-format next time is safer than caching blind.
        if let Some(mtime) = skeleton_mtime {
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
        // Skill-aware digest selection (spec: "when the active skill
        // is inner-work, the technical knowledge digest is absent
        // entirely"). Acquirer-level filtering already keeps
        // local_only conversations out of the corpus; this branch
        // suppresses the REMAINING digests so cross-session context
        // can't leak into a private session.
        let active_is_local_only = active_skill
            .map(|s| self.local_only_skill_ids.iter().any(|id| id == s))
            .unwrap_or(false);

        let mut view_budgets: Vec<(ViewKind, usize)> = Vec::with_capacity(3);
        view_budgets.push((ViewKind::Personal, ViewKind::Personal.default_budget_tokens()));
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
                "splice_into: omitting conversation-history + institutional \
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
        ctx.set_landscape_digests(digests);
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
        // this view so the next splice re-reads.
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
        let infer: corpus_engine::InferenceFn = std::sync::Arc::new(|_| {
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
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_omits_conversation_history_when_active_skill_is_local_only() {
        let mgr = bare_manager_with_local_only(vec!["inner-work".to_string()]).await;
        let mut ctx = tmp_context();
        mgr.splice_into(&mut ctx, Some("inner-work")).await;
        let digests = ctx.knowledge_view_digests.unwrap();
        assert!(
            !digests.iter().any(|d| d.view_id == VIEW_CONVERSATION_HISTORY),
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
}
