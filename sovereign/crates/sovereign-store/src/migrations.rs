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

/// Add antifragile-routing signal columns to `routing_log`.
///
/// Captured when the user clicks the redirect chip on a
/// `MoveKind::Propose` banner — the most diagnostically useful
/// signal the UI produces. `was_redirected = 1` tells a future
/// calibration job "at the confidence tier we picked, the initial
/// commit was a miss"; `redirect_to` names the intent_hint the user
/// actually wanted.
///
/// PR4 captures the signal from day 1; no calibration job yet.
/// Deferred work (calibration, implicit-acceptance signal from
/// 30s-no-redirect, clarification-answer signal) is tracked in
/// `SYSTEM_OVERVIEW.md §12 Architecture Roadmap`.
pub fn run_antifragile_routing_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // was_redirected: 0 (not redirected, default) | 1 (user redirected away
    // from the initially-routed intent)
    let _ = conn
        .execute_batch("ALTER TABLE routing_log ADD COLUMN was_redirected INTEGER NOT NULL DEFAULT 0");
    // redirect_to: wire-form intent hint the user chose via the
    // InterpretationBanner redirect chip. NULL when was_redirected = 0.
    let _ = conn.execute_batch("ALTER TABLE routing_log ADD COLUMN redirect_to TEXT");
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

/// Document asset library — persistent documents that are ingested once
/// and queried many times.
pub fn run_document_asset_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS document_assets (
            id              TEXT PRIMARY KEY,
            title           TEXT NOT NULL,
            filename        TEXT NOT NULL,
            file_size_mb    REAL NOT NULL,
            word_count      INTEGER NOT NULL,
            chunk_count     INTEGER NOT NULL,
            document_type   TEXT NOT NULL DEFAULT 'Unknown',
            ingested_at     INTEGER NOT NULL,
            index_id        TEXT NOT NULL,
            -- AssetState serialised as JSON so variants with fields
            -- (Indexing{chunks_done, chunks_total}) round-trip cleanly.
            state_json      TEXT NOT NULL,
            -- DocumentSkeleton as JSON. NULL until skeleton extraction
            -- completes. Can be large (50–200 KB for a novel).
            skeleton_json   TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_document_assets_ingested
            ON document_assets(ingested_at DESC);

        -- Each document gets its own conversation thread. This is a
        -- regular conversation that the conversation view renders,
        -- but scoped to a single document asset.
        CREATE TABLE IF NOT EXISTS document_conversations (
            id          TEXT PRIMARY KEY,
            asset_id    TEXT NOT NULL REFERENCES document_assets(id) ON DELETE CASCADE,
            created_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_docconv_asset
            ON document_conversations(asset_id);

        -- Track which operation was used for each document response.
        -- The operation badge in the UI reads from message metadata,
        -- but this table enables analytics and debugging.
        CREATE TABLE IF NOT EXISTS document_operations (
            message_id      TEXT PRIMARY KEY,
            asset_id        TEXT NOT NULL,
            operation_json  TEXT NOT NULL,
            duration_ms     INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_docops_asset
            ON document_operations(asset_id);
        ",
    )
}

/// RAPTOR atlas — cluster-summarize-recurse tree replacing the
/// per-chunk LLM skeleton, plus a TF-IDF motif index that captures
/// lexical recurrences RAPTOR's abstraction loses.
///
/// Both tables hang off `document_assets(id)` with ON DELETE CASCADE
/// so cleanup is automatic when an asset is deleted.
pub fn run_raptor_atlas_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS raptor_nodes (
            node_id                 TEXT    PRIMARY KEY,
            asset_id                TEXT    NOT NULL REFERENCES document_assets(id) ON DELETE CASCADE,
            level                   INTEGER NOT NULL,
            summary                 TEXT    NOT NULL,
            -- Embeddings stored as raw little-endian f32 bytes for
            -- compactness; encode/decode helpers live in sqlite.rs.
            summary_embedding       BLOB    NOT NULL,
            centroid_embedding      BLOB    NOT NULL,
            children_node_ids       TEXT    NOT NULL,    -- JSON array of UUIDs
            direct_member_chunk_ids TEXT,                -- JSON array, NULL above level 0
            evidence_chunk_ids      TEXT    NOT NULL,    -- JSON array
            quote_spans             TEXT    NOT NULL,    -- JSON array of QuoteSpan
            primary_entities        TEXT    NOT NULL,    -- JSON array of name strings
            cluster_coherence       REAL    NOT NULL,
            created_at              INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_raptor_nodes_asset_level
            ON raptor_nodes(asset_id, level);

        CREATE TABLE IF NOT EXISTS asset_motifs (
            asset_id             TEXT    NOT NULL REFERENCES document_assets(id) ON DELETE CASCADE,
            term                 TEXT    NOT NULL,
            tf_idf_score         REAL    NOT NULL,
            occurrence_chunk_ids TEXT    NOT NULL,    -- JSON array
            is_distinctive       INTEGER NOT NULL,    -- 0 / 1
            PRIMARY KEY (asset_id, term)
        );

        CREATE INDEX IF NOT EXISTS idx_asset_motifs_distinctive
            ON asset_motifs(asset_id, is_distinctive DESC, tf_idf_score DESC);
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

/// KnowledgeView v1 additive columns.
///
/// - `memories.source_conversation_id` — links an extracted memory
///   back to the conversation it came from. The `personal-knowledge`
///   acquirer joins on this column so the enrichment pipeline can
///   surface cluster membership alongside conversation metadata.
/// - `conversations.skill_id` — identifies which skill was active
///   when the conversation started. The `conversation-history`
///   acquirer filters conversations tagged with any `privacy =
///   "local_only"` skill (notably `inner-work`) OUT of the view —
///   strict structural privacy separation, no consent UI required
///   for v1.
///
/// Both columns are `NULL` on existing rows. Read paths must tolerate
/// `NULL` — a memory predating this migration simply has no linkage,
/// and a conversation predating this migration has no skill attribution.
pub fn run_knowledge_view_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute_batch(
        "ALTER TABLE memories ADD COLUMN source_conversation_id TEXT",
    );
    let _ = conn.execute_batch(
        "ALTER TABLE conversations ADD COLUMN skill_id TEXT",
    );
    Ok(())
}

/// Inner-work memory wall (2026-05-05). Adds a denormalized
/// `source_skill_id` column to `memories` so recall can be filtered
/// at the SQL layer instead of a join: in inner-work conversations
/// only `source_skill_id = 'inner-work'` recalls; in non-inner-work
/// conversations memories with `source_skill_id = 'inner-work'` are
/// excluded. The wall is bidirectional.
///
/// Backfill: existing memories whose `source_conversation_id` resolves
/// to a conversation with a `skill_id` get their `source_skill_id`
/// populated from that conversation. Memories predating the
/// `source_conversation_id` migration (or with a NULL conversation
/// link) stay NULL — they're treated as "general" pool, recallable
/// anywhere except scoped contexts. The backfill is one-shot
/// (`WHERE source_skill_id IS NULL`); re-running is a no-op.
pub fn run_inner_work_memory_wall_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute_batch(
        "ALTER TABLE memories ADD COLUMN source_skill_id TEXT",
    );
    // Backfill: tag memories whose source conversation has a known skill.
    // Idempotent because of the `IS NULL` guard.
    let _ = conn.execute_batch(
        "UPDATE memories SET source_skill_id = (
             SELECT skill_id FROM conversations
             WHERE conversations.id = memories.source_conversation_id
         )
         WHERE source_skill_id IS NULL
           AND source_conversation_id IS NOT NULL",
    );
    // Index for the recall filter — every relational/witness recall
    // touches this column, so an index is worth the write cost on
    // memory inserts (which are bursty and not in the hot path).
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memories_source_skill_id
         ON memories(source_skill_id) WHERE deleted_at IS NULL",
    );
    Ok(())
}

