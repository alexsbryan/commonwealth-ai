// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign audit --recover` (Phase 7.3).
//!
//! When a session SIGKILL's mid-flight, the in-process pattern
//! matcher's `tokio::spawn` may not have finished writing its
//! observed-source notes. The tool_call_log ring buffer is durable
//! (it's persisted to SQLite synchronously), but the derived
//! pattern notes aren't. Recovery walks the log, replays the
//! matcher's pure scan against each session's rows, and writes the
//! same `source='observed'` notes the live path would have — so a
//! crash doesn't cost the audit its observed-patterns floor.
//!
//! ## Idempotency
//!
//! Each recovered match is keyed by `(session_id, content)`.
//! Before persisting, we check the store for an existing observed
//! note with the same content under the same session id; if one
//! exists, we skip. Re-running `audit --recover` on the same
//! database is a no-op.
//!
//! ## Scope
//!
//! Recovery surfaces two streams:
//!
//! 1. **`source='observed'`** — replay the
//!    [`ToolPatternMatcher`](sovereign_tools::notes::patterns::ToolPatternMatcher)
//!    against `tool_call_log` rows for every session that has
//!    them. Closes "daemon crashed mid-session, lost the
//!    pattern matches the live path would have written."
//!
//! 2. **`source='inferred'`** — walk the
//!    `sovereign_store::ConversationStore::messages` table and
//!    run [`response_mine::mine`] over each assistant-role row,
//!    persisting decision-shaped sentences as inferred-source
//!    notes (gap D, Phase 7.3). Closes "daemon crashed before
//!    the per-turn `decision_extractor` middleware could
//!    persist its candidate."
//!
//! The `conversation_id` from sovereign-store maps to the
//! `session_id` column on the inferred notes verbatim. Two
//! daemon-side identifiers (`X-Session-Id` for the inference
//! pipeline, conversation_id for the persisted transcript) are
//! tracked separately upstream; recovery uses whichever of them
//! is the load-bearing key for the data source it's reading
//! from. The audit doesn't group decisions on session_id at the
//! user-visible layer, so the cross-key boundary doesn't
//! muddle anything.
//!
//! Future scope:
//! - Detect session_start hashes from gossiped session metadata
//!   and run `diff_extract` for each unrecovered session
//!   (gap E, Phase 7.3).
//!
//! ## Output
//!
//! Prints a one-line summary per session it touched plus a final
//! "recovered N notes across M sessions" line. Returns exit code
//! 0 on success regardless of how many notes were written —
//! recovery is best-effort and an empty result ("everything
//! already recorded") is success.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use sovereign_core::traits::ConversationStore;
use sovereign_core::types::Role;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::notes::patterns::{ObservedPattern, PatternRule, ToolPatternMatcher};
use sovereign_tools::notes::response_mine;

/// Maximum sessions inspected in a single recover run. The ring
/// buffer caps at 10k rows — that's typically far fewer than 10k
/// distinct sessions, but we cap defensively so a runaway log
/// can't tie up the audit indefinitely.
const MAX_RECOVER_SESSIONS: usize = 200;

/// Maximum rows pulled from `tool_call_log` per session. Phase
/// 7.1's matcher uses an 8-row sliding window in production; we
/// allow more here since recovery scans the full session history.
const MAX_ROWS_PER_SESSION: usize = 256;

/// Maximum conversations walked during the inferred-source pass.
/// The conversations table can grow without bound; recovery
/// processes the most-recently-updated ones first and caps to
/// avoid timing out on a long-running install.
const MAX_RECOVER_CONVERSATIONS: usize = 200;

/// Maximum inferred-source notes persisted per conversation. A
/// chatty 200-turn session running through the full
/// `MAX_MATCHES_PER_CALL = 12` cap of `response_mine` could
/// produce 2,400 candidate matches; capping at 20 per
/// conversation keeps the audit's "Open questions" /
/// inferred-decision bucket tractable for an end-of-week reader.
const MAX_INFERRED_PER_CONVERSATION: usize = 20;

