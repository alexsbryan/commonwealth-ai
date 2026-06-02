CREATE TABLE IF NOT EXISTS features (
    id             TEXT PRIMARY KEY,
    title          TEXT NOT NULL,
    charter_md     TEXT NOT NULL,
    sovereign_md   TEXT NOT NULL DEFAULT '',
    state          TEXT NOT NULL CHECK(state IN
                     ('provisioned','active','paused','archived','completed',
                      'recipe_authoring')),
    stop_condition TEXT NOT NULL DEFAULT '',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    archived_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_features_state ON features(state);

CREATE TABLE IF NOT EXISTS feature_milestones (
    id                     TEXT PRIMARY KEY,
    feature_id             TEXT NOT NULL REFERENCES features(id) ON DELETE CASCADE,
    ordinal                INTEGER NOT NULL,
    brief_md               TEXT NOT NULL,
    started_at             INTEGER,
    ended_at               INTEGER,
    compliance_report_json TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_milestones_feat_ord
    ON feature_milestones(feature_id, ordinal);

-- ATOS run ledger. One row per driver invocation of a milestone. The
-- orchestrator creates this row on `start-milestone`, exports its id
-- as $ATOS_RUN_ID to the driver subprocess, and closes it out on
-- `end-milestone` with stop_condition exit + duration.
--
-- `mode` added in M3.2: 'normal' runs are agent drivers; 'redteam'
-- runs use a restricted tool surface + narrowed brief and land in the
-- report renderer's Red Team Findings section. The column has a
-- DEFAULT so V1/V2 rows created before M3.2 naturally become 'normal'.
--
-- `stop_stdout` added in M3.2: captures the shell output from
-- `end-milestone`'s stop_condition run so the milestone-<n>.md renderer
-- can quote test results inline without re-running anything.
-- Bounded at 8KB by the orchestrator before insert.
CREATE TABLE IF NOT EXISTS atos_runs (
    id            TEXT PRIMARY KEY,
    feature_id    TEXT NOT NULL,
    milestone_id  TEXT NOT NULL,
    driver        TEXT NOT NULL CHECK(driver IN ('claude','opencode','codex')),
    session_id    TEXT,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    exit_code     INTEGER,
    stop_passed   INTEGER,
    mode          TEXT NOT NULL DEFAULT 'normal'
                  CHECK(mode IN ('normal','redteam')),
    stop_stdout   TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_feature   ON atos_runs(feature_id);
CREATE INDEX IF NOT EXISTS idx_runs_milestone ON atos_runs(milestone_id);
CREATE INDEX IF NOT EXISTS idx_runs_mode      ON atos_runs(mode);

-- Per-tool event log. Populated by the opencode plugin via
-- `record_atos_event`, and (eventually) by a Claude-side wrapper that
-- mirrors MCP tool_call_log rows into this table. Ring-buffered at
-- 10k rows per run so a runaway loop can't bloat features.db.
CREATE TABLE IF NOT EXISTS atos_tool_events (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES atos_runs(id) ON DELETE CASCADE,
    call_id     TEXT NOT NULL,
    tool_name   TEXT NOT NULL,
    phase       TEXT NOT NULL CHECK(phase IN ('before','after','parse_error')),
    args_json   TEXT,
    outcome     TEXT,
    duration_ms INTEGER,
    fired_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_run  ON atos_tool_events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_tool ON atos_tool_events(tool_name);
