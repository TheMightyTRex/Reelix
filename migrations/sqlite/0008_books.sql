-- Reelix M8: books. Ebook and audiobook are two formats of one book, each with its own
-- quality profile and root folder (on the author). Timestamps INTEGER unix ms; dates TEXT 'YYYY-MM-DD'.
-- Profile / root folder / tag references are checked by the books service (no FKs), like 0004.

CREATE TABLE authors (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    openlibrary_id           TEXT    NOT NULL UNIQUE,          -- e.g. OL23919A
    name                     TEXT    NOT NULL,
    sort_name                TEXT    NOT NULL,
    clean_name               TEXT    NOT NULL,
    overview                 TEXT,
    birth_date               TEXT,
    death_date               TEXT,
    images                   TEXT    NOT NULL DEFAULT '{}',    -- ImagesDto JSON
    links                    TEXT    NOT NULL DEFAULT '[]',
    monitored                INTEGER NOT NULL DEFAULT 1,
    monitor_new_items        TEXT    NOT NULL DEFAULT 'all' CHECK (monitor_new_items IN ('all', 'none')),
    -- Ebook format: NULL profile = ebooks not wanted for this author.
    quality_profile_id       INTEGER,
    root_folder_id           INTEGER,
    path                     TEXT,
    -- Audiobook format: NULL profile = audiobooks not wanted.
    audio_quality_profile_id INTEGER,
    audio_root_folder_id     INTEGER,
    audio_path               TEXT,
    tags                     TEXT    NOT NULL DEFAULT '[]',    -- JSON tag ids (no media_tags rows)
    added_by_request_id      INTEGER,
    last_refreshed           INTEGER,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL,
    CHECK (quality_profile_id IS NOT NULL OR audio_quality_profile_id IS NOT NULL)
);
CREATE INDEX idx_authors_root ON authors(root_folder_id);
CREATE INDEX idx_authors_audio_root ON authors(audio_root_folder_id);

CREATE TABLE books (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    author_id            INTEGER NOT NULL REFERENCES authors(id) ON DELETE CASCADE,
    openlibrary_work_id  TEXT    NOT NULL UNIQUE,              -- e.g. OL45883W
    title                TEXT    NOT NULL,
    sort_title           TEXT    NOT NULL,
    clean_title          TEXT    NOT NULL,
    subtitle             TEXT,
    overview             TEXT,
    release_date         TEXT,
    year                 INTEGER,
    genres               TEXT    NOT NULL DEFAULT '[]',
    images               TEXT    NOT NULL DEFAULT '{}',
    ratings              TEXT    NOT NULL DEFAULT '{}',
    isbn13               TEXT,
    asin                 TEXT,
    page_count           INTEGER,
    language             TEXT,
    monitored_ebook      INTEGER NOT NULL DEFAULT 0,
    monitored_audiobook  INTEGER NOT NULL DEFAULT 0,
    added_by_request_id  INTEGER,
    last_refreshed       INTEGER,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
);
CREATE INDEX idx_books_author  ON books(author_id);
CREATE INDEX idx_books_isbn13  ON books(isbn13);
CREATE INDEX idx_books_asin    ON books(asin);
CREATE INDEX idx_books_release ON books(release_date);

-- Credited authors (multi-author books); authors here need not be in the library.
CREATE TABLE book_authors (
    book_id        INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    openlibrary_id TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    role           TEXT,                                       -- author | narrator | translator | editor
    position       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book_id, openlibrary_id)
);
CREATE INDEX idx_book_authors_ol ON book_authors(openlibrary_id);

CREATE TABLE book_editions (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id                INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    openlibrary_edition_id TEXT    NOT NULL UNIQUE,            -- e.g. OL7353617M
    title                  TEXT    NOT NULL,
    isbn13                 TEXT,
    isbn10                 TEXT,
    asin                   TEXT,
    publisher              TEXT,
    publish_date           TEXT,
    language               TEXT,
    page_count             INTEGER,
    physical_format        TEXT,
    is_audiobook           INTEGER NOT NULL DEFAULT 0,
    images                 TEXT    NOT NULL DEFAULT '{}',
    -- Preferred edition for metadata / tag writing (at most one per book, enforced by the service).
    monitored              INTEGER NOT NULL DEFAULT 0,
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL
);
CREATE INDEX idx_book_editions_book ON book_editions(book_id);
CREATE INDEX idx_book_editions_isbn ON book_editions(isbn13);
CREATE INDEX idx_book_editions_asin ON book_editions(asin);

CREATE TABLE book_series (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT    NOT NULL,
    clean_title TEXT    NOT NULL UNIQUE,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE book_series_links (
    series_id     INTEGER NOT NULL REFERENCES book_series(id) ON DELETE CASCADE,
    book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    position      TEXT,                                        -- display, e.g. "1", "2.5", "0.5"
    position_sort REAL,
    PRIMARY KEY (series_id, book_id)
);
CREATE INDEX idx_book_series_links_book ON book_series_links(book_id);

-- One file record per book and format. Multi-file audiobooks: relative_path is the folder,
-- parts lists the files ([{relative_path,size,duration_ms}]), all relative to the author's path/audio_path.
CREATE TABLE book_files (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id             INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format              TEXT    NOT NULL CHECK (format IN ('ebook', 'audiobook')),
    relative_path       TEXT    NOT NULL,
    parts               TEXT    NOT NULL DEFAULT '[]',
    size                INTEGER NOT NULL DEFAULT 0,
    quality             TEXT    NOT NULL,                      -- QualityModel JSON
    bitrate_kbps        INTEGER,
    duration_seconds    INTEGER,
    narrator            TEXT,
    edition_id          INTEGER REFERENCES book_editions(id) ON DELETE SET NULL,
    release_group       TEXT,
    scene_name          TEXT,
    original_path       TEXT,
    custom_format_ids   TEXT    NOT NULL DEFAULT '[]',
    custom_format_score INTEGER NOT NULL DEFAULT 0,
    date_added          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (book_id, format)
);
