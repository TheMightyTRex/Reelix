-- Optional minimum / maximum release size (MB, per episode) on quality profiles.
ALTER TABLE quality_profiles ADD COLUMN min_size_mb INTEGER;
ALTER TABLE quality_profiles ADD COLUMN max_size_mb INTEGER;
