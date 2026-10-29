-- Reelix M0 schema: users, auth, settings, jobs and scheduled tasks.
-- All timestamps are INTEGER unix milliseconds.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    email         TEXT,
    display_name  TEXT,
    avatar        TEXT,
    password_hash TEXT,
    auth_source   TEXT    NOT NULL DEFAULT 'local',
    external_id   TEXT,
    permissions   INTEGER NOT NULL DEFAULT 0,
    locale        TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash  TEXT    PRIMARY KEY NOT NULL,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    user_agent  TEXT,
    ip          TEXT
);
CREATE INDEX idx_sessions_user ON sessions(user_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);

CREATE TABLE api_keys (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    prefix       TEXT    NOT NULL,
    key_hash     TEXT    NOT NULL UNIQUE,
    scopes       TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX idx_api_keys_user ON api_keys(user_id);

CREATE TABLE settings (
    key        TEXT    PRIMARY KEY NOT NULL,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE jobs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,
    payload      TEXT    NOT NULL DEFAULT '{}',
    status       TEXT    NOT NULL DEFAULT 'queued'
                 CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    priority     INTEGER NOT NULL DEFAULT 0,
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    run_after    INTEGER NOT NULL,
    dedupe_key   TEXT,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER,
    error        TEXT
);
CREATE INDEX idx_jobs_claim ON jobs(kind, status, priority DESC, run_after, id);
CREATE INDEX idx_jobs_finished ON jobs(status, finished_at);
-- At most one active (queued or running) job per dedupe key.
CREATE UNIQUE INDEX idx_jobs_dedupe_active ON jobs(dedupe_key)
    WHERE dedupe_key IS NOT NULL AND status IN ('queued', 'running');

CREATE TABLE scheduled_tasks (
    kind             TEXT    PRIMARY KEY NOT NULL,
    name             TEXT    NOT NULL,
    interval_secs    INTEGER NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    last_run         INTEGER,
    next_run         INTEGER,
    last_duration_ms INTEGER,
    last_error       TEXT
);
