//! A reading-list export the person gives Reelix: the CSV file Goodreads ("Export library") or
//! StoryGraph ("Export your data") makes, or any CSV with Title, Author and ISBN columns. Read
//! from a file on the server or an `https` address, so nothing signs in on the person's behalf
//! (StoryGraph has no public API or feed).

use std::collections::HashMap;

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};

use crate::common::{self, Settings};
use crate::{
    BookListItem, BuildContext, ImportList, ListContent, ListError, ListItem, clean_isbn, dedupe_books,
};

pub const IMPLEMENTATION: &str = "book_export";
/// Largest export read.
const MAX_BYTES: usize = 20 * 1024 * 1024;
/// Most rows read from one export.
const MAX_ROWS: usize = 20_000;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Goodreads or StoryGraph export".into(),
        description: Some(
            "Books from the CSV export Goodreads or StoryGraph gives you, or any CSV with Title, Author and ISBN columns."
                .into(),
        ),
        info_url: Some("https://www.goodreads.com/review/import".into()),
        fields: vec![
            FieldSchema::new("path", "File on the server", FieldKind::Text)
                .placeholder("/config/exports/goodreads_library_export.csv")
                .help("Where Reelix can read the export: a full path ending in .csv. Leave empty to use an address."),
            FieldSchema::new("url", "Address", FieldKind::Url)
                .help("Or an https address the export can be downloaded from."),
            FieldSchema::new("shelf", "Shelf", FieldKind::Text)
                .default_value("to-read")
                .help("Only books on this shelf or with this read status (to-read, currently-reading, read). Leave empty for every book."),
            common::sync_interval_field(),
        ],
    }
}

#[derive(Debug, Clone)]
enum Source {
    Path(String),
    Url(String),
}

/// A configured export.
#[derive(Debug, Clone)]
pub struct BookExport {
    http: reqwest::Client,
    source: Source,
    shelf: Option<String>,
}

impl BookExport {
    pub fn from_settings(v: &serde_json::Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), v)?;
        let source = match (s.str("path"), s.str("url")) {
            (Some(p), _) => {
                let path = std::path::Path::new(&p);
                let csv = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv"));
                if !path.is_absolute() || !csv {
                    return Err(ListError::InvalidSettings(
                        "path: a full path to a .csv file is required".into(),
                    ));
                }
                Source::Path(p)
            }
            (None, Some(u)) if u.starts_with("https://") || u.starts_with("http://") => Source::Url(u),
            (None, Some(_)) => {
                return Err(ListError::InvalidSettings(
                    "url must start with https:// or http://".into(),
                ));
            }
            (None, None) => {
                return Err(ListError::InvalidSettings(
                    "a path or an address is required".into(),
                ));
            }
        };
        Ok(Self {
            http: ctx.http.clone(),
            source,
            shelf: s.str("shelf").map(|x| x.to_lowercase()),
        })
    }

    async fn read(&self) -> Result<String, ListError> {
        let bytes = match &self.source {
            Source::Path(p) => {
                let meta = tokio::fs::metadata(p)
                    .await
                    .map_err(|e| ListError::NotFound(format!("{p}: {e}")))?;
                if meta.len() > MAX_BYTES as u64 {
                    return Err(ListError::Parse("the export is larger than 20 MB".into()));
                }
                tokio::fs::read(p)
                    .await
                    .map_err(|e| ListError::NotFound(format!("{p}: {e}")))?
            }
            Source::Url(u) => common::read_limited(common::send(self.http.get(u)).await?, MAX_BYTES).await?,
        };
        let text =
            String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Ok(text.trim_start_matches('\u{feff}').to_string())
    }
}

/// Splits CSV text into rows (RFC 4180: quoted fields, doubled quotes, line breaks in quotes).
pub fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
                if rows.len() > MAX_ROWS {
                    return rows;
                }
            }
            _ => field.push(c),
        }
    }
    row.push(field);
    if row.iter().any(|f| !f.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

/// The books of an export, filtered to `shelf` when the export has shelves.
pub fn parse_export(text: &str, shelf: Option<&str>) -> Result<Vec<BookListItem>, ListError> {
    let mut rows = csv_rows(text).into_iter();
    let header = rows
        .next()
        .ok_or_else(|| ListError::Parse("the export is empty".into()))?;
    let col: HashMap<String, usize> = header
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_lowercase(), i))
        .collect();
    let find = |names: &[&str]| names.iter().find_map(|n| col.get(*n).copied());
    let title_col = find(&["title", "book title"])
        .ok_or_else(|| ListError::Parse("the export has no Title column".into()))?;
    let author_col = find(&["author", "authors", "author name"]);
    let extra_authors = find(&["additional authors"]);
    let isbn13_col = find(&["isbn13", "isbn 13"]);
    let isbn_col = find(&["isbn", "isbn/uid", "isbn10"]);
    let asin_col = find(&["asin"]);
    let id_col = find(&["book id", "isbn/uid"]);
    let year_col = find(&["original publication year", "year published", "year"]);
    // Goodreads: Exclusive Shelf and Bookshelves; StoryGraph: Read Status; others: Shelf.
    let shelf_cols: Vec<usize> = ["exclusive shelf", "bookshelves", "read status", "shelf"]
        .iter()
        .filter_map(|n| col.get(*n).copied())
        .collect();
    let shelf = shelf.map(str::trim).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    for r in rows.take(MAX_ROWS) {
        let get = |i: Option<usize>| {
            i.and_then(|i| r.get(i))
                .map(|v| {
                    v.trim()
                        .trim_start_matches('=')
                        .trim_matches('"')
                        .trim()
                        .to_string()
                })
                .filter(|v| !v.is_empty())
        };
        let Some(title) = get(Some(title_col)) else {
            continue;
        };
        if let Some(want) = shelf
            && !shelf_cols.is_empty()
        {
            let on_shelf = shelf_cols.iter().any(|c| {
                r.get(*c).is_some_and(|v| {
                    v.split(',')
                        .map(|s| s.split('(').next().unwrap_or_default().trim().to_lowercase())
                        .any(|s| s == want)
                })
            });
            if !on_shelf {
                continue;
            }
        }
        let mut authors: Vec<String> = Vec::new();
        for a in [get(author_col), get(extra_authors)].into_iter().flatten() {
            authors.extend(
                a.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(|x| x.chars().take(300).collect::<String>()),
            );
        }
        authors.truncate(10);
        let isbn = get(isbn13_col)
            .and_then(|i| clean_isbn(&i))
            .or_else(|| get(isbn_col).and_then(|i| clean_isbn(&i)));
        let id = get(id_col).or_else(|| isbn.clone()).unwrap_or_else(|| {
            format!(
                "{}|{}",
                title.to_lowercase(),
                authors.first().map(|a| a.to_lowercase()).unwrap_or_default()
            )
        });
        out.push(BookListItem {
            title: title.chars().take(600).collect(),
            authors,
            isbn,
            asin: get(asin_col).filter(|a| a.len() == 10),
            year: get(year_col).and_then(|y| y.get(..4).and_then(|y| y.parse().ok())),
            source_id: format!("export:{}", id.chars().take(200).collect::<String>()),
        });
    }
    Ok(out)
}

