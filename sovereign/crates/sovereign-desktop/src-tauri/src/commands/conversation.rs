// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

#[tauri::command]
pub async fn create_conversation(
    state: State<'_, Arc<AppState>>,
    surface_skill_id: Option<String>,
) -> Result<CreateConversationResponse, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = now_epoch();

    // Persist the conversation row with its surface tag NOW so the
    // first chat dispatch already knows which surface created this
    // conversation. Pre-2026-05-24 this was a lazy create — the row
    // appeared only when the first message was saved, and the
    // runtime auto-tagged with whatever was in
    // `SkillRegistry::primary_skill_id_for_conversation()` at dispatch
    // time. That coupled routing to global mutable registry state +
    // required every workspace surface to toggle skills via
    // `rebuild_runtime` on mount/destroy (15s × N rebuilds). With
    // the surface declaring its skill at create-time, routing
    // becomes a stateless per-turn lookup and the lifecycle glue
    // disappears.
    //
    // `surface_skill_id == None` is the default-chat case: no
    // workspace tag, routing follows intent-derived policy.
    if let Some(sqlite) = state.sqlite_store.read().await.as_ref() {
        sqlite
            .insert_empty_conversation(&id, created_at, surface_skill_id.as_deref())
            .await
            .map_err(|e| format!("create_conversation insert: {e}"))?;
    } else {
        // Sqlite store unavailable (early boot, IO error). Fall
        // through: the conversation row still gets created lazily
        // on first message save and runtime's older auto-tag path
        // handles attribution best-effort.
        tracing::warn!("create_conversation: sqlite store unavailable, deferring insert");
    }

    Ok(CreateConversationResponse { id, created_at })
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    offset: Option<usize>,
    surface_skill_id: Option<String>,
) -> Result<Vec<ConversationEntry>, String> {
    let _guard = require_runtime!(state);
    // Surface-scoped listing: each surface only sees its own
    // conversations. The default-chat sidebar passes `None` and
    // gets back only conversations with `skill_id IS NULL`; the
    // Inner Work history drawer passes `Some("inner-work")`;
    // Recipe Author passes `Some("recipe-author")`. No "all
    // conversations" mode — cross-surface visibility is structurally
    // restricted (2026-05-24 architecture redesign).
    let convos = if let Some(sqlite) = state.sqlite_store.read().await.as_ref() {
        sqlite
            .list_conversations_for_surface(
                surface_skill_id.as_deref(),
                limit.unwrap_or(50),
                offset.unwrap_or(0),
            )
            .await
            .map_err(|e| e.to_string())?
    } else {
        return Err("list_conversations: sqlite store unavailable".to_string());
    };

    Ok(convos
        .into_iter()
        .map(|c| ConversationEntry {
            id: c.id,
            title: c.title,
            created_at: c.created_at,
            updated_at: c.updated_at,
        })
        .collect())
}

/// List the conversations scoped to one notebook (corpus), newest first
/// — the notebook's Ask-tab history. Default-chat surface only;
/// "everything"-scoped conversations are excluded (see
/// `SqliteStateStore::list_conversations_for_corpus`).
#[tauri::command]
pub async fn notebook_conversations(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<ConversationEntry>, String> {
    let _guard = require_runtime!(state);
    let convos = if let Some(sqlite) = state.sqlite_store.read().await.as_ref() {
        sqlite
            .list_conversations_for_corpus(&corpus_id, limit.unwrap_or(20), offset.unwrap_or(0))
            .await
            .map_err(|e| e.to_string())?
    } else {
        return Err("notebook_conversations: sqlite store unavailable".to_string());
    };

    Ok(convos
        .into_iter()
        .map(|c| ConversationEntry {
            id: c.id,
            title: c.title,
            created_at: c.created_at,
            updated_at: c.updated_at,
        })
        .collect())
}

#[tauri::command]
pub async fn get_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<ConversationDetail, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let convo = runtime
        .store
        .get_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())?;

    Ok(ConversationDetail {
        id: conversation_id,
        title: convo.title,
        messages: convo
            .messages
            .into_iter()
            .map(|m| {
                let role = m.role_str().to_string();
                MessageEntry {
                    id: m.id,
                    role,
                    content: m.content,
                    created_at: m.created_at,
                    metadata: m.metadata,
                }
            })
            .collect(),
        created_at: convo.created_at,
        updated_at: convo.updated_at,
        enabled_corpora: convo.enabled_corpora,
    })
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    runtime
        .store
        .delete_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rename_conversation(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("Title cannot be empty".to_string());
    }
    // Guard against unreasonably long titles.
    let title = if trimmed.chars().count() > 200 {
        trimmed.chars().take(200).collect::<String>()
    } else {
        trimmed.to_string()
    };

    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    runtime
        .store
        .update_conversation_title(&conversation_id, &title)
        .await
        .map_err(|e| e.to_string())?;

    let _ = app_handle.emit("conversations:changed", ());
    Ok(())
}

/// Persist the per-conversation corpus allow-list — the user-toggled
/// set of parent corpus_ids that retrieval is allowed to search for
/// this conversation. `None` clears the column ("all installed"),
/// `Some(vec)` writes the explicit subset. Layer/satellite corpora
/// follow their parent at retrieval time, so the allow-list only
/// needs parent ids. See `Conversation::enabled_corpora` for the
/// full contract. Called by `CorpusFilterStrip.svelte` whenever the
/// user toggles a chip.
#[tauri::command]
pub async fn set_conversation_enabled_corpora(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    enabled_corpora: Option<Vec<String>>,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    runtime
        .store
        .set_conversation_enabled_corpora(&conversation_id, enabled_corpora)
        .await
        .map_err(|e| e.to_string())?;

    let _ = app_handle.emit("conversations:changed", ());
    Ok(())
}

