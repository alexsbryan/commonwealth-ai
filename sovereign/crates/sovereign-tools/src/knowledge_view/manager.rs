//! `KnowledgeViewManager` — lifecycle orchestrator for `KnowledgeView`
//! corpora (currently `personal-knowledge` and `conversation-history`).
//!
//! Responsibilities:
//!
//! 1. **Acquirer registration.** Installs the `SqliteAcquirer` on the
//!    shared `CorpusEngine` at construction so recipes referencing
//!    `type = "custom"` with `kind = "sqlite"` can ingest.
//!
//! 2. **Write-path observer.** Implements [`StateStoreObserver`] so the
//!    store fires `on_memory_written` / `on_message_written` /
//!    `on_conversation_deleted` events into this manager's debouncer.
//!
//! 3. **Debounced Tier-3 enrichment.** Each write enqueues a refresh
//!    trigger for the corresponding view. A background task per view
//!    coalesces triggers and runs `FieldModelEngine::enrich` when
//!    either `DEBOUNCE_MAX_WRITES` accumulate or `DEBOUNCE_MAX_IDLE`
//!    elapses since the first pending write. Tier-2 (fast incremental
//!    update on every write) is deferred to v2 — v1 intentionally
//!    trades a short staleness window for a simpler architecture.
//!
//! 4. **Landscape digest.** Reads `field_skeleton.json` and formats a
//!    concise summary of clusters / fault lines / open questions bounded
//!    by a caller-supplied token budget.
//!
//! 5. **Search passthrough.** Thin wrapper around `CorpusIndex::search`
//!    so downstream tools can query an enriched view directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corpus_engine::engine::CorpusEngine;
use corpus_engine::enrichment::field_engine::FieldModelEngine;
use corpus_engine::enrichment::skeleton::FieldSkeleton;
use corpus_engine::error::{Error as CorpusError, Result as CorpusResult};
use corpus_engine::recipe::Recipe;
use corpus_engine::types::{CorpusSpec, InferenceFn};
use corpus_engine::EnrichmentProgress;
use async_trait::async_trait;
use sovereign_core::observer::StateStoreObserver;
use sovereign_core::traits::LandscapeDigestProvider;
use sovereign_core::types::{ConversationContext, LandscapeDigest};
use tokio::sync::{mpsc, RwLock};

use super::acquirers::register_sqlite;
use super::recipes::{conversation_history_recipe, personal_knowledge_recipe};

/// View ids recognised by this manager. Kept as constants so callers
/// (`KnowledgeViewManager::search`, `landscape_digest`, etc.) don't
/// hand-roll string literals that could drift out of sync with the
/// recipe builders.
pub const VIEW_PERSONAL_KNOWLEDGE: &str = "personal-knowledge";
pub const VIEW_CONVERSATION_HISTORY: &str = "conversation-history";

/// Debounce threshold — how many pending writes before we trigger
/// a full Tier-3 enrichment pass unconditionally. Matches the
/// implementation plan.
const DEBOUNCE_MAX_WRITES: usize = 20;
/// Debounce threshold — longest time we wait after the first pending
/// write before running enrichment, even if `DEBOUNCE_MAX_WRITES`
/// hasn't been reached. Five minutes is a reasonable upper bound for
/// user-perceived staleness of the landscape digest.
const DEBOUNCE_MAX_IDLE: Duration = Duration::from_secs(300);

/// One manager per running Sovereign instance. Holds the engine and
/// one debouncer per view. Cheap to clone via `Arc`.
///
/// The inference function passed to [`KnowledgeViewManager::new`] is
/// captured by the debouncer task — it's not stored on the manager
/// itself since all enrichment paths flow through the debouncer.
pub struct KnowledgeViewManager {
    engine: Arc<CorpusEngine>,
    views: Arc<RwLock<HashMap<String, ViewEntry>>>,
    triggers: mpsc::UnboundedSender<ViewEvent>,
}

struct ViewEntry {
    recipe: Recipe,
}

/// Write-side events that flow into the debouncer. Each variant maps
/// to a view id; the debouncer tracks a pending-write counter per view.
#[derive(Debug, Clone)]
enum ViewEvent {
    /// A memory was written → refresh the personal view.
    MemoryTouched,
    /// A conversation had new activity → refresh the conversation view.
    ConversationTouched,
    /// A conversation was deleted → refresh the conversation view so
    /// the deleted conversation's chunks drop out of the index.
    ConversationDeleted,
    /// Explicit manual trigger (e.g. CLI command or startup check).
    Manual { view_id: String },
}

