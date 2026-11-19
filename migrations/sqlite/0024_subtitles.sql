-- Subtitles fetched by Reelix (fork issue #62, RLX-09). One row per video file, language and
-- variant (normal, sdh or forced). `media_kind` is `movie` (file in movie_files) or `episode`
-- (file in episode_files); rows go with their file. `match_level` is `hash` (made for this file),
-- `id` (same film or episode) or `release` (release name only), best first.
CREATE TABLE subtitle_files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    media_kind   TEXT    NOT NULL CHECK (media_kind IN ('movie', 'episode')),
    file_id      INTEGER NOT NULL,
    language     TEXT    NOT NULL,
    variant      TEXT    NOT NULL CHECK (variant IN ('normal', 'sdh', 'forced')),
    path         TEXT    NOT NULL,
    source       TEXT    NOT NULL,
    source_id    TEXT    NOT NULL,
    match_level  TEXT    NOT NULL CHECK (match_level IN ('hash', 'id', 'release')),
    release_name TEXT,
    fetched_at   INTEGER NOT NULL,
    UNIQUE (media_kind, file_id, language, variant)
);
CREATE INDEX idx_subtitle_files_file ON subtitle_files(media_kind, file_id);

-- When each video file was last searched and when the schedule may ask the sources again:
-- a day while a subtitle is missing, a week while only a better match could come, a month when
-- every subtitle is made for the file. Cleared when the languages in the settings change.
CREATE TABLE subtitle_searches (
    media_kind       TEXT    NOT NULL,
    file_id          INTEGER NOT NULL,
    last_searched_at INTEGER NOT NULL,
    next_search_at   INTEGER NOT NULL,
    PRIMARY KEY (media_kind, file_id)
);
CREATE INDEX idx_subtitle_searches_next ON subtitle_searches(next_search_at);

CREATE TRIGGER trg_subtitles_movie_file_deleted AFTER DELETE ON movie_files BEGIN
    DELETE FROM subtitle_files WHERE media_kind = 'movie' AND file_id = OLD.id;
    DELETE FROM subtitle_searches WHERE media_kind = 'movie' AND file_id = OLD.id;
END;
CREATE TRIGGER trg_subtitles_episode_file_deleted AFTER DELETE ON episode_files BEGIN
    DELETE FROM subtitle_files WHERE media_kind = 'episode' AND file_id = OLD.id;
    DELETE FROM subtitle_searches WHERE media_kind = 'episode' AND file_id = OLD.id;
END;
