-- M7 integrations: import list sync state + exclusions, media server availability.
-- Timestamps are unix milliseconds.

-- Media that import lists must never add (matched by TMDB id per kind).
CREATE TABLE import_list_exclusions (
    id         INTEGER PRIMARY KEY,
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    tmdb_id    INTEGER NOT NULL,
    title      TEXT    NOT NULL,
    year       INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE (media_kind, tmdb_id)
);

-- Last sync outcome per import list (provider row id).
CREATE TABLE import_list_status (
    list_id     INTEGER PRIMARY KEY,
    last_sync   INTEGER NOT NULL,
    last_error  TEXT,
    items_found INTEGER NOT NULL DEFAULT 0,
    items_added INTEGER NOT NULL DEFAULT 0
);

-- Library items found on a media server by the availability sync.
CREATE TABLE media_server_availability (
    server_id  INTEGER NOT NULL,
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    media_id   INTEGER NOT NULL,
    item_id    TEXT    NOT NULL,
    synced_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, media_kind, media_id)
);
CREATE INDEX idx_ms_availability_media ON media_server_availability(media_kind, media_id);

-- Per-server sync summary.
CREATE TABLE media_server_sync (
    server_id   INTEGER PRIMARY KEY,
    synced_at   INTEGER NOT NULL,
    items_total INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT
);
