-- Soulseek (slskd) as a third download protocol.
--
-- `delay_profiles` is rebuilt so `preferred_protocol` accepts 'soulseek' and gains
-- `enable_soulseek` / `soulseek_delay`. Every other column, default and the AUTOINCREMENT
-- counter are kept (no table has a foreign key pointing at it). The `protocol` columns of the
-- pipeline tables are unconstrained TEXT and take 'soulseek' as is.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE delay_profiles_new (
    id                                  INTEGER PRIMARY KEY AUTOINCREMENT,
    enable_usenet                       INTEGER NOT NULL DEFAULT 1,
    enable_torrent                      INTEGER NOT NULL DEFAULT 1,
    enable_soulseek                     INTEGER NOT NULL DEFAULT 1,
    preferred_protocol                  TEXT    NOT NULL DEFAULT 'usenet'
                                        CHECK (preferred_protocol IN ('usenet', 'torrent', 'soulseek')),
    usenet_delay                        INTEGER NOT NULL DEFAULT 0,
    torrent_delay                       INTEGER NOT NULL DEFAULT 0,
    soulseek_delay                      INTEGER NOT NULL DEFAULT 0,
    bypass_if_highest_quality           INTEGER NOT NULL DEFAULT 1,
    bypass_if_above_custom_format_score INTEGER NOT NULL DEFAULT 0,
    minimum_custom_format_score         INTEGER NOT NULL DEFAULT 0,
    tags                                TEXT    NOT NULL DEFAULT '[]',
    sort_order                          INTEGER NOT NULL DEFAULT 2147483647,
    created_at                          INTEGER NOT NULL,
    updated_at                          INTEGER NOT NULL
);
INSERT INTO delay_profiles_new (id, enable_usenet, enable_torrent, preferred_protocol, usenet_delay, torrent_delay,
        bypass_if_highest_quality, bypass_if_above_custom_format_score, minimum_custom_format_score, tags,
        sort_order, created_at, updated_at)
    SELECT id, enable_usenet, enable_torrent, preferred_protocol, usenet_delay, torrent_delay,
        bypass_if_highest_quality, bypass_if_above_custom_format_score, minimum_custom_format_score, tags,
        sort_order, created_at, updated_at
    FROM delay_profiles;
DELETE FROM sqlite_sequence WHERE name = 'delay_profiles_new';
UPDATE sqlite_sequence SET name = 'delay_profiles_new' WHERE name = 'delay_profiles';
DROP TABLE delay_profiles;
ALTER TABLE delay_profiles_new RENAME TO delay_profiles;
