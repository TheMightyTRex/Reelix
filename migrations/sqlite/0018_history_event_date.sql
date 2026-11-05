-- Filtered history pages (Activity, History, by event type) and their counts read the history table in
-- date order for one event type. Without this index each filtered page scans the whole table, which grows
-- without limit.
CREATE INDEX IF NOT EXISTS idx_history_event_date ON history(event_type, date);
