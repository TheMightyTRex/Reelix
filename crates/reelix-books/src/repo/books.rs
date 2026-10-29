//! `books` + `book_authors` rows ⇄ [`BookDto`] (files are attached by the service).

use std::collections::HashMap;

use reelix_api_types::books::{BookCreditDto, BookDto};
use reelix_core::BookFormat;
use reelix_db::{Db, from_ms, now_ms};
use reelix_metadata::WorkAuthor;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{clean, date_str, from_json, json, parse_date, placeholders, series};
use crate::error::Result;

const COLS: &str = "b.id, b.author_id, a.name AS author_name, b.openlibrary_work_id, b.title, b.sort_title, \
    b.subtitle, b.overview, b.release_date, b.year, b.genres, b.images, b.ratings, b.isbn13, b.asin, \
    b.page_count, b.language, b.monitored_ebook, b.monitored_audiobook, b.added_by_request_id, b.created_at";

/// Which books to load.
#[derive(Debug, Clone, Copy, Default)]
pub struct BookFilter<'a> {
    pub ids: Option<&'a [i64]>,
    pub author_ids: Option<&'a [i64]>,
}

fn map(r: &SqliteRow) -> Result<BookDto> {
    Ok(BookDto {
        id: r.try_get("id")?,
        author_id: r.try_get("author_id")?,
        author_name: r.try_get("author_name")?,
        openlibrary_work_id: r.try_get("openlibrary_work_id")?,
        title: r.try_get("title")?,
        sort_title: r.try_get("sort_title")?,
        subtitle: r.try_get("subtitle")?,
        overview: r.try_get("overview")?,
        release_date: parse_date(r.try_get("release_date")?),
        year: r.try_get::<Option<i64>, _>("year")?.map(|y| y as i32),
        genres: from_json(r.try_get("genres")?),
        images: from_json(r.try_get("images")?),
        ratings: from_json(r.try_get("ratings")?),
        isbn13: r.try_get("isbn13")?,
        asin: r.try_get("asin")?,
        page_count: r.try_get::<Option<i64>, _>("page_count")?.map(|v| v as u32),
        language: r.try_get("language")?,
        series: Vec::new(),
        credits: Vec::new(),
        monitored_ebook: r.try_get("monitored_ebook")?,
        monitored_audiobook: r.try_get("monitored_audiobook")?,
        ebook_file: None,
        audiobook_file: None,
        added_by_request_id: r.try_get("added_by_request_id")?,
        added: from_ms(r.try_get("created_at")?),
    })
}

/// Books with series and credits, ordered by author then release date and title.
pub async fn load(db: &Db, f: BookFilter<'_>) -> Result<Vec<BookDto>> {
    let mut sql = format!("SELECT {COLS} FROM books b JOIN authors a ON a.id = b.author_id WHERE 1 = 1");
    if let Some(ids) = f.ids {
        sql.push_str(&format!(" AND b.id IN ({})", placeholders(ids.len())));
    }
    if let Some(ids) = f.author_ids {
        sql.push_str(&format!(" AND b.author_id IN ({})", placeholders(ids.len())));
    }
    sql.push_str(" ORDER BY a.sort_name, b.release_date IS NULL, b.release_date, b.sort_title");
    let mut q = sqlx::query(&sql);
    for id in f
        .ids
        .unwrap_or_default()
        .iter()
        .chain(f.author_ids.unwrap_or_default())
    {
        q = q.bind(id);
    }
    let mut books: Vec<BookDto> = q
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<_>>()?;
    if books.is_empty() {
        return Ok(books);
    }
    let ids: Vec<i64> = books.iter().map(|b| b.id).collect();
    let mut refs = series::refs_of(db, &ids).await?;
    let mut credits = credits_of(db, &ids).await?;
    for b in &mut books {
        b.series = refs.remove(&b.id).unwrap_or_default();
        b.credits = credits.remove(&b.id).unwrap_or_default();
    }
    Ok(books)
}

pub async fn id_by_work(db: &Db, work_id: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM books WHERE openlibrary_work_id = ?")
            .bind(work_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// Work id → book id.
pub async fn work_index(db: &Db) -> Result<HashMap<String, i64>> {
    let rows = sqlx::query("SELECT id, openlibrary_work_id FROM books")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("openlibrary_work_id")?, r.try_get("id")?)))
        .collect()
}

