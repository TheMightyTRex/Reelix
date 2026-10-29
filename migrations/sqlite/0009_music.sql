-- Reelix M9: music. Albums are MusicBrainz release groups; album_releases are their releases
-- (editions); tracks belong to one release. Timestamps INTEGER unix ms; dates TEXT.

CREATE TABLE metadata_profiles (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    primary_types    TEXT    NOT NULL DEFAULT '["album","ep"]',      -- AlbumType JSON
    secondary_types  TEXT    NOT NULL DEFAULT '[]',                  -- SecondaryAlbumType JSON (allowed)
    release_statuses TEXT    NOT NULL DEFAULT '["official"]',        -- ReleaseStatus JSON
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);
INSERT INTO metadata_profiles (name, primary_types, secondary_types, release_statuses, created_at, updated_at)
VALUES ('Standard', '["album","ep"]', '[]', '["official"]', 0, 0);

CREATE TABLE artists (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    musicbrainz_id      TEXT    NOT NULL UNIQUE,
    name                TEXT    NOT NULL,
    sort_name           TEXT    NOT NULL,
    clean_name          TEXT    NOT NULL,
    disambiguation      TEXT,
    artist_type         TEXT,                                  -- person | group | orchestra | choir | character | other
    country             TEXT,
    overview            TEXT,
    genres              TEXT    NOT NULL DEFAULT '[]',
    images              TEXT    NOT NULL DEFAULT '{}',
    links               TEXT    NOT NULL DEFAULT '[]',
    monitored           INTEGER NOT NULL DEFAULT 1,
    monitor_new_items   TEXT    NOT NULL DEFAULT 'all' CHECK (monitor_new_items IN ('all', 'none', 'new')),
    quality_profile_id  INTEGER NOT NULL,
    metadata_profile_id INTEGER NOT NULL,
    root_folder_id      INTEGER NOT NULL,
    path                TEXT    NOT NULL,
    tags                TEXT    NOT NULL DEFAULT '[]',
    added_by_request_id INTEGER,
    last_refreshed      INTEGER,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
CREATE INDEX idx_artists_root ON artists(root_folder_id);

CREATE TABLE albums (
    id                           INTEGER PRIMARY KEY AUTOINCREMENT,
    artist_id                    INTEGER NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    musicbrainz_release_group_id TEXT    NOT NULL UNIQUE,
    title                        TEXT    NOT NULL,
    clean_title                  TEXT    NOT NULL,
    disambiguation               TEXT,
    album_type                   TEXT    NOT NULL DEFAULT 'album',   -- AlbumType
    secondary_types              TEXT    NOT NULL DEFAULT '[]',
    release_date                 TEXT,
    genres                       TEXT    NOT NULL DEFAULT '[]',
    images                       TEXT    NOT NULL DEFAULT '{}',
    ratings                      TEXT    NOT NULL DEFAULT '{}',
    monitored                    INTEGER NOT NULL DEFAULT 1,
    -- Release whose tracklist is wanted; NULL = not chosen yet (auto-pick on refresh).
    selected_release_id          INTEGER,
    -- Accept any release of the group at import (matched by track count/durations).
    any_release_ok               INTEGER NOT NULL DEFAULT 1,
    added_by_request_id          INTEGER,
    last_refreshed               INTEGER,
    created_at                   INTEGER NOT NULL,
    updated_at                   INTEGER NOT NULL
);
CREATE INDEX idx_albums_artist  ON albums(artist_id);
CREATE INDEX idx_albums_release ON albums(release_date);

CREATE TABLE album_releases (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id               INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    musicbrainz_release_id TEXT    NOT NULL UNIQUE,
    title                  TEXT    NOT NULL,
    status                 TEXT,                               -- official | promotion | bootleg | pseudo-release
    country                TEXT,
    label                  TEXT,
    catalog_number         TEXT,
    barcode                TEXT,
    format                 TEXT,                               -- "CD", "Digital Media", "2×Vinyl"…
    disc_count             INTEGER NOT NULL DEFAULT 1,
    track_count            INTEGER NOT NULL DEFAULT 0,
    release_date           TEXT,
    disambiguation         TEXT,
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL
);
CREATE INDEX idx_album_releases_album ON album_releases(album_id);

CREATE TABLE track_files (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id            INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    relative_path       TEXT    NOT NULL,                      -- relative to artists.path
    size                INTEGER NOT NULL DEFAULT 0,
    quality             TEXT    NOT NULL,                      -- QualityModel JSON
    bitrate_kbps        INTEGER,
    sample_rate_hz      INTEGER,
    bit_depth           INTEGER,
    channels            INTEGER,
    duration_ms         INTEGER,
    tags                TEXT    NOT NULL DEFAULT '{}',         -- tags read from the file (JSON)
    release_group       TEXT,
    scene_name          TEXT,
    original_path       TEXT,
    custom_format_ids   TEXT    NOT NULL DEFAULT '[]',
    custom_format_score INTEGER NOT NULL DEFAULT 0,
    date_added          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (album_id, relative_path)
);

CREATE TABLE tracks (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id                 INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    release_id               INTEGER NOT NULL REFERENCES album_releases(id) ON DELETE CASCADE,
    musicbrainz_track_id     TEXT,
    musicbrainz_recording_id TEXT,
    disc_number              INTEGER NOT NULL DEFAULT 1,
    track_number             INTEGER NOT NULL,
    absolute_number          INTEGER,
    number                   TEXT,                              -- MusicBrainz display number ("A1")
    title                    TEXT    NOT NULL,
    duration_ms              INTEGER,
    artist_credit            TEXT,
    track_file_id            INTEGER REFERENCES track_files(id) ON DELETE SET NULL,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL,
    UNIQUE (release_id, disc_number, track_number)
);
CREATE INDEX idx_tracks_album ON tracks(album_id);
CREATE INDEX idx_tracks_file  ON tracks(track_file_id);

-- Artists / albums music import lists must never add.
CREATE TABLE music_list_exclusions (
    id             INTEGER PRIMARY KEY,
    kind           TEXT    NOT NULL CHECK (kind IN ('artist', 'album')),
    musicbrainz_id TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    created_at     INTEGER NOT NULL,
    UNIQUE (kind, musicbrainz_id)
);
