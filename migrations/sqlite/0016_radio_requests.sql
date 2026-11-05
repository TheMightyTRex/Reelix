-- Radio on Discover: radio show requests and the ids a followed show was found under.
--
-- `requests` is rebuilt to accept `media_kind = 'radio'` (foreign id `{provider}:{external_id}`)
-- and gets `radio_show_id` (the followed show once approved). Same steps as 0010: every column,
-- default, constraint and index is kept (no migration since 0010 altered the table and no table
-- has a foreign key pointing at it), and the AUTOINCREMENT counter is carried over.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE requests_new (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_kind     TEXT    NOT NULL CHECK (media_kind IN ('movie', 'series', 'book', 'music', 'radio')),
    -- Required for movie/series (see the table CHECK below).
    tmdb_id        INTEGER,
    tvdb_id        INTEGER,
    imdb_id        TEXT,
    -- Open Library work id (book), MusicBrainz release-group id (music) or
    -- `{provider}:{external_id}` (radio).
    foreign_id     TEXT,
    title          TEXT    NOT NULL,
    -- Author / artist / station display name (book/music/radio).
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
    -- The followed radio show (radio requests, once approved).
    radio_show_id  INTEGER,
    -- `manual` or `watchlist`.
    source         TEXT    NOT NULL DEFAULT 'manual',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    approved_at    INTEGER,
    available_at   INTEGER,
    CHECK ((media_kind IN ('movie', 'series') AND tmdb_id IS NOT NULL)
        OR (media_kind IN ('book', 'music', 'radio') AND foreign_id IS NOT NULL))
);
INSERT INTO requests_new (id, user_id, media_kind, tmdb_id, tvdb_id, imdb_id, foreign_id, title, subtitle, year,
        poster, backdrop, seasons, book_formats, status, is_4k, profile_id, root_folder_id, tags, modified_by,
        decline_reason, failure_reason, movie_id, series_id, book_id, author_id, album_id, artist_id, source,
        created_at, updated_at, approved_at, available_at)
    SELECT id, user_id, media_kind, tmdb_id, tvdb_id, imdb_id, foreign_id, title, subtitle, year,
        poster, backdrop, seasons, book_formats, status, is_4k, profile_id, root_folder_id, tags, modified_by,
        decline_reason, failure_reason, movie_id, series_id, book_id, author_id, album_id, artist_id, source,
        created_at, updated_at, approved_at, available_at
    FROM requests;
DELETE FROM sqlite_sequence WHERE name = 'requests_new';
UPDATE sqlite_sequence SET name = 'requests_new' WHERE name = 'requests';
DROP TABLE requests;
ALTER TABLE requests_new RENAME TO requests;
CREATE INDEX idx_requests_media   ON requests(media_kind, tmdb_id);
CREATE INDEX idx_requests_foreign ON requests(media_kind, foreign_id);
CREATE INDEX idx_requests_user    ON requests(user_id, created_at);
CREATE INDEX idx_requests_status  ON requests(status);

-- Other ids a followed show was added under: a directory id handed to another provider
-- (`apple` + Apple id → an `rss` show), or a BBC series pid that resolved to its brand. Lets
-- Discover and search mark such hits as followed without asking the source again.
CREATE TABLE radio_show_aliases (
    provider    TEXT    NOT NULL,
    external_id TEXT    NOT NULL,
    show_id     INTEGER NOT NULL REFERENCES radio_shows(id) ON DELETE CASCADE,
    PRIMARY KEY (provider, external_id)
);
CREATE INDEX idx_radio_show_aliases_show ON radio_show_aliases(show_id);