pub async fn cmd_audit_recover() -> i32 {
    let Some(notes_db) = locate_notes_db() else {
        eprintln!(
            "  sovereign audit --recover: could not locate notes.db. \
             Run from inside an initialised sovereign repo, or run \
             `sovereign init` first."
        );
        return 1;
    };

    let store = match NoteStore::open(&notes_db) {
        Ok(s) => std::sync::Arc::new(s),
        Err(e) => {
            eprintln!(
                "  sovereign audit --recover: cannot open {}: {e}",
                notes_db.display()
            );
            return 1;
        }
    };

    // Pull every reachable tool_call_log row, then group by session.
    // The ring buffer is bounded at 10k rows; reading them all once
    // is cheap.
    let rows = match store.tool_call_log_rows(0, 10_000).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  sovereign audit --recover: failed to read tool_call_log: {e}");
            return 1;
        }
    };

    if rows.is_empty() {
        println!("  sovereign audit --recover: tool_call_log is empty; nothing to recover.");
        return 0;
    }

    // Group rows by session_id. The reader returns newest-first,
    // which is what `ToolPatternMatcher::scan` expects.
    let mut by_session: HashMap<String, Vec<corpus_engine_notes::ToolCallLogRow>> = HashMap::new();
    for row in rows {
        by_session
            .entry(row.session_id.clone())
            .or_default()
            .push(row);
    }
    if by_session.len() > MAX_RECOVER_SESSIONS {
        // Take the most-recent N sessions by their newest row's
        // timestamp. A 10k-row log with 200+ sessions is unusual
        // but not impossible (lots of short sessions); we'd rather
        // process the latest than time out.
        let mut sessions: Vec<(String, Vec<_>)> = by_session.into_iter().collect();
        sessions.sort_by(|(_, a), (_, b)| {
            b.first()
                .map(|r| r.called_at)
                .unwrap_or(0)
                .cmp(&a.first().map(|r| r.called_at).unwrap_or(0))
        });
        sessions.truncate(MAX_RECOVER_SESSIONS);
        by_session = sessions.into_iter().collect();
    }

    println!(
        "  sovereign audit --recover: scanning {} session{}",
        by_session.len(),
        if by_session.len() == 1 { "" } else { "s" }
    );

    let mut total_recovered = 0_usize;
    let mut sessions_touched = 0_usize;
    for (session_id, session_rows) in &by_session {
        // Trim per-session rows to the recovery cap. They're
        // already newest-first thanks to the reader's ORDER BY.
        let limited: Vec<&corpus_engine_notes::ToolCallLogRow> =
            session_rows.iter().take(MAX_ROWS_PER_SESSION).collect();

        // Pure scan — fresh cooldown set so all rule fires are
        // considered. The recovery path doesn't model the
        // production cooldown because we don't know what state
        // the live matcher had when it crashed; better to surface
        // the patterns than miss them.
        let mut cooldowns = HashMap::<PatternRule, u32>::new();
        let hits = ToolPatternMatcher::scan_for_recovery(&limited, &mut cooldowns);

        if hits.is_empty() {
            continue;
        }

        // Existing observed-source notes for this session — used
        // to dedup. We compare on body content rather than UUID
        // so a fresh recover on the same data is a no-op.
        let existing_bodies = existing_observed_bodies(&store, session_id).await;

        let mut wrote = 0_usize;
        for hit in &hits {
            if existing_bodies.contains(&hit.message) {
                continue;
            }
            if let Err(e) = persist_recovered(&store, session_id, hit).await {
                tracing::warn!(
                    session_id = %session_id,
                    rule = hit.rule.as_str(),
                    error = %e,
                    "audit_recover: failed to persist observed note"
                );
                continue;
            }
            wrote += 1;
        }

        if wrote > 0 {
            sessions_touched += 1;
            total_recovered += wrote;
            println!(
                "    {session_id}: recovered {wrote} observed pattern{} from {} log row{}",
                if wrote == 1 { "" } else { "s" },
                limited.len(),
                if limited.len() == 1 { "" } else { "s" },
            );
        }
    }

    // ── Inferred-source pass over messages table ─────────────────
    //
    // Best-effort: if the conversation store isn't reachable
    // (different deployment, no daemon ever ran, custom data dir)
    // we log a warning and skip. The observed-source pass above
    // already wrote whatever it could; the audit floor stays
    // non-empty.
    let (inferred_total, conversations_touched) = recover_inferred_from_messages(&store).await;

    println!();
    println!(
        "  Recovered {total_recovered} observed note{} across {sessions_touched} session{} \
         + {inferred_total} inferred note{} across {conversations_touched} conversation{}.",
        if total_recovered == 1 { "" } else { "s" },
        if sessions_touched == 1 { "" } else { "s" },
        if inferred_total == 1 { "" } else { "s" },
        if conversations_touched == 1 { "" } else { "s" },
    );
    0
}

