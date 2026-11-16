-- Items added with the owner's own details and no outside match (RFX-90, `POST /customitems`).
-- `custom = 1` marks them: metadata refreshes leave their typed title, year, overview and
-- picture alone, and scans make their episodes, books and albums from the files on disk.
-- The outside id columns are NOT NULL and UNIQUE, so custom items hold a placeholder there:
-- movies and series a negative TMDB id (`-id`), authors `custom:{id}`, artists `custom:{id}`,
-- radio shows provider `custom` with the Audiobookshelf item id. The API shows TMDB id 0
-- (movies) or none (series) for them.
ALTER TABLE movies ADD COLUMN custom INTEGER NOT NULL DEFAULT 0;
ALTER TABLE series ADD COLUMN custom INTEGER NOT NULL DEFAULT 0;
ALTER TABLE authors ADD COLUMN custom INTEGER NOT NULL DEFAULT 0;
ALTER TABLE artists ADD COLUMN custom INTEGER NOT NULL DEFAULT 0;
ALTER TABLE radio_shows ADD COLUMN custom INTEGER NOT NULL DEFAULT 0;
