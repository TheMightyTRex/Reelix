//! Book import lists (RFX-88): Goodreads shelves, Hardcover shelves and lists, and Goodreads or
//! StoryGraph exports. Each entry is matched to an Open Library work and added through the books
//! service as an ebook, an audiobook or both, as the list says.
//!
//! Matching: the ISBN first (`isbn:` lookup); otherwise a work search for title and first author,
//! taking the first hit whose title (subtitle, punctuation and a leading article ignored) equals
//! the entry's and whose author shares a surname with it. Anything else counts as unresolved and
//! is tried again on the next sync.
//!
//! Each list remembers the entries it has handled (`book_list_items`), so an entry is added once:
//! a book removed from the library later is not added back by the same list, and handled entries
//! cost no lookups on later syncs.

use std::collections::HashSet;

use reelix_api_types::books::{AddBookRequest, BookLookupDto};
use reelix_books::{BooksError, BooksService};
use reelix_core::{BookFormat, ContentKind};
use reelix_db::{Db, now_ms};
use reelix_lists::BookListItem;
use reelix_parser::QualityFamily;
use reelix_providers::ProviderRow;
use serde_json::{Map, Value};

use super::lists::ListSyncOutcome;

/// Why an add did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// Already in the library.
    Exists,
    Failed(String),
}

/// Ebook and audiobook defaults used when a list leaves a target empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BookDefaults {
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub audio_quality_profile_id: Option<i64>,
    pub audio_root_folder_id: Option<i64>,
}

/// The slice of the books library the list sync needs ([`BooksService`] in production).
#[async_trait::async_trait]
pub trait BookLibrary: Send + Sync {
    /// Open Library work search (`isbn:…` looks up one ISBN), with library flags.
    async fn lookup(&self, term: &str) -> Result<Vec<BookLookupDto>, String>;
    async fn defaults(&self) -> Result<BookDefaults, String>;
    /// Returns the added book's title.
    async fn add_book(&self, req: AddBookRequest) -> Result<String, AddError>;
}

#[async_trait::async_trait]
impl BookLibrary for BooksService {
    async fn lookup(&self, term: &str) -> Result<Vec<BookLookupDto>, String> {
        self.lookup_books(term).await.map_err(|e| e.to_string())
    }

    async fn defaults(&self) -> Result<BookDefaults, String> {
        let profiles = |f| async move {
            self.db()
                .quality_profiles()
                .list_family(f)
                .await
                .map(|p| p.first().map(|p| p.id))
                .map_err(|e| e.to_string())
        };
        let roots = self.root_folders().await.map_err(|e| e.to_string())?;
        let root = |k: ContentKind| roots.iter().find(|r| r.kind == k).map(|r| r.id);
        Ok(BookDefaults {
            quality_profile_id: profiles(QualityFamily::Ebook).await?,
            root_folder_id: root(ContentKind::Ebook),
            audio_quality_profile_id: profiles(QualityFamily::Audiobook).await?,
            audio_root_folder_id: root(ContentKind::Audiobook),
        })
    }

    async fn add_book(&self, req: AddBookRequest) -> Result<String, AddError> {
        BooksService::add_book(self, req)
            .await
            .map(|b| b.title)
            .map_err(|e| match e {
                BooksError::Conflict(_) => AddError::Exists,
                e => AddError::Failed(e.to_string()),
            })
    }
}

/// Per-list add options of a book list (fields added by the providers crate).
#[derive(Debug, Clone, PartialEq)]
pub struct BookListOptions {
    pub enabled_auto_add: bool,
    pub formats: Vec<BookFormat>,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub audio_quality_profile_id: Option<i64>,
    pub audio_root_folder_id: Option<i64>,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
}

fn id_setting(s: &Map<String, Value>, key: &str) -> Option<i64> {
    match s.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
    .filter(|id| *id > 0)
}

