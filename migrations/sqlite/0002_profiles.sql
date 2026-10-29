-- Reelix M1: quality definitions, quality profiles, custom formats, delay and release profiles.
-- Structured parts are JSON text; defaults are seeded by the repositories on first access.

CREATE TABLE quality_definitions (
    quality_id     INTEGER PRIMARY KEY NOT NULL,
    title          TEXT    NOT NULL,
    min_size       REAL    NOT NULL DEFAULT 0,
    preferred_size REAL,
    max_size       REAL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE quality_profiles (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    name                     TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    items                    TEXT    NOT NULL,
    cutoff                   INTEGER NOT NULL,
    upgrade_allowed          INTEGER NOT NULL DEFAULT 1,
    min_format_score         INTEGER NOT NULL DEFAULT 0,
    cutoff_format_score      INTEGER NOT NULL DEFAULT 0,
    min_upgrade_format_score INTEGER NOT NULL DEFAULT 1,
    format_items             TEXT    NOT NULL DEFAULT '[]',
    language                 INTEGER,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL
);

CREATE TABLE custom_formats (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    include_in_rename INTEGER NOT NULL DEFAULT 0,
    specifications    TEXT    NOT NULL DEFAULT '[]',
    trash_id          TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

CREATE TABLE delay_profiles (
    id                                  INTEGER PRIMARY KEY AUTOINCREMENT,
    enable_usenet                       INTEGER NOT NULL DEFAULT 1,
    enable_torrent                      INTEGER NOT NULL DEFAULT 1,
    preferred_protocol                  TEXT    NOT NULL DEFAULT 'usenet'
                                        CHECK (preferred_protocol IN ('usenet', 'torrent')),
    usenet_delay                        INTEGER NOT NULL DEFAULT 0,
    torrent_delay                       INTEGER NOT NULL DEFAULT 0,
    bypass_if_highest_quality           INTEGER NOT NULL DEFAULT 1,
    bypass_if_above_custom_format_score INTEGER NOT NULL DEFAULT 0,
    minimum_custom_format_score         INTEGER NOT NULL DEFAULT 0,
    tags                                TEXT    NOT NULL DEFAULT '[]',
    sort_order                          INTEGER NOT NULL DEFAULT 2147483647,
    created_at                          INTEGER NOT NULL,
    updated_at                          INTEGER NOT NULL
);

CREATE TABLE release_profiles (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL DEFAULT '',
    enabled    INTEGER NOT NULL DEFAULT 1,
    required   TEXT    NOT NULL DEFAULT '[]',
    ignored    TEXT    NOT NULL DEFAULT '[]',
    indexer_id INTEGER,
    tags       TEXT    NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