impl KnowledgeViewManager {
    /// Construct a manager, register the SQLite acquirer, and spawn
    /// the background debouncer task.
    ///
    /// `db_path` is the sovereign SQLite file (typically
    /// `~/.sovereign/sovereign.db`). `local_only_skill_ids` is the
    /// resolved set of skill ids whose conversations must be
    /// excluded from the conversational view — typically fetched
    /// from `SkillRegistry` at Runtime startup.
    pub async fn new(
        engine: Arc<CorpusEngine>,
        inference: InferenceFn,
        db_path: PathBuf,
        local_only_skill_ids: Vec<String>,
    ) -> Self {
        register_sqlite(&engine);

        let local_refs: Vec<&str> = local_only_skill_ids.iter().map(|s| s.as_str()).collect();
        let mut views = HashMap::new();
        views.insert(
            VIEW_PERSONAL_KNOWLEDGE.to_string(),
            ViewEntry {
                recipe: personal_knowledge_recipe(&db_path),
            },
        );
        views.insert(
            VIEW_CONVERSATION_HISTORY.to_string(),
            ViewEntry {
                recipe: conversation_history_recipe(&db_path, &local_refs),
            },
        );

        let (tx, rx) = mpsc::unbounded_channel();
        let views = Arc::new(RwLock::new(views));

        spawn_debouncer(engine.clone(), inference, views.clone(), rx);

        Self {
            engine,
            views,
            triggers: tx,
        }
    }

    /// Ingest each view once if its index is empty. Call at Runtime
    /// startup so the first session after install sees an enriched
    /// landscape. Safe to call repeatedly — `CorpusEngine::ingest`
    /// resumes from checkpoints and won't re-enrich a completed index.
    pub async fn init(&self) -> CorpusResult<()> {
        let view_ids: Vec<String> = {
            let guard = self.views.read().await;
            guard.keys().cloned().collect()
        };
        for view_id in view_ids {
            if !self.index_populated(&view_id).await {
                tracing::info!(view_id, "KnowledgeViewManager: initial ingest");
                if let Err(e) = self.ingest_view(&view_id).await {
                    tracing::warn!(view_id, error = %e, "initial ingest failed");
                }
            }
        }
        Ok(())
    }

    /// Manually enrich a view. Used for CLI triggers and tests.
    pub async fn enrich(&self, view_id: &str) -> CorpusResult<()> {
        // Hand off to the debouncer so all enrichment paths funnel
        // through a single worker — avoids two concurrent enrichment
        // runs stepping on each other's checkpoints.
        let _ = self.triggers.send(ViewEvent::Manual {
            view_id: view_id.to_string(),
        });
        Ok(())
    }