#[tauri::command]
pub async fn search_messages(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<SearchResult>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let messages = runtime
        .store
        .search_messages(&query)
        .await
        .map_err(|e| e.to_string())?;

    Ok(messages
        .into_iter()
        .take(50)
        .map(|m| SearchResult {
            content: m.content,
            conversation_id: m.conversation_id,
        })
        .collect())
}

#[tauri::command]
pub async fn submit_approval(
    state: State<'_, Arc<AppState>>,
    key: String,
    approved: bool,
) -> Result<bool, String> {
    Ok(state.approval.submit_approval(&key, approved).await)
}

#[tauri::command]
pub async fn submit_input(
    state: State<'_, Arc<AppState>>,
    key: String,
    response: String,
) -> Result<bool, String> {
    Ok(state.approval.submit_input(&key, response).await)
}

/// Resolve a pending information-request the agent surfaced via an
/// `AwaitUserInfo` step. `content = None` means the user pressed skip;
/// `Some(text)` means they pasted a passage / paragraph / source.
/// Returns true when the key was matched, false when no pending request
/// exists for that key (e.g. stale UI submission).
#[tauri::command]
pub async fn submit_information_response(
    state: State<'_, Arc<AppState>>,
    key: String,
    content: Option<String>,
) -> Result<bool, String> {
    Ok(state
        .approval
        .submit_information_response(&key, content)
        .await)
}

/// Per-source provenance row returned to the desktop when the search
/// affordance succeeds. The frontend stashes the list on the message
/// that's about to be refined so the post-refine bubble can render
/// "Augmented via web search: <query> (N sources)" with each URL
/// clickable. Mirrors `SearchResult` minus the snippet, which the
/// model already absorbs through the formatted-results paste.
#[derive(Serialize, Clone)]
pub struct SearchAugmentationSource {
    pub title: String,
    pub url: String,
}

/// What `submit_information_search` returns when the search succeeds
/// AND the runtime accepts the resolution. The frontend correlates
/// this with the next `message-refined` event for the same
/// conversation to attach search provenance to the refined bubble.
#[derive(Serialize, Clone)]
pub struct SearchAugmentation {
    pub query: String,
    pub backend_id: String,
    pub sources: Vec<SearchAugmentationSource>,
    /// Whether the runtime accepted the resolution. `false` here
    /// means the channel was already resolved between the
    /// `has_pending_information` probe and the resolve call (rare
    /// race — the frontend should ignore the augmentation in that
    /// case rather than render orphaned provenance).
    pub accepted: bool,
}