/// Walk the `messages` table, run `response_mine::mine` on each
/// assistant-role row, persist `source='inferred'` notes for every
/// match that isn't already on file. Returns `(notes_written,
/// conversations_touched)`.
///
/// Best-effort: opens the canonical state.db; a missing or
/// unreadable store yields `(0, 0)` with a warn-level log. The
/// caller's summary line then naturally reads "0 inferred notes"
/// rather than the harvest crashing.
async fn recover_inferred_from_messages(notes: &Arc<NoteStore>) -> (usize, usize) {
    let Some(state_db) = locate_state_db() else {
        tracing::info!("audit_recover: no state.db reachable; skipping inferred-source pass");
        return (0, 0);
    };

    let store = match SqliteStateStore::open(&state_db) {
        Ok(s) => Arc::new(s) as Arc<dyn ConversationStore>,
        Err(e) => {
            tracing::warn!(
                state_db = %state_db.display(),
                error = %e,
                "audit_recover: cannot open state.db for inferred-source pass"
            );
            return (0, 0);
        }
    };

    recover_inferred_with_store(notes, store.as_ref()).await
}

/// Pure inner loop — accepts any [`ConversationStore`]. Pulled out
/// so unit tests can drive it with an in-memory store.
async fn recover_inferred_with_store(
    notes: &Arc<NoteStore>,
    store: &dyn ConversationStore,
) -> (usize, usize) {
    // `list_conversations` orders by `updated_at DESC` so the cap
    // takes the most-recent ones. Soft-deleted conversations are
    // already excluded by the WHERE clause.
    let convos = match store.list_conversations(MAX_RECOVER_CONVERSATIONS, 0).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "audit_recover: list_conversations failed"
            );
            return (0, 0);
        }
    };
    if convos.is_empty() {
        return (0, 0);
    }

    let mut total_inferred = 0_usize;
    let mut conversations_touched = 0_usize;
    for convo in &convos {
        // `list_conversations` doesn't load messages by default
        // (the SQL only selects conversation columns); re-fetch
        // the full conversation to get the messages array.
        let full = match store.get_conversation(&convo.id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    conversation_id = %convo.id,
                    error = %e,
                    "audit_recover: get_conversation failed; skipping"
                );
                continue;
            }
        };
        // `dedup_seed` is the set of bodies we've already
        // persisted for this conversation — used to avoid
        // re-writing the same inferred note on repeated recover
        // runs. Pre-loaded once per conversation rather than
        // per-match so we hit the notes DB at most once.
        let mut dedup_seed = existing_inferred_bodies(notes, &convo.id).await;
        let mut wrote_for_convo = 0_usize;

        for msg in &full.messages {
            if msg.role != Role::Assistant {
                continue;
            }
            if wrote_for_convo >= MAX_INFERRED_PER_CONVERSATION {
                break;
            }
            for hit in response_mine::mine(&msg.content) {
                if wrote_for_convo >= MAX_INFERRED_PER_CONVERSATION {
                    break;
                }
                if dedup_seed.contains(&hit.sentence) {
                    continue;
                }
                if let Err(e) = persist_inferred(notes, &convo.id, &hit.sentence).await {
                    tracing::warn!(
                        conversation_id = %convo.id,
                        error = %e,
                        "audit_recover: failed to persist inferred note"
                    );
                    continue;
                }
                dedup_seed.insert(hit.sentence);
                wrote_for_convo += 1;
            }
        }
        if wrote_for_convo > 0 {
            conversations_touched += 1;
            total_inferred += wrote_for_convo;
            println!(
                "    convo {}: recovered {wrote_for_convo} inferred decision{}",
                short_id(&convo.id),
                if wrote_for_convo == 1 { "" } else { "s" },
            );
        }
    }
    (total_inferred, conversations_touched)
}

