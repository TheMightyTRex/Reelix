-- Book import lists (RFX-88): entries a list has already handled, so each entry is added once.
-- A book removed from the library afterwards is not added again by the same list. Rows go with
-- their list.
CREATE TABLE IF NOT EXISTS book_list_items (
    list_id             INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    source_id           TEXT    NOT NULL,   -- goodreads:123, hardcover:456, export:<id>
    openlibrary_work_id TEXT    NOT NULL,
    created_at          INTEGER NOT NULL,
    PRIMARY KEY (list_id, source_id)
);
