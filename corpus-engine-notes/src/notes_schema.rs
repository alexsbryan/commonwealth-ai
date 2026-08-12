// SPDX-License-Identifier: AGPL-3.0-or-later
//! NoteStore SQL schema + migrations — extracted out of `crate::notes`.
//!
//! Each const here is the static SQL for one schema state. The owner
//! (`NoteStore::open` in `notes.rs`) reads `PRAGMA user_version` and
//! fires the appropriate migration. Holding the SQL as `const &str` —
//! versus `include_str!`-ing per-version `.sql` files — keeps the
//! schema versions and their forward migrations physically adjacent
//! to each other, which makes review of "what does v6→v7 actually
//! change?" a single buffer scroll. Per ARCH §6.2 the SQL is data
//! that could move out further still; this split is the staging step.

pub(crate) const SCHEMA_NEW: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation','tool_decision'
    )),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;
";

// ─── Schema migration v3 → v4 (ATOS M4 deviation note kind) ─────────────────

/// Applied to databases at `user_version = 3`. Expands the
/// `notes.kind` CHECK constraint to admit `deviation`, the kind the
/// M4 approval-gate middleware writes when it detects spec-hash
/// drift. Rename-recreate pattern; rebuilds FTS5.
pub(crate) const MIGRATION_V4: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v3;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
FROM notes_v3;

DROP TABLE notes_v3;

CREATE INDEX IF NOT EXISTS idx_notes_kind           ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created        ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name      ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at     ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature  ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 4;

COMMIT;
";

// ─── Schema migration v4 → v5 (Relational + Strategic note kinds + related_entity) ──

/// Applied to databases at `user_version = 4`. Two interleaved
/// changes for the Relational + Strategic Awareness changeset
/// (requirements §6):
///
/// 1. Expand the `notes.kind` CHECK constraint to admit
///    `commitment`, `follow_up`, and `goal`.
/// 2. Add a `related_entity TEXT` column. NULL for all pre-v5 rows;
///    populated by the suggest_note tool (Phase 6) and any future
///    relational write API.
///
/// Same rename-recreate-copy pattern as the prior CHECK migrations.
/// FTS5 + triggers are rebuilt because they reference the table by
/// name. A new partial index on `related_entity` accelerates the
/// digest's `WHERE related_entity = ?` lookups.
pub(crate) const MIGRATION_V5: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v4;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation','commitment','follow_up','goal'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT,
    related_entity TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    NULL
FROM notes_v4;

DROP TABLE notes_v4;

CREATE INDEX IF NOT EXISTS idx_notes_kind            ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created         ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name       ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at      ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature   ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_related_entity
    ON notes(related_entity) WHERE related_entity IS NOT NULL;

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 5;

COMMIT;
";

// ─── Schema migration v5 → v6 (Audit hardening: source + supersedes) ──────────

/// Applied to databases at `user_version = 5`. Adds two columns
/// supporting the audit-hardening workstream:
///
/// 1. `source TEXT NOT NULL DEFAULT 'agent'` — provenance dimension.
///    Values: `'agent'` (explicit `note` tool call), `'committed'`
///    (harvested from a git commit message), `'extracted'` (LLM pass
///    over session diff), `'inferred'` (regex-mined from agent
///    response text), `'observed'` (tool-call pattern match). CHECK
///    is enforced at the API layer (in `write_note_with_source`)
///    rather than via SQL constraint — adding a new source becomes a
///    one-line code change rather than a rename-recreate migration.
///
/// 2. `supersedes TEXT` — note id (in same table) that this note
///    reverses. NULL when the note doesn't reverse anything. The
///    referenced row is left intact; only the audit display treats
///    this as a reversal (`↳ REVERSED` line under the original).
///
/// `session_id` is already present on the v5 schema (line 1097), so
/// no third column is added.
///
/// New columns are added via plain `ALTER TABLE ADD COLUMN` — no
/// CHECK constraint changes here, so the rename-recreate dance the
/// prior migrations needed is avoided. Two new partial indexes:
/// `(source, created_at DESC)` for audit-assembly queries that order
/// by source priority, and `supersedes` for the reversal-display
/// lookup. Idempotent: gated by `PRAGMA user_version < 6`.
pub(crate) const MIGRATION_V6: &str = "
BEGIN;

ALTER TABLE notes ADD COLUMN source TEXT NOT NULL DEFAULT 'agent';
ALTER TABLE notes ADD COLUMN supersedes TEXT;

