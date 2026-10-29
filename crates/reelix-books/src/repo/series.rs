//! `book_series` + `book_series_links`.

use std::collections::HashMap;

use reelix_api_types::books::{BookSeriesDto, BookSeriesRefDto};
use reelix_db::{Db, now_ms};
use sqlx::Row;

use super::{clean, placeholders};
use crate::error::Result;

/// A series membership to store: title, display position and sort key.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesLink {
    pub title: String,
    pub position: Option<String>,
}

/// Sort key of a display position (`"2.5"` → 2.5, `"1-3"` → 1).
pub fn position_sort(position: Option<&str>) -> Option<f64> {
    let p = position?.trim();
    let num: String = p
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.trim_end_matches('.').parse().ok()
}

async fn upsert(db: &Db, title: &str) -> Result<i64> {
    let key = clean(title);
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM book_series WHERE clean_title = ?")
        .bind(&key)
        .fetch_optional(db.pool())
        .await?
    {
        return Ok(id);
    }
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO book_series (title, clean_title, created_at, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(clean_title) DO UPDATE SET updated_at = excluded.updated_at",
    )
    .bind(title.trim())
    .bind(&key)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(res.last_insert_rowid())
}

/// Replaces the series links of a book; series without books are removed.
pub async fn set_links(db: &Db, book_id: i64, links: &[SeriesLink]) -> Result<()> {
    let mut ids = Vec::new();
    for l in links {
        if clean(&l.title).is_empty() {
            continue;
        }
        let id = upsert(db, &l.title).await?;
        if !ids.iter().any(|(i, _): &(i64, &SeriesLink)| *i == id) {
            ids.push((id, l));
        }
    }
    let mut tx = db.pool().begin().await?;
    sqlx::query("DELETE FROM book_series_links WHERE book_id = ?")
        .bind(book_id)
        .execute(&mut *tx)
        .await?;
    for (id, l) in ids {
        sqlx::query(
            "INSERT INTO book_series_links (series_id, book_id, position, position_sort) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(book_id)
        .bind(&l.position)
        .bind(position_sort(l.position.as_deref()))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    prune(db).await
}

/// Deletes series that no book links to.
pub async fn prune(db: &Db) -> Result<()> {
    sqlx::query("DELETE FROM book_series WHERE id NOT IN (SELECT series_id FROM book_series_links)")
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn refs_of(db: &Db, book_ids: &[i64]) -> Result<HashMap<i64, Vec<BookSeriesRefDto>>> {
    let sql = format!(
        "SELECT l.book_id, s.id, s.title, l.position FROM book_series_links l \
         JOIN book_series s ON s.id = l.series_id WHERE l.book_id IN ({}) ORDER BY s.title",
        placeholders(book_ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in book_ids {
        q = q.bind(id);
    }
    let mut out: HashMap<i64, Vec<BookSeriesRefDto>> = HashMap::new();
    for r in q.fetch_all(db.pool()).await? {
        out.entry(r.try_get("book_id")?)
            .or_default()
            .push(BookSeriesRefDto {
                id: r.try_get("id")?,
                title: r.try_get("title")?,
                position: r.try_get("position")?,
            });
    }
    Ok(out)
}

/// Every series with its books ordered by position.
pub async fn list(db: &Db) -> Result<Vec<BookSeriesDto>> {
    let rows = sqlx::query(
        "SELECT s.id, s.title, l.book_id, l.position FROM book_series s \
         LEFT JOIN book_series_links l ON l.series_id = s.id \
         ORDER BY s.title, s.id, l.position_sort IS NULL, l.position_sort, l.book_id",
    )
    .fetch_all(db.pool())
    .await?;
    let mut out: Vec<BookSeriesDto> = Vec::new();
    for r in rows {
        let id: i64 = r.try_get("id")?;
        if out.last().is_none_or(|s| s.id != id) {
            out.push(BookSeriesDto {
                id,
                title: r.try_get("title")?,
                books: Vec::new(),
            });
        }
        if let Some(book) = r.try_get::<Option<i64>, _>("book_id")? {
            let pos: Option<String> = r.try_get("position")?;
            if let Some(s) = out.last_mut() {
                s.books.push((pos.unwrap_or_default(), book));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_sort_keys() {
        assert_eq!(position_sort(Some("2.5")), Some(2.5));
        assert_eq!(position_sort(Some("#3")), Some(3.0));
        assert_eq!(position_sort(Some("1-3")), Some(1.0));
        assert_eq!(position_sort(Some("x")), None);
        assert_eq!(position_sort(None), None);
    }
}