#[async_trait]
impl ImportList for BookExport {
    fn implementation(&self) -> &'static str {
        IMPLEMENTATION
    }

    fn content(&self) -> ListContent {
        ListContent::Books
    }

    async fn fetch(&self) -> Result<Vec<ListItem>, ListError> {
        Ok(Vec::new())
    }

    async fn fetch_books(&self) -> Result<Vec<BookListItem>, ListError> {
        let text = self.read().await?;
        Ok(dedupe_books(parse_export(&text, self.shelf.as_deref())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_and_line_breaks() {
        let rows = csv_rows("a,b,c\r\n\"x, y\",\"say \"\"hi\"\"\",\"two\nlines\"\n\n1,2,3");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["x, y", "say \"hi\"", "two\nlines"]);
        assert_eq!(rows[2], vec!["1", "2", "3"]);
    }

    #[test]
    fn goodreads_export() {
        let csv = "Book Id,Title,Author,Author l-f,Additional Authors,ISBN,ISBN13,My Rating,Year Published,Original Publication Year,Bookshelves,Exclusive Shelf\n\
            234225,Dune,Frank Herbert,\"Herbert, Frank\",,\"=\"\"0441013597\"\"\",\"=\"\"9780441013593\"\"\",0,2005,1965,,to-read\n\
            6969,Emma,Jane Austen,\"Austen, Jane\",,\"=\"\"\"\"\",\"=\"\"\"\"\",5,2003,1815,classics,read\n\
            1,Good Omens,Terry Pratchett,\"Pratchett, Terry\",Neil Gaiman,,,0,2006,1990,\"favourites, to-read (#3)\",read\n";
        let items = parse_export(csv, Some("to-read")).unwrap();
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].isbn.as_deref(), Some("9780441013593"));
        assert_eq!(items[0].year, Some(1965));
        assert_eq!(items[0].source_id, "export:234225");
        assert_eq!(
            items[1].authors,
            vec!["Terry Pratchett".to_string(), "Neil Gaiman".to_string()]
        );
        assert_eq!(parse_export(csv, None).unwrap().len(), 3);
    }

    #[test]
    fn storygraph_export() {
        let csv = "Title,Authors,Contributors,ISBN/UID,Format,Read Status,Date Added\n\
            Project Hail Mary,Andy Weir,Ray Porter (Narrator),9780593135204,audio,to-read,2024/01/02\n\
            Piranesi,Susanna Clarke,,9781635575637,digital,read,2023/05/06\n";
        let items = parse_export(csv, Some("to-read")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Project Hail Mary");
        assert_eq!(items[0].authors, vec!["Andy Weir".to_string()]);
        assert_eq!(items[0].isbn.as_deref(), Some("9780593135204"));
    }

    #[test]
    fn plain_csv_and_bad_files() {
        let items = parse_export("title,author,isbn\nDune,Frank Herbert,\n", Some("to-read")).unwrap();
        assert_eq!(items.len(), 1, "no shelf column: the shelf is ignored");
        assert_eq!(items[0].source_id, "export:dune|frank herbert");
        assert!(parse_export("", None).is_err());
        assert!(parse_export("name,colour\nx,y\n", None).is_err());
    }

    #[test]
    fn settings_need_a_csv_path_or_an_address() {
        let ctx = BuildContext::new(reqwest::Client::new());
        let ok = |v| BookExport::from_settings(&v, &ctx).is_ok();
        assert!(ok(serde_json::json!({"path": "/data/export.csv"})));
        assert!(!ok(serde_json::json!({"path": "relative.csv"})));
        assert!(!ok(serde_json::json!({"path": "/etc/passwd"})));
        assert!(ok(serde_json::json!({"url": "https://example.com/x.csv"})));
        assert!(!ok(serde_json::json!({})));
    }
}
