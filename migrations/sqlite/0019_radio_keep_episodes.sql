-- Radio shows can keep only the newest N episodes (RFX-106). NULL keeps every episode; with a
-- number, the files of older episodes are deleted from the podcast library folder and those
-- episodes are unmonitored so they are not fetched again.
ALTER TABLE radio_shows ADD COLUMN keep_episodes INTEGER;