/// Rolling-summary memory compaction (2026-05-23). Adds three columns
/// to `memories` so the compaction worker can:
///
/// - distinguish raw extractions from mechanical distillations
///   (`kind` — `'raw' | 'summary'`);
/// - record which raw rows a summary collapsed (`source_memory_ids` —
///   JSON array of ids); and
/// - mark a raw row as folded into a newer summary
///   (`superseded_by` — fk into `memories(id)`, no FK constraint to
///   keep rebuilds cheap).
///
/// All three columns are nullable / default-empty. Existing rows
/// surface as `kind = 'raw'`, `source_memory_ids = '[]'`,
/// `superseded_by = NULL` — the implicit pre-compaction shape.
/// Retrieval paths filter `superseded_by IS NULL` so superseded rows
/// stop appearing in recall; the body stays for `sovereign memory
/// expand <summary-id>` provenance walks.
///
/// The retrieval-filter index is partial on `superseded_by IS NULL`
/// because every witness turn hits the path; the bursty insert cost
/// is fine. The `source_conversation_id` lookup the worker runs
/// piggybacks on the existing scope index — no new index for that.
pub fn run_memory_compaction_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute_batch(
        "ALTER TABLE memories ADD COLUMN kind TEXT NOT NULL DEFAULT 'raw'",
    );
    let _ = conn.execute_batch(
        "ALTER TABLE memories ADD COLUMN source_memory_ids TEXT NOT NULL DEFAULT '[]'",
    );
    let _ = conn.execute_batch(
        "ALTER TABLE memories ADD COLUMN superseded_by TEXT",
    );
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memories_superseded_by
         ON memories(superseded_by)",
    );
    // The compaction worker enumerates non-superseded memories per
    // conversation; an index that combines the two columns makes
    // that scan cheap even on a populated store.
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memories_conv_active
         ON memories(source_conversation_id)
         WHERE superseded_by IS NULL AND deleted_at IS NULL",
    );
    Ok(())
}

