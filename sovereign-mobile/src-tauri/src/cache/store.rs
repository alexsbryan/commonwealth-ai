//! Cache-first reads + reconcile writes over the cached projections.
//!
//! Reads return the local cache immediately (offline-read / instant
//! relaunch — `MOBILE.md` §8, §9). When the host is reachable, the
//! caller refetches and calls the `upsert_*` reconcilers, which advance
//! a row only when the host's version is newer. A completed stream
//! writes the assistant message + provenance + citations in ONE
//! transaction so "persists on completion" + "survives restart" hold
//! even if the app is killed immediately after.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::remote::dto::{CitationDto, ConversationDto, CorpusRefDto, MessageDto, ProvenanceDto};

pub fn upsert_conversation(conn: &Connection, host_id: &str, c: &ConversationDto) -> Result<()> {
    conn.execute(
        "INSERT INTO conversation (id, host_connection_id, title, indexed_in_corpus, created_at, updated_at, synced_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
             title             = excluded.title,
             indexed_in_corpus = excluded.indexed_in_corpus,
             updated_at        = excluded.updated_at,
             synced_version    = excluded.synced_version
         WHERE excluded.synced_version >= conversation.synced_version",
        params![
            c.id,
            host_id,
            c.title,
            c.indexed_in_corpus as i64,
            c.created_at,
            c.updated_at,
            c.synced_version.unwrap_or(0),
        ],
    )?;
    Ok(())
}