/// Inserts a book (`b.id` is ignored).
pub async fn insert(db: &Db, b: &BookDto) -> Result<i64> {
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO books (author_id, openlibrary_work_id, title, sort_title, clean_title, subtitle, overview, \
         release_date, year, genres, images, ratings, isbn13, asin, page_count, language, monitored_ebook, \
         monitored_audiobook, added_by_request_id, last_refreshed, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(b.author_id)
    .bind(&b.openlibrary_work_id)
    .bind(&b.title)
    .bind(&b.sort_title)
    .bind(clean(&b.title))
    .bind(&b.subtitle)
    .bind(&b.overview)
    .bind(date_str(b.release_date))
    .bind(b.year)
    .bind(json(&b.genres))
    .bind(json(&b.images))
    .bind(json(&b.ratings))
    .bind(&b.isbn13)
    .bind(&b.asin)
    .bind(b.page_count)
    .bind(&b.language)
    .bind(b.monitored_ebook)
    .bind(b.monitored_audiobook)
    .bind(b.added_by_request_id)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(res.last_insert_rowid())
}

/// Writes metadata and monitoring of `b`.
pub async fn save(db: &Db, b: &BookDto) -> Result<()> {
    sqlx::query(
        "UPDATE books SET title = ?, sort_title = ?, clean_title = ?, subtitle = ?, overview = ?, release_date = ?, \
         year = ?, genres = ?, images = ?, ratings = ?, isbn13 = ?, asin = ?, page_count = ?, language = ?, \
         monitored_ebook = ?, monitored_audiobook = ?, added_by_request_id = ?, last_refreshed = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&b.title)
    .bind(&b.sort_title)
    .bind(clean(&b.title))
    .bind(&b.subtitle)
    .bind(&b.overview)
    .bind(date_str(b.release_date))
    .bind(b.year)
    .bind(json(&b.genres))
    .bind(json(&b.images))
    .bind(json(&b.ratings))
    .bind(&b.isbn13)
    .bind(&b.asin)
    .bind(b.page_count)
    .bind(&b.language)
    .bind(b.monitored_ebook)
    .bind(b.monitored_audiobook)
    .bind(b.added_by_request_id)
    .bind(now_ms())
    .bind(now_ms())
    .bind(b.id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_monitored(db: &Db, ids: &[i64], format: Option<BookFormat>, monitored: bool) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let set = match format {
        Some(BookFormat::Ebook) => "monitored_ebook = ?1",
        Some(BookFormat::Audiobook) => "monitored_audiobook = ?1",
        None => "monitored_ebook = ?1, monitored_audiobook = ?1",
    };
    let sql = format!(
        "UPDATE books SET {set}, updated_at = ?2 WHERE id IN ({})",
        (0..ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut q = sqlx::query(&sql).bind(monitored).bind(now_ms());
    for id in ids {
        q = q.bind(id);
    }
    q.execute(db.pool()).await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM books WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Replaces the credited authors of a book.
pub async fn set_credits(db: &Db, book_id: i64, credits: &[WorkAuthor]) -> Result<()> {
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM book_authors WHERE book_id = ?")
        .bind(book_id)
        .execute(&mut *tx)
        .await?;
    for (pos, c) in credits.iter().enumerate() {
        if c.id.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO book_authors (book_id, openlibrary_id, name, role, position) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(book_id)
        .bind(&c.id)
        .bind(&c.name)
        .bind(&c.role)
        .bind(pos as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn credits_of(db: &Db, book_ids: &[i64]) -> Result<HashMap<i64, Vec<BookCreditDto>>> {
    let sql = format!(
        "SELECT c.book_id, c.openlibrary_id, c.name, c.role, a.id AS author_id FROM book_authors c \
         LEFT JOIN authors a ON a.openlibrary_id = c.openlibrary_id \
         WHERE c.book_id IN ({}) ORDER BY c.book_id, c.position",
        placeholders(book_ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in book_ids {
        q = q.bind(id);
    }
    let mut out: HashMap<i64, Vec<BookCreditDto>> = HashMap::new();
    for r in q.fetch_all(db.pool()).await? {
        out.entry(r.try_get("book_id")?).or_default().push(BookCreditDto {
            openlibrary_id: r.try_get("openlibrary_id")?,
            name: r.try_get("name")?,
            role: r.try_get("role")?,
            author_id: r.try_get("author_id")?,
        });
    }
    Ok(out)
}
