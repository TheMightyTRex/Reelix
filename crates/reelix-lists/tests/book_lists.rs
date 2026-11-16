//! Book list adapters (Goodreads shelf feed, Hardcover, exports) against wiremock and temp
//! files (no real network).

use reelix_lists::{BuildContext, ImportList, ListContent, ListError, build, content_of};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, BuildContext) {
    let server = MockServer::start().await;
    let ctx = BuildContext::new(reqwest::Client::new()).with_all_base_urls(&server.uri());
    (server, ctx)
}

fn list(ctx: &BuildContext, imp: &str, settings: Value) -> Box<dyn ImportList> {
    build(imp, &settings, ctx).unwrap_or_else(|e| panic!("build {imp}: {e}"))
}

fn feed(items: &[(&str, &str)]) -> String {
    let body: String = items
        .iter()
        .map(|(id, title)| {
            format!("<item><title>{title}</title><book_id>{id}</book_id><author_name>Frank Herbert</author_name><isbn></isbn></item>")
        })
        .collect();
    format!("<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>shelf</title>{body}</channel></rss>")
}

#[test]
fn book_lists_declare_book_content() {
    let ctx = BuildContext::new(reqwest::Client::new());
    for (imp, s) in [
        ("goodreads", json!({"user_id": "123"})),
        ("hardcover", json!({"token": "t", "shelf": "read"})),
        ("book_export", json!({"url": "https://example.com/export.csv"})),
    ] {
        assert_eq!(content_of(imp), ListContent::Books);
        let l = list(&ctx, imp, s);
        assert_eq!(l.content(), ListContent::Books, "{imp}");
    }
    assert!(matches!(
        build("goodreads", &json!({"user_id": "nobody"}), &ctx),
        Err(ListError::InvalidSettings(_))
    ));
    assert!(matches!(
        build("hardcover", &json!({"token": "t", "shelf": "list"}), &ctx),
        Err(ListError::InvalidSettings(_))
    ));
}

#[tokio::test]
async fn goodreads_reads_pages_until_empty() {
    let (server, ctx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/review/list_rss/123"))
        .and(query_param("shelf", "to-read"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(feed(&[("1", "Dune"), ("2", "Dune Messiah")])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/review/list_rss/123"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(feed(&[("3", "Children of Dune")])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/review/list_rss/123"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_string(feed(&[])))
        .expect(1)
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "goodreads",
        json!({"user_id": "https://www.goodreads.com/user/show/123-reader"}),
    );
    let books = l.fetch_books().await.unwrap();
    let titles: Vec<&str> = books.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["Dune", "Dune Messiah", "Children of Dune"]);
    assert!(l.fetch().await.unwrap().is_empty(), "no films or series");
}

#[tokio::test]
async fn goodreads_private_shelf_is_unauthorised() {
    let (server, ctx) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let l = list(&ctx, "goodreads", json!({"user_id": "5"}));
    assert!(matches!(l.test().await, Err(ListError::Unauthorized)));
}

#[tokio::test]
async fn hardcover_pages_a_shelf_with_the_token() {
    let (server, ctx) = setup().await;
    let row = |id: i64, book: i64, title: &str| {
        json!({"id": id, "book": {"id": book, "title": title, "release_year": 2020,
            "cached_contributors": [{"author": {"name": "Andy Weir"}, "contribution": null}]},
            "edition": {"isbn_13": null, "isbn_10": null, "asin": "B08G9PRS1K"}})
    };
    let first: Vec<Value> = (1..=100)
        .map(|i| row(i, 1000 + i, &format!("Book {i}")))
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/graphql"))
        .and(header("authorization", "Bearer secret-token"))
        .and(body_string_contains("\"after\":0"))
        .and(body_string_contains("\"status\":1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": {"me": [{"user_books": first}]}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/graphql"))
        .and(body_string_contains("\"after\":100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data": {"me": [{"user_books": [row(101, 1001, "Book 1 again"), row(102, 5, "Last")]}]}}),
        ))
        .mount(&server)
        .await;
    let l = list(&ctx, "hardcover", json!({"token": "Bearer secret-token"}));
    let books = l.fetch_books().await.unwrap();
    assert_eq!(books.len(), 101, "the repeated book is dropped");
    assert_eq!(books[100].title, "Last");
    assert_eq!(books[0].asin.as_deref(), Some("B08G9PRS1K"));

    Mock::given(method("POST"))
        .and(path("/v1/graphql"))
        .and(header("authorization", "Bearer wrong"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"errors": [{"message": "Could not verify JWT"}]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let bad = list(&ctx, "hardcover", json!({"token": "wrong"}));
    assert!(matches!(bad.test().await, Err(ListError::Unauthorized)));
}

#[tokio::test]
async fn hardcover_list_by_slug() {
    let (server, ctx) = setup().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"slug\":\"summer\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"me": [{"lists": []}]}})))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "hardcover",
        json!({"token": "t", "shelf": "list", "list_slug": "summer"}),
    );
    assert!(matches!(l.fetch_books().await, Err(ListError::NotFound(_))));
}

#[tokio::test]
async fn export_from_a_file_and_an_address() {
    let dir = std::env::temp_dir().join(format!("reelix-book-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("export.csv");
    std::fs::write(
        &file,
        "\u{feff}Title,Authors,ISBN/UID,Read Status\nPiranesi,Susanna Clarke,9781635575637,to-read\nEmma,Jane Austen,,read\n",
    )
    .unwrap();
    let ctx = BuildContext::new(reqwest::Client::new());
    let l = list(&ctx, "book_export", json!({"path": file.to_string_lossy()}));
    let books = l.fetch_books().await.unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].title, "Piranesi");
    let missing = list(
        &ctx,
        "book_export",
        json!({"path": dir.join("gone.csv").to_string_lossy()}),
    );
    assert!(matches!(missing.fetch_books().await, Err(ListError::NotFound(_))));
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_dir(&dir).unwrap();

    let (server, ctx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/export.csv"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Title,Author\nDune,Frank Herbert\n"))
        .mount(&server)
        .await;
    let l = list(
        &ctx,
        "book_export",
        json!({"url": format!("{}/export.csv", server.uri()), "shelf": ""}),
    );
    assert_eq!(l.fetch_books().await.unwrap().len(), 1);
}