/// Conversation tiered-retrieval port (Phase B; spec
/// `sovereign/docs/specs/CONV_TIERED_PORT.md`).
///
/// Mirrors the attached-doc `raptor_nodes` / `asset_motifs` shape but
/// keys on `(corpus_id, conv_uuid)` so a single SQLite store can host
/// tiered enrichment for every conversation corpus (claude.ai export,
/// Sovereign-internal personal chats, future imports). No FK to
/// `document_assets` — conversations live in Lance, not in
/// `document_assets`.
///
/// The three tables together carry the per-conversation T2 + T3
/// enrichment output:
///
/// - `conv_skeletons` — per-conv state machine + T2 partial skeleton
///   (entity index, action atoms) + T3 overview + segments
/// - `conv_raptor_nodes` — per-conv RAPTOR tree (flat node list,
///   level + children + chunk membership + quote spans)
/// - `conv_motifs` — TF-IDF motif index per conv
pub fn run_conv_tiered_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS conv_skeletons (
            corpus_id      TEXT    NOT NULL,
            conv_uuid      TEXT    NOT NULL,
            state          TEXT    NOT NULL,    -- 'Pending'|'PartiallyReady'|'MultiHopReady'|'Ready'|'Failed'
            skeleton_json  TEXT,                -- T2 partial: main_entities, entity_index, actions, structural_moments
            overview       TEXT,                -- T3 overview (reused conv.title for opt-3 in v0)
            segments_json  TEXT,                -- T3 TextTiling segments (NULL for short convs)
            chunk_count    INTEGER NOT NULL DEFAULT 0,
            updated_at     INTEGER NOT NULL,
            PRIMARY KEY (corpus_id, conv_uuid)
        );

        CREATE INDEX IF NOT EXISTS idx_conv_skeletons_state
            ON conv_skeletons(corpus_id, state);

        CREATE TABLE IF NOT EXISTS conv_raptor_nodes (
            node_id                 TEXT    PRIMARY KEY,
            corpus_id               TEXT    NOT NULL,
            conv_uuid               TEXT    NOT NULL,
            level                   INTEGER NOT NULL,
            summary                 TEXT    NOT NULL,
            -- Embeddings stored as raw little-endian f32 bytes
            -- (mirrors raptor_nodes encoding in run_raptor_atlas_migration).
            summary_embedding       BLOB    NOT NULL,
            centroid_embedding      BLOB    NOT NULL,
            children_node_ids       TEXT    NOT NULL,    -- JSON array
            direct_member_chunk_ids TEXT,                -- JSON array of Lance chunk ids; NULL above level 0
            evidence_chunk_ids      TEXT    NOT NULL,    -- JSON array
            quote_spans             TEXT    NOT NULL,    -- JSON array of QuoteSpan
            primary_entities        TEXT    NOT NULL,    -- JSON array of name strings
            cluster_coherence       REAL    NOT NULL,
            created_at              INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_conv_raptor_nodes_conv_level
            ON conv_raptor_nodes(corpus_id, conv_uuid, level);

        CREATE TABLE IF NOT EXISTS conv_motifs (
            corpus_id            TEXT    NOT NULL,
            conv_uuid            TEXT    NOT NULL,
            term                 TEXT    NOT NULL,
            tf_idf_score         REAL    NOT NULL,
            occurrence_chunk_ids TEXT    NOT NULL,    -- JSON array
            is_distinctive       INTEGER NOT NULL,    -- 0 / 1
            PRIMARY KEY (corpus_id, conv_uuid, term)
        );

        CREATE INDEX IF NOT EXISTS idx_conv_motifs_distinctive
            ON conv_motifs(corpus_id, conv_uuid, is_distinctive DESC, tf_idf_score DESC);
        ",
    )
}