/// Reconcile a fetched message + its provenance/citations. Newer
/// `server_version` wins; equal/older is ignored (so a stale refetch
/// never clobbers a freshly streamed bubble).
pub fn upsert_message_full(
    conn: &mut Connection,
    m: &MessageDto,
    provenance: Option<&ProvenanceDto>,
    citations: &[CitationDto],
) -> Result<()> {
    let tx = conn.transaction()?;
    let newer: bool = tx
        .query_row(
            "SELECT ?2 >= COALESCE((SELECT server_version FROM message WHERE id = ?1), -1)",
            params![m.id, m.server_version.unwrap_or(0)],
            |r| r.get(0),
        )
        .unwrap_or(true);
    if newer {
        tx.execute(
            "INSERT INTO message (id, conversation_id, role, content, status, created_at, server_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 content        = excluded.content,
                 status         = excluded.status,
                 server_version = excluded.server_version",
            params![
                m.id,
                m.conversation_id,
                m.role,
                m.content,
                m.status.as_deref().unwrap_or("complete"),
                m.created_at,
                m.server_version.unwrap_or(0),
            ],
        )?;
        if let Some(p) = provenance {
            tx.execute(
                "INSERT INTO response_provenance
                     (message_id, inference_backend, routing_tier, ttft_ms, total_ms,
                      finish_reason, max_tokens_budget, completion_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(message_id) DO UPDATE SET
                     inference_backend = excluded.inference_backend,
                     routing_tier      = excluded.routing_tier,
                     ttft_ms           = excluded.ttft_ms,
                     total_ms          = excluded.total_ms,
                     finish_reason     = excluded.finish_reason,
                     max_tokens_budget = excluded.max_tokens_budget,
                     completion_tokens = excluded.completion_tokens",
                params![
                    m.id,
                    p.inference_backend,
                    p.routing_tier,
                    p.ttft_ms,
                    p.total_ms,
                    p.finish_reason,
                    p.max_tokens_budget,
                    p.completion_tokens
                ],
            )?;
        }
        // Citations are immutable per message — replace wholesale.
        tx.execute("DELETE FROM citation WHERE message_id = ?1", params![m.id])?;
        for c in citations {
            tx.execute(
                "INSERT INTO citation (id, message_id, corpus_id, chunk_id, title, snippet, score, rank)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    format!("{}:{}", m.id, c.rank),
                    m.id,
                    c.corpus_id,
                    c.chunk_id,
                    c.title,
                    c.snippet,
                    c.score,
                    c.rank,
                ],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Mark a message's status (e.g. `streaming` → `failed` on a dropped
/// socket, so reconnect re-fetches it).
pub fn set_message_status(conn: &Connection, message_id: &str, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE message SET status = ?2 WHERE id = ?1",
        params![message_id, status],
    )?;
    Ok(())
}

pub fn replace_corpus_refs(conn: &mut Connection, host_id: &str, refs: &[CorpusRefDto]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM corpus_ref WHERE host_connection_id = ?1",
        params![host_id],
    )?;
    for r in refs {
        tx.execute(
            "INSERT INTO corpus_ref (corpus_id, host_connection_id, display_name, category, icon, chunk_count, scope, mesh_shared, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s','now'))",
            params![
                r.corpus_id,
                host_id,
                r.display_name,
                r.category,
                r.icon,
                r.chunk_count,
                r.scope,
                r.mesh_shared as i64,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Resolve a citation's snippet from cache — the (corpus_id, chunk_id)
/// handle that proves "leveraging an installed corpus" (§4).
pub fn citation_snippet(conn: &Connection, corpus_id: &str, chunk_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT snippet FROM citation WHERE corpus_id = ?1 AND chunk_id = ?2 LIMIT 1",
            params![corpus_id, chunk_id],
            |r| r.get(0),
        )
        .ok())
}

// ─── Cache-first reads (offline-read / instant relaunch) ──────

/// Conversation list (no message bodies) from cache, newest first.
pub fn read_conversations(conn: &Connection, host_id: &str) -> Result<Vec<ConversationDto>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, indexed_in_corpus, created_at, updated_at, synced_version
         FROM conversation WHERE host_connection_id = ?1 ORDER BY updated_at DESC",
    )?;
    let rows = stmt
        .query_map(params![host_id], |r| {
            Ok(ConversationDto {
                id: r.get(0)?,
                title: r.get(1)?,
                messages: Vec::new(),
                indexed_in_corpus: r.get::<_, i64>(2)? != 0,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
                synced_version: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Full conversation (messages + provenance + citations) from cache.
/// `sources` are not persisted in the cache (a provenance nicety, not a
/// citation), so they come back empty on offline reads.
pub fn read_conversation(conn: &Connection, id: &str) -> Result<Option<ConversationDto>> {
    let base = conn.query_row(
        "SELECT title, indexed_in_corpus, created_at, updated_at, synced_version
         FROM conversation WHERE id = ?1",
        params![id],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        },
    );
    let (title, indexed_in_corpus, created_at, updated_at, synced_version) = match base {
        Ok(b) => b,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let mut stmt = conn.prepare(
        "SELECT id, role, content, status, created_at, server_version
         FROM message WHERE conversation_id = ?1 ORDER BY created_at ASC",
    )?;
    let mut messages: Vec<MessageDto> = stmt
        .query_map(params![id], |r| {
            Ok(MessageDto {
                id: r.get(0)?,
                conversation_id: id.to_string(),
                role: r.get(1)?,
                content: r.get(2)?,
                status: r.get(3)?,
                created_at: r.get(4)?,
                server_version: r.get(5)?,
                provenance: None,
                citations: Vec::new(),
                metadata: None,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for m in &mut messages {
        m.provenance = conn
            .query_row(
                "SELECT inference_backend, routing_tier, ttft_ms, total_ms,
                        finish_reason, max_tokens_budget, completion_tokens
                 FROM response_provenance WHERE message_id = ?1",
                params![m.id],
                |r| {
                    Ok(ProvenanceDto {
                        inference_backend: r.get(0)?,
                        routing_tier: r.get(1)?,
                        ttft_ms: r.get(2)?,
                        total_ms: r.get(3)?,
                        finish_reason: r.get(4)?,
                        max_tokens_budget: r.get(5)?,
                        completion_tokens: r.get(6)?,
                        sources: Vec::new(),
                    })
                },
            )
            .ok();
        let mut cs = conn.prepare(
            "SELECT corpus_id, chunk_id, title, snippet, score, rank
             FROM citation WHERE message_id = ?1 ORDER BY rank ASC",
        )?;
        m.citations = cs
            .query_map(params![m.id], |r| {
                Ok(CitationDto {
                    corpus_id: r.get(0)?,
                    chunk_id: r.get(1)?,
                    title: r.get(2)?,
                    snippet: r.get(3)?,
                    score: r.get(4)?,
                    rank: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    Ok(Some(ConversationDto {
        id: id.to_string(),
        title,
        messages,
        indexed_in_corpus,
        created_at,
        updated_at,
        synced_version,
    }))
}
