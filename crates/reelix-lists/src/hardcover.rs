//! Hardcover shelves (Want to read, Currently reading, Read, Did not finish) and the person's own
//! lists, through Hardcover's official GraphQL API with their API token
//! (hardcover.app/account/api). Pages of 100, keyed by row id.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{self, Settings};
use crate::{
    BookListItem, BuildContext, ImportList, ListContent, ListError, ListItem, clean_isbn, dedupe_books,
};

pub const IMPLEMENTATION: &str = "hardcover";
const PAGE: i64 = 100;
/// Most entries read per sync.
const MAX_ENTRIES: usize = 5000;

const SHELVES: &[(&str, &str)] = &[
    ("want_to_read", "Want to read"),
    ("currently_reading", "Currently reading"),
    ("read", "Read"),
    ("did_not_finish", "Did not finish"),
    ("list", "One of my lists"),
];

const BOOK_FIELDS: &str = "id title release_year cached_contributors";
const EDITION_FIELDS: &str = "isbn_13 isbn_10 asin";

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Hardcover".into(),
        description: Some("A Hardcover shelf or one of your Hardcover lists, with your API token.".into()),
        info_url: Some("https://hardcover.app/account/api".into()),
        fields: vec![
            FieldSchema::new("token", "API token", FieldKind::Secret)
                .required()
                .help("From Hardcover's account settings, under API. Kept as a secret."),
            FieldSchema::new("shelf", "Shelf", common::select(SHELVES))
                .required()
                .default_value("want_to_read"),
            FieldSchema::new("list_slug", "List", FieldKind::Text)
                .placeholder("my-summer-reading")
                .help("For One of my lists: the end of the list's address on Hardcover."),
            common::base_url_field("https://api.hardcover.app/v1/graphql"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured Hardcover shelf or list.
#[derive(Debug, Clone)]
pub struct HardcoverList {
    http: reqwest::Client,
    url: String,
    token: String,
    /// Hardcover status id; `None` for a list.
    status: Option<i64>,
    list_slug: Option<String>,
}

fn status_of(shelf: &str) -> Result<Option<i64>, ListError> {
    Ok(match shelf {
        "want_to_read" => Some(1),
        "currently_reading" => Some(2),
        "read" => Some(3),
        "did_not_finish" => Some(5),
        "list" => None,
        o => return Err(ListError::InvalidSettings(format!("unknown shelf {o:?}"))),
    })
}

impl HardcoverList {
    pub fn from_settings(v: &Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), v)?;
        let token = s.req("token")?;
        // Hardcover shows the token with its `Bearer ` prefix.
        let token = token
            .strip_prefix("Bearer ")
            .or_else(|| token.strip_prefix("bearer "))
            .unwrap_or(&token)
            .trim()
            .to_string();
        let status = status_of(&s.str("shelf").unwrap_or_else(|| "want_to_read".into()))?;
        let list_slug = s.str("list_slug");
        if status.is_none() && list_slug.is_none() {
            return Err(ListError::InvalidSettings(
                "list_slug is required for One of my lists".into(),
            ));
        }
        Ok(Self {
            http: ctx.http.clone(),
            url: s.base_url(&ctx.base_urls.hardcover),
            token,
            status,
            list_slug,
        })
    }

    async fn query(&self, query: &str, variables: Value) -> Result<Value, ListError> {
        let req = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }));
        let v: Value = common::get_json(req).await?;
        if let Some(errs) = v.get("errors").and_then(Value::as_array)
            && let Some(first) = errs.first()
        {
            let msg = common::value_str(first.get("message")).unwrap_or_else(|| "Hardcover error".into());
            let lower = msg.to_ascii_lowercase();
            return Err(
                if lower.contains("jwt") || lower.contains("auth") || lower.contains("token") {
                    ListError::Unauthorized
                } else {
                    ListError::Status {
                        status: 200,
                        body: msg,
                    }
                },
            );
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }

    /// One page of rows after row id `after`: `(rows, last id)`.
    async fn page(&self, after: i64) -> Result<(Vec<BookListItem>, Option<i64>), ListError> {
        let (query, vars, path): (String, Value, &[&str]) = match self.status {
            Some(status) => (
                format!(
                    "query Shelf($status: Int!, $after: Int!) {{ me {{ user_books(where: {{status_id: {{_eq: $status}}, id: {{_gt: $after}}}}, order_by: {{id: asc}}, limit: {PAGE}) {{ id book {{ {BOOK_FIELDS} }} edition {{ {EDITION_FIELDS} }} }} }} }}"
                ),
                json!({ "status": status, "after": after }),
                &["me", "user_books"],
            ),
            None => (
                format!(
                    "query List($slug: String!, $after: Int!) {{ me {{ lists(where: {{slug: {{_eq: $slug}}}}, limit: 1) {{ list_books(where: {{id: {{_gt: $after}}}}, order_by: {{id: asc}}, limit: {PAGE}) {{ id book {{ {BOOK_FIELDS} }} edition {{ {EDITION_FIELDS} }} }} }} }} }}"
                ),
                json!({ "slug": self.list_slug, "after": after }),
                &["me", "lists", "list_books"],
            ),
        };
        let data = self.query(&query, vars).await?;
        let rows = dig(&data, path).ok_or_else(|| {
            if self.status.is_none() {
                ListError::NotFound(format!(
                    "Hardcover list {}",
                    self.list_slug.as_deref().unwrap_or("")
                ))
            } else {
                ListError::Parse("Hardcover's answer has no shelf".into())
            }
        })?;
        let last = rows
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_i64))
            .max();
        Ok((
            rows.iter().filter_map(parse_row).collect(),
            last.filter(|_| rows.len() as i64 >= PAGE),
        ))
    }
}