/// Resolve a pending information-request by running a web search and
/// feeding the formatted results back as if the user had pasted them.
/// Powers the InformationRequest "Search the web" affordance — the
/// user is operator-vouching that the search itself is acceptable
/// evidence for re-synthesis, mirroring the paste flow's contract.
///
/// Returns `SearchAugmentation` on success so the frontend can render
/// the search provenance on the refined bubble; the runtime itself
/// still sees an `Option<String>` (the formatted paste-shaped block)
/// and runs the existing post-stream refinement path. Splitting the
/// metadata out as a Tauri return value avoids changing the
/// `ApprovalChannel` trait or the runtime's refinement contract
/// just to surface "this refine was search-sourced" in the UI.
///
/// Builds a fresh `SearchOrchestrator` per call from the persisted
/// `config.search_backend`. This mirrors `state.rs` build-tools
/// logic intentionally — the orchestrator is cheap to construct
/// (wraps stateless backend trait objects) and rebuilding here
/// keeps the affordance live against config edits without needing
/// to thread a long-lived handle through `AppState`.
///
/// Returns an error string when:
///   - no pending information request matches `key` (stale UI)
///   - the search backend returns zero results (don't fabricate a
///     "search succeeded" signal back to the runtime)
///   - the search backend errors entirely (network / API failure)
// `conversation_id` (Option<String>) is the active conversation for
// the Tool-Mastery `tool_decision` write. When `Some`, the runtime's
// per-conversation dossier pre-pass surfaces the prior unsuccessful
// lookup on the next turn. `None` falls back to a global write that
// won't filter into any single conversation's dossier.
#[tauri::command]
pub async fn submit_information_search(
    state: State<'_, Arc<AppState>>,
    key: String,
    query: String,
    conversation_id: Option<String>,
) -> Result<SearchAugmentation, String> {
    use sovereign_tools::web::search::{
        BraveBackendImpl, BudgetView, DuckDuckGoBackendImpl, SearchOrchestrator, SearchPrivacy,
        SelectInputs, TavilyBackendImpl, WebSearchBackend, WebSearchRegistry,
    };

    let query = query.trim();
    if query.is_empty() {
        return Err("query must not be empty".to_string());
    }

    if !state.approval.has_pending_information(&key).await {
        // Stale submission — the request was already resolved
        // (paste / skip / timed out). Don't spend a search budget.
        return Err("no pending information request for this key".to_string());
    }

    // Tool-Mastery Layer 3 — the click itself IS the user telling
    // us the prior tool didn't satisfy. Write that outcome BEFORE
    // the web search runs (regardless of whether the search will
    // succeed) so the next turn's dossier surfaces "the
    // in-conversation lookup came up short and the user reached
    // for the external escape hatch." Soft-fail: missing NoteStore
    // is silently skipped. See `dossier::record_tool_outcome`.
    {
        let notes_guard = state.notes.read().await;
        let notes_ref: Option<&corpus_engine_notes::NoteStore> =
            notes_guard.as_ref().map(|arc| arc.as_ref());
        sovereign_core::dossier::record_tool_outcome(
            notes_ref,
            // `key` is a per-conversation-turn opaque id (see
            // approval::TauriApprovalChannel) — using it as the
            // session-id proxy keeps the audit trail traceable
            // back to the originating INFORMATION REQUEST card.
            &key,
            conversation_id.as_deref(),
            "knowledge_lookup",
            sovereign_core::memory::ToolDecisionOutcome::NoResults,
            "user clicked Search-the-web on the INFORMATION REQUEST card \
             — prior in-conversation lookup did not satisfy",
            // Tier 1: no summary/evidence_ids/turn_index — this
            // write fires from a USER click, not a tool-result
            // post-stream hook. The originating turn's baseline
            // write (from the runtime's KQ dispatch) already
            // carries those fields; this is an audit overlay.
            sovereign_core::memory::ToolDecisionExtras::none(),
        )
        .await;
    }

    let config_snapshot = state.config.read().await.clone();

    let mut registry = WebSearchRegistry::new();
    // DuckDuckGo is always available — the zero-config fallback.
    // Registered first so even a missing operator key still has
    // a backend to dispatch to.
    registry.register(Arc::new(DuckDuckGoBackendImpl::new()));
    let preferred: Box<dyn WebSearchBackend> =
        match config_snapshot.search_backend.provider.as_str() {
            "tavily" => config_snapshot.search_backend.api_key.as_ref().map(
                |k| -> Box<dyn WebSearchBackend> { Box::new(TavilyBackendImpl::new(k.clone())) },
            ),
            "brave" => config_snapshot.search_backend.api_key.as_ref().map(
                |k| -> Box<dyn WebSearchBackend> { Box::new(BraveBackendImpl::new(k.clone())) },
            ),
            _ => None,
        }
        .unwrap_or_else(|| Box::new(DuckDuckGoBackendImpl::new()));
    registry.register(Arc::from(preferred));

    let orchestrator = SearchOrchestrator::new(Arc::new(registry));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;
    let budget = BudgetView::new();
    let prefer = match config_snapshot.search_backend.provider.as_str() {
        "tavily" => &["tavily", "duckduckgo"][..],
        "brave" => &["brave", "duckduckgo"][..],
        _ => &["duckduckgo"][..],
    };
    // Glassbox (§9): record the backend decision + query *length* (never
    // the query text, §9.3) so a stuck search is diagnosable from logs.
    tracing::info!(
        provider = %config_snapshot.search_backend.provider,
        query_len = query.len(),
        "submit_information_search: dispatching web search"
    );
    let out = orchestrator
        .search(
            &client,
            SelectInputs {
                query,
                max_results: 5,
                max_privacy: SearchPrivacy::External {
                    provider: "duckduckgo",
                },
                budget: &budget,
                prefer,
            },
        )
        .await;

    if out.results.is_empty() {
        tracing::warn!(
            backend_id = %out.backend_id,
            query_len = query.len(),
            "submit_information_search: backend returned 0 results"
        );
        // Treat as a soft failure surfaced to the UI. The pending
        // request stays open so the user can paste / skip / retry
        // with a tighter query without rebuilding the card.
        return Err(format!(
            "web search returned 0 results via {} (DDG may be bot-blocking; \
             try a tighter query or paste a source instead)",
            out.backend_id,
        ));
    }

    tracing::info!(
        backend_id = %out.backend_id,
        results = out.results.len(),
        query_len = query.len(),
        "submit_information_search: synthesizing from results"
    );

    // Format as a paste-shaped block so the runtime's re-synthesis
    // path treats this identically to user-pasted content. Each
    // entry is numbered (matches the gym runner's tool-result shape
    // that the URL-allowlist constraint was trained against).
    let mut formatted = format!(
        "Web search results for \"{}\" (via {}):\n\n",
        query, out.backend_id
    );
    for (i, r) in out.results.iter().enumerate() {
        formatted.push_str(&format!("[{}] {}\n    {}\n", i + 1, r.title, r.url));
        if !r.snippet.is_empty() {
            formatted.push_str(&format!("    {}\n", r.snippet));
        }
        formatted.push('\n');
    }

    let sources: Vec<SearchAugmentationSource> = out
        .results
        .iter()
        .map(|r| SearchAugmentationSource {
            title: r.title.clone(),
            url: r.url.clone(),
        })
        .collect();

    // Marathon-graceful M3 — fold the new URLs into the conversation's
    // cumulative `searched_sources` registry. Dedupe by URL: existing
    // entries get their `last_referenced_turn` bumped to the current
    // turn; new entries are appended with
    // `first_seen_turn = last_referenced_turn = current_turn`. The
    // synthesis system message later renders this as a "Web sources
    // gathered so far" block so the model has stable awareness of
    // which URLs the user has already been shown.
    //
    // Soft-fail: a missing conversation_id (legacy callers, tests
    // without a wired conversation) skips the registry update; the
    // search still feeds through to refinement so the bench's
    // `submit_information_response` path is unaffected.
    if let Some(ref cid) = conversation_id {
        let store_arc: Option<Arc<dyn sovereign_core::traits::StateStore>> = {
            let guard = state.store.read().await;
            guard.as_ref().map(Arc::clone)
        };
        if let Some(store) = store_arc {
            match store.get_conversation(cid).await {
                Ok(conv) => {
                    let current_turn = conv.messages.len();
                    let mut entries = conv.searched_sources.unwrap_or_default();
                    let mut url_seen: std::collections::HashSet<String> =
                        entries.iter().map(|e| e.url.clone()).collect();
                    for r in &out.results {
                        if url_seen.contains(&r.url) {
                            if let Some(existing) = entries.iter_mut().find(|e| e.url == r.url) {
                                existing.last_referenced_turn = current_turn;
                            }
                        } else {
                            entries.push(sovereign_core::types::SearchedSourceEntry {
                                url: r.url.clone(),
                                title: r.title.clone(),
                                first_seen_turn: current_turn,
                                last_referenced_turn: current_turn,
                                search_query: query.to_string(),
                            });
                            url_seen.insert(r.url.clone());
                        }
                    }
                    if let Err(e) = store
                        .set_conversation_searched_sources(cid, Some(entries))
                        .await
                    {
                        tracing::warn!(
                            conversation_id = %cid,
                            error = %e,
                            "submit_information_search: failed to persist searched_sources — search proceeds, model loses cumulative-URL awareness this turn"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        conversation_id = %cid,
                        error = %e,
                        "submit_information_search: could not load conversation for searched_sources update — skipping"
                    );
                }
            }
        }
    }

    let accepted = state
        .approval
        .submit_information_response(&key, Some(formatted))
        .await;
    Ok(SearchAugmentation {
        query: query.to_string(),
        backend_id: out.backend_id,
        sources,
        accepted,
    })
}

/// Trigger memory extraction on a finished inner-work conversation.
///
/// Until 2026-05-05 the desktop had no path to invoke memory
/// extraction — `Runtime::end_conversation` was called only from the
/// CLI, so a desktop-only inner-work user accumulated zero
/// long-term memory across sessions despite the storage and recall
/// pipelines being fully wired. This command closes that gap.
///
/// Caller is `InnerWorkSurface.onDestroy`. Best-effort: we ignore
/// errors at the runtime layer so a failure here doesn't stall the
/// surface unmount. The runtime's own `end_conversation` is a no-op
/// when the conversation has fewer than 4 messages, so empty inner-
/// work entries don't trigger extraction noise.
///
/// The skill_id wall is enforced inside `Runtime::end_conversation`:
/// each extracted memory is stamped with `source_skill_id` =
/// `conversations.skill_id`. Inner-work conversations therefore
/// produce inner-work-scoped memories, never general-pool ones.
#[tauri::command]
pub async fn finalize_inner_work_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    if let Err(e) = runtime.end_conversation(&conversation_id).await {
        tracing::warn!(
            error = %e,
            conversation_id = %conversation_id,
            "finalize_inner_work_conversation: extraction failed"
        );
    }
    Ok(())
}