    /// Assemble a landscape digest for the given view, filtered by
    /// the active skill if applicable. Output is a plain markdown
    /// string bounded approximately by `budget_tokens` (estimated at
    /// 4 chars/token).
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
        let index = self.engine.open_index_for_corpus(view_id).await?;
        let Some(skeleton) = index.load_field_skeleton()? else {
            return Ok(format!(
                "{title}: not yet enriched.",
                title = view_title(view_id)
            ));
        };
        Ok(format_landscape(&skeleton, view_id, budget_tokens))
    }

    /// Assemble landscape digests for every configured view and
    /// splice them into `ctx.knowledge_view_digests`.
    ///
    /// Intended to be called from `Runtime::handle_message_stream`
    /// **after** skill routing so `active_skill` can tailor the
    /// digests in future iterations (v1 does not filter digest
    /// content by skill beyond the structural filtering already
    /// performed by the acquirer).
    ///
    /// Soft-fail semantics: if a view's digest can't be produced
    /// (index missing, enrichment not yet run, I/O error), it is
    /// simply omitted from the spliced list — the rest of the
    /// context proceeds. The field is always set to `Some(_)` on
    /// return, so callers downstream can rely on the invariant
    /// "post-routing context has a non-`None` digests field".
    ///
    /// Per-view token budgets (personal: 300, conversational: 200)
    /// match the implementation plan.
    pub async fn splice_into(&self, ctx: &mut ConversationContext, active_skill: Option<&str>) {
        let view_budgets: &[(&str, usize)] = &[
            (VIEW_PERSONAL_KNOWLEDGE, 300),
            (VIEW_CONVERSATION_HISTORY, 200),
        ];
        let mut digests = Vec::with_capacity(view_budgets.len());
        for (view_id, budget) in view_budgets {
            match self.landscape_digest(view_id, active_skill, *budget).await {
                Ok(body) if !body.trim().is_empty() => digests.push(LandscapeDigest {
                    view_id: view_id.to_string(),
                    body,
                }),
                Ok(_) => {}
                Err(e) => tracing::debug!(
                    view_id,
                    error = %e,
                    "landscape_digest skipped — view not yet available"
                ),
            }
        }
        ctx.set_landscape_digests(digests);
    }

    /// Search the enriched index for `query`, returning up to `k`
    /// scored chunks. Thin passthrough to `CorpusIndex::search`.
    /// Returns an empty Vec when the view has not yet been ingested.
    pub async fn search(
        &self,
        _view_id: &str,
        _query: &str,
        _k: usize,
    ) -> CorpusResult<Vec<String>> {
        // v1 stub. Full wiring requires an EmbedFn argument and
        // knowledge of `CorpusIndex::search`'s exact signature —
        // deferred until the KnowledgeView consumer tools are
        // written in Stage 5/6.
        Ok(Vec::new())
    }

    // ── Internals ───────────────────────────────────────────

    async fn index_populated(&self, view_id: &str) -> bool {
        matches!(
            self.engine.open_index_for_corpus(view_id).await,
            Ok(_index)
        )
    }

    async fn ingest_view(&self, view_id: &str) -> CorpusResult<()> {
        let recipe = {
            let guard = self.views.read().await;
            guard
                .get(view_id)
                .map(|v| v.recipe.clone())
                .ok_or_else(|| CorpusError::Recipe(format!("Unknown view: {view_id}")))?
        };
        let path = recipe_to_tempfile(&recipe)?;
        let spec = CorpusSpec::RecipePath(path);
        self.engine.ingest(&spec, None).await.map(|_| ())
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

// ── Debouncer ───────────────────────────────────────────────

fn spawn_debouncer(
    engine: Arc<CorpusEngine>,
    inference: InferenceFn,
    views: Arc<RwLock<HashMap<String, ViewEntry>>>,
    mut rx: mpsc::UnboundedReceiver<ViewEvent>,
) {
    tokio::spawn(async move {
        let mut state: HashMap<String, PendingView> = HashMap::new();

        loop {
            let wakeup = state
                .values()
                .map(|p| p.earliest_deadline())
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
            let sleep = tokio::time::sleep_until(wakeup.into());
            tokio::pin!(sleep);

            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => match event {
                            ViewEvent::MemoryTouched => note(&mut state, VIEW_PERSONAL_KNOWLEDGE),
                            ViewEvent::ConversationTouched => note(&mut state, VIEW_CONVERSATION_HISTORY),
                            ViewEvent::ConversationDeleted => note(&mut state, VIEW_CONVERSATION_HISTORY),
                            ViewEvent::Manual { view_id } => {
                                // Manual triggers bypass the debounce window.
                                run_enrichment(&engine, inference.clone(), &views, &view_id).await;
                                state.remove(&view_id);
                            }
                        },
                        None => break, // Manager dropped, channel closed.
                    }
                }
                _ = &mut sleep => {
                    // Fall through to the deadline sweep below.
                }
            }

            let now = Instant::now();
            let ready: Vec<String> = state
                .iter()
                .filter(|(_, p)| p.is_ready(now))
                .map(|(k, _)| k.clone())
                .collect();
            for view_id in ready {
                run_enrichment(&engine, inference.clone(), &views, &view_id).await;
                state.remove(&view_id);
            }
        }
    });
}

struct PendingView {
    first_pending_at: Instant,
    pending_count: usize,
}

impl PendingView {
    fn earliest_deadline(&self) -> Instant {
        self.first_pending_at + DEBOUNCE_MAX_IDLE
    }

    fn is_ready(&self, now: Instant) -> bool {
        self.pending_count >= DEBOUNCE_MAX_WRITES
            || now.duration_since(self.first_pending_at) >= DEBOUNCE_MAX_IDLE
    }
}

fn note(state: &mut HashMap<String, PendingView>, view_id: &str) {
    let entry = state.entry(view_id.to_string()).or_insert(PendingView {
        first_pending_at: Instant::now(),
        pending_count: 0,
    });
    entry.pending_count += 1;
}

