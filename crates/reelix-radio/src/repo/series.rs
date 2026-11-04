//! `radio_series` rows ⇄ [`RadioSeriesDto`].

use reelix_api_types::radio::{RadioNumberSource, RadioSeriesDto};
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::convert::{enum_str, from_ms_opt, parse_enum, u32_opt};
use crate::error::Result;
use crate::provider::SeriesData;

const COLS: &str = "x.id, x.show_id, x.external_id, x.kind, x.number, x.number_source, x.title, x.synopsis, \
    x.expected_episodes, x.settled_at, x.source_removed_at, \
    (SELECT COUNT(*) FROM radio_episodes e WHERE e.series_id = x.id) AS n_eps";

fn map(r: &SqliteRow) -> Result<RadioSeriesDto> {
    Ok(RadioSeriesDto {
        id: r.try_get("id")?,
        show_id: r.try_get("show_id")?,
        external_id: r.try_get("external_id")?,
        kind: parse_enum(&r.try_get::<String, _>("kind")?).unwrap_or_default(),
        number: u32_opt(r.try_get("number")?),
        number_source: parse_enum(&r.try_get::<String, _>("number_source")?).unwrap_or_default(),
        title: r.try_get("title")?,
        synopsis: r.try_get("synopsis")?,
        expected_episodes: u32_opt(r.try_get("expected_episodes")?),
        episode_count: r.try_get::<i64, _>("n_eps")?.max(0) as u32,
        settled_at: from_ms_opt(r.try_get("settled_at")?),
        source_removed_at: from_ms_opt(r.try_get("source_removed_at")?),
    })
}

/// Series of a show: numbered series in order, then unnumbered, then the specials.
pub async fn for_show(db: &Db, show_id: i64) -> Result<Vec<RadioSeriesDto>> {
    sqlx::query(&format!(
        "SELECT {COLS} FROM radio_series x WHERE x.show_id = ? \
         ORDER BY x.kind = 'specials', x.number IS NULL, x.number, x.id"
    ))
    .bind(show_id)
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(map)
    .collect()
}

/// Inserts or updates a series from the provider and returns its id. A series the source
/// lists again is no longer removed; a number the source no longer gives is kept.
pub async fn upsert(db: &Db, show_id: i64, s: &SeriesData) -> Result<i64> {
    let now = now_ms();
    let source = s.number.is_some();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO radio_series (show_id, external_id, kind, number, number_source, title, synopsis, \
            expected_episodes, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (show_id, external_id) DO UPDATE SET kind = excluded.kind, \
            number = CASE WHEN ? THEN excluded.number ELSE radio_series.number END, \
            number_source = CASE WHEN ? THEN 'source' ELSE radio_series.number_source END, \
            title = COALESCE(excluded.title, radio_series.title), \
            synopsis = COALESCE(excluded.synopsis, radio_series.synopsis), \
            expected_episodes = COALESCE(excluded.expected_episodes, radio_series.expected_episodes), \
            source_removed_at = NULL, updated_at = excluded.updated_at \
         RETURNING id",
    )
    .bind(show_id)
    .bind(&s.external_id)
    .bind(enum_str(&s.kind))
    .bind(s.number.map(i64::from))
    .bind(enum_str(if source {
        &RadioNumberSource::Source
    } else {
        &RadioNumberSource::BroadcastOrder
    }))
    .bind(&s.title)
    .bind(&s.synopsis)
    .bind(s.expected_episodes.map(i64::from))
    .bind(now)
    .bind(now)
    .bind(source)
    .bind(source)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Marks series the source no longer lists (kept, never deleted).
pub async fn mark_removed(db: &Db, ids: &[i64]) -> Result<()> {
    let now = now_ms();
    for id in ids {
        sqlx::query(
            "UPDATE radio_series SET source_removed_at = ?, updated_at = ? \
             WHERE id = ? AND source_removed_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// A number Reelix gave (broadcast order).
pub async fn set_derived_number(db: &Db, id: i64, number: u32) -> Result<()> {
    sqlx::query(
        "UPDATE radio_series SET number = ?, number_source = 'broadcast_order', updated_at = ? \
         WHERE id = ? AND number_source = 'broadcast_order' AND number IS NOT ?",
    )
    .bind(i64::from(number))
    .bind(now_ms())
    .bind(id)
    .bind(i64::from(number))
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn set_settled(db: &Db, id: i64, at: i64) -> Result<()> {
    sqlx::query("UPDATE radio_series SET settled_at = ?, updated_at = ? WHERE id = ?")
        .bind(at)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}
