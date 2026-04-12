use rusqlite::Connection;

pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        -- Conversations and messages
        CREATE TABLE IF NOT EXISTS conversations (
            id          TEXT PRIMARY KEY,
            title       TEXT,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            role            TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system')),
            content         TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            metadata        TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_messages_conversation
            ON messages(conversation_id, created_at);

        -- FTS5 for full-text search over messages
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            content,
            content=messages,
            content_rowid=rowid
        );

        -- Triggers to keep FTS index in sync
        CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
        END;

        CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.rowid, old.content);
        END;

        CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
        END;

        -- Task execution state
        CREATE TABLE IF NOT EXISTS tasks (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            goal            TEXT NOT NULL,
            plan            TEXT NOT NULL,
            state           TEXT NOT NULL,
            status          TEXT NOT NULL CHECK(status IN ('running', 'paused', 'completed', 'failed')),
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        -- RAG: document store and embeddings
        CREATE TABLE IF NOT EXISTS documents (
            id          TEXT PRIMARY KEY,
            source      TEXT NOT NULL,
            content     TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            embedding   BLOB,
            created_at  INTEGER NOT NULL
        );

        -- FTS5 for full-text search over documents
        CREATE VIRTUAL TABLE IF NOT EXISTS documents_fts USING fts5(
            content,
            source,
            content=documents,
            content_rowid=rowid
        );

        CREATE TRIGGER IF NOT EXISTS documents_ai AFTER INSERT ON documents BEGIN
            INSERT INTO documents_fts(rowid, content, source) VALUES (new.rowid, new.content, new.source);
        END;

        CREATE TRIGGER IF NOT EXISTS documents_ad AFTER DELETE ON documents BEGIN
            INSERT INTO documents_fts(documents_fts, rowid, content, source) VALUES('delete', old.rowid, old.content, old.source);
        END;

        -- Long-term user memory
        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT PRIMARY KEY,
            content     TEXT NOT NULL,
            source      TEXT NOT NULL,
            confidence  REAL NOT NULL,
            created_at  INTEGER NOT NULL,
            last_used   INTEGER NOT NULL
        );

        -- FTS5 for full-text search over memories
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content,
            content=memories,
            content_rowid=rowid
        );

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
        END;

        -- Tool permissions
        CREATE TABLE IF NOT EXISTS permissions (
            tool_id     TEXT NOT NULL,
            scope       TEXT NOT NULL,
            granted     INTEGER NOT NULL,
            granted_at  INTEGER NOT NULL,
            PRIMARY KEY (tool_id, scope)
        );

        -- Router performance log
        CREATE TABLE IF NOT EXISTS routing_log (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            message_hash    TEXT,
            classified_as   TEXT,
            was_correct     INTEGER,
            latency_ms      INTEGER,
            oicp_match_quality TEXT,
            oicp_model_id   TEXT,
            created_at      INTEGER NOT NULL
        );

        -- Knowledge base: corpus state tracking
        CREATE TABLE IF NOT EXISTS corpus_state (
            corpus_id    TEXT PRIMARY KEY,
            installed_at INTEGER NOT NULL,
            source_date  TEXT NOT NULL,
            chunks_count INTEGER NOT NULL DEFAULT 0,
            index_size_mb INTEGER NOT NULL DEFAULT 0,
            last_updated INTEGER NOT NULL
        );

        -- Knowledge base: web search budget tracking
        CREATE TABLE IF NOT EXISTS search_budget (
            backend         TEXT PRIMARY KEY,
            monthly_limit   INTEGER NOT NULL,
            used_this_month INTEGER NOT NULL DEFAULT 0,
            reset_date      INTEGER NOT NULL
        );

        -- Health: per-cycle check reports
        CREATE TABLE IF NOT EXISTS health_reports (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            component   TEXT    NOT NULL,
            status      TEXT    NOT NULL,
            issues_json TEXT    NOT NULL,
            summary     TEXT    NOT NULL,
            measured_at INTEGER NOT NULL
        );

        -- Health: pending decisions that require user action
        CREATE TABLE IF NOT EXISTS pending_health_decisions (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            component    TEXT    NOT NULL,
            issue_json   TEXT    NOT NULL,
            question     TEXT    NOT NULL,
            options_json TEXT    NOT NULL,
            consequence  TEXT    NOT NULL,
            surfaced_at  INTEGER NOT NULL,
            resolved_at  INTEGER
        );
        ",
    )
}

/// Add columns to the documents table for knowledge base support.
/// These are run separately because SQLite does not support
/// `ALTER TABLE ADD COLUMN IF NOT EXISTS`.
pub fn run_column_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute_batch("ALTER TABLE documents ADD COLUMN source_type TEXT DEFAULT 'user'");
    let _ = conn.execute_batch("ALTER TABLE documents ADD COLUMN corpus_id TEXT");
    Ok(())
}

