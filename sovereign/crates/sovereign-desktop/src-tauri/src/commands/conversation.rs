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
    // sv-surface D9b — `POST /v1/conversations`. The row is SEEDED, not
    // created lazily, and that is exactly why it has to be seeded on the
    // store the turn will be answered against: the surface tag decides
    // routing at dispatch, and since R5 the dispatch is the daemon's. The
    // local arm seeded this process's `sqlite_store`, which in attach is a
    // different file from the one serving the turn — the tag was written
    // where nobody would read it.
    //
    // The old `else` branch is gone with it, and that is a §18.3 repair,
    // not a loss: "sqlite store unavailable" used to `warn!` and return a
    // conversation id anyway, leaving the frontend holding an id for a row
    // that did not exist and would be lazily minted UNTAGGED by the first
    // turn. The route either seeds the row or says why.
    //
    // `enabled_corpora: None` — the desktop's own create seeds no
    // allow-list; the client verifies the daemon echoed what was sent, so
    // a host that ignores the field is an error rather than a silent
    // "everything is searchable".
    let created = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .create_conversation(surface_skill_id.as_deref(), None)
        .await
        .map_err(|e| format!("create_conversation: {e}"))?;

    Ok(CreateConversationResponse {
        id: created.id,
        created_at: created.created_at,
        enabled_corpora: created.enabled_corpora,
    })
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    offset: Option<usize>,
    surface_skill_id: Option<String>,
) -> Result<Vec<ConversationEntry>, String> {
    // sv-surface D9b — NOT repointed, and the reason is a missing route,
    // not an oversight. `GET /v1/conversations` takes `limit`/`offset` and
    // nothing else; this listing is SURFACE-SCOPED
    // (`list_conversations_for_surface`), and cross-surface visibility is a
    // structural restriction, so serving it through the unscoped route
    // would widen it silently — the §18.3 substitution, in the one place
    // the redesign made load-bearing. Owed: `GET /v1/conversations?
    // skill_id=` with `list_conversations_for_surface` as the decider.
    //
    // Readiness gates on the DATABASE, not the chat Runtime: listing
    // conversations is not a chat operation (Phase 0).
    let _ = require_store!(state);
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
/// sv-surface D9b — NOT repointed: no corpus-scoped listing route exists.
/// Owed: `GET /v1/conversations?corpus_id=` over
/// `SqliteStateStore::list_conversations_for_corpus`.
#[tauri::command]
pub async fn notebook_conversations(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<ConversationEntry>, String> {
    let _ = require_store!(state);
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
    // sv-surface D9b — NOT repointed, and this one is a wire GAP, not a
    // missing route. `GET /v1/conversations/{id}` exists and `export_answer`
    // below rides it. But it answers the TYPED projection
    // (provenance/citations/epistemic_state) and drops two fields this DTO
    // carries: `metadata`, the verbatim blob the frontend types as `unknown`
    // and reads with pointers, and `enabled_corpora`, which
    // `CorpusFilterStrip` renders. Repointing today would quietly empty
    // both. Two ways out, both outside this rung: widen the route's
    // `ConversationResponse`, or convert the renderer onto the projections
    // (`conversation_wire_census.rs` already names that as rung 6's job).
    let store = require_store!(state);

    let convo = store
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
                // sv-surface D7/G9: `metadata` stays the verbatim blob.
                // The frontend types it as `unknown` and reads it with
                // pointers, so retyping this contract is a later rung —
                // and `MessageEntry` itself lives in `commands/mod.rs`,
                // outside this rung's zone. `ask_document` carries the
                // typed projection beside the blob (document_asset.rs)
                // and `export_answer` below already renders from it.
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
    // sv-surface D9b — `DELETE /v1/conversations/{id}`, in BOTH modes. The
    // daemon's store is the one the sidebar was listed from; deleting the
    // desktop's own row left the served row in place, so the entry came
    // back on the next list.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
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

    // sv-surface D9b — NOT repointed: the daemon serves no conversation
    // update. Owed: `PATCH /v1/conversations/{id}` with `{title}` over
    // `update_conversation_title`. Until then the rename lands on this
    // process's row and the served sidebar keeps the old title.
    let store = require_store!(state);

    store
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
    // sv-surface D9b — NOT repointed: no route writes the allow-list after
    // create. `POST /v1/conversations` accepts `enabled_corpora` at SEED
    // time only. Owed: `PUT /v1/conversations/{id}/enabled-corpora`. This is
    // the sharpest of the five holdouts — retrieval reads the allow-list on
    // the DAEMON's row, so in attach every chip the user toggles is written
    // where the retrieval that honours it will never look.
    let store = require_store!(state);

    store
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
    // The daemon's store is the one writer, so search crosses the wire in
    // BOTH modes (sv-surface D2). The route serves the same `search_messages`
    // trait call behind the same 50-row cap the deleted local arm applied —
    // one query decider, so the answer cannot depend on which boot asked.
    let messages = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .search_conversations(&query)
        .await
        .map_err(|e| e.to_string())?;
    Ok(messages
        .into_iter()
        .map(|m| SearchResult {
            content: m.content,
            conversation_id: m.conversation_id,
        })
        .collect())
}

/// Answer a prompt from a LIVE WIRE TURN (sv-surface R5): the card's
/// `key` is the daemon-minted prompt id, and the sender parked for the
/// conversation that prompt belongs to answers on the turn socket while
/// the drain keeps reading.
///
/// `None` means "not a wire prompt" and the caller falls back to the
/// local desk, which still serves the in-process turn shapes (redirect,
/// resume). Two things used to be conflated with that (review C8): an
/// answer for a key NOTHING is parked under went onto the wire anyway and
/// came back `true`, and the sender it went to was whichever turn was
/// most recent rather than this card's. Both are refusals now, by name.
///
/// `Some(true)` is still optimistic about the far end — a queued send is
/// not a resolved question — but it is no longer optimistic about
/// whether the card exists. The authoritative word is the `ResolveAck`
/// notice, which `pump_wire_frames` renders (a `WrongKind` re-raises the
/// card; a `NoSuchPending` drops it).
async fn answer_wire_prompt(
    state: &AppState,
    key: &str,
    answer: sovereign_contracts::types::TurnAnswer,
) -> Option<bool> {
    let parked = state.pending_prompts.get(key).await?;
    let Some(sender) = state.turn_wire.sender_for(&parked.conversation_id).await else {
        // The card is ours but its turn's socket is gone — the turn ended
        // while the card was on screen. Refuse by name and forget the
        // card; claiming it reached a question is the §18.3 substitution.
        tracing::warn!(
            prompt_id = %key,
            conversation_id = %parked.conversation_id,
            "answer_wire_prompt: the turn this card belongs to is no longer parked"
        );
        state.pending_prompts.resolve(key).await;
        return Some(false);
    };
    match sender.send_answer(key, &answer) {
        Ok(()) => Some(true),
        Err(e) => {
            tracing::warn!(
                prompt_id = %key,
                conversation_id = %parked.conversation_id,
                error = %e,
                "answer_wire_prompt: the turn socket's writer is gone"
            );
            Some(false)
        }
    }
}

#[tauri::command]
pub async fn submit_approval(
    state: State<'_, Arc<AppState>>,
    key: String,
    approved: bool,
) -> Result<bool, String> {
    if let Some(hit) = answer_wire_prompt(
        &state,
        &key,
        sovereign_contracts::types::TurnAnswer::Approved(approved),
    )
    .await
    {
        return Ok(hit);
    }
    Ok(state.approval.submit_approval(&key, approved))
}

#[tauri::command]
pub async fn submit_input(
    state: State<'_, Arc<AppState>>,
    key: String,
    response: String,
) -> Result<bool, String> {
    if let Some(hit) = answer_wire_prompt(
        &state,
        &key,
        sovereign_contracts::types::TurnAnswer::Text(response.clone()),
    )
    .await
    {
        return Ok(hit);
    }
    Ok(state.approval.submit_input(&key, response))
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
    if let Some(hit) = answer_wire_prompt(
        &state,
        &key,
        sovereign_contracts::types::TurnAnswer::Information {
            content: content.clone(),
            sources: Vec::new(),
        },
    )
    .await
    {
        return Ok(hit);
    }
    Ok(state.approval.submit_information_response(&key, content))
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
        BraveBackendImpl, DuckDuckGoBackendImpl, SearchOrchestrator, SearchPrivacy, SelectInputs,
        TavilyBackendImpl, WebSearchBackend, WebSearchRegistry,
    };

    let query = query.trim();
    if query.is_empty() {
        return Err("query must not be empty".to_string());
    }

    let parked = state.pending_prompts.get(&key).await;
    if parked.is_some() {
        // The active wire turn put this card up; the guard below is the
        // LOCAL desk's and cannot see it. The Answer's own named refusal
        // covers staleness daemon-side.
    } else if !state.approval.has_pending_information(&key) {
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
    // The notes.db the dossier writes is the daemon's in both modes
    // (sv-surface D2), so the write crosses the wire either way. Soft-fail
    // preserved — the deleted local arm skipped a missing NoteStore, and a
    // daemon without a notes surface answers the named 503; the search below
    // still runs regardless.
    let outcome = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .notes_tool_outcome(sovereign_turn_client::ToolOutcome {
            session_id: &key,
            conversation_id: conversation_id.as_deref(),
            tool_id: "knowledge_lookup",
            outcome: sovereign_core::memory::ToolDecisionOutcome::NoResults,
            reasoning: "user clicked Search-the-web on the INFORMATION REQUEST card \
                         — prior in-conversation lookup did not satisfy",
            // Tier 1: no summary/evidence_ids/turn_index — this write fires
            // from a USER click, not a tool-result post-stream hook. The
            // originating turn's baseline write (from the runtime's KQ
            // dispatch) already carries those fields; this is an audit
            // overlay.
            extras_summary: None,
            evidence_ids: Vec::new(),
            turn_index: 0,
        })
        .await;
    if let Err(e) = outcome {
        tracing::info!(
            error = %e,
            "submit_information_search: tool-outcome write skipped (daemon-side notes unavailable)"
        );
    }

    let config_snapshot = state.config.read().await.clone();

    // The ONE registry construction (§10.6) — the same one `state.rs`
    // and the deep-research loop use, over the operator's `[search]`.
    let orchestrator = SearchOrchestrator::new(Arc::new(crate::state::effective_search_registry()));

    // The ONE egress boundary (order deep-research-t2a): the client is
    // built by sovereign-core's egress module (the F26 census enforces
    // that this file constructs no reqwest client of its own), and the
    // query egress passes the boundary's release gate BEFORE it leaves.
    let client = sovereign_core::egress::search_client()
        .map_err(|e| format!("egress boundary search client build: {e}"))?;
    let provider_static: &'static str = match config_snapshot.search_backend.provider.as_str() {
        "tavily" => "tavily",
        "brave" => "brave",
        _ => "duckduckgo",
    };
    // The click IS the user's action and the query IS the user's own
    // words — the release rule's user-formed-query clause (what=="query"
    // && user_formed) covers this egress without a grant.
    sovereign_core::egress::verify(
        &sovereign_core::egress::EgressPayload {
            privacy: SearchPrivacy::External {
                provider: provider_static,
            },
            custody: sovereign_contracts::types::Custody::Personal,
            what: "query",
            target: provider_static,
            detail: query,
            user_formed: true,
        },
        None,
    )
    .map_err(|r| format!("web search refused: {r}"))?;
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
    // cumulative `searched_sources` registry, through the ONE merge
    // (`sovereign_core::searched_sources`) the daemon's Answer path also
    // uses (sv-surface G3b; the inline copy this replaced was the second
    // spelling).
    //
    // ONE predicate, not two (review C4, ARCH §10.6). This gate and the
    // wire resolve below decide the SAME question — who folds these
    // sources into the conversation — and they used to disagree: this one
    // asked the boot-mode fork, the resolve asked whether a wire turn was
    // parked. Since R5 a Local turn rides the wire too, so both were true
    // at once and the sources were folded TWICE, once by this write and
    // once by the daemon resolving the Answer. The wire turn's existence
    // is the only fact either needs, and the daemon's fold is the atomic
    // one (it stamps the conversation's REAL current turn), so the local
    // write runs only when no wire turn will do it.
    //
    // Soft-fail: a missing conversation_id (legacy callers, tests
    // without a wired conversation) skips the registry update; the
    // search still feeds through to refinement so the bench's
    // `submit_information_response` path is unaffected.
    if let Some(ref cid) = conversation_id {
        // sv-surface D9b — NOT repointed: `searched_sources` has no wire
        // accessor (neither `get_conversation`'s response nor any write
        // route carries it). This arm is already dead whenever a wire turn
        // is parked, which since R5 is every real turn; it survives for
        // legacy callers and for bench paths with no conversation wired.
        // Owed with the enabled-corpora write above, or delete the arm once
        // the daemon's fold is proven to be the only one.
        if !state.turn_wire.has(cid).await {
            let store_arc: Option<Arc<dyn sovereign_core::traits::StateStore>> = {
                let guard = state.store.read().await;
                guard.as_ref().map(Arc::clone)
            };
            if let Some(store) = store_arc {
                match store.get_conversation(cid).await {
                    Ok(conv) => {
                        let current_turn = conv.messages.len();
                        let fresh = out
                            .results
                            .iter()
                            .map(|r| (r.url.clone(), r.title.clone(), query.to_string()));
                        let merged = sovereign_core::searched_sources::merge_into(
                            conv.searched_sources,
                            fresh,
                            current_turn,
                        );
                        if let Err(e) = store
                            .set_conversation_searched_sources(cid, Some(merged))
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
    }

    // Wire-first resolve (sv-surface R5/G3b): a search-built answer
    // carries its registry rows IN the Answer, and the daemon folds them
    // into the conversation as part of the resolve — one user action, one
    // atomic effect. The local desk fallback keeps the in-process turn
    // shapes working with the local registry write above.
    // RB5: the socket is THIS card's turn's, found through the
    // conversation the prompt was parked under — not whichever turn
    // started most recently. A card whose turn ended between the click
    // and the search finishing falls through to the local desk.
    let wire_sender = match parked {
        Some(ref p) => state.turn_wire.sender_for(&p.conversation_id).await,
        None => None,
    };
    if let Some(sender) = wire_sender {
        let wire_sources: Vec<sovereign_core::types::SearchedSourceEntry> = sources
            .iter()
            .map(|s| {
                // Turn stamps are placeholders — the daemon's merge stamps
                // the conversation's REAL current turn (pinned by the G3b
                // e2e); the client cannot know it and must not pretend.
                sovereign_core::types::SearchedSourceEntry {
                    url: s.url.clone(),
                    title: s.title.clone(),
                    first_seen_turn: 0,
                    last_referenced_turn: 0,
                    search_query: query.to_string(),
                }
            })
            .collect();
        let sent = sender
            .send_answer(
                &key,
                &sovereign_contracts::types::TurnAnswer::Information {
                    content: Some(formatted.clone()),
                    sources: wire_sources,
                },
            )
            .is_ok();
        state.pending_prompts.resolve(&key).await;
        return Ok(SearchAugmentation {
            query: query.to_string(),
            backend_id: out.backend_id,
            sources,
            accepted: sent,
        });
    }

    let accepted = state
        .approval
        .submit_information_response(&key, Some(formatted));
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
    // sv-surface D9b — `POST /v1/conversations/{id}/end`. The extraction
    // pass has to run on the Runtime that OWNS the conversation's memories,
    // which since R5 is the serving one. This process's `Runtime` in attach
    // has never seen the conversation, so the local call extracted from an
    // empty history and stamped nothing.
    //
    // The soft-fail contract is UNCHANGED on purpose: a failed extraction
    // warns and still answers `Ok(())`, because the user closing an
    // inner-work session must not see an error for a background pass. What
    // changed is only WHICH runtime does the work.
    if let Err(e) = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .end_conversation(&conversation_id)
        .await
    {
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
    // The tombstone crosses the wire in BOTH modes (sv-surface D2) — the
    // daemon's row is the one recall reads, and its route drives the same
    // `delete_memory` decider the deleted local arm called.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
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
    // ONE halving decider, and it is the DAEMON's route (§10.6). This command
    // used to re-derive the formula behind an `is_attach_mode()` fork — read
    // every memory, find the row, `confidence * 0.5` — a second implementation
    // of the same threshold, which is exactly the twin the smell table names.
    // Deleted (sv-surface D2). The new confidence is persisted server-side
    // against the daemon's own row; the command keeps its `Ok(())` frontend
    // contract.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .weaken_memory(&memory_id)
        .await
        .map(|_| ())
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
///
/// sv-surface D9 — reads the DAEMON's register, in BOTH boot modes.
/// This command used to ask THIS process's `Runtime`, which since R5
/// never runs the turn: in attach the register was structurally empty
/// and the pane rendered "no provenance yet" forever. One of the three
/// no-fork degradations the ladder names — broken with no
/// `is_attach_mode()` branch to point at, because nobody wrote one.
#[tauri::command]
pub async fn get_last_turn_provenance(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<Option<sovereign_core::runtime::TurnProvenance>, String> {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .last_turn_provenance::<sovereign_core::runtime::TurnProvenance>(&conversation_id)
        .await
        .map_err(|e| e.to_string())
}

/// The skills the SERVING runtime registered, and which of them it has
/// active.
///
/// sv-surface D9, and the same correction as the command above: the
/// registry that matters is the one the turn is answered against. In
/// attach that is the daemon's, and this process's copy — built from
/// the same manifests but activated from the DESKTOP's config — could
/// disagree with it about the active set with nothing to say so.
#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillEntry>, String> {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .list_skills::<SkillEntry>()
        .await
        .map_err(|e| e.to_string())
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
/// implementation guarantees uniform idempotency. (Pre-2026-05-24
/// also served per-workspace wrappers like
/// `recipe_author_set_workspace_active`; those were removed when
/// routing moved to conversation-tag-driven primary skill selection.)
///
/// Idempotent: if the requested state already matches the stored
/// `config.active_skills`, returns early without `config.save()` and
/// without touching the serving registry. Diagnosed 2026-05-23: the
/// InnerWork surface and a parallel App.svelte view-effect both called
/// the older non-idempotent `toggle_skill` on view-enter, kicking off
/// two ~15s runtime rebuilds that locked the UI for ~30s. Removing
/// that rebuild is what sv-surface D9b did (see the body); the no-op
/// short-circuit stays because it is the right shape for a toggle —
/// callers shouldn't have to track local state to avoid a round trip.
pub async fn toggle_skill_impl(
    state: &Arc<AppState>,
    skill_id: String,
    active: bool,
) -> Result<(), String> {
    let skill_id_for_wire = skill_id.clone();
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

    // sv-surface D9b — `PUT /v1/skills/{id}/active`. The registry that
    // decides which skills a turn may use is the SERVING runtime's, and
    // since R5 that is the daemon's in both modes (in Local the daemon is
    // handed `Arc::clone(&runtime_arc)`, so this PUT reaches the very
    // object this process holds — one registry, one answer).
    //
    // `rebuild_runtime` is GONE from this path, and it is the deletion
    // that matters: a ~15s drop-and-recommission of the whole Runtime, to
    // change one bool in a set. It could not have been doing the job in
    // attach anyway — rebuilding THIS process's registry while the turn is
    // answered against the daemon's is the C2 divergence in miniature.
    // The config write above stays: it is the persisted PREFERENCE the
    // next boot activates from, a different fact from the live set.
    //
    // A 404 means the daemon has no such skill registered. That is a
    // named refusal, not an `Ok(())` — the pane must not paint a toggle
    // the serving runtime never accepted (ARCH §18.3).
    let echoed = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .set_skill_active::<SkillEntry>(&skill_id_for_wire, active)
        .await
        .map_err(|e| format!("toggle_skill: {e}"))?;
    match echoed {
        Some(entry) => {
            tracing::info!(
                skill_id = %entry.id,
                active = entry.active,
                "toggle_skill: the serving registry echoed back"
            );
            Ok(())
        }
        None => Err(format!(
            "toggle_skill: the serving runtime has no skill `{skill_id_for_wire}` registered"
        )),
    }
}

/// The answer document — its extraction, its block flattening and its
/// Markdown rendering — moved to
/// [`sovereign_contracts::types::answer_doc`] in sv-surface D7/G9. It
/// only ever formatted a turn's result, so it was never desktop
/// business; a wire-attached client renders the same export now.
///
/// What stayed here, and why: the `.docx` and `.pdf` encoders below
/// (format-specific byte layout, host business) and `export_answer`'s
/// write to the user's chosen path — a file write to a user-picked
/// destination cannot cross a wire (sv-surface D7 "CANNOT CROSS"). Both
/// walk [`Block`], which is why the shared module exposes it.
use sovereign_contracts::types::answer_doc::{
    doc_blocks, render_answer_markdown, AnswerDoc, Block,
};

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
    // sv-surface D9b — `GET /v1/conversations/{id}`. The wire row already
    // carries the TYPED projection this export needs, run by
    // `project_message_metadata` on the daemon: one reader of the metadata
    // shape, not one per host (ARCH §10.6). So the local
    // `project_message_metadata` call below is gone, not moved — an
    // exported document now cannot depend on which process rendered it.
    let convo = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .get_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())?;
    let msg = convo
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .ok_or_else(|| format!("message {message_id} not found"))?;
    // Format follows the extension the user picked in the save dialog.
    let ext = std::path::Path::new(&dest_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    // The document is built from the TYPED projection the host already
    // ran, not from a second local read of the persisted blob.
    let doc = AnswerDoc::from_projection(&msg.content, msg.provenance.as_ref(), &msg.citations);
    let bytes: Vec<u8> = match ext.as_str() {
        "pdf" => render_answer_pdf(&doc)?,
        "docx" => render_answer_docx(&doc)?,
        // `.md` and anything else fall back to Markdown.
        _ => render_answer_markdown(&doc).into_bytes(),
    };
    std::fs::write(&dest_path, bytes).map_err(|e| format!("write {dest_path}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod export_tests {
    use super::{render_answer_docx, render_answer_pdf};
    use sovereign_contracts::types::answer_doc::{render_answer_markdown, AnswerDoc};
    use sovereign_contracts::types::projection::project_message_metadata;

    /// The persisted-metadata shape the export path actually reads.
    fn fixture_blob() -> serde_json::Value {
        serde_json::json!({
            "provenance": {
                "inference_backend": "Qwen3-8B-Q4_K_M",
                "sources": [
                    {"origin": "case-files-7f2a", "count": 3, "display_name": "Case Files"},
                    {"origin": "sep", "count": 2},
                    {"origin": "wikipedia", "count": 0}
                ]
            },
            "retrieved_chunks": [
                {"title": "Free Will", "corpus_id": "sep", "chunk_id": "sep:fw:3",
                 "snippet": "Compatibilism holds that...\nfreedom is not the absence of cause.",
                 "score": 0.91, "url": "https://plato.stanford.edu/entries/free-will/"},
                {"title": "", "corpus_id": "case-files-7f2a", "chunk_id": "cf:9",
                 "snippet": "The deposition of 12 March.", "score": 0.6}
            ]
        })
    }

    /// Exactly what `export_answer` does to reach a document, minus the
    /// store fetch and the file write.
    fn fixture_doc() -> AnswerDoc {
        let (prov, cites) = project_message_metadata(&Some(fixture_blob()));
        AnswerDoc::from_projection(
            "Free will is compatible with determinism.",
            prov.as_ref(),
            &cites,
        )
    }

    /// THE GOLDEN — sv-surface D7/G9. These bytes were produced by the
    /// blob-reading renderer this file carried before the move, on
    /// `fixture_blob()`; `markdown_golden_survived_the_move` asserted
    /// old == new while both existed, and this pins the surviving half.
    /// The twin lives at
    /// `sovereign_contracts::types::answer_doc::tests::markdown_golden`.
    /// A diff here is a diff in what a user's exported document says.
    const GOLDEN_MD: &str = concat!(
        "# svrnmesh answer\n",
        "\n",
        "*answered by Qwen3-8B-Q4_K_M \u{00B7} searched Case Files, sep*\n",
        "\n",
        "Free will is compatible with determinism.\n",
        "\n",
        "---\n",
        "\n",
        "## Sources\n",
        "\n",
        "This answer was grounded in the following passages from your indexed corpora:\n",
        "\n",
        "1. **Free Will** \u{2014} `sep`\n",
        "   > Compatibilism holds that...\n",
        "   > freedom is not the absence of cause.\n",
        "   <https://plato.stanford.edu/entries/free-will/>\n",
        "\n",
        "2. **(untitled passage)** \u{2014} `case-files-7f2a`\n",
        "   > The deposition of 12 March.\n",
        "\n",
        "---\n",
        "*Exported from svrnmesh \u{2014} provenance preserved.*\n",
    );

    #[test]
    fn markdown_golden_survived_the_move() {
        assert_eq!(render_answer_markdown(&fixture_doc()), GOLDEN_MD);
    }

    #[test]
    fn markdown_without_sources_says_so() {
        let md = render_answer_markdown(&AnswerDoc::from_projection("Hello.", None, &[]));
        assert!(md.contains("Hello."));
        assert!(md.contains("No corpus passages were cited"));
        // Never silently implies sources that aren't there.
        assert!(!md.contains("## Sources"));
    }

    #[test]
    fn answerdoc_extracts_provenance() {
        let d = fixture_doc();
        assert_eq!(d.answered_by.as_deref(), Some("Qwen3-8B-Q4_K_M"));
        // count:0 dropped; the folder's typed name beats its slug.
        assert_eq!(d.corpora, vec!["Case Files".to_string(), "sep".to_string()]);
        assert_eq!(d.sources.len(), 2);
        assert_eq!(d.sources[0].corpus_id, "sep");
    }

    #[test]
    fn docx_is_a_valid_zip_package() {
        let bytes = render_answer_docx(&fixture_doc()).expect("docx renders");
        // Real .docx is an OOXML zip — starts with the PK zip-local-header.
        assert_eq!(&bytes[..2], b"PK");
        assert!(bytes.len() > 300);
    }

    #[test]
    fn pdf_has_pdf_header() {
        let doc = AnswerDoc::from_projection("A short answer.", None, &[]);
        let bytes = render_answer_pdf(&doc).expect("pdf renders");
        assert_eq!(&bytes[..5], b"%PDF-");
        assert!(bytes.len() > 300);
    }
}
