-- Builds between 0010 and the book/music quality defaults seeded nothing for the new families:
-- they claimed the one-time seed markers without inserting profiles, and wrote placeholder
-- definitions with no size limits. Undo both so the real defaults are seeded on next access.
-- Video rows (quality ids < 100) and families that already have profiles are untouched.

DELETE FROM quality_definitions
WHERE quality_id >= 100
  AND min_size = 0
  AND preferred_size IS NULL
  AND max_size IS NULL;

DELETE FROM settings
WHERE key IN ('seeded.quality_profiles.ebook',
              'seeded.quality_profiles.audiobook',
              'seeded.quality_profiles.music')
  AND NOT EXISTS (
      SELECT 1 FROM quality_profiles
      WHERE quality_profiles.media_kind = substr(settings.key, length('seeded.quality_profiles.') + 1)
  );