/// Tombstone a memory the user has flagged as wrong. Soft-delete via
/// `delete_memory` (sets `deleted_at`) — the row is preserved for
/// audit but excluded from all recall paths. Used by the inner-work
/// "drop this memory" affordance.
#[tauri::command]
pub async fn forget_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    runtime
        .store
        .delete_memory(&memory_id)
        .await
        .map_err(|e| e.to_string())
}

/// Halve the confidence of a memory. Used by the "this is partly
/// right but the witness over-extrapolated" path — the memory stays
/// recallable but with reduced weight, and the standard decay floor
/// will eventually prune it if the user keeps weakening.
#[tauri::command]
pub async fn weaken_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    let all = runtime
        .store
        .get_all_memories()
        .await
        .map_err(|e| e.to_string())?;
    let current = all
        .iter()
        .find(|m| m.id == memory_id)
        .ok_or_else(|| format!("memory {memory_id} not found"))?;
    let new_conf = (current.confidence * 0.5).max(0.0);
    runtime
        .store
        .update_memory_confidence(&memory_id, new_conf)
        .await
        .map_err(|e| e.to_string())
}

/// Glassbox: return the most recent witness-turn provenance the
/// runtime captured for `conversation_id`, if any.
///
/// Used by the desktop's inner-work surface bound to Cmd+? to surface
/// "what did the model actually see" — the assembled system prompt,
/// the recalled memories, the conversation history slice (today: empty
/// — the streaming witness path doesn't pass prior turns to the
/// model), the model id + token budget, and Pass A timing.
///
/// Returns `Ok(None)` when no provenance is recorded for that
/// conversation in this Runtime's lifetime — typically because the
/// conversation hasn't received a streaming witness response yet, or
/// because it ran on the non-streaming path (we don't capture there
/// today; mirror the capture in `handle_expressive_query` if needed).
#[tauri::command]
pub async fn get_last_turn_provenance(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<Option<sovereign_core::runtime::TurnProvenance>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    Ok(runtime.get_last_turn_provenance(&conversation_id))
}

