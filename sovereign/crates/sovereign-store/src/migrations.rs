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
            created_at      INTEGER NOT NULL
        );
        ",
    )
}
