//! A Goodreads shelf through its public RSS feed (`/review/list_rss/{user}?shelf=`), no sign-in.
//! A private profile's feed needs the `key` from the shelf's RSS link. Reviews, ratings and notes
//! in the feed are never read.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;

use crate::common::{self, Settings};
use crate::{
    BookListItem, BuildContext, ImportList, ListContent, ListError, ListItem, clean_isbn, dedupe_books,
};

pub const IMPLEMENTATION: &str = "goodreads";
/// Pages of the feed read per sync (Goodreads sends up to 100 entries a page).
const MAX_PAGES: u32 = 20;
const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;

pub fn schema() -> ProviderSchema {
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "Goodreads shelf".into(),
        description: Some(
            "Books on a Goodreads shelf, read from the shelf's public RSS feed. No sign-in.".into(),
        ),
        info_url: Some("https://www.goodreads.com/review/list".into()),
        fields: vec![
            FieldSchema::new("user_id", "Goodreads user", FieldKind::Text)
                .required()
                .placeholder("12345678")
                .help("Your Goodreads user number, or the address of your profile or shelf."),
            FieldSchema::new("shelf", "Shelf", FieldKind::Text)
                .default_value("to-read")
                .help("The shelf name as Goodreads shows it in the address, for example to-read."),
            FieldSchema::new("key", "Feed key", FieldKind::Secret)
                .help("Only for a private profile: the key= value from the shelf's RSS link."),
            common::base_url_field("https://www.goodreads.com"),
            common::sync_interval_field(),
        ],
    }
}

/// A configured Goodreads shelf.
#[derive(Debug, Clone)]
pub struct GoodreadsShelf {
    http: reqwest::Client,
    base_url: String,
    user_id: String,
    shelf: String,
    key: Option<String>,
}

/// The user number from a number or a Goodreads address (`/user/show/123-name`,
/// `/review/list/123`, `/review/list_rss/123`).
pub fn user_id(s: &str) -> Option<String> {
    let s = s.trim();
    let tail = ["/user/show/", "/review/list_rss/", "/review/list/"]
        .iter()
        .find_map(|m| s.split_once(m).map(|(_, r)| r))
        .unwrap_or(s);
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty() && digits.len() <= 20 && digits != "0").then_some(digits)
}

impl GoodreadsShelf {
    pub fn from_settings(v: &serde_json::Value, ctx: &BuildContext) -> Result<Self, ListError> {
        let s = Settings::validate(&schema(), v)?;
        let user_id = user_id(&s.req("user_id")?).ok_or_else(|| {
            ListError::InvalidSettings("user_id: a Goodreads user number is required".into())
        })?;
        let shelf = s.str("shelf").unwrap_or_else(|| "to-read".into());
        if shelf.len() > 100 || shelf.chars().any(|c| c.is_control() || c == '&' || c == '#') {
            return Err(ListError::InvalidSettings("shelf: not a shelf name".into()));
        }
        Ok(Self {
            http: ctx.http.clone(),
            base_url: s.base_url(&ctx.base_urls.goodreads),
            user_id,
            shelf,
            key: s.str("key"),
        })
    }

    async fn page(&self, page: u32) -> Result<Vec<BookListItem>, ListError> {
        let url = format!("{}/review/list_rss/{}", self.base_url, self.user_id);
        let mut query = vec![("shelf", self.shelf.clone()), ("page", page.to_string())];
        if let Some(k) = &self.key {
            query.push(("key", k.clone()));
        }
        let resp = common::send(self.http.get(&url).query(&query)).await?;
        let body = common::read_limited(resp, MAX_FEED_BYTES).await?;
        parse_feed(&String::from_utf8_lossy(&body))
    }
}

#[derive(Debug, Deserialize)]
struct Rss {
    channel: Channel,
}

#[derive(Debug, Deserialize)]
struct Channel {
    #[serde(default, rename = "item")]
    items: Vec<FeedItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FeedItem {
    title: String,
    book_id: String,
    author_name: String,
    isbn: String,
    isbn13: String,
    book_published: String,
}

/// The entries of one feed page.
pub fn parse_feed(xml: &str) -> Result<Vec<BookListItem>, ListError> {
    let rss: Rss = quick_xml::de::from_str(xml)
        .map_err(|e| ListError::Parse(format!("not a Goodreads shelf feed: {e}")))?;
    Ok(rss
        .channel
        .items
        .into_iter()
        .filter_map(|i| {
            let title: String = i.title.trim().chars().take(600).collect();
            let id = i.book_id.trim();
            if title.is_empty() || id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let author = i.author_name.trim();
            Some(BookListItem {
                title,
                authors: (!author.is_empty())
                    .then(|| author.chars().take(300).collect())
                    .into_iter()
                    .collect(),
                isbn: clean_isbn(&i.isbn13).or_else(|| clean_isbn(&i.isbn)),
                asin: None,
                year: i.book_published.trim().get(..4).and_then(|y| y.parse().ok()),
                source_id: format!("goodreads:{id}"),
            })
        })
        .collect())
}

#[async_trait]
impl ImportList for GoodreadsShelf {
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
        let mut out: Vec<BookListItem> = Vec::new();
        for page in 1..=MAX_PAGES {
            let items = self.page(page).await?;
            let before = out.len();
            let n = items.len();
            out.extend(items);
            out = dedupe_books(out);
            // An empty page, or one that only repeats earlier entries, is the end.
            if n == 0 || out.len() == before {
                break;
            }
        }
        Ok(out)
    }

    async fn test(&self) -> Result<(), ListError> {
        self.page(1).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids() {
        assert_eq!(user_id("12345").as_deref(), Some("12345"));
        assert_eq!(
            user_id("https://www.goodreads.com/user/show/12345-some-reader").as_deref(),
            Some("12345")
        );
        assert_eq!(
            user_id("https://www.goodreads.com/review/list_rss/987?shelf=to-read").as_deref(),
            Some("987")
        );
        assert_eq!(user_id("someone"), None);
    }

    #[test]
    fn parses_a_feed_page() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>x</title>
            <item><guid>a</guid><title><![CDATA[Dune & Friends]]></title><book_id>234225</book_id>
              <book><num_pages>604</num_pages></book><author_name>Frank Herbert</author_name>
              <isbn>0441013597</isbn><user_review>secret</user_review><book_published>1965</book_published>
              <description><![CDATA[<a href="x">cover</a>]]></description></item>
            <item><title>No id</title></item>
            <item><title>Emma</title><book_id>6969</book_id><author_name>Jane Austen</author_name><isbn></isbn></item>
          </channel></rss>"#;
        let items = parse_feed(xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Dune & Friends");
        assert_eq!(items[0].isbn.as_deref(), Some("0441013597"));
        assert_eq!(items[0].year, Some(1965));
        assert_eq!(items[0].source_id, "goodreads:234225");
        assert_eq!(items[1].isbn, None);
        assert!(parse_feed("<html>nope</html>").is_err());
    }
}
