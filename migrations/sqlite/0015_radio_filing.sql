-- Radio indexer search and filing: pipeline rows can point at a radio episode, and an episode
-- remembers the file Reelix filed for it until the podcast library sync matches it.

ALTER TABLE history ADD COLUMN radio_episode_id INTEGER;
CREATE INDEX idx_history_radio_episode ON history(radio_episode_id, date);

ALTER TABLE blocklist ADD COLUMN radio_episode_id INTEGER;
CREATE INDEX idx_blocklist_radio_episode ON blocklist(radio_episode_id);

ALTER TABLE tracked_downloads ADD COLUMN radio_episode_id INTEGER;

ALTER TABLE pending_releases ADD COLUMN radio_episode_id INTEGER;

-- Path of the file filed into the podcast library (as Reelix sees it) and when; cleared when
-- the episode's matched file goes away. An episode with a filed file that still exists is not
-- searched again.
ALTER TABLE radio_episodes ADD COLUMN filed_path TEXT;
ALTER TABLE radio_episodes ADD COLUMN filed_at INTEGER;
