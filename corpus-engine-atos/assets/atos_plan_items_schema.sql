PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS plan_items (
    id            TEXT    PRIMARY KEY,
    phase         INTEGER NOT NULL,
    title         TEXT    NOT NULL,
    body          TEXT    NOT NULL,
    realizes      TEXT,
    depends_on    TEXT    NOT NULL DEFAULT '[]',
    stop_hint     TEXT,
    state         TEXT    NOT NULL CHECK(state IN ('open','in-progress','done','deferred')),
    design_hash   TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_plan_phase ON plan_items(phase);
CREATE INDEX IF NOT EXISTS idx_plan_state ON plan_items(state);
CREATE INDEX IF NOT EXISTS idx_plan_design_hash ON plan_items(design_hash);

PRAGMA user_version = 1;