/// Find the canonical `state.db` for the current install. Mirrors
/// the awareness commands' search order:
///
/// 1. `~/.sovereign/state.db` — the user-scoped store the daemon
///    writes to by default.
/// 2. `./.sovereign/state.db` if no home dir.
fn locate_state_db() -> Option<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".sovereign").join("state.db");
        if p.exists() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir()
        .ok()?
        .join(".sovereign")
        .join("state.db");
    if cwd.exists() {
        Some(cwd)
    } else {
        None
    }
}

/// First eight chars of a UUID-style id — short enough for the
/// summary line, long enough to disambiguate.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Mirror of [`existing_observed_bodies`] for the inferred-source
/// pass. Same dedup-by-body strategy: a recovered note is keyed
/// off its full body text, so repeated `--recover` runs are
/// idempotent.
async fn existing_inferred_bodies(store: &NoteStore, session_id: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let rows = match store.read_notes(None, &[], &[], &[], 500, false).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "audit_recover: read_notes failed for inferred dedup"
            );
            return out;
        }
    };
    for n in rows {
        if n.session_id == session_id && n.source == NoteSource::Inferred.as_str() {
            out.insert(n.content);
        }
    }
    out
}

/// Persist an `inferred`-source note. Writes the matched sentence
/// verbatim as a `kind='decision'` row — the audit's source
/// priority sort puts inferred below agent/committed/extracted,
/// so reviewers see this as the lowest-confidence decision
/// stream. The matcher's stoplist already filters mechanical
/// chatter at the source.
async fn persist_inferred(
    store: &NoteStore,
    conversation_id: &str,
    sentence: &str,
) -> Result<(), String> {
    store
        .write_note_with_source(
            "decision",
            sentence,
            Vec::new(),
            Vec::new(),
            conversation_id,
            NoteScope::Global,
            None,
            None,
            NoteSource::Inferred,
            None,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Walk the standard search path for `notes.db`:
///
/// 1. `<repo>/.sovereign/notes.db` (project-scoped)
/// 2. `~/.sovereign/notes.db` (user-scoped fallback for sessions
///    that ran outside a repo)
fn locate_notes_db() -> Option<PathBuf> {
    if let Some(repo_root) = crate::project_cmd::find_repo_root() {
        let p = repo_root.join(".sovereign").join("notes.db");
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".sovereign").join("notes.db");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Read the bodies of every active observed-source note for a
/// given session. Used to dedup before persisting the matcher's
/// output during recovery.
async fn existing_observed_bodies(store: &NoteStore, session_id: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    // No source-filtered reader exists; pull a window of recent
    // notes and post-filter. 500 rows comfortably covers a single
    // session's observation set for the typical agent workflow.
    let rows = match store.read_notes(None, &[], &[], &[], 500, false).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "audit_recover: read_notes failed; treating dedup set as empty"
            );
            return out;
        }
    };
    for n in rows {
        if n.session_id == session_id && n.source == NoteSource::Observed.as_str() {
            out.insert(n.content);
        }
    }
    out
}