#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillEntry>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let all_skills = runtime.skills.list();
    let active_ids: Vec<String> = runtime
        .skills
        .active_skills()
        .iter()
        .map(|s| s.id.clone())
        .collect();

    Ok(all_skills
        .iter()
        .map(|s| SkillEntry {
            active: active_ids.contains(&s.id),
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            trust_level: format!("{:?}", s.trust_level).to_lowercase(),
        })
        .collect())
}

#[tauri::command]
pub async fn toggle_skill(
    state: State<'_, Arc<AppState>>,
    skill_id: String,
    active: bool,
) -> Result<(), String> {
    toggle_skill_impl(&state, skill_id, active).await
}

/// Shared body for the `toggle_skill` Tauri command. Single
/// implementation guarantees uniform idempotency + rebuild
/// behavior. (Pre-2026-05-24 also served per-workspace wrappers
/// like `recipe_author_set_workspace_active`; those were removed
/// when routing moved to conversation-tag-driven primary skill
/// selection.)
///
/// Idempotent: if the requested state already matches the stored
/// `config.active_skills`, returns early without `config.save()`
/// or `rebuild_runtime`. Diagnosed 2026-05-23: the InnerWork
/// surface and a parallel App.svelte view-effect both called the
/// older non-idempotent `toggle_skill` on view-enter, kicking off
/// two ~15s `rebuild_runtime` passes that locked the UI for ~30s.
/// Even after removing the redundant caller, no-op short-circuit
/// is the right shape for a toggle — callers shouldn't have to
/// track local state to avoid thrashing the registry.
pub async fn toggle_skill_impl(
    state: &Arc<AppState>,
    skill_id: String,
    active: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        let already = config.active_skills.contains(&skill_id);
        if active && already {
            return Ok(());
        }
        if !active && !already {
            return Ok(());
        }
        if active {
            config.active_skills.push(skill_id);
        } else {
            config.active_skills.retain(|id| *id != skill_id);
        }
        config.save()?;
    }

    state::rebuild_runtime(state).await
}

/// Render an assistant answer + its provenance as a self-contained
/// Markdown document — the "provenance survives the handoff" guarantee.
/// Built entirely from the persisted message (content + metadata), so no
/// re-fetch and no schema change: the metadata already carries
/// `provenance` (model + per-corpus source summary) and `retrieved_chunks`
/// (the actual grounding passages). Pure + unit-tested so the **source
/// ledger** can't silently regress to dead text.
fn render_answer_markdown(content: &str, metadata: Option<&serde_json::Value>) -> String {
    let mut md = String::from("# svrnmesh answer\n\n");

    // Provenance meta line: who answered + which corpora grounded it.
    let mut meta_bits: Vec<String> = Vec::new();
    if let Some(backend) = metadata
        .and_then(|m| m.pointer("/provenance/inference_backend"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        meta_bits.push(format!("answered by {backend}"));
    }
    if let Some(sources) = metadata
        .and_then(|m| m.pointer("/provenance/sources"))
        .and_then(|v| v.as_array())
    {
        let names: Vec<String> = sources
            .iter()
            .filter(|s| s.get("count").and_then(|v| v.as_u64()).unwrap_or(0) > 0)
            .filter_map(|s| {
                s.get("display_name")
                    .and_then(|v| v.as_str())
                    .filter(|x| !x.is_empty())
                    .or_else(|| s.get("origin").and_then(|v| v.as_str()))
                    .map(str::to_string)
            })
            .collect();
        if !names.is_empty() {
            meta_bits.push(format!("searched {}", names.join(", ")));
        }
    }
    if !meta_bits.is_empty() {
        md.push_str(&format!("*{}*\n\n", meta_bits.join(" · ")));
    }

    md.push_str(content.trim());
    md.push_str("\n\n");

    // The source ledger — every grounding passage, traceable to its corpus.
    let chunks = metadata
        .and_then(|m| m.get("retrieved_chunks"))
        .and_then(|v| v.as_array())
        .filter(|c| !c.is_empty());
    if let Some(chunks) = chunks {
        md.push_str(
            "---\n\n## Sources\n\nThis answer was grounded in the following passages \
             from your indexed corpora:\n\n",
        );
        for (i, c) in chunks.iter().enumerate() {
            let title = c
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|x| !x.is_empty())
                .unwrap_or("(untitled passage)");
            let corpus = c.get("corpus_id").and_then(|v| v.as_str()).unwrap_or("");
            md.push_str(&format!("{}. **{}** — `{}`\n", i + 1, title, corpus));
            if let Some(snippet) = c
                .get("snippet")
                .and_then(|v| v.as_str())
                .filter(|x| !x.is_empty())
            {
                for line in snippet.lines() {
                    md.push_str(&format!("   > {line}\n"));
                }
            }
            if let Some(url) = c
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|x| !x.is_empty())
            {
                md.push_str(&format!("   <{url}>\n"));
            }
            md.push('\n');
        }
    } else {
        md.push_str(
            "---\n\n*No corpus passages were cited for this answer — it came from the \
             model's own knowledge or a non-retrieval path.*\n\n",
        );
    }

    md.push_str("---\n*Exported from svrnmesh — provenance preserved.*\n");
    md
}

/// Structured view of an answer + its provenance — the shared intermediate
/// the docx and PDF renderers walk, so neither re-parses the metadata. (The
/// Markdown renderer above predates this and extracts inline; left as-is to
/// avoid churning a tested path.)
struct SourceEntry {
    title: String,
    corpus_id: String,
    snippet: Option<String>,
    url: Option<String>,
}