impl BookListOptions {
    pub fn from_row(row: &ProviderRow) -> Self {
        let s = &row.settings;
        let formats = match s.get("formats").and_then(Value::as_str).unwrap_or("both") {
            "ebook" => vec![BookFormat::Ebook],
            "audiobook" => vec![BookFormat::Audiobook],
            _ => BookFormat::ALL.to_vec(),
        };
        Self {
            enabled_auto_add: row.setting_bool("enabled_auto_add", true),
            formats,
            quality_profile_id: id_setting(s, "quality_profile_id"),
            root_folder_id: id_setting(s, "root_folder_id"),
            audio_quality_profile_id: id_setting(s, "audio_quality_profile_id"),
            audio_root_folder_id: id_setting(s, "audio_root_folder_id"),
            search_on_add: row.setting_bool("search_on_add", true),
            tags: row.tags.clone(),
        }
    }
}

// ---------------------------------------------------------------- handled entries

async fn handled(db: &Db, list_id: i64) -> sqlx::Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT source_id FROM book_list_items WHERE list_id = ?")
        .bind(list_id)
        .fetch_all(db.pool())
        .await?;
    Ok(rows.into_iter().collect())
}

async fn remember(db: &Db, list_id: i64, source_id: &str, work_id: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO book_list_items (list_id, source_id, openlibrary_work_id, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (list_id, source_id) DO NOTHING",
    )
    .bind(list_id)
    .bind(source_id)
    .bind(work_id)
    .bind(now_ms())
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Forgets what a list has handled, so its next sync looks at every entry again. (Deleting the
/// list removes its rows by itself.)
pub async fn forget_list(db: &Db, list_id: i64) -> sqlx::Result<u64> {
    Ok(sqlx::query("DELETE FROM book_list_items WHERE list_id = ?")
        .bind(list_id)
        .execute(db.pool())
        .await?
        .rows_affected())
}

// ---------------------------------------------------------------- matching

/// Lower-case words without punctuation, a leading article dropped, the subtitle cut off.
pub fn normalize_title(s: &str) -> String {
    let main = s.split([':', '(', '[']).next().unwrap_or(s);
    let words: Vec<String> = main
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let skip = usize::from(words.len() > 1 && matches!(words[0].as_str(), "the" | "a" | "an"));
    words[skip..].join(" ")
}

fn surname(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .rfind(|w| !w.is_empty())
        .unwrap_or_default()
        .to_lowercase()
}

/// The Open Library work of a list entry.
async fn resolve(books: &dyn BookLibrary, item: &BookListItem) -> Option<BookLookupDto> {
    if let Some(isbn) = &item.isbn
        && let Ok(hits) = books.lookup(&format!("isbn:{isbn}")).await
        && let Some(hit) = hits.into_iter().next()
    {
        return Some(hit);
    }
    let title = item.title.trim();
    if title.is_empty() {
        return None;
    }
    let want = normalize_title(title);
    let surnames: Vec<String> = item
        .authors
        .iter()
        .map(|a| surname(a))
        .filter(|s| !s.is_empty())
        .collect();
    let term = match item.authors.first() {
        Some(a) => format!("{title} {a}"),
        None => title.to_string(),
    };
    let hits = books.lookup(&term).await.ok()?;
    hits.into_iter().find(|h| {
        normalize_title(&h.title) == want
            && (surnames.is_empty()
                || h.author_name
                    .as_deref()
                    .is_some_and(|a| surnames.contains(&surname(a))))
    })
}

/// Adds the entries of a book list through `books`, updating `o`.
pub async fn add_book_items(
    db: &Db,
    books: &dyn BookLibrary,
    row: &ProviderRow,
    opts: &BookListOptions,
    items: Vec<BookListItem>,
    o: &mut ListSyncOutcome,
) -> sqlx::Result<()> {
    let done = handled(db, row.id).await?;
    let defaults = match books.defaults().await {
        Ok(d) => d,
        Err(e) => {
            o.error = Some(e);
            return Ok(());
        }
    };
    let wants = |f: BookFormat| opts.formats.contains(&f);
    let req_base = AddBookRequest {
        openlibrary_work_id: String::new(),
        formats: opts.formats.clone(),
        quality_profile_id: opts.quality_profile_id.or(defaults.quality_profile_id),
        root_folder_id: opts.root_folder_id.or(defaults.root_folder_id),
        audio_quality_profile_id: opts
            .audio_quality_profile_id
            .or(defaults.audio_quality_profile_id),
        audio_root_folder_id: opts.audio_root_folder_id.or(defaults.audio_root_folder_id),
        tags: opts.tags.clone(),
        search_on_add: opts.search_on_add,
        added_by_request_id: None,
    };
    let ebook_ready = req_base.quality_profile_id.is_some() && req_base.root_folder_id.is_some();
    let audio_ready = req_base.audio_quality_profile_id.is_some() && req_base.audio_root_folder_id.is_some();
    if !((wants(BookFormat::Ebook) && ebook_ready) || (wants(BookFormat::Audiobook) && audio_ready)) {
        let msg = "cannot add books: no book quality profile or book root folder configured".to_string();
        tracing::warn!(list = %row.name, "{msg}");
        o.error = Some(msg);
        return Ok(());
    }
    for item in items {
        if done.contains(&item.source_id) {
            o.existing += 1;
            continue;
        }
        let Some(hit) = resolve(books, &item).await else {
            tracing::debug!(list = %row.name, book = %item.label(), "book list entry not found on Open Library");
            o.unresolved += 1;
            continue;
        };
        if hit.book_id.is_some() {
            o.existing += 1;
            remember(db, row.id, &item.source_id, &hit.openlibrary_work_id).await?;
            continue;
        }
        let req = AddBookRequest {
            openlibrary_work_id: hit.openlibrary_work_id.clone(),
            ..req_base.clone()
        };
        match books.add_book(req).await {
            Ok(title) => {
                o.added += 1;
                tracing::info!(list = %row.name, %title, work = %hit.openlibrary_work_id, "added from book list");
            }
            Err(AddError::Exists) => o.existing += 1,
            Err(AddError::Failed(e)) => {
                tracing::warn!(list = %row.name, book = %item.label(), error = %e, "book list add failed");
                continue;
            }
        }
        remember(db, row.id, &item.source_id, &hit.openlibrary_work_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn hit(work: &str, title: &str, author: &str, book_id: Option<i64>) -> BookLookupDto {
        BookLookupDto {
            openlibrary_work_id: work.into(),
            title: title.into(),
            author_name: Some(author.into()),
            author_openlibrary_id: None,
            year: None,
            cover: None,
            isbn13: None,
            book_id,
        }
    }

    #[derive(Default)]
    struct FakeBooks {
        added: Mutex<Vec<AddBookRequest>>,
        lookups: Mutex<Vec<String>>,
        no_defaults: bool,
    }

    #[async_trait::async_trait]
    impl BookLibrary for FakeBooks {
        async fn lookup(&self, term: &str) -> Result<Vec<BookLookupDto>, String> {
            self.lookups.lock().unwrap().push(term.to_string());
            let added: Vec<String> = self
                .added
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.openlibrary_work_id.clone())
                .collect();
            let flag = |w: &str| added.iter().any(|a| a == w).then_some(1);
            Ok(match term {
                "isbn:9780441013593" => vec![hit("OL893415W", "Dune", "Frank Herbert", flag("OL893415W"))],
                t if t.starts_with("Emma") => vec![
                    hit("OL1W", "Emma Watson: A Life", "Someone Else", None),
                    hit("OL66562W", "Emma", "Jane Austen", flag("OL66562W")),
                ],
                t if t.starts_with("The Hobbit") => {
                    vec![hit("OL27448W", "The Hobbit", "J.R.R. Tolkien", Some(7))]
                }
                _ => vec![hit("OL9W", "Something Else", "Nobody", None)],
            })
        }
        async fn defaults(&self) -> Result<BookDefaults, String> {
            if self.no_defaults {
                return Ok(BookDefaults::default());
            }
            Ok(BookDefaults {
                quality_profile_id: Some(1),
                root_folder_id: Some(2),
                audio_quality_profile_id: Some(3),
                audio_root_folder_id: Some(4),
            })
        }
        async fn add_book(&self, req: AddBookRequest) -> Result<String, AddError> {
            self.added.lock().unwrap().push(req.clone());
            Ok(req.openlibrary_work_id)
        }
    }

    fn row(settings: Value) -> ProviderRow {
        ProviderRow {
            id: 5,
            kind: reelix_providers::ProviderKind::ImportList,
            implementation: "goodreads".into(),
            name: "Goodreads to-read".into(),
            enabled: true,
            priority: 25,
            tags: vec![8],
            settings: settings.as_object().cloned().unwrap_or_default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn item(title: &str, author: &str, isbn: Option<&str>, id: &str) -> BookListItem {
        BookListItem {
            title: title.into(),
            authors: vec![author.into()],
            isbn: isbn.map(String::from),
            asin: None,
            year: None,
            source_id: id.into(),
        }
    }

    #[test]
    fn titles_normalise() {
        assert_eq!(normalize_title("The Hobbit: or There and Back Again"), "hobbit");
        assert_eq!(normalize_title("Emma"), "emma");
        assert_eq!(surname("J.R.R. Tolkien"), "tolkien");
    }

    #[test]
    fn options_from_settings() {
        let o = BookListOptions::from_row(&row(
            serde_json::json!({"formats": "audiobook", "audio_quality_profile_id": "9"}),
        ));
        assert_eq!(o.formats, vec![BookFormat::Audiobook]);
        assert_eq!(o.audio_quality_profile_id, Some(9));
        assert!(o.search_on_add);
        assert_eq!(o.tags, vec![8]);
        let o = BookListOptions::from_row(&row(serde_json::json!({})));
        assert_eq!(o.formats, BookFormat::ALL.to_vec());
    }

    async fn db_with_list() -> Db {
        let db = Db::in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, kind, implementation, name, created_at, updated_at)
             VALUES (5, 'import_list', 'goodreads', 'Goodreads to-read', 0, 0)",
        )
        .execute(db.pool())
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn adds_each_entry_once() {
        let db = db_with_list().await;
        let books = FakeBooks::default();
        let r = row(serde_json::json!({"formats": "ebook", "search_on_add": false}));
        let opts = BookListOptions::from_row(&r);
        let items = vec![
            item("Dune", "Frank Herbert", Some("9780441013593"), "goodreads:1"),
            item("Emma", "Jane Austen", None, "goodreads:2"),
            item("The Hobbit", "J. R. R. Tolkien", None, "goodreads:3"),
            item("Unknown Book", "Nobody Known", None, "goodreads:4"),
        ];
        let mut o = ListSyncOutcome::default();
        add_book_items(&db, &books, &r, &opts, items.clone(), &mut o)
            .await
            .unwrap();
        assert_eq!((o.added, o.existing, o.unresolved), (2, 1, 1), "{o:?}");
        let added = books.added.lock().unwrap().clone();
        assert_eq!(added[0].openlibrary_work_id, "OL893415W");
        assert_eq!(
            added[1].openlibrary_work_id, "OL66562W",
            "the title must match, not the first hit"
        );
        assert_eq!(added[0].formats, vec![BookFormat::Ebook]);
        assert_eq!(added[0].quality_profile_id, Some(1));
        assert_eq!(added[0].audio_root_folder_id, Some(4));
        assert_eq!(added[0].tags, vec![8]);
        assert!(!added[0].search_on_add);

        // Second sync: handled entries cost no lookups; the unresolved one is tried again.
        books.lookups.lock().unwrap().clear();
        let mut o = ListSyncOutcome::default();
        add_book_items(&db, &books, &r, &opts, items, &mut o)
            .await
            .unwrap();
        assert_eq!((o.added, o.existing, o.unresolved), (0, 3, 1));
        assert_eq!(books.lookups.lock().unwrap().len(), 1);
        assert_eq!(forget_list(&db, 5).await.unwrap(), 3);
        add_book_items(
            &db,
            &books,
            &r,
            &opts,
            vec![item(
                "Dune",
                "Frank Herbert",
                Some("9780441013593"),
                "goodreads:1",
            )],
            &mut o,
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM providers WHERE id = 5")
            .execute(db.pool())
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_list_items")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(left, 0, "rows go with their list");
    }

    #[tokio::test]
    async fn says_when_nothing_can_be_added() {
        let db = db_with_list().await;
        let books = FakeBooks {
            no_defaults: true,
            ..FakeBooks::default()
        };
        let r = row(serde_json::json!({}));
        let mut o = ListSyncOutcome::default();
        add_book_items(
            &db,
            &books,
            &r,
            &BookListOptions::from_row(&r),
            vec![item("Dune", "Frank Herbert", None, "x")],
            &mut o,
        )
        .await
        .unwrap();
        assert!(o.error.unwrap().contains("no book quality profile"));
        assert!(books.added.lock().unwrap().is_empty());
    }
}