/// Add version and deleted_at columns for sync-readiness.
/// These enable future multi-device sync without schema migration.
pub fn run_sync_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // version: Lamport timestamp set on every write.
    let _ = conn.execute_batch("ALTER TABLE conversations ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE conversations ADD COLUMN deleted_at INTEGER");
    let _ = conn.execute_batch("ALTER TABLE messages ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE tasks ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE memories ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE memories ADD COLUMN deleted_at INTEGER");
    let _ = conn.execute_batch("ALTER TABLE documents ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE documents ADD COLUMN deleted_at INTEGER");
    let _ = conn.execute_batch("ALTER TABLE permissions ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE corpus_state ADD COLUMN version INTEGER DEFAULT 0");
    let _ = conn.execute_batch("ALTER TABLE corpus_state ADD COLUMN deleted_at INTEGER");
    let _ = conn.execute_batch("ALTER TABLE search_budget ADD COLUMN version INTEGER DEFAULT 0");
    Ok(())
}

/// Add metacognition observability columns to routing_log.
/// Records the coarse intent from Pass 1 and the self-assessment outcome
/// (if triggered) so routing corrections have richer signal.
pub fn run_metacognition_log_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // coarse_intent: "SIMPLE" | "LOOKUP" | "REASONING" | "ACTION"
    let _ = conn.execute_batch("ALTER TABLE routing_log ADD COLUMN coarse_intent TEXT");
    // self_assessment: "Confident" | "Uncertain" | "NeedsWebSearch" | null (not triggered)
    let _ = conn.execute_batch("ALTER TABLE routing_log ADD COLUMN self_assessment TEXT");
    Ok(())
}

/// Create insight_nodes table and FTS5 virtual table for insight capture.
pub fn run_insight_migrations(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS insight_nodes (
            id               TEXT    PRIMARY KEY,
            clipped_text     TEXT    NOT NULL,
            message_id       TEXT    NOT NULL,
            paragraph_index  INTEGER NOT NULL,
            source_json      TEXT    NOT NULL,
            position_json    TEXT,
            adjacent_json    TEXT    NOT NULL,
            embedding        BLOB,
            created_at       INTEGER NOT NULL,
            sink_state_json  TEXT    NOT NULL,
            deleted_at       INTEGER
        );

        CREATE INDEX IF NOT EXISTS insight_nodes_created
            ON insight_nodes (created_at DESC)
            WHERE deleted_at IS NULL;

        CREATE INDEX IF NOT EXISTS insight_nodes_message
            ON insight_nodes (message_id)
            WHERE deleted_at IS NULL;

        CREATE VIRTUAL TABLE IF NOT EXISTS insight_nodes_fts
            USING fts5(id, clipped_text, content='insight_nodes', content_rowid='rowid');

        CREATE TRIGGER IF NOT EXISTS insight_nodes_ai AFTER INSERT ON insight_nodes BEGIN
            INSERT INTO insight_nodes_fts(rowid, id, clipped_text)
                VALUES (new.rowid, new.id, new.clipped_text);
        END;

        CREATE TRIGGER IF NOT EXISTS insight_nodes_ad AFTER DELETE ON insight_nodes BEGIN
            INSERT INTO insight_nodes_fts(insight_nodes_fts, rowid, id, clipped_text)
                VALUES('delete', old.rowid, old.id, old.clipped_text);
        END;

        CREATE TRIGGER IF NOT EXISTS insight_nodes_au AFTER UPDATE ON insight_nodes BEGIN
            INSERT INTO insight_nodes_fts(insight_nodes_fts, rowid, id, clipped_text)
                VALUES('delete', old.rowid, old.id, old.clipped_text);
            INSERT INTO insight_nodes_fts(rowid, id, clipped_text)
                VALUES (new.rowid, new.id, new.clipped_text);
        END;
        ",
    )
}

/// Document sessions for the document-analyst skill.
/// Persists map/reduce prompts and structured output so follow-up
/// questions can reference results without re-running the operation.
pub fn run_document_session_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS document_sessions (
            id                TEXT    PRIMARY KEY,
            conversation_id   TEXT    NOT NULL,
            filename          TEXT    NOT NULL,
            source            TEXT    NOT NULL,
            word_count        INTEGER NOT NULL DEFAULT 0,
            chunk_count       INTEGER NOT NULL DEFAULT 0,
            created_at        INTEGER NOT NULL,
            operation         TEXT    NOT NULL,
            map_prompt        TEXT    NOT NULL DEFAULT '',
            reduce_prompt     TEXT    NOT NULL DEFAULT '',
            last_output       TEXT,
            history           TEXT    NOT NULL DEFAULT '[]'
        );

        CREATE INDEX IF NOT EXISTS idx_docsess_conv
            ON document_sessions(conversation_id);
        ",
    )
}

/// Add vector index readiness tracking to corpus_state.
/// `vector_index_ready = 1` means the IVF-PQ index is built and semantic
/// search is available. Defaults to 0 so existing corpora start unverified;
/// the startup verification pass sets the correct value on first run.
pub fn run_index_readiness_migration(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute_batch(
        "ALTER TABLE corpus_state ADD COLUMN vector_index_ready INTEGER NOT NULL DEFAULT 0",
    );
    Ok(())
}
