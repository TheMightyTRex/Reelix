-- A series' episodes in aired (TVDB and broadcast) order, mapped to TMDB's numbers (RFX-104).
-- Taken from the TMDB episode group the metadata refresh picks; kept only when its numbers
-- differ from TMDB's. The import reads it for a file whose numbers TMDB's own cannot place.
-- `entries` is a JSON array of {season, episode, tmdb_season, tmdb_episode}.
CREATE TABLE series_episode_map (
    series_id    INTEGER PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
    group_id     TEXT NOT NULL,
    group_name   TEXT NOT NULL,
    entries      TEXT NOT NULL,
    refreshed_at INTEGER NOT NULL
);
