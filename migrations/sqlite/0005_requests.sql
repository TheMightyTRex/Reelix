-- Reelix M6: requests, issues, per-user quotas and notification preferences.
-- All timestamps are INTEGER unix milliseconds.

-- Per-user quota overrides (NULL = global default from the request settings).
ALTER TABLE users ADD COLUMN movie_quota_limit INTEGER;
ALTER TABLE users ADD COLUMN movie_quota_days INTEGER;
ALTER TABLE users ADD COLUMN tv_quota_limit INTEGER;
ALTER TABLE users ADD COLUMN tv_quota_days INTEGER;
-- Encrypted (SecretBox) plex.tv token of Plex-linked users, used for watchlist sync.
ALTER TABLE users ADD COLUMN plex_token TEXT;

-- One Reelix account per external identity.
CREATE UNIQUE INDEX idx_users_external ON users(auth_source, external_id)
    WHERE external_id IS NOT NULL;

CREATE TABLE requests (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_kind     TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    tmdb_id        INTEGER NOT NULL,
    tvdb_id        INTEGER,
    imdb_id        TEXT,
    title          TEXT    NOT NULL,
    year           INTEGER,
    poster         TEXT,
    backdrop       TEXT,
    -- JSON array of season numbers (series only).
    seasons        TEXT    NOT NULL DEFAULT '[]',
    status         TEXT    NOT NULL DEFAULT 'pending'
                   CHECK (status IN ('pending', 'approved', 'declined', 'processing',
                                     'partially_available', 'available', 'failed')),
    is_4k          INTEGER NOT NULL DEFAULT 0,
    profile_id     INTEGER,
    root_folder_id INTEGER,
    -- JSON array of tag ids.
    tags           TEXT    NOT NULL DEFAULT '[]',
    modified_by    INTEGER REFERENCES users(id) ON DELETE SET NULL,
    decline_reason TEXT,
    failure_reason TEXT,
    movie_id       INTEGER,
    series_id      INTEGER,
    -- `manual` or `watchlist`.
    source         TEXT    NOT NULL DEFAULT 'manual',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    approved_at    INTEGER,
    available_at   INTEGER
);
CREATE INDEX idx_requests_media ON requests(media_kind, tmdb_id);
CREATE INDEX idx_requests_user ON requests(user_id, created_at);
CREATE INDEX idx_requests_status ON requests(status);

CREATE TABLE issues (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_kind  TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    tmdb_id     INTEGER NOT NULL,
    title       TEXT    NOT NULL,
    year        INTEGER,
    poster      TEXT,
    issue_type  TEXT    NOT NULL CHECK (issue_type IN ('video', 'audio', 'subtitles', 'other')),
    status      TEXT    NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
    season      INTEGER,
    episode     INTEGER,
    message     TEXT    NOT NULL,
    resolved_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE INDEX idx_issues_media ON issues(media_kind, tmdb_id);
CREATE INDEX idx_issues_user ON issues(user_id);
CREATE INDEX idx_issues_status ON issues(status);

CREATE TABLE issue_comments (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id   INTEGER NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message    TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_issue_comments_issue ON issue_comments(issue_id, id);

CREATE TABLE user_notification_settings (
    user_id    INTEGER PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- JSON NotificationPrefsDto.
    settings   TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);