/// Follows `path` through objects, taking the first element of any array on the way (Hardcover
/// returns `me` and `lists` as arrays).
fn dig<'a>(mut v: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    for (i, key) in path.iter().enumerate() {
        if let Some(arr) = v.as_array() {
            v = arr.first()?;
        }
        v = v.get(*key)?;
        if i + 1 == path.len() {
            return v.as_array();
        }
    }
    None
}

/// One `user_books` or `list_books` row.
pub fn parse_row(r: &Value) -> Option<BookListItem> {
    let book = r.get("book")?;
    let id = book.get("id").and_then(Value::as_i64)?;
    let title: String = common::value_str(book.get("title"))?.chars().take(600).collect();
    let authors = book
        .get("cached_contributors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| {
            c.get("contribution")
                .and_then(Value::as_str)
                .is_none_or(|r| r.trim().is_empty() || r.eq_ignore_ascii_case("author"))
        })
        .filter_map(|c| common::value_str(c.get("author").and_then(|a| a.get("name"))))
        .take(10)
        .collect();
    let edition = r.get("edition").filter(|e| e.is_object());
    let field = |k: &str| edition.and_then(|e| common::value_str(e.get(k)));
    Some(BookListItem {
        title,
        authors,
        isbn: field("isbn_13")
            .and_then(|i| clean_isbn(&i))
            .or_else(|| field("isbn_10").and_then(|i| clean_isbn(&i))),
        asin: field("asin"),
        year: common::value_year(book.get("release_year")),
        source_id: format!("hardcover:{id}"),
    })
}

#[async_trait]
impl ImportList for HardcoverList {
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
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            let (items, next) = self.page(after).await?;
            out.extend(items);
            match next {
                Some(n) if n > after && out.len() < MAX_ENTRIES => after = n,
                _ => break,
            }
        }
        out.truncate(MAX_ENTRIES);
        Ok(dedupe_books(out))
    }

    async fn test(&self) -> Result<(), ListError> {
        self.page(0).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows() {
        let r = json!({"id": 9, "book": {"id": 312460, "title": "Project Hail Mary", "release_year": 2021,
            "cached_contributors": [{"author": {"name": "Andy Weir"}, "contribution": null},
                                    {"author": {"name": "Ray Porter"}, "contribution": "Narrator"}]},
            "edition": {"isbn_13": "9780593135204", "isbn_10": null, "asin": null}});
        let b = parse_row(&r).unwrap();
        assert_eq!(b.authors, vec!["Andy Weir".to_string()]);
        assert_eq!(b.isbn.as_deref(), Some("9780593135204"));
        assert_eq!(b.year, Some(2021));
        assert_eq!(b.source_id, "hardcover:312460");
        let no_edition = json!({"book": {"id": 1, "title": "X", "cached_contributors": []}, "edition": null});
        assert_eq!(parse_row(&no_edition).unwrap().isbn, None);
        assert!(parse_row(&json!({"book": {"title": "No id"}})).is_none());
    }

    #[test]
    fn digs_through_arrays() {
        let v = json!({"me": [{"lists": [{"list_books": [{"id": 1}]}]}]});
        assert_eq!(dig(&v, &["me", "lists", "list_books"]).unwrap().len(), 1);
        assert!(dig(&json!({"me": []}), &["me", "user_books"]).is_none());
        assert!(status_of("nope").is_err());
    }
}
