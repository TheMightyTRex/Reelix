-- M8/M9 shared changes: root folder kinds, quality profile kinds, request kinds, quotas,
-- pipeline media references, metadata cache.
--
-- `root_folders` and `requests` are rebuilt to widen their CHECK constraints. Every existing
-- column, default, constraint and index is kept (verified against 0004/0005; no later migration
-- alters either table and no table has a foreign key pointing at them). The AUTOINCREMENT
-- counter is carried over so ids of deleted rows are never reused.
-- sqlx runs this file in a transaction, where `PRAGMA foreign_keys` cannot change; deferring
-- the checks to COMMIT is enough for the rebuilds.
PRAGMA defer_foreign_keys = ON;

-- 1. root_folders: widen media_kind (table rebuild; nothing references root_folders).
CREATE TABLE root_folders_new (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    media_kind TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series', 'ebook', 'audiobook', 'music')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
INSERT INTO root_folders_new (id, path, media_kind, created_at, updated_at)
    SELECT id, path, media_kind, created_at, updated_at FROM root_folders;
DELETE FROM sqlite_sequence WHERE name = 'root_folders_new';
UPDATE sqlite_sequence SET name = 'root_folders_new' WHERE name = 'root_folders';
DROP TABLE root_folders;
ALTER TABLE root_folders_new RENAME TO root_folders;

-- 2. quality profiles per family (existing rows are video profiles).
ALTER TABLE quality_profiles ADD COLUMN media_kind TEXT NOT NULL DEFAULT 'video'
    CHECK (media_kind IN ('video', 'ebook', 'audiobook', 'music'));
CREATE INDEX idx_quality_profiles_kind ON quality_profiles(media_kind, id);

-- 3. per-user book/music quotas (NULL = global default).
ALTER TABLE users ADD COLUMN book_quota_limit INTEGER;
ALTER TABLE users ADD COLUMN book_quota_days INTEGER;
ALTER TABLE users ADD COLUMN music_quota_limit INTEGER;
ALTER TABLE users ADD COLUMN music_quota_days INTEGER;

-- 4. requests: book/music kinds, tmdb_id nullable, foreign ids (rebuild).
CREATE TABLE requests_new (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_kind     TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series', 'book', 'music')),
    -- Required for movie/series (see the table CHECK below).
    tmdb_id        INTEGER,
    tvdb_id        INTEGER,
    imdb_id        TEXT,
    -- Open Library work id (book) or MusicBrainz release-group id (music).
    foreign_id     TEXT,
    title          TEXT    NOT NULL,
    -- Author / artist display name (book/music).
    subtitle       TEXT,
    year           INTEGER,
    poster         TEXT,
    backdrop       TEXT,
    -- JSON array of season numbers (series only).
    seasons        TEXT    NOT NULL DEFAULT '[]',
    -- Book: requested formats, JSON ["ebook","audiobook"].
    book_formats   TEXT    NOT NULL DEFAULT '[]',
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
    book_id        INTEGER,
    author_id      INTEGER,
    album_id       INTEGER,
    artist_id      INTEGER,
    -- `manual` or `watchlist`.
    source         TEXT    NOT NULL DEFAULT 'manual',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    approved_at    INTEGER,
    available_at   INTEGER,
    CHECK ((media_kind IN ('movie', 'series') AND tmdb_id IS NOT NULL)
        OR (media_kind IN ('book', 'music') AND foreign_id IS NOT NULL))
);
INSERT INTO requests_new (id, user_id, media_kind, tmdb_id, tvdb_id, imdb_id, title, year, poster, backdrop,
        seasons, status, is_4k, profile_id, root_folder_id, tags, modified_by, decline_reason, failure_reason,
        movie_id, series_id, source, created_at, updated_at, approved_at, available_at)
    SELECT id, user_id, media_kind, tmdb_id, tvdb_id, imdb_id, title, year, poster, backdrop,
        seasons, status, is_4k, profile_id, root_folder_id, tags, modified_by, decline_reason, failure_reason,
        movie_id, series_id, source, created_at, updated_at, approved_at, available_at
    FROM requests;
DELETE FROM sqlite_sequence WHERE name = 'requests_new';
UPDATE sqlite_sequence SET name = 'requests_new' WHERE name = 'requests';
DROP TABLE requests;
ALTER TABLE requests_new RENAME TO requests;
CREATE INDEX idx_requests_media   ON requests(media_kind, tmdb_id);
CREATE INDEX idx_requests_foreign ON requests(media_kind, foreign_id);
CREATE INDEX idx_requests_user    ON requests(user_id, created_at);
CREATE INDEX idx_requests_status  ON requests(status);

-- 5. pipeline media references.
ALTER TABLE history ADD COLUMN book_id INTEGER;
ALTER TABLE history ADD COLUMN book_format TEXT;            -- ebook | audiobook
ALTER TABLE history ADD COLUMN author_id INTEGER;
ALTER TABLE history ADD COLUMN album_id INTEGER;
ALTER TABLE history ADD COLUMN artist_id INTEGER;
CREATE INDEX idx_history_book   ON history(book_id, date);
CREATE INDEX idx_history_author ON history(author_id, date);
CREATE INDEX idx_history_album  ON history(album_id, date);
CREATE INDEX idx_history_artist ON history(artist_id, date);

ALTER TABLE blocklist ADD COLUMN book_id INTEGER;
ALTER TABLE blocklist ADD COLUMN book_format TEXT;
ALTER TABLE blocklist ADD COLUMN album_id INTEGER;
CREATE INDEX idx_blocklist_book  ON blocklist(book_id);
CREATE INDEX idx_blocklist_album ON blocklist(album_id);

ALTER TABLE tracked_downloads ADD COLUMN book_id INTEGER;
ALTER TABLE tracked_downloads ADD COLUMN book_format TEXT;
ALTER TABLE tracked_downloads ADD COLUMN album_id INTEGER;

ALTER TABLE pending_releases ADD COLUMN book_id INTEGER;
ALTER TABLE pending_releases ADD COLUMN book_format TEXT;
ALTER TABLE pending_releases ADD COLUMN album_id INTEGER;

-- 6. persistent metadata cache (MusicBrainz is limited to 1 req/s; optional use by books/music).
CREATE TABLE IF NOT EXISTS metadata_cache (
    provider   TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    payload    TEXT    NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (provider, key)
);
CREATE INDEX IF NOT EXISTS idx_metadata_cache_expiry ON metadata_cache(expires_at);
