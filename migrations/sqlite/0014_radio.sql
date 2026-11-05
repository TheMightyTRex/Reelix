-- Reelix radio: stations, shows, series, episodes and the episode files of the Audiobookshelf
-- podcast library. Timestamps INTEGER unix ms (first broadcasts included, so "broadcast from
-- now on" compares with `followed_at`). Rows are never deleted when a source drops an item;
-- `source_removed_at` records it instead.
--
-- `root_folders` is rebuilt to accept kind `radio` (the podcast library folder as Reelix sees
-- it). Same steps as 0010: every column, constraint and the AUTOINCREMENT counter are kept and
-- nothing references root_folders.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE root_folders_new (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series', 'ebook', 'audiobook', 'music', 'radio')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
INSERT INTO root_folders_new (id, path, media_kind, created_at, updated_at)
    SELECT id, path, media_kind, created_at, updated_at FROM root_folders;
DELETE FROM sqlite_sequence WHERE name = 'root_folders_new';
UPDATE sqlite_sequence SET name = 'root_folders_new' WHERE name = 'root_folders';
DROP TABLE root_folders;
ALTER TABLE root_folders_new RENAME TO root_folders;

-- A broadcaster's service (BBC Radio 4) or a podcast network; `external_id` is the provider's
-- service id.
CREATE TABLE radio_stations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    provider    TEXT    NOT NULL,
    external_id TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    logo_url    TEXT,
    logo_path   TEXT,                                  -- downloaded logo, relative to the data folder
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (provider, external_id)
);

CREATE TABLE radio_shows (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    provider           TEXT    NOT NULL,               -- RadioProvider::id(): rss, bbc, …
    external_id        TEXT    NOT NULL,               -- feed URL for rss, programme pid for bbc
    title              TEXT    NOT NULL,
    sort_title         TEXT    NOT NULL,
    clean_title        TEXT    NOT NULL,               -- matching key (no case, accents, punctuation, "The")
    synopsis           TEXT,
    author             TEXT,
    web_url            TEXT,
    first_broadcast    INTEGER,
    station_id         INTEGER REFERENCES radio_stations(id) ON DELETE SET NULL,
    artwork_url        TEXT,
    artwork_path       TEXT,                           -- downloaded artwork, relative to the data folder
    feed_url           TEXT,
    follow             TEXT    NOT NULL DEFAULT 'all' CHECK (follow IN ('all', 'from_now_on', 'none')),
    followed_at        INTEGER,
    monitored          INTEGER NOT NULL DEFAULT 1,
    tags               TEXT    NOT NULL DEFAULT '[]',
    root_folder_id     INTEGER,
    abs_item_id        TEXT,                           -- Audiobookshelf podcast item it maps to
    quality_profile_id INTEGER,
    refreshed_at       INTEGER,
    added_at           INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    UNIQUE (provider, external_id)
);
CREATE INDEX idx_radio_shows_station ON radio_shows(station_id);
CREATE INDEX idx_radio_shows_sort    ON radio_shows(sort_title COLLATE NOCASE);
CREATE INDEX idx_radio_shows_abs     ON radio_shows(abs_item_id);

CREATE TABLE radio_series (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    show_id           INTEGER NOT NULL REFERENCES radio_shows(id) ON DELETE CASCADE,
    external_id       TEXT    NOT NULL,                -- provider id, or `season:N` / `specials` for feeds
    kind              TEXT    NOT NULL DEFAULT 'series' CHECK (kind IN ('series', 'specials')),
    number            INTEGER,                         -- specials are series 0 when filed
    number_source     TEXT    NOT NULL DEFAULT 'source' CHECK (number_source IN ('source', 'broadcast_order')),
    title             TEXT,
    synopsis          TEXT,
    expected_episodes INTEGER,
    settled_at        INTEGER,                         -- set once complete and quiet; refresh skips it
    source_removed_at INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    UNIQUE (show_id, external_id)
);
CREATE INDEX idx_radio_series_show ON radio_series(show_id);

-- One row per episode file of the Audiobookshelf podcast library (filled by radio_library_sync).
CREATE TABLE radio_episode_files (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    abs_item_id    TEXT    NOT NULL,
    abs_episode_id TEXT    NOT NULL,
    item_title     TEXT,                               -- the podcast's title in Audiobookshelf
    title          TEXT,
    season         INTEGER,
    episode        INTEGER,
    guid           TEXT,
    enclosure_url  TEXT,
    published_at   INTEGER,
    duration_secs  INTEGER,
    path           TEXT,
    size           INTEGER,
    seen_at        INTEGER NOT NULL,                   -- last sync that saw the file
    created_at     INTEGER NOT NULL,
    UNIQUE (abs_item_id, abs_episode_id)
);
CREATE INDEX idx_radio_episode_files_guid ON radio_episode_files(guid);

CREATE TABLE radio_episodes (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    show_id           INTEGER NOT NULL REFERENCES radio_shows(id) ON DELETE CASCADE,
    series_id         INTEGER NOT NULL REFERENCES radio_series(id) ON DELETE CASCADE,
    external_id       TEXT    NOT NULL,                -- provider id (feed guid, programme pid)
    number            INTEGER,
    number_source     TEXT    NOT NULL DEFAULT 'source' CHECK (number_source IN ('source', 'broadcast_order')),
    title             TEXT    NOT NULL,
    synopsis          TEXT,
    first_broadcast   INTEGER,
    duration_secs     INTEGER,
    monitored         INTEGER NOT NULL DEFAULT 0,      -- set from the show's follow choice
    status            TEXT    NOT NULL DEFAULT 'not_requested'
                      CHECK (status IN ('in_library', 'wanted', 'not_released', 'not_requested')),
    file_id           INTEGER REFERENCES radio_episode_files(id) ON DELETE SET NULL,
    manual_match      INTEGER NOT NULL DEFAULT 0,
    enclosure_url     TEXT,
    guid              TEXT,
    source_removed_at INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    UNIQUE (show_id, external_id)
);
CREATE INDEX idx_radio_episodes_series ON radio_episodes(series_id);
CREATE INDEX idx_radio_episodes_status ON radio_episodes(status);
-- At most one episode per file.
CREATE UNIQUE INDEX idx_radio_episodes_file ON radio_episodes(file_id) WHERE file_id IS NOT NULL;
