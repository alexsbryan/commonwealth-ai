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
        tracing::warn!(
            "create_conversation: sqlite store unavailable, deferring insert"
        );
    }

    Ok(CreateConversationResponse {
        id,
        created_at,
    })
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
        BraveBackendImpl, BudgetView, DuckDuckGoBackendImpl, SearchOrchestrator,
        SearchPrivacy, SelectInputs, TavilyBackendImpl, WebSearchBackend,
        WebSearchRegistry,
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
                |k| -> Box<dyn WebSearchBackend> {
                    Box::new(TavilyBackendImpl::new(k.clone()))
                },
            ),
            "brave" => config_snapshot.search_backend.api_key.as_ref().map(
                |k| -> Box<dyn WebSearchBackend> {
                    Box::new(BraveBackendImpl::new(k.clone()))
                },
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
        // Treat as a soft failure surfaced to the UI. The pending
        // request stays open so the user can paste / skip / retry
        // with a tighter query without rebuilding the card.
        return Err(format!(
            "web search returned 0 results via {} (DDG may be bot-blocking; \
             try a tighter query or paste a source instead)",
            out.backend_id,
        ));
    }

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
                    let mut entries =
                        conv.searched_sources.unwrap_or_default();
                    let mut url_seen: std::collections::HashSet<String> =
                        entries.iter().map(|e| e.url.clone()).collect();
                    for r in &out.results {
                        if url_seen.contains(&r.url) {
                            if let Some(existing) =
                                entries.iter_mut().find(|e| e.url == r.url)
                            {
                                existing.last_referenced_turn = current_turn;
                            }
                        } else {
                            entries.push(
                                sovereign_core::types::SearchedSourceEntry {
                                    url: r.url.clone(),
                                    title: r.title.clone(),
                                    first_seen_turn: current_turn,
                                    last_referenced_turn: current_turn,
                                    search_query: query.to_string(),
                                },
                            );
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

