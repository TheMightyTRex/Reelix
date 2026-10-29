-- Reelix providers: indexers, download clients, notifications, media servers and import lists,
-- plus indexer health, tags and remote path mappings. Timestamps are INTEGER unix ms.

CREATE TABLE providers (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    kind           TEXT    NOT NULL
                   CHECK (kind IN ('indexer', 'download_client', 'notification', 'media_server', 'import_list')),
    implementation TEXT    NOT NULL,
    name           TEXT    NOT NULL COLLATE NOCASE,
    enabled        INTEGER NOT NULL DEFAULT 1,
    priority       INTEGER NOT NULL DEFAULT 25,
    -- JSON array of tag ids.
    tags           TEXT    NOT NULL DEFAULT '[]',
    -- JSON object; every Secret field is stored encrypted (`enc:` + SecretBox output).
    settings       TEXT    NOT NULL DEFAULT '{}',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    UNIQUE (kind, name)
);
CREATE INDEX idx_providers_kind ON providers(kind, priority, id);

-- Persisted IndexerManager back-off state.
CREATE TABLE indexer_status (
    provider_id         INTEGER PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    failure_count       INTEGER NOT NULL DEFAULT 0,
    disabled_until      INTEGER,
    last_failure_reason TEXT,
    updated_at          INTEGER NOT NULL
);

CREATE TABLE tags (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    label      TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE remote_path_mappings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    host        TEXT    NOT NULL COLLATE NOCASE,
    remote_path TEXT    NOT NULL,
    local_path  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (host, remote_path)
);