/// GliNER-extracted per-chunk entities (spec
/// `sovereign/docs/specs/CONV_TIERED_PORT.md` §"Phase 1 — GliNER
/// per-chunk entities"). Distinct from the LLM-extracted entities
/// that live inside `conv_raptor_nodes.primary_entities` (cluster-
/// scope) or atlas `atoms.json` (corpus-scope) — this is the
/// per-chunk NER surface that produces ~10x denser entity coverage,
/// gives previously-empty Tiny convs an entity set, and enables a
/// `ConvEntityGraph::from_chunk_entities` builder layered alongside
/// the existing RAPTOR-entities builder.
///
/// Idempotent extraction: the table is keyed on
/// `(corpus_id, chunk_id, text, label)` so re-running the extractor
/// on a chunk replaces its prior mentions without growing
/// duplicate rows.
pub fn run_chunk_entities_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS chunk_entities (
            corpus_id   TEXT    NOT NULL,
            chunk_id    INTEGER NOT NULL,
            text        TEXT    NOT NULL,
            label       TEXT    NOT NULL,
            -- Character offsets into the chunk content (NOT the
            -- original conv source) for highlight rendering. Stored
            -- as i64 because rusqlite's u64 binding is fussy with
            -- some columns; values fit in i32 in practice.
            char_start  INTEGER NOT NULL,
            char_end    INTEGER NOT NULL,
            -- GliNER softmax score in [0, 1]. Production threshold
            -- is 0.6 (see scripts/extract_entities.py); persisted
            -- so future re-bench passes can re-threshold without
            -- re-running NER.
            score       REAL    NOT NULL,
            -- Conv_uuid carried as denormalised join key so the
            -- conv-entity-graph builder can fetch
            -- `WHERE corpus_id = ? AND conv_uuid = ?` without
            -- hitting Lance for the chunk lookup. NULL for
            -- non-conv corpora (atoms-style corpora may also
            -- populate this table in future).
            conv_uuid   TEXT,
            extracted_at INTEGER NOT NULL,
            PRIMARY KEY (corpus_id, chunk_id, text, label)
        );

        CREATE INDEX IF NOT EXISTS idx_chunk_entities_corpus_conv
            ON chunk_entities(corpus_id, conv_uuid, label);

        CREATE INDEX IF NOT EXISTS idx_chunk_entities_text
            ON chunk_entities(corpus_id, text);

        -- Per-corpus extraction-progress sidecar so the CLI can
        -- report '15234 / 16404 chunks extracted' without scanning
        -- the full chunk_entities table on every progress poll.
        CREATE TABLE IF NOT EXISTS chunk_entity_progress (
            corpus_id           TEXT    PRIMARY KEY,
            chunks_processed    INTEGER NOT NULL DEFAULT 0,
            chunks_total        INTEGER NOT NULL DEFAULT 0,
            mentions_extracted  INTEGER NOT NULL DEFAULT 0,
            last_chunk_id       INTEGER,
            started_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            finished_at         INTEGER,
            -- 'running' | 'complete' | 'failed' | 'paused'
            state               TEXT    NOT NULL DEFAULT 'running',
            -- GliNER model id + threshold + label set, recorded for
            -- provenance. Re-extracting with different settings
            -- bumps these.
            model_id            TEXT,
            threshold           REAL,
            labels_json         TEXT,
            error_msg           TEXT
        );
        ",
    )
}

/// Surface-skill backfill (2026-05-24). The pre-redesign routing
/// model toggled the inner-work / recipe-author skill in the global
/// `active_skills` registry on workspace mount; the runtime then
/// resolved the primary skill via
/// `SkillRegistry::primary_skill_id_for_conversation` at dispatch
/// time. Conversations created under that model never recorded
/// which surface owned them on their own row.
///
/// The new model tags `conversations.skill_id` at create-time from
/// `SURFACE_SKILL_ID` constants exported by each surface
/// (`RecipeChatSurface`, `InnerWorkSurface`). Conversations created
/// under the old model have `skill_id IS NULL` and would surface
/// in the wrong workspace list + lose their workspace prompt.
///
/// Backfill rule: a conversation whose extracted memories carry
/// `source_skill_id = '<surface>'` was demonstrably owned by that
/// surface. Tag the conversation accordingly. Guarded on
/// `skill_id IS NULL` so re-running is a no-op and conversations
/// already tagged (by the new path) are not clobbered. Memories
/// are the truth-source because the inner-work memory wall already
/// stamped them at extraction time.
pub fn run_surface_skill_backfill(conn: &Connection) -> rusqlite::Result<()> {
    for surface in ["inner-work", "recipe-author"] {
        let _ = conn.execute(
            "UPDATE conversations
             SET skill_id = ?1
             WHERE skill_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM memories
                 WHERE memories.source_conversation_id = conversations.id
                   AND memories.source_skill_id = ?1
                   AND memories.deleted_at IS NULL
               )",
            [surface],
        );
    }
    Ok(())
}