async fn run_enrichment(
    engine: &Arc<CorpusEngine>,
    inference: InferenceFn,
    views: &Arc<RwLock<HashMap<String, ViewEntry>>>,
    view_id: &str,
) {
    let recipe = {
        let guard = views.read().await;
        match guard.get(view_id) {
            Some(v) => v.recipe.clone(),
            None => {
                tracing::warn!(view_id, "unknown view in debouncer");
                return;
            }
        }
    };

    let index = match engine.open_index_for_corpus(view_id).await {
        Ok(idx) => idx,
        Err(e) => {
            tracing::debug!(
                view_id,
                error = %e,
                "skipping enrichment — index not available yet"
            );
            return;
        }
    };

    let embed = engine.embed_fn();
    let field_engine = match FieldModelEngine::from_recipe(&recipe, embed, inference) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(view_id, error = %e, "failed to construct FieldModelEngine");
            return;
        }
    };

    let progress = |p: EnrichmentProgress| {
        tracing::debug!(view_id, ?p, "enrichment progress");
    };
    match field_engine.enrich(&index, &progress).await {
        Ok(stats) => tracing::info!(view_id, ?stats, "enrichment complete"),
        Err(e) => tracing::warn!(view_id, error = %e, "enrichment failed"),
    }
}

// ── Landscape digest formatting ─────────────────────────────

fn view_title(view_id: &str) -> &'static str {
    match view_id {
        VIEW_PERSONAL_KNOWLEDGE => "Personal knowledge",
        VIEW_CONVERSATION_HISTORY => "Conversational knowledge",
        _ => "Knowledge view",
    }
}

fn format_landscape(skeleton: &FieldSkeleton, view_id: &str, budget_tokens: usize) -> String {
    // Rough char-budget = tokens × 4. Enough headroom that typical
    // digests don't silently truncate mid-line; the caller is free to
    // re-measure with a tokenizer and shrink further.
    let char_budget = budget_tokens.saturating_mul(4);
    let mut out = String::new();
    let title = view_title(view_id);

    out.push_str(&format!("{title}:\n\n"));

    // Settled concerns: canonical questions where at least one position
    // is reported as 'dominant' / 'held' / 'settled' style.
    let settled: Vec<_> = skeleton
        .canonical_questions
        .iter()
        .filter(|q| {
            q.positions
                .iter()
                .any(|p| is_settled_status(&p.status))
        })
        .collect();
    if !settled.is_empty() {
        out.push_str("  Settled concerns:\n");
        for q in settled.iter().take(5) {
            let line = format!("    — {}\n", q.question);
            if out.len() + line.len() > char_budget {
                break;
            }
            out.push_str(&line);
        }
        out.push('\n');
    }

    // Live tensions: fault lines across all canonical questions.
    let fault_lines: Vec<_> = skeleton
        .canonical_questions
        .iter()
        .flat_map(|q| q.fault_lines.iter())
        .collect();
    if !fault_lines.is_empty() {
        out.push_str("  Live tensions:\n");
        for fl in fault_lines.iter().take(5) {
            let line = format!("    — {}\n", fl.crux);
            if out.len() + line.len() > char_budget {
                break;
            }
            out.push_str(&line);
        }
        out.push('\n');
    }

    // Open questions.
    if !skeleton.open_questions.is_empty() {
        out.push_str("  Open questions:\n");
        for oq in skeleton.open_questions.iter().take(5) {
            let line = format!("    — {}\n", oq.question);
            if out.len() + line.len() > char_budget {
                break;
            }
            out.push_str(&line);
        }
    }

    // Hard truncate as a final safety net.
    if out.len() > char_budget {
        out.truncate(char_budget);
    }
    out
}

