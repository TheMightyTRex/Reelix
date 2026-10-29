//! `book_editions` rows ⇄ [`EditionDto`].

use reelix_api_types::books::EditionDto;
use reelix_api_types::library::ImagesDto;
use reelix_db::{Db, now_ms};
use reelix_metadata::EditionMetadata;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{from_json, json};
use crate::error::Result;

const COLS: &str = "id, book_id, openlibrary_edition_id, title, isbn13, isbn10, asin, publisher, publish_date, \
    language, page_count, physical_format, is_audiobook, images, monitored";

fn map(r: &SqliteRow) -> Result<EditionDto> {
    Ok(EditionDto {
        id: r.try_get("id")?,
        book_id: r.try_get("book_id")?,
        openlibrary_edition_id: r.try_get("openlibrary_edition_id")?,
        title: r.try_get("title")?,
        isbn13: r.try_get("isbn13")?,
        isbn10: r.try_get("isbn10")?,
        asin: r.try_get("asin")?,
        publisher: r.try_get("publisher")?,
        publish_date: r.try_get("publish_date")?,
        language: r.try_get("language")?,
        page_count: r.try_get::<Option<i64>, _>("page_count")?.map(|v| v as u32),
        physical_format: r.try_get("physical_format")?,
        is_audiobook: r.try_get("is_audiobook")?,
        images: from_json(r.try_get("images")?),
        monitored: r.try_get("monitored")?,
    })
}

/// Editions of a book: preferred first, then by publish date.
pub async fn list(db: &Db, book_id: i64) -> Result<Vec<EditionDto>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM book_editions WHERE book_id = ? ORDER BY monitored DESC, publish_date DESC, id"
    ))
    .bind(book_id)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(map).collect()
}

pub async fn count(db: &Db, book_id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM book_editions WHERE book_id = ?")
            .bind(book_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// Inserts or refreshes an edition (keeps its preferred flag). An edition already stored
/// for another book is moved to `book_id`.
pub async fn upsert(db: &Db, book_id: i64, e: &EditionMetadata) -> Result<()> {
    let now = now_ms();
    let images = ImagesDto {
        poster: e.cover_url.clone(),
        backdrop: None,
        logo: None,
    };
    sqlx::query(
        "INSERT INTO book_editions (book_id, openlibrary_edition_id, title, isbn13, isbn10, asin, publisher, \
         publish_date, language, page_count, physical_format, is_audiobook, images, monitored, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?) \
         ON CONFLICT(openlibrary_edition_id) DO UPDATE SET book_id = excluded.book_id, title = excluded.title, \
         isbn13 = excluded.isbn13, isbn10 = excluded.isbn10, asin = excluded.asin, publisher = excluded.publisher, \
         publish_date = excluded.publish_date, language = excluded.language, page_count = excluded.page_count, \
         physical_format = excluded.physical_format, is_audiobook = excluded.is_audiobook, images = excluded.images, \
         updated_at = excluded.updated_at",
    )
    .bind(book_id)
    .bind(&e.id)
    .bind(&e.title)
    .bind(&e.isbn13)
    .bind(&e.isbn10)
    .bind(&e.asin)
    .bind(e.publishers.first())
    .bind(&e.publish_date)
    .bind(&e.language)
    .bind(e.page_count)
    .bind(&e.physical_format)
    .bind(e.is_audiobook)
    .bind(json(&images))
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Marks `edition_id` as the preferred edition of `book_id` (false when it is not one of its editions).
pub async fn set_preferred(db: &Db, book_id: i64, edition_id: i64) -> Result<bool> {
    let mut tx = db.pool().begin().await?;
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM book_editions WHERE id = ? AND book_id = ?")
        .bind(edition_id)
        .bind(book_id)
        .fetch_optional(&mut *tx)
        .await?;
    if found.is_none() {
        return Ok(false);
    }
    sqlx::query("UPDATE book_editions SET monitored = (id = ?), updated_at = ? WHERE book_id = ?")
        .bind(edition_id)
        .bind(now_ms())
        .bind(book_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Book ids whose editions carry `isbn` (10 or 13 digits) or `asin`.
pub async fn books_by_identifier(db: &Db, isbn: Option<&str>, asin: Option<&str>) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    if let Some(isbn) = isbn {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT book_id FROM book_editions WHERE isbn13 = ?1 OR isbn10 = ?1 \
             UNION SELECT id FROM books WHERE isbn13 = ?1",
        )
        .bind(isbn)
        .fetch_all(db.pool())
        .await?;
        out.extend(ids);
    }
    if let Some(asin) = asin {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT book_id FROM book_editions WHERE asin = ?1 COLLATE NOCASE \
             UNION SELECT id FROM books WHERE asin = ?1 COLLATE NOCASE",
        )
        .bind(asin)
        .fetch_all(db.pool())
        .await?;
        out.extend(ids);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}
