-- Reelix M2/M5: movie & series library.
-- Timestamps are INTEGER unix ms; calendar dates are TEXT 'YYYY-MM-DD'; structured data is JSON text.
-- Quality profile / root folder / tag references are checked by the library service (no FKs),
-- so deleting a profile or tag never fails with a raw constraint error.

CREATE TABLE root_folders (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE collections (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id              INTEGER NOT NULL UNIQUE,
    title                TEXT    NOT NULL,
    sort_title           TEXT    NOT NULL,
    overview             TEXT,
    images               TEXT    NOT NULL DEFAULT '{}',
    -- Cached collection parts (JSON array of {tmdb_id,title,year,overview,poster}).
    parts                TEXT    NOT NULL DEFAULT '[]',
    monitored            INTEGER NOT NULL DEFAULT 0,
    quality_profile_id   INTEGER NOT NULL,
    root_folder_id       INTEGER NOT NULL,
    minimum_availability TEXT    NOT NULL DEFAULT 'released',
    search_on_add        INTEGER NOT NULL DEFAULT 0,
    tags                 TEXT    NOT NULL DEFAULT '[]',
    last_synced          INTEGER,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
);

CREATE TABLE movies (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id              INTEGER NOT NULL UNIQUE,
    imdb_id              TEXT,
    title                TEXT    NOT NULL,
    original_title       TEXT,
    sort_title           TEXT    NOT NULL,
    clean_title          TEXT    NOT NULL,
    year                 INTEGER,
    status               TEXT    NOT NULL DEFAULT 'tba',
    overview             TEXT,
    tagline              TEXT,
    runtime              INTEGER,
    studio               TEXT,
    genres               TEXT    NOT NULL DEFAULT '[]',
    certification        TEXT,
    original_language    TEXT,
    images               TEXT    NOT NULL DEFAULT '{}',
    ratings              TEXT    NOT NULL DEFAULT '{}',
    credits              TEXT    NOT NULL DEFAULT '[]',
    in_cinemas           TEXT,
    digital_release      TEXT,
    physical_release     TEXT,
    youtube_trailer      TEXT,
    homepage             TEXT,
    monitored            INTEGER NOT NULL DEFAULT 1,
    minimum_availability TEXT    NOT NULL DEFAULT 'released',
    quality_profile_id   INTEGER NOT NULL,
    root_folder_id       INTEGER NOT NULL,
    path                 TEXT    NOT NULL,
    collection_id        INTEGER REFERENCES collections(id) ON DELETE SET NULL,
    added_by_request_id  INTEGER,
    last_refreshed       INTEGER,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
);
CREATE INDEX idx_movies_collection ON movies(collection_id);
CREATE INDEX idx_movies_in_cinemas ON movies(in_cinemas);
CREATE INDEX idx_movies_digital ON movies(digital_release);
CREATE INDEX idx_movies_physical ON movies(physical_release);
CREATE INDEX idx_movies_root ON movies(root_folder_id);

CREATE TABLE movie_files (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    movie_id            INTEGER NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
    relative_path       TEXT    NOT NULL,
    size                INTEGER NOT NULL DEFAULT 0,
    quality             TEXT    NOT NULL,
    languages           TEXT    NOT NULL DEFAULT '[]',
    release_group       TEXT,
    edition             TEXT,
    scene_name          TEXT,
    original_path       TEXT,
    media_info          TEXT,
    custom_format_ids   TEXT    NOT NULL DEFAULT '[]',
    custom_format_score INTEGER NOT NULL DEFAULT 0,
    date_added          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (movie_id, relative_path)
);

CREATE TABLE series (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    tmdb_id             INTEGER NOT NULL UNIQUE,
    tvdb_id             INTEGER,
    imdb_id             TEXT,
    title               TEXT    NOT NULL,
    original_title      TEXT,
    sort_title          TEXT    NOT NULL,
    clean_title         TEXT    NOT NULL,
    year                INTEGER,
    status              TEXT    NOT NULL DEFAULT 'continuing',
    overview            TEXT,
    network             TEXT,
    series_type         TEXT    NOT NULL DEFAULT 'standard',
    runtime             INTEGER,
    genres              TEXT    NOT NULL DEFAULT '[]',
    certification       TEXT,
    original_language   TEXT,
    images              TEXT    NOT NULL DEFAULT '{}',
    ratings             TEXT    NOT NULL DEFAULT '{}',
    credits             TEXT    NOT NULL DEFAULT '[]',
    first_aired         TEXT,
    last_aired          TEXT,
    next_airing         TEXT,
    monitored           INTEGER NOT NULL DEFAULT 1,
    monitor_new_items   TEXT    NOT NULL DEFAULT 'all',
    season_folder       INTEGER NOT NULL DEFAULT 1,
    quality_profile_id  INTEGER NOT NULL,
    root_folder_id      INTEGER NOT NULL,
    path                TEXT    NOT NULL,
    added_by_request_id INTEGER,
    last_refreshed      INTEGER,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
CREATE INDEX idx_series_tvdb ON series(tvdb_id);
CREATE INDEX idx_series_root ON series(root_folder_id);

CREATE TABLE seasons (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    series_id     INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number INTEGER NOT NULL,
    monitored     INTEGER NOT NULL DEFAULT 1,
    title         TEXT,
    overview      TEXT,
    poster        TEXT,
    air_date      TEXT,
    UNIQUE (series_id, season_number)
);

CREATE TABLE episode_files (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    series_id           INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number       INTEGER NOT NULL,
    relative_path       TEXT    NOT NULL,
    size                INTEGER NOT NULL DEFAULT 0,
    quality             TEXT    NOT NULL,
    languages           TEXT    NOT NULL DEFAULT '[]',
    release_group       TEXT,
    edition             TEXT,
    scene_name          TEXT,
    original_path       TEXT,
    media_info          TEXT,
    custom_format_ids   TEXT    NOT NULL DEFAULT '[]',
    custom_format_score INTEGER NOT NULL DEFAULT 0,
    date_added          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (series_id, relative_path)
);

CREATE TABLE episodes (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    series_id             INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number         INTEGER NOT NULL,
    episode_number        INTEGER NOT NULL,
    absolute_number       INTEGER,
    scene_season_number   INTEGER,
    scene_episode_number  INTEGER,
    scene_absolute_number INTEGER,
    title                 TEXT,
    overview              TEXT,
    air_date              TEXT,
    runtime               INTEGER,
    still                 TEXT,
    finale                TEXT,
    tmdb_id               INTEGER,
    tvdb_id               INTEGER,
    monitored             INTEGER NOT NULL DEFAULT 1,
    episode_file_id       INTEGER REFERENCES episode_files(id) ON DELETE SET NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    UNIQUE (series_id, season_number, episode_number)
);
CREATE INDEX idx_episodes_air_date ON episodes(air_date);
CREATE INDEX idx_episodes_file ON episodes(episode_file_id);

-- Alternative titles of movies and series (used for matching and lookup).
CREATE TABLE alternative_titles (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    media_kind  TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    media_id    INTEGER NOT NULL,
    title       TEXT    NOT NULL,
    clean_title TEXT    NOT NULL,
    country     TEXT
);
CREATE INDEX idx_alt_titles_media ON alternative_titles(media_kind, media_id);

-- Tags on movies and series (tag ids come from the `tags` table owned by providers).
CREATE TABLE media_tags (
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series')),
    media_id   INTEGER NOT NULL,
    tag_id     INTEGER NOT NULL,
    PRIMARY KEY (media_kind, media_id, tag_id)
);
CREATE INDEX idx_media_tags_tag ON media_tags(tag_id);