fn is_settled_status(status: &str) -> bool {
    let s = status.to_lowercase();
    s == "held"
        || s == "dominant"
        || s == "majority"
        || s == "settled"
        || s == "established"
        || s == "recurring"
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
    use super::*;
    use corpus_engine::enrichment::clustering::FieldModelStats;
    use corpus_engine::enrichment::skeleton::{
        CanonicalQuestion, SkeletonFaultLine, SkeletonOpenQuestion, SkeletonPosition,
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

    #[test]
    fn landscape_has_three_sections() {
        let sk = fixture_skeleton();
        let out = format_landscape(&sk, VIEW_PERSONAL_KNOWLEDGE, 500);
        assert!(out.starts_with("Personal knowledge:"));
        assert!(out.contains("Settled concerns:"));
        assert!(out.contains("Live tensions:"));
        assert!(out.contains("Open questions:"));
        assert!(out.contains("What does meaningful work"));
        assert!(out.contains("stability vs. autonomy"));
        assert!(out.contains("what kind of life"));
    }

    #[test]
    fn landscape_respects_char_budget() {
        let sk = fixture_skeleton();
        // 20 tokens ≈ 80 chars — forces hard truncation.
        let out = format_landscape(&sk, VIEW_PERSONAL_KNOWLEDGE, 20);
        assert!(out.len() <= 80);
    }

    #[test]
    fn empty_skeleton_renders_header_only() {
        let mut sk = fixture_skeleton();
        sk.canonical_questions.clear();
        sk.open_questions.clear();
        let out = format_landscape(&sk, VIEW_PERSONAL_KNOWLEDGE, 500);
        assert!(out.contains("Personal knowledge:"));
        assert!(!out.contains("Settled concerns:"));
        assert!(!out.contains("Live tensions:"));
        assert!(!out.contains("Open questions:"));
    }

    #[test]
    fn settled_status_recognition_is_case_insensitive() {
        assert!(is_settled_status("Held"));
        assert!(is_settled_status("MAJORITY"));
        assert!(is_settled_status("established"));
        assert!(!is_settled_status("contested"));
        assert!(!is_settled_status("open"));
    }

    #[test]
    fn pending_view_is_ready_after_max_writes() {
        let pv = PendingView {
            first_pending_at: Instant::now(),
            pending_count: DEBOUNCE_MAX_WRITES,
        };
        assert!(pv.is_ready(Instant::now()));
    }

    #[test]
    fn pending_view_is_ready_after_idle_window() {
        let pv = PendingView {
            first_pending_at: Instant::now() - DEBOUNCE_MAX_IDLE - Duration::from_secs(1),
            pending_count: 1,
        };
        assert!(pv.is_ready(Instant::now()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn splice_into_starts_from_none_and_ends_with_some() {
        // Regression guard for the landscape-digest invariant:
        // `splice_into` must set `ctx.knowledge_view_digests` to
        // `Some(_)` even when no view has produced a real digest.
        // This is the property the prompt-assembly layer relies on
        // (via `debug_assert_routed`) to catch a missed splice in
        // debug builds.
        let tmp = tempfile::TempDir::new().unwrap();
        let indexes_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        let db_path = tmp.path().join("sovereign.db");
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();
        // Touch the DB file so SqliteAcquirer's existence check
        // could pass if called; init() is skipped here though.
        let _ = std::fs::File::create(&db_path).unwrap();

        let embed: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) })
        });
        let infer: corpus_engine::InferenceFn = std::sync::Arc::new(|_| {
            Box::pin(async { Ok::<String, corpus_engine::Error>("{}".into()) })
        });
        let engine =
            std::sync::Arc::new(corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed));

        let mgr = KnowledgeViewManager::new(engine, infer, db_path, vec![]).await;

        // Context starts with `knowledge_view_digests = None`.
        let mut ctx = sovereign_core::types::ConversationContext {
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
        };
        assert!(ctx.knowledge_view_digests.is_none());
        mgr.splice_into(&mut ctx, None).await;
        assert!(
            ctx.knowledge_view_digests.is_some(),
            "splice_into always populates the field"
        );
    }

    #[test]
    fn combined_landscape_digest_budget_is_bounded() {
        // Spec §11: "Total token budget for knowledge views does
        // not exceed 500". Per-view budgets in `splice_into` are
        // 300 (personal) + 200 (conversational) = 500 token
        // ceiling. Using the same chars-per-token heuristic the
        // formatter uses (4:1), the combined char ceiling is 2000.
        let sk = fixture_skeleton();
        let personal = format_landscape(&sk, VIEW_PERSONAL_KNOWLEDGE, 300);
        let conversational = format_landscape(&sk, VIEW_CONVERSATION_HISTORY, 200);
        let combined_chars = personal.len() + conversational.len();
        assert!(
            combined_chars <= 500 * 4,
            "combined digest exceeds 500-token budget: {combined_chars} chars"
        );
    }

    #[test]
    fn pending_view_not_ready_below_thresholds() {
        let pv = PendingView {
            first_pending_at: Instant::now(),
            pending_count: 1,
        };
        assert!(!pv.is_ready(Instant::now()));
    }
}