CREATE INDEX IF NOT EXISTS idx_notes_source_created
    ON notes(source, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_supersedes
    ON notes(supersedes) WHERE supersedes IS NOT NULL;

PRAGMA user_version = 6;

COMMIT;
";

// ─── Schema migration v6 → v7 (Recipe-author note kinds + payload_json) ───────

/// Applied to databases at `user_version = 6`. Two interleaved
/// changes for the recipe-author surface (single source of truth for
/// recipe-project state lives in NoteStore + sidecar files):
///
/// 1. Expand the `notes.kind` CHECK constraint to admit six new
///    kinds: `research_finding`, `capability_request`, `recipe_issue`,
///    `checkpoint`, `checkpoint_restored`, `deferred_question`.
/// 2. Add a nullable `payload_json TEXT` column for per-kind
///    structured data.
///
/// Same rename-recreate-copy pattern as MIGRATION_V5 — SQLite can't
/// ALTER a CHECK constraint in place. FTS5 + triggers are rebuilt
/// because they reference the table by name.
pub(crate) const MIGRATION_V7: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v6;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation','commitment','follow_up','goal',
        'research_finding','capability_request','recipe_issue',
        'checkpoint','checkpoint_restored','deferred_question'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT,
    related_entity TEXT,
    source        TEXT    NOT NULL DEFAULT 'agent',
    supersedes    TEXT,
    payload_json  TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, payload_json
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, NULL
FROM notes_v6;

DROP TABLE notes_v6;

CREATE INDEX IF NOT EXISTS idx_notes_kind            ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created         ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name       ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at      ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature   ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_related_entity
    ON notes(related_entity) WHERE related_entity IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_source_created
    ON notes(source, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_supersedes
    ON notes(supersedes) WHERE supersedes IS NOT NULL;

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 7;

COMMIT;
";

// ─── Schema migration v7 → v8 (Tool-Mastery Framework: tool_decision kind) ─

/// Applied to databases at `user_version = 7`. Adds a single new
/// `kind` value — `tool_decision` — used by the Tool-Mastery
/// framework's Layer 3 to record the outcome of an agent's tool
/// invocation against the running conversation (`useful` / `stale` /
/// `wrong-tool` / `no-results`). The structured fields ride in the
/// existing v7 `payload_json` column; only the CHECK constraint
/// changes here.
///
/// Same rename-recreate-copy pattern as MIGRATION_V7 — SQLite can't
/// ALTER a CHECK constraint in place. FTS5 + triggers are rebuilt
/// because they reference the `notes` table by name.
pub(crate) const MIGRATION_V8: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v7;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation','commitment','follow_up','goal',
        'research_finding','capability_request','recipe_issue',
        'checkpoint','checkpoint_restored','deferred_question',
        'tool_decision'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT,
    related_entity TEXT,
    source        TEXT    NOT NULL DEFAULT 'agent',
    supersedes    TEXT,
    payload_json  TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, payload_json
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, payload_json
FROM notes_v7;

DROP TABLE notes_v7;

CREATE INDEX IF NOT EXISTS idx_notes_kind            ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created         ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name       ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at      ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature   ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_related_entity
    ON notes(related_entity) WHERE related_entity IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_source_created
    ON notes(source, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_supersedes
    ON notes(supersedes) WHERE supersedes IS NOT NULL;

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 8;

COMMIT;
";

// ─── Schema migration v8 → v9 (Tiered retrieval + mesh propagation) ─────────

/// Applied to databases at `user_version = 8`. Lands the additive
/// surface for the tiered-retrieval-over-NoteStore port (T1 embeddings)
/// and mesh-wide propagation (per
/// `sovereign/docs/specs/NOTES_TIERED.md` + the
/// `~/.claude/plans/let-s-work-on-this-compiled-whale.md` plan).
///
/// Five new columns on `notes`:
///
/// - `private INTEGER NOT NULL DEFAULT 0` — opt-out flag. Private
///   notes never enter the mesh wire (`app_id = "notes-private"` is
///   structurally `GOSSIP_EXCLUDED`).
/// - `origin_node_id TEXT` — node that authored the note. NULL for
///   pre-v9 rows + locally-authored rows whose author wasn't
///   recorded; non-NULL after first ingest from a peer. Carries
///   authorship across toolbx `node_id` rotation — the
///   `content_hash` is what dedupes, this field is informational.
/// - `tombstone INTEGER NOT NULL DEFAULT 0` — soft-delete flag for
///   propagation. Tombstoned notes still exist locally (so the
///   `supersedes` chain can land later edits) but are filtered out
///   of the audit display and the propagation delta scan. Tombstone
///   wins regardless of edit `updated_at`.
/// - `content_hash TEXT` — deterministic id over
///   `kind || US || content || US || scope || US || COALESCE(feature_id,'') || US || session_id`
///   (where US is 0x1F). Stable across `origin_node_id` rotation;
///   primary key on the gossip wire; idempotent insert on collision.
///   Backfilled by `NoteStore::open` in Rust post-migration (SQLite
///   has no built-in cryptographic hash) — see
///   `notes::backfill_content_hashes`.
/// - `fork_of TEXT` — set when a remote-ingested note has the same
///   `supersedes` target as a locally-known note that's also
///   superseded by a sibling. Points to the sibling head. Reader
///   surfaces forks; no silent LWW collapse. Knowledge notes are
///   not source code merges — losing an edit because the other
///   peer's clock was 200ms ahead would be a high-cost failure
///   mode.
///
/// Two new tables:
///
/// - `note_embeddings` — one row per note with a computed T1
///   embedding. ON DELETE CASCADE so cleanup is automatic.
/// - `note_propagation_watermark` — one row per peer tracking the
///   last note id this peer was successfully shipped. Drives the
///   per-round delta query so a 10k-note backlog catches up across
///   multiple gossip rounds instead of a full re-ship every round.
///
/// Three new indexes:
///
/// - `idx_notes_propagation` — `(scope, private, created_at DESC)`
///   partial index that the delta scan uses (`WHERE private=0 AND
///   tombstone=0 AND scope='global' AND created_at > ?`).
/// - `idx_notes_content_hash` — partial index on content_hash for
///   the propagation-receive dedup lookup.
/// - `idx_notes_fork_of` — partial index on fork_of for the
///   resolve-fork operator action's "find all forks" scan.
///
/// Pure additive — no `notes.kind` CHECK constraint change, so no
/// rename-recreate-copy dance, no FTS5 rebuild. Same shape as
/// `MIGRATION_V6` (the prior plain-`ADD COLUMN` migration).
/// Idempotent: gated by `PRAGMA user_version < 9` in
/// `NoteStore::open`.
pub(crate) const MIGRATION_V9: &str = "
BEGIN;

ALTER TABLE notes ADD COLUMN private        INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notes ADD COLUMN origin_node_id TEXT;
ALTER TABLE notes ADD COLUMN tombstone      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notes ADD COLUMN content_hash   TEXT;
ALTER TABLE notes ADD COLUMN fork_of        TEXT;

CREATE TABLE IF NOT EXISTS note_embeddings (
    note_id    TEXT    PRIMARY KEY
        REFERENCES notes(id) ON DELETE CASCADE,
    embedding  BLOB    NOT NULL,
    model_id   TEXT    NOT NULL,
    dim        INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS note_propagation_watermark (
    peer_node_id         TEXT    PRIMARY KEY,
    last_sent_created_at INTEGER NOT NULL,
    last_sent_note_id    TEXT    NOT NULL,
    last_acked_at        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_notes_propagation
    ON notes(scope, private, created_at DESC)
    WHERE private = 0 AND tombstone = 0;

CREATE INDEX IF NOT EXISTS idx_notes_content_hash
    ON notes(content_hash)
    WHERE content_hash IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_notes_fork_of
    ON notes(fork_of)
    WHERE fork_of IS NOT NULL;

PRAGMA user_version = 9;

COMMIT;
";

// ─── Schema migration v9 → v10 (T2 entity-graph: note_entities) ─────

/// Applied to databases at `user_version = 9`. Adds the T2 tier
/// entity-extraction surface — one row per `(note_id, entity,
/// kind)` tuple — so the related-notes lookup
/// (`NoteStore::read_notes_related`) has a graph to traverse.
///
/// Mirrors the `chunk_entities` shape in `corpus-engine` so the
/// PPR utilities under `sovereign-tools::entity_graph` can wrap
/// this table without translation (per `PROGRESSIVE_ENRICHMENT.md`
/// step 2). Two indexes — one each by entity and by kind — cover
/// the two access patterns ("who else mentions X" and "what are
/// the Tech/Person/Org seeds I've ever written about").
///
/// Pure additive — no notes.kind CHECK change, no FTS5 rebuild.
/// Idempotent: gated by `PRAGMA user_version < 10`.
pub(crate) const MIGRATION_V10: &str = "
BEGIN;

CREATE TABLE IF NOT EXISTS note_entities (
    note_id    TEXT    NOT NULL
        REFERENCES notes(id) ON DELETE CASCADE,
    entity     TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    salience   REAL    NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (note_id, entity, kind)
);

CREATE INDEX IF NOT EXISTS idx_note_entities_entity ON note_entities(entity);
CREATE INDEX IF NOT EXISTS idx_note_entities_kind   ON note_entities(kind);

PRAGMA user_version = 10;

COMMIT;
";

// ─── Schema migration v10 → v11 (TEACHABLE: lesson kind) ────────────────────

/// Applied to databases at `user_version = 10`. Adds a single new
/// `kind` value — `lesson` — the behavior lane of the TEACHABLE
/// feature (`sovereign-desktop/TEACHABLE.md`). Lesson structure
/// (display / prompt_form / enforcement / taught_from / enabled /
/// compiler_version) rides the existing v7 `payload_json` column;
/// only the CHECK constraint changes here.
///
/// NOT the V8 rename-first pattern. Since v9, `note_embeddings` and
/// `note_entities` carry `REFERENCES notes(id) ON DELETE CASCADE`;
/// on SQLite ≥ 3.25, `ALTER TABLE notes RENAME TO notes_v10` would
/// rewrite those child REFERENCES clauses to point at the renamed
/// temp table — which then gets dropped, corrupting the child
/// schema. Create-copy-drop-rename avoids this under every SQLite
/// rename behavior: the final rename's target (`notes_v11`) has no
/// inbound references, so nothing is rewritten.
///
/// The script must also disable foreign keys for its duration: the
/// bundled libsqlite3-sys compiles with SQLITE_DEFAULT_FOREIGN_KEYS,
/// so connections open FK-ON, and under FK-ON `DROP TABLE notes`
/// performs an implicit DELETE that fires `ON DELETE CASCADE` into
/// the child tables — silently emptying `note_embeddings` /
/// `note_entities` (caught by the migration test). The pragma is a
/// no-op inside a transaction, so it brackets the BEGIN/COMMIT, and
/// is restored to ON afterwards (the store's operating mode —
/// `delete_note` relies on the cascade for embedding cleanup). All
/// child rows point at ids the copy preserves — the migration test
/// asserts `pragma_foreign_key_check` is empty afterwards.
///
/// FTS5 + triggers are rebuilt because they reference `notes` by
/// name; the index list is the UNION of V8's nine and v9's three
/// (dropping `notes` drops them all).
///
/// Idempotent: gated by `PRAGMA user_version < 11` in
/// `NoteStore::open`.
pub(crate) const MIGRATION_V11: &str = "
PRAGMA foreign_keys=OFF;

BEGIN;

CREATE TABLE notes_v11 (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation','commitment','follow_up','goal',
        'research_finding','capability_request','recipe_issue',
        'checkpoint','checkpoint_restored','deferred_question',
        'tool_decision','lesson'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT,
    related_entity TEXT,
    source        TEXT    NOT NULL DEFAULT 'agent',
    supersedes    TEXT,
    payload_json  TEXT,
    private        INTEGER NOT NULL DEFAULT 0,
    origin_node_id TEXT,
    tombstone      INTEGER NOT NULL DEFAULT 0,
    content_hash   TEXT,
    fork_of        TEXT
);

INSERT INTO notes_v11 (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, payload_json,
    private, origin_node_id, tombstone, content_hash, fork_of
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from,
    related_entity, source, supersedes, payload_json,
    private, origin_node_id, tombstone, content_hash, fork_of
FROM notes;

DROP TABLE IF EXISTS notes_fts;
DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;
DROP TABLE notes;

ALTER TABLE notes_v11 RENAME TO notes;

CREATE INDEX IF NOT EXISTS idx_notes_kind            ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created         ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name       ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at      ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature   ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_related_entity
    ON notes(related_entity) WHERE related_entity IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_source_created
    ON notes(source, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_supersedes
    ON notes(supersedes) WHERE supersedes IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_propagation
    ON notes(scope, private, created_at DESC)
    WHERE private = 0 AND tombstone = 0;
CREATE INDEX IF NOT EXISTS idx_notes_content_hash
    ON notes(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_notes_fork_of
    ON notes(fork_of)
    WHERE fork_of IS NOT NULL;

CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 11;

COMMIT;

PRAGMA foreign_keys=ON;
";

// ─── Schema migration v11 → v12 (receipt stamps) ─────────────────────────────

/// Applied to databases at `user_version = 11`. Adds two nullable
/// receipt columns (order `commons-fluency` fix 3):
///
/// - `sent_at`     — when THIS node's daemon last successfully
///                   published the note via the mesh sink (NULL:
///                   never published — the write stayed node-local).
/// - `received_at` — when THIS node's daemon first applied the note
///                   from the wire (`ingest_remote_notes`; NULL:
///                   the note was authored here, never received).
///
/// Two-sided receipts: a note replicated A→B carries A's `sent_at`
/// (origin clock, inside the propagation event) and B's `received_at`
/// (receiver clock, stamped on the local row). Both NULL for local
/// never-published notes; `sent_at` NULL + `received_at` SET for
/// notes received from a pre-v12 origin.
///
/// Plain ADD COLUMN — no CHECK change, no rename-recreate, no FTS5
/// rebuild, no backfill (NULL is the honest value for everything
/// written before the columns existed). Idempotent: gated by
/// `PRAGMA user_version < 12` in `NoteStore::open`.
pub(crate) const MIGRATION_V12: &str = "
BEGIN;

ALTER TABLE notes ADD COLUMN sent_at INTEGER;
ALTER TABLE notes ADD COLUMN received_at INTEGER;

PRAGMA user_version = 12;

COMMIT;
";

// ─── Schema migration v2 → v3 (ATOS note kinds: uncertainty,
//     postmortem_pointer, redteam_finding) ─────────────────────────────────

/// Applied to databases at `user_version = 2`. Expands the `notes.kind`
/// CHECK constraint. SQLite cannot alter a CHECK in-place, so we
/// rename-copy-rebuild — same pattern as `MIGRATION_V1`. The FTS5
/// virtual table and triggers are rebuilt because they reference the
/// `notes` table by name; the rebuild trigger repopulates the index
/// from the copied rows.
///
/// Idempotent across a single install: gated by `PRAGMA user_version <
/// 3` in `NoteStore::open`.
pub(crate) const MIGRATION_V3: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v2;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
FROM notes_v2;

DROP TABLE notes_v2;

CREATE INDEX IF NOT EXISTS idx_notes_kind           ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created        ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name      ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at     ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature  ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

-- Rebuild FTS5 + triggers. The old `notes_v2` has been dropped, so
-- any pre-existing rowid mappings in `notes_fts` are stale — blow it
-- away and repopulate from the current `notes` table.
DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 3;

COMMIT;
";

// ─── Schema migration v1 → v2 (ATOS scoping) ─────────────────────────────────

/// Applied to databases at `user_version = 1`. Adds ATOS scope/feature_id
/// columns to `notes`, creates the `meta_counters` singleton for the
/// `notes_version` clock, and provisions the `note_digest_cache` table
/// used by the Fast-slot digest in M1.4.
///
/// Idempotent across a single install: the migration is gated by
/// `PRAGMA user_version < 2` in `NoteStore::open`.
pub(crate) const MIGRATION_V2: &str = "
BEGIN;

ALTER TABLE notes ADD COLUMN scope         TEXT NOT NULL DEFAULT 'global';
ALTER TABLE notes ADD COLUMN feature_id    TEXT;
ALTER TABLE notes ADD COLUMN promoted_from TEXT;

CREATE INDEX IF NOT EXISTS idx_notes_scope_feature ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS meta_counters (
    key TEXT PRIMARY KEY,
    val INTEGER NOT NULL
);
INSERT OR IGNORE INTO meta_counters(key, val) VALUES ('notes_version', 0);

CREATE TABLE IF NOT EXISTS note_digest_cache (
    scope_hash    TEXT    NOT NULL,
    notes_version INTEGER NOT NULL,
    digest_md     TEXT    NOT NULL,
    token_count   INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY(scope_hash, notes_version)
);
CREATE INDEX IF NOT EXISTS idx_digest_created
    ON note_digest_cache(created_at DESC);

PRAGMA user_version = 2;

COMMIT;
";

// ─── Schema migration v0 → v1 ────────────────────────────────────────────────

/// Applied to existing databases whose `user_version = 0`. Uses SQLite's
/// standard rename-recreate-copy-drop pattern to update the CHECK constraint
/// and add three new nullable columns. Rebuilds FTS5 and triggers.
pub(crate) const MIGRATION_V1: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v0;

CREATE TABLE notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo','reflection')),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);

INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
SELECT id, kind, content, symbols, files, session_id, created_at, updated_at FROM notes_v0;

DROP TABLE notes_v0;

CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;

COMMIT;
";
