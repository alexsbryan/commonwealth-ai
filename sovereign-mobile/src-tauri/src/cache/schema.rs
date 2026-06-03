//! SQLite schema — client-owned tables + cached projections of host
//! state, mirroring the `MOBILE.md` ERD.
//!
//! - **Client-owned** (source of truth on device): `host_connection`.
//!   The token is NOT stored here — it lives in the keychain.
//!   `credential` holds only the non-secret tenant metadata.
//! - **Cached projections** (server is source of truth; keyed by
//!   server-origin IDs; safe to evict + re-fetch): `conversation`,
//!   `message`, `response_provenance`, `citation`, `corpus_ref`.
//!   Reconciled via `synced_version` / `server_version`.

use rusqlite::Connection;

use crate::error::Result;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        -- Client-owned: the only records the phone authors.
        CREATE TABLE IF NOT EXISTS host_connection (
            id              TEXT PRIMARY KEY,
            display_name    TEXT NOT NULL,
            tailnet_address TEXT NOT NULL,
            is_default      INTEGER NOT NULL DEFAULT 0,
            last_status     TEXT NOT NULL DEFAULT 'off_tailnet',
            created_at      INTEGER NOT NULL
        );

        -- Non-secret tenant metadata. The token lives in the keychain
        -- under key "sovereign.token.<host_connection_id>".
        CREATE TABLE IF NOT EXISTS credential (
            id                 TEXT PRIMARY KEY,
            host_connection_id TEXT NOT NULL REFERENCES host_connection(id) ON DELETE CASCADE,
            tenant_id          TEXT NOT NULL,
            issued_at          INTEGER,
            expires_at         INTEGER
        );

        -- Cached projections (server-origin ids; evictable).
        CREATE TABLE IF NOT EXISTS conversation (
            id                 TEXT PRIMARY KEY,
            host_connection_id TEXT NOT NULL REFERENCES host_connection(id) ON DELETE CASCADE,
            title              TEXT,
            -- true once the host indexes this conversation into the
            -- per-identity conversation corpus (then retrievable as a corpus).
            indexed_in_corpus  INTEGER NOT NULL DEFAULT 0,
            created_at         INTEGER,
            updated_at         INTEGER,
            synced_version     INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS message (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'complete', -- streaming|complete|failed
            created_at      INTEGER,
            server_version  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS response_provenance (
            message_id        TEXT PRIMARY KEY REFERENCES message(id) ON DELETE CASCADE,
            inference_backend TEXT NOT NULL,
            routing_tier      TEXT,
            ttft_ms           INTEGER,
            total_ms          INTEGER
        );

        CREATE TABLE IF NOT EXISTS citation (
            id          TEXT PRIMARY KEY,
            message_id  TEXT NOT NULL REFERENCES message(id) ON DELETE CASCADE,
            corpus_id   TEXT NOT NULL,
            chunk_id    TEXT NOT NULL,
            title       TEXT,
            snippet     TEXT,
            score       REAL,
            rank        INTEGER
        );

        CREATE TABLE IF NOT EXISTS corpus_ref (
            corpus_id          TEXT PRIMARY KEY,
            host_connection_id TEXT NOT NULL REFERENCES host_connection(id) ON DELETE CASCADE,
            display_name       TEXT,
            category           TEXT,
            icon               TEXT,
            chunk_count        INTEGER,
            -- Privacy posture: 'local' (private to this host) vs 'mesh'.
            scope              TEXT,
            mesh_shared        INTEGER NOT NULL DEFAULT 0,
            last_seen          INTEGER
        );

        -- APPROVAL_REQUEST is modeled but not wired in v1 (tool approvals
        -- are out of scope for the milestone).
        CREATE TABLE IF NOT EXISTS approval_request (
            id                 TEXT PRIMARY KEY,
            host_connection_id TEXT NOT NULL REFERENCES host_connection(id) ON DELETE CASCADE,
            tool_name          TEXT,
            summary            TEXT,
            status             TEXT,
            requested_at       INTEGER,
            expires_at         INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_message_conv ON message(conversation_id);
        CREATE INDEX IF NOT EXISTS idx_citation_msg ON citation(message_id);
        CREATE INDEX IF NOT EXISTS idx_conv_host    ON conversation(host_connection_id);
        "#,
    )?;
    Ok(())
}
