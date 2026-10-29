-- Search → grab → download → import pipeline (M4). Timestamps are unix milliseconds.

CREATE TABLE history (
    id             INTEGER PRIMARY KEY,
    -- grabbed | imported | download_failed | import_failed | file_deleted | renamed | ignored
    event_type     TEXT    NOT NULL,
    movie_id       INTEGER,
    series_id      INTEGER,
    episode_id     INTEGER,
    source_title   TEXT    NOT NULL,
    quality        TEXT,              -- QualityModel JSON
    languages      TEXT    NOT NULL DEFAULT '[]',
    custom_formats TEXT    NOT NULL DEFAULT '[]', -- [{id,name}]
    format_score   INTEGER NOT NULL DEFAULT 0,
    download_id    TEXT,
    protocol       TEXT,
    indexer        TEXT,
    download_client TEXT,
    data           TEXT    NOT NULL DEFAULT '{}', -- event-specific details
    date           INTEGER NOT NULL
);
CREATE INDEX idx_history_movie    ON history(movie_id, date);
CREATE INDEX idx_history_series   ON history(series_id, date);
CREATE INDEX idx_history_episode  ON history(episode_id, date);
CREATE INDEX idx_history_download ON history(download_id);
CREATE INDEX idx_history_date     ON history(date);

CREATE TABLE blocklist (
    id                INTEGER PRIMARY KEY,
    movie_id          INTEGER,
    series_id         INTEGER,
    episode_ids       TEXT    NOT NULL DEFAULT '[]',
    source_title      TEXT    NOT NULL,
    quality           TEXT,
    languages         TEXT    NOT NULL DEFAULT '[]',
    protocol          TEXT,
    indexer           TEXT,
    size              INTEGER,
    publish_date      INTEGER,
    torrent_info_hash TEXT,
    message           TEXT,
    date              INTEGER NOT NULL
);
CREATE INDEX idx_blocklist_movie  ON blocklist(movie_id);
CREATE INDEX idx_blocklist_series ON blocklist(series_id);
CREATE INDEX idx_blocklist_title  ON blocklist(source_title);

-- Downloads Reelix is responsible for (created on grab, driven by the download monitor).
CREATE TABLE tracked_downloads (
    id                 INTEGER PRIMARY KEY,
    download_client_id INTEGER NOT NULL,
    download_id        TEXT    NOT NULL,
    protocol           TEXT    NOT NULL,
    title              TEXT    NOT NULL,
    movie_id           INTEGER,
    series_id          INTEGER,
    episode_ids        TEXT    NOT NULL DEFAULT '[]',
    release            TEXT    NOT NULL DEFAULT '{}', -- GrabbedRelease JSON snapshot
    -- downloading | import_pending | importing | imported | failed | failed_pending | ignored | removed
    state              TEXT    NOT NULL,
    status_messages    TEXT    NOT NULL DEFAULT '[]',
    output_path        TEXT,
    added              INTEGER NOT NULL,
    updated            INTEGER NOT NULL,
    UNIQUE (download_client_id, download_id)
);
CREATE INDEX idx_tracked_state ON tracked_downloads(state);

-- Releases held back by a delay profile (or awaiting a preferred protocol).
CREATE TABLE pending_releases (
    id          INTEGER PRIMARY KEY,
    movie_id    INTEGER,
    series_id   INTEGER,
    episode_ids TEXT    NOT NULL DEFAULT '[]',
    title       TEXT    NOT NULL,
    release     TEXT    NOT NULL, -- GrabbedRelease JSON snapshot incl. indexer + download URL
    reason      TEXT    NOT NULL, -- delay | preferred_protocol
    added       INTEGER NOT NULL,
    release_at  INTEGER NOT NULL
);
CREATE INDEX idx_pending_release_at ON pending_releases(release_at);