async fn persist_recovered(
    store: &NoteStore,
    session_id: &str,
    hit: &ObservedPattern,
) -> Result<(), String> {
    store
        .write_note_with_source(
            "reflection",
            &hit.message,
            hit.tools.clone(),
            Vec::new(),
            session_id,
            NoteScope::Global,
            None,
            None,
            NoteSource::Observed,
            None,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Recovery writes observed-source notes for sessions that
    /// have tool calls but no matching notes yet.
    #[tokio::test]
    async fn recover_writes_observed_notes_when_session_log_unobserved() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        // Seed a blast→build session — the InvestigateThenAct rule
        // fires.
        store
            .log_tool_call("recover-sess-1", "blast", "success")
            .await
            .unwrap();
        store
            .log_tool_call("recover-sess-1", "build", "success")
            .await
            .unwrap();

        // Pre-condition: no observed notes yet for this session.
        let pre = existing_observed_bodies(&store, "recover-sess-1").await;
        assert!(pre.is_empty());

        // Hand-roll the recovery flow over this store.
        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        let session_rows: Vec<&corpus_engine_notes::ToolCallLogRow> = rows
            .iter()
            .filter(|r| r.session_id == "recover-sess-1")
            .collect();
        let mut cooldowns = HashMap::new();
        let hits = ToolPatternMatcher::scan_for_recovery(&session_rows, &mut cooldowns);
        assert!(!hits.is_empty(), "matcher should fire on blast→build");
        for hit in &hits {
            persist_recovered(&store, "recover-sess-1", hit)
                .await
                .unwrap();
        }

        let post = existing_observed_bodies(&store, "recover-sess-1").await;
        assert!(!post.is_empty(), "recovery should have written notes");
    }

    /// Recovery is idempotent: running the same scan a second
    /// time writes nothing new because the dedup set already
    /// contains the bodies.
    #[tokio::test]
    async fn recover_is_idempotent_via_body_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        store
            .log_tool_call("idem-sess", "blast", "success")
            .await
            .unwrap();
        store
            .log_tool_call("idem-sess", "build", "success")
            .await
            .unwrap();

        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        let session_rows: Vec<&corpus_engine_notes::ToolCallLogRow> = rows
            .iter()
            .filter(|r| r.session_id == "idem-sess")
            .collect();

        // First pass.
        let mut cooldowns = HashMap::new();
        let hits = ToolPatternMatcher::scan_for_recovery(&session_rows, &mut cooldowns);
        let mut existing = existing_observed_bodies(&store, "idem-sess").await;
        let mut first_wrote = 0;
        for hit in &hits {
            if !existing.contains(&hit.message) {
                persist_recovered(&store, "idem-sess", hit).await.unwrap();
                existing.insert(hit.message.clone());
                first_wrote += 1;
            }
        }
        assert!(first_wrote >= 1);

        // Second pass.
        let mut cooldowns2 = HashMap::new();
        let hits2 = ToolPatternMatcher::scan_for_recovery(&session_rows, &mut cooldowns2);
        let existing2 = existing_observed_bodies(&store, "idem-sess").await;
        let mut second_wrote = 0;
        for hit in &hits2 {
            if !existing2.contains(&hit.message) {
                persist_recovered(&store, "idem-sess", hit).await.unwrap();
                second_wrote += 1;
            }
        }
        assert_eq!(
            second_wrote, 0,
            "second recovery pass should write zero new notes — dedup failed"
        );
    }

    /// Sessions with only logged calls but no qualifying patterns
    /// produce no recovery output (and don't error).
    #[tokio::test]
    async fn recover_emits_nothing_when_no_patterns_qualify() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        // Just a single read — no sequence/gap pattern qualifies.
        store
            .log_tool_call("solo-sess", "callers", "success")
            .await
            .unwrap();

        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        let session_rows: Vec<&corpus_engine_notes::ToolCallLogRow> = rows
            .iter()
            .filter(|r| r.session_id == "solo-sess")
            .collect();
        let mut cooldowns = HashMap::new();
        let hits = ToolPatternMatcher::scan_for_recovery(&session_rows, &mut cooldowns);
        assert!(
            hits.is_empty(),
            "single read shouldn't fire any patterns: {hits:?}"
        );
    }

    // ─── Inferred-source recovery from messages table ─────────────

    use sovereign_core::types::{Message, Role};

    fn assistant_msg(convo_id: &str, content: &str, idx: i64) -> Message {
        Message {
            id: format!("{convo_id}-{idx}"),
            conversation_id: convo_id.into(),
            role: Role::Assistant,
            content: content.into(),
            // The sqlite store uses unix-second epoch timestamps;
            // sequential `idx` keeps ordering deterministic in tests.
            created_at: 1_700_000_000 + idx,
            metadata: None,
            version: 0,
        }
    }

    fn user_msg(convo_id: &str, content: &str, idx: i64) -> Message {
        Message {
            id: format!("{convo_id}-u-{idx}"),
            conversation_id: convo_id.into(),
            role: Role::User,
            content: content.into(),
            created_at: 1_700_000_000 + idx,
            metadata: None,
            version: 0,
        }
    }

    /// `recover_inferred_with_store` walks an in-memory state
    /// store, mines assistant rows for decision phrases, and
    /// persists `source='inferred'` notes against the
    /// conversation id.
    #[tokio::test]
    async fn inferred_recovery_persists_decisions_from_assistant_messages() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        let state = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        let convo = "conv-decisions";
        // Two assistant rows with decision phrasing + one without
        // + one user row that should be ignored.
        state
            .save_message(&user_msg(convo, "what should we use?", 1))
            .await
            .unwrap();
        state
            .save_message(&assistant_msg(
                convo,
                "I'll use BTreeMap because we need ordered iteration.",
                2,
            ))
            .await
            .unwrap();
        state
            .save_message(&assistant_msg(
                convo,
                "Reading the file now.", // no trigger
                3,
            ))
            .await
            .unwrap();
        state
            .save_message(&assistant_msg(
                convo,
                "Decided to use libsodium for chart encryption.",
                4,
            ))
            .await
            .unwrap();

        let (wrote, touched) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert_eq!(wrote, 2, "expected 2 inferred notes from 2 decision rows");
        assert_eq!(touched, 1, "one conversation touched");

        // Verify they're tagged source='inferred' and bound to the
        // conversation id as session_id.
        let rows = notes
            .read_notes(None, &[], &[], &["decision".into()], 100, false)
            .await
            .unwrap();
        let inferred: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Inferred.as_str())
            .collect();
        assert_eq!(inferred.len(), 2);
        assert!(inferred.iter().all(|n| n.session_id == convo));
    }

    /// Re-running recovery on the same state.db is idempotent —
    /// the dedup-by-body path drops every match the second time.
    #[tokio::test]
    async fn inferred_recovery_is_idempotent_via_body_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        let state = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        state
            .save_message(&assistant_msg(
                "c1",
                "I'll use Postgres for the storage layer.",
                1,
            ))
            .await
            .unwrap();

        let (first, _) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert_eq!(first, 1);

        let (second, _) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert_eq!(
            second, 0,
            "second pass should write zero — dedup by body failed"
        );
    }

    /// User-role messages are never mined. Only `assistant`.
    #[tokio::test]
    async fn inferred_recovery_ignores_user_messages() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        let state = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        // Decision-shaped phrasing in a USER message — must be skipped.
        state
            .save_message(&user_msg(
                "u-only",
                "I'll use BTreeMap because ordered iteration matters.",
                1,
            ))
            .await
            .unwrap();
        // Plus a non-decision assistant message so the conversation
        // is reachable but produces zero matches.
        state
            .save_message(&assistant_msg("u-only", "OK noted.", 2))
            .await
            .unwrap();

        let (wrote, touched) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert_eq!(wrote, 0, "user-role decision should not fire response_mine");
        assert_eq!(touched, 0);
    }

    /// Per-conversation cap prevents a 200-turn chatty session
    /// from dominating the audit. We seed >cap matches and assert
    /// the persistence stops at the limit.
    #[tokio::test]
    async fn inferred_recovery_caps_per_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        let state = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        // Each row carries multiple decision phrases; together they
        // far exceed MAX_INFERRED_PER_CONVERSATION even after dedup.
        for i in 0..10 {
            state
                .save_message(&assistant_msg(
                    "chatty",
                    &format!(
                        "I'll use option {i} because of reason {i}. \
                         Decided to ship variant-{i} this turn. \
                         Chose path {i} over the alternative for reasons-{i}.",
                    ),
                    i as i64,
                ))
                .await
                .unwrap();
        }

        let (wrote, _) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert!(
            wrote <= MAX_INFERRED_PER_CONVERSATION,
            "expected ≤{MAX_INFERRED_PER_CONVERSATION} inferred notes; got {wrote}"
        );
        assert!(
            wrote >= 1,
            "should have written at least one — empty result indicates a bug"
        );
    }

    /// An empty store yields (0, 0) without error.
    #[tokio::test]
    async fn inferred_recovery_empty_store_returns_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

        let state = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        let (wrote, touched) = recover_inferred_with_store(&notes, state.as_ref()).await;
        assert_eq!(wrote, 0);
        assert_eq!(touched, 0);
    }
}
