CREATE TABLE direct_downloads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request TEXT NOT NULL,
    dedup_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued',
    progress REAL,
    message TEXT NOT NULL DEFAULT '',
    output_dir TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX direct_download_dedup ON direct_downloads(dedup_key)
    WHERE state IN ('queued', 'running', 'completed');