struct AnswerDoc {
    answered_by: Option<String>,
    corpora: Vec<String>,
    body: String,
    sources: Vec<SourceEntry>,
}

impl AnswerDoc {
    fn from_message(content: &str, metadata: Option<&serde_json::Value>) -> Self {
        let answered_by = metadata
            .and_then(|m| m.pointer("/provenance/inference_backend"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let corpora = metadata
            .and_then(|m| m.pointer("/provenance/sources"))
            .and_then(|v| v.as_array())
            .map(|srcs| {
                srcs.iter()
                    .filter(|s| s.get("count").and_then(|v| v.as_u64()).unwrap_or(0) > 0)
                    .filter_map(|s| {
                        s.get("display_name")
                            .and_then(|v| v.as_str())
                            .filter(|x| !x.is_empty())
                            .or_else(|| s.get("origin").and_then(|v| v.as_str()))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sources = metadata
            .and_then(|m| m.get("retrieved_chunks"))
            .and_then(|v| v.as_array())
            .map(|chunks| {
                chunks
                    .iter()
                    .map(|c| SourceEntry {
                        title: c
                            .get("title")
                            .and_then(|v| v.as_str())
                            .filter(|x| !x.is_empty())
                            .unwrap_or("(untitled passage)")
                            .to_string(),
                        corpus_id: c
                            .get("corpus_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        snippet: c
                            .get("snippet")
                            .and_then(|v| v.as_str())
                            .filter(|x| !x.is_empty())
                            .map(str::to_string),
                        url: c
                            .get("url")
                            .and_then(|v| v.as_str())
                            .filter(|x| !x.is_empty())
                            .map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            answered_by,
            corpora,
            body: content.trim().to_string(),
            sources,
        }
    }

    fn meta_line(&self) -> Option<String> {
        let mut bits = Vec::new();
        if let Some(b) = &self.answered_by {
            bits.push(format!("answered by {b}"));
        }
        if !self.corpora.is_empty() {
            bits.push(format!("searched {}", self.corpora.join(", ")));
        }
        (!bits.is_empty()).then(|| bits.join(" \u{00B7} "))
    }
}

/// A flat, format-agnostic block sequence both renderers walk.
enum Block {
    Title(String),
    Heading(String),
    Meta(String),
    Para(String),
    SourceTitle(String),
    Quote(String),
    Url(String),
    Footer(String),
}

fn doc_blocks(doc: &AnswerDoc) -> Vec<Block> {
    let mut blocks = vec![Block::Title("svrnmesh answer".to_string())];
    if let Some(meta) = doc.meta_line() {
        blocks.push(Block::Meta(meta));
    }
    for para in doc.body.split("\n\n") {
        let cleaned = strip_markdown_light(para);
        if !cleaned.is_empty() {
            blocks.push(Block::Para(cleaned));
        }
    }
    if doc.sources.is_empty() {
        blocks.push(Block::Para(
            "No corpus passages were cited for this answer — it came from the model's own \
             knowledge or a non-retrieval path."
                .to_string(),
        ));
    } else {
        blocks.push(Block::Heading("Sources".to_string()));
        for (i, s) in doc.sources.iter().enumerate() {
            blocks.push(Block::SourceTitle(format!(
                "{}. {} \u{2014} {}",
                i + 1,
                s.title,
                s.corpus_id
            )));
            if let Some(snippet) = &s.snippet {
                blocks.push(Block::Quote(strip_markdown_light(snippet)));
            }
            if let Some(url) = &s.url {
                blocks.push(Block::Url(url.clone()));
            }
        }
    }
    blocks.push(Block::Footer(
        "Exported from svrnmesh — provenance preserved.".to_string(),
    ));
    blocks
}

/// Light Markdown de-noising so prose reads cleanly in PDF/Word (which don't
/// interpret Markdown): drops `**`, leading `#`/`>`, and backticks. Not a
/// parser — just enough to avoid stray markup in the exported document.
fn strip_markdown_light(s: &str) -> String {
    let mut lines = Vec::new();
    for line in s.lines() {
        let l = line.trim_start();
        let l = l.trim_start_matches(['#', '>']).trim_start();
        let cleaned = l.replace("**", "").replace("__", "").replace('`', "");
        lines.push(cleaned.trim_end().to_string());
    }
    lines.join("\n").trim().to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Greedy word-wrap to a max char count — PDF has no layout engine, so we
/// wrap ourselves with a conservative per-line character budget.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(8);
    let mut out = Vec::new();
    for src_line in text.lines() {
        if src_line.trim().is_empty() {
            continue;
        }
        let mut cur = String::new();
        for word in src_line.split_whitespace() {
            if cur.is_empty() {
                cur.push_str(word);
            } else if cur.chars().count() + 1 + word.chars().count() <= max {
                cur.push(' ');
                cur.push_str(word);
            } else {
                out.push(std::mem::take(&mut cur));
                cur.push_str(word);
            }
            while cur.chars().count() > max {
                let head: String = cur.chars().take(max).collect();
                cur = cur.chars().skip(max).collect();
                out.push(head);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Fold the few non-ASCII glyphs we emit (·, dashes, curly quotes) to ASCII
/// so the PDF's built-in Helvetica renders them; drop other non-ASCII rather
/// than emit tofu.
fn pdf_text(s: &str) -> String {
    s.replace('\u{2026}', "...")
        .chars()
        .map(|c| match c {
            '\u{00B7}' | '\u{2022}' | '\u{2014}' | '\u{2013}' => '-',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2018}' | '\u{2019}' => '\'',
            c if c.is_ascii() => c,
            _ => ' ',
        })
        .collect()
}

/// Render the answer + source ledger as a Word (.docx) — hand-rolled minimal
/// OOXML zipped with the `zip` crate already in the tree (no new dep).
fn render_answer_docx(doc: &AnswerDoc) -> Result<Vec<u8>, String> {
    use std::io::Write as _;

    let mut xml_body = String::new();
    let para = |out: &mut String, text: &str, bold: bool, italic: bool, half_pt: u32| {
        let mut rpr = String::new();
        if bold {
            rpr.push_str("<w:b/>");
        }
        if italic {
            rpr.push_str("<w:i/>");
        }
        if half_pt > 0 {
            rpr.push_str(&format!("<w:sz w:val=\"{half_pt}\"/>"));
        }
        let rpr = if rpr.is_empty() {
            String::new()
        } else {
            format!("<w:rPr>{rpr}</w:rPr>")
        };
        out.push_str(&format!(
            "<w:p><w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
            xml_escape(text)
        ));
    };

    for block in doc_blocks(doc) {
        match block {
            Block::Title(t) => para(&mut xml_body, &t, true, false, 36),
            Block::Heading(t) => para(&mut xml_body, &t, true, false, 28),
            Block::Meta(t) => para(&mut xml_body, &t, false, true, 18),
            Block::Para(t) => {
                for line in t.split('\n') {
                    para(&mut xml_body, line, false, false, 22);
                }
            }
            Block::SourceTitle(t) => para(&mut xml_body, &t, true, false, 22),
            Block::Quote(t) => {
                for line in t.split('\n') {
                    para(&mut xml_body, line, false, true, 20);
                }
            }
            Block::Url(t) => para(&mut xml_body, &t, false, false, 18),
            Block::Footer(t) => para(&mut xml_body, &t, false, true, 16),
        }
    }

    let document_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{xml_body}</w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";

    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("[Content_Types].xml", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(content_types.as_bytes())
            .map_err(|e| e.to_string())?;
        zw.start_file("_rels/.rels", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(rels.as_bytes()).map_err(|e| e.to_string())?;
        zw.start_file("word/document.xml", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(document_xml.as_bytes())
            .map_err(|e| e.to_string())?;
        zw.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// Render the answer + source ledger as a PDF via `lopdf` (already in the
/// tree). Built-in Helvetica (no embedded font), greedy word-wrap, simple
/// pagination — a narrow exporter, not a typesetting engine.
fn render_answer_pdf(doc: &AnswerDoc) -> Result<Vec<u8>, String> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    const PAGE_W: f64 = 595.0;
    const PAGE_H: f64 = 842.0;
    const MARGIN: f64 = 56.0;
    const TOP: f64 = PAGE_H - MARGIN;

    struct Line {
        text: String,
        bold: bool,
        size: f64,
        gap_before: f64,
    }
    let push =
        |lines: &mut Vec<Line>, text: &str, bold: bool, size: f64, gap: f64, indent: &str| {
            let max_chars = ((PAGE_W - 2.0 * MARGIN) / (size * 0.5)) as usize;
            for (i, wl) in wrap_text(text, max_chars).into_iter().enumerate() {
                lines.push(Line {
                    text: pdf_text(&format!("{}{}", if i == 0 { "" } else { indent }, wl)),
                    bold,
                    size,
                    gap_before: if i == 0 { gap } else { 0.0 },
                });
            }
        };

    let mut lines: Vec<Line> = Vec::new();
    for block in doc_blocks(doc) {
        match block {
            Block::Title(t) => push(&mut lines, &t, true, 18.0, 0.0, ""),
            Block::Heading(t) => push(&mut lines, &t, true, 13.0, 14.0, ""),
            Block::Meta(t) => push(&mut lines, &t, false, 9.0, 3.0, ""),
            Block::Para(t) => push(&mut lines, &t, false, 11.0, 9.0, ""),
            Block::SourceTitle(t) => push(&mut lines, &t, true, 11.0, 9.0, ""),
            Block::Quote(t) => push(&mut lines, &t, false, 10.0, 3.0, "    "),
            Block::Url(t) => push(&mut lines, &t, false, 9.0, 1.0, "    "),
            Block::Footer(t) => push(&mut lines, &t, false, 8.0, 16.0, ""),
        }
    }

    // Paginate into pages of (baseline_y, line_index).
    let mut pages: Vec<Vec<(f64, usize)>> = Vec::new();
    let mut current: Vec<(f64, usize)> = Vec::new();
    let mut y = TOP;
    for (idx, ln) in lines.iter().enumerate() {
        let line_h = ln.size * 1.35;
        if y - ln.gap_before - line_h < MARGIN && !current.is_empty() {
            pages.push(std::mem::take(&mut current));
            y = TOP;
        }
        y -= ln.gap_before;
        current.push((y, idx));
        y -= line_h;
    }
    if !current.is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }

    let mut pdf = Document::with_version("1.5");
    let pages_id = pdf.new_object_id();
    let helv = pdf.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let helv_bold = pdf.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold",
    });
    let resources_id = pdf.add_object(dictionary! {
        "Font" => dictionary! { "F1" => helv, "F2" => helv_bold },
    });

    let mut kids: Vec<Object> = Vec::new();
    for page in &pages {
        let mut ops: Vec<Operation> = Vec::new();
        for (line_y, idx) in page {
            let ln = &lines[*idx];
            if ln.text.is_empty() {
                continue;
            }
            ops.push(Operation::new("BT", vec![]));
            ops.push(Operation::new(
                "Tf",
                vec![(if ln.bold { "F2" } else { "F1" }).into(), ln.size.into()],
            ));
            ops.push(Operation::new("Td", vec![MARGIN.into(), (*line_y).into()]));
            ops.push(Operation::new(
                "Tj",
                vec![Object::string_literal(ln.text.clone())],
            ));
            ops.push(Operation::new("ET", vec![]));
        }
        let content = Content { operations: ops };
        let content_id = pdf.add_object(Stream::new(
            dictionary! {},
            content.encode().map_err(|e| e.to_string())?,
        ));
        let page_id = pdf.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        kids.push(page_id.into());
    }

    let count = kids.len() as i64;
    pdf.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
            "Resources" => resources_id,
            "MediaBox" => vec![0f64.into(), 0f64.into(), PAGE_W.into(), PAGE_H.into()],
        }),
    );
    let catalog_id = pdf.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id,
    });
    pdf.trailer.set("Root", catalog_id);
    pdf.compress();

    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Export a single assistant answer to a file, carrying its citations +
/// source ledger. Format follows the `dest_path` extension chosen in the
/// frontend save dialog — `.md` (Markdown), `.pdf`, or `.docx` — all built
/// from the same persisted message metadata, with zero new dependencies.
#[tauri::command]
pub async fn export_answer(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    message_id: String,
    dest_path: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    let convo = runtime
        .store
        .get_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())?;
    let msg = convo
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .ok_or_else(|| format!("message {message_id} not found"))?;
    let metadata = msg.metadata.as_ref();
    // Format follows the extension the user picked in the save dialog.
    let ext = std::path::Path::new(&dest_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let bytes: Vec<u8> = match ext.as_str() {
        "pdf" => render_answer_pdf(&AnswerDoc::from_message(&msg.content, metadata))?,
        "docx" => render_answer_docx(&AnswerDoc::from_message(&msg.content, metadata))?,
        // `.md` and anything else fall back to Markdown.
        _ => render_answer_markdown(&msg.content, metadata).into_bytes(),
    };
    std::fs::write(&dest_path, bytes).map_err(|e| format!("write {dest_path}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod export_tests {
    use super::{render_answer_docx, render_answer_markdown, render_answer_pdf, AnswerDoc};

    #[test]
    fn markdown_includes_source_ledger() {
        let meta = serde_json::json!({
            "provenance": {
                "inference_backend": "Qwen3-8B-Q4_K_M",
                "sources": [{ "origin": "sep", "count": 3 }]
            },
            "retrieved_chunks": [
                { "title": "Free Will", "corpus_id": "sep", "snippet": "Compatibilism holds that..." }
            ]
        });
        let md = render_answer_markdown("Free will is compatible with determinism.", Some(&meta));
        assert!(md.contains("# svrnmesh answer"));
        assert!(md.contains("Free will is compatible with determinism."));
        assert!(md.contains("answered by Qwen3-8B-Q4_K_M"));
        assert!(md.contains("searched sep"));
        // The ledger: title, corpus handle, and the grounding quote.
        assert!(md.contains("## Sources"));
        assert!(md.contains("**Free Will**"));
        assert!(md.contains("`sep`"));
        assert!(md.contains("> Compatibilism holds that..."));
    }

    #[test]
    fn markdown_without_sources_says_so() {
        let md = render_answer_markdown("Hello.", None);
        assert!(md.contains("Hello."));
        assert!(md.contains("No corpus passages were cited"));
        // Never silently implies sources that aren't there.
        assert!(!md.contains("## Sources"));
    }

    #[test]
    fn answerdoc_extracts_provenance() {
        let meta = serde_json::json!({
            "provenance": {
                "inference_backend": "Darwin-36B",
                "sources": [{ "origin": "sep", "count": 2 }, { "origin": "empty", "count": 0 }]
            },
            "retrieved_chunks": [{ "title": "T", "corpus_id": "sep", "snippet": "q" }]
        });
        let d = AnswerDoc::from_message("Body", Some(&meta));
        assert_eq!(d.answered_by.as_deref(), Some("Darwin-36B"));
        assert_eq!(d.corpora, vec!["sep".to_string()]); // count:0 dropped
        assert_eq!(d.sources.len(), 1);
        assert_eq!(d.sources[0].corpus_id, "sep");
    }

    #[test]
    fn docx_is_a_valid_zip_package() {
        let meta = serde_json::json!({
            "retrieved_chunks": [{ "title": "Free Will", "corpus_id": "sep", "snippet": "grounding quote" }]
        });
        let doc = AnswerDoc::from_message("The answer body.", Some(&meta));
        let bytes = render_answer_docx(&doc).expect("docx renders");
        // Real .docx is an OOXML zip — starts with the PK zip-local-header.
        assert_eq!(&bytes[..2], b"PK");
        assert!(bytes.len() > 300);
    }

    #[test]
    fn pdf_has_pdf_header() {
        let doc = AnswerDoc::from_message("A short answer.", None);
        let bytes = render_answer_pdf(&doc).expect("pdf renders");
        assert_eq!(&bytes[..5], b"%PDF-");
        assert!(bytes.len() > 300);
    }
}
