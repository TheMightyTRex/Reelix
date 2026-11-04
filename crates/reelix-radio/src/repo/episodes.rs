//! `radio_episodes` rows ⇄ [`RadioEpisodeDto`].

use reelix_api_types::radio::{RadioEpisodeDto, RadioEpisodeStatus, RadioNumberSource};
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::placeholders;
use crate::convert::{enum_str, from_ms_opt, ms, parse_enum, u32_opt};
use crate::error::{RadioError, Result};
use crate::provider::EpisodeData;

const COLS: &str = "e.id, e.show_id, e.series_id, e.external_id, x.number AS series_number, x.kind AS series_kind, \
    e.number, e.number_source, e.title, e.synopsis, e.first_broadcast, e.duration_secs, e.monitored, e.status, \
    e.file_id, e.manual_match, e.enclosure_url, e.guid, e.source_removed_at";

const FROM: &str = "radio_episodes e JOIN radio_series x ON x.id = e.series_id";

/// Series in order (specials last), then episode number, then first broadcast.
const ORDER: &str = "x.kind = 'specials', x.number IS NULL, x.number, x.id, e.number IS NULL, e.number, \
    e.first_broadcast, e.id";

fn map(r: &SqliteRow) -> Result<RadioEpisodeDto> {
    Ok(RadioEpisodeDto {
        id: r.try_get("id")?,
        show_id: r.try_get("show_id")?,
        series_id: r.try_get("series_id")?,
        external_id: r.try_get("external_id")?,
        series_number: u32_opt(r.try_get("series_number")?),
        series_kind: parse_enum(&r.try_get::<String, _>("series_kind")?).unwrap_or_default(),
        number: u32_opt(r.try_get("number")?),
        number_source: parse_enum(&r.try_get::<String, _>("number_source")?).unwrap_or_default(),
        title: r.try_get("title")?,
        synopsis: r.try_get("synopsis")?,
        first_broadcast: from_ms_opt(r.try_get("first_broadcast")?),
        duration_secs: u32_opt(r.try_get("duration_secs")?),
        monitored: r.try_get("monitored")?,
        status: parse_enum(&r.try_get::<String, _>("status")?).unwrap_or_default(),
        file_id: r.try_get("file_id")?,
        manual_match: r.try_get("manual_match")?,
        enclosure_url: r.try_get("enclosure_url")?,
        guid: r.try_get("guid")?,
        source_removed_at: from_ms_opt(r.try_get("source_removed_at")?),
        file: None,
    })
}

pub async fn for_show(db: &Db, show_id: i64) -> Result<Vec<RadioEpisodeDto>> {
    sqlx::query(&format!(
        "SELECT {COLS} FROM {FROM} WHERE e.show_id = ? ORDER BY {ORDER}"
    ))
    .bind(show_id)
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(map)
    .collect()
}

pub async fn load(db: &Db, ids: &[i64]) -> Result<Vec<RadioEpisodeDto>> {
    let sql = format!(
        "SELECT {COLS} FROM {FROM} WHERE e.id IN ({}) ORDER BY {ORDER}",
        placeholders(ids.len())
    );
    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }
    query.fetch_all(db.pool()).await?.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<RadioEpisodeDto>> {
    Ok(load(db, &[id]).await?.pop())
}

pub async fn require(db: &Db, id: i64) -> Result<RadioEpisodeDto> {
    get(db, id)
        .await?
        .ok_or_else(|| RadioError::NotFound(format!("radio episode {id} not found")))
}

/// The episode a file is linked to.
pub async fn for_file(db: &Db, file_id: i64) -> Result<Option<RadioEpisodeDto>> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM {FROM} WHERE e.file_id = ?"))
        .bind(file_id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(map).transpose()
}

pub async fn insert(db: &Db, show_id: i64, series_id: i64, e: &EpisodeData, monitored: bool) -> Result<i64> {
    let now = now_ms();
    let source = if e.number.is_some() {
        RadioNumberSource::Source
    } else {
        RadioNumberSource::BroadcastOrder
    };
    let res = sqlx::query(
        "INSERT INTO radio_episodes (show_id, series_id, external_id, number, number_source, title, synopsis, \
            first_broadcast, duration_secs, monitored, enclosure_url, guid, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(show_id)
    .bind(series_id)
    .bind(&e.external_id)
    .bind(e.number.map(i64::from))
    .bind(enum_str(&source))
    .bind(&e.title)
    .bind(&e.synopsis)
    .bind(ms(e.first_broadcast))
    .bind(e.duration_secs.map(i64::from))
    .bind(monitored)
    .bind(&e.enclosure_url)
    .bind(&e.guid)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(res.last_insert_rowid())
}

/// Copies the provider's current episode fields; values it no longer gives are kept. The
/// episode is listed again, so it is no longer removed.
pub async fn update_from_source(db: &Db, id: i64, series_id: i64, e: &EpisodeData) -> Result<()> {
    let source = e.number.is_some();
    sqlx::query(
        "UPDATE radio_episodes SET series_id = ?, \
            number = CASE WHEN ? THEN ? ELSE number END, \
            number_source = CASE WHEN ? THEN 'source' WHEN number_source = 'source' THEN 'broadcast_order' \
                ELSE number_source END, \
            title = ?, synopsis = COALESCE(?, synopsis), first_broadcast = COALESCE(?, first_broadcast), \
            duration_secs = COALESCE(?, duration_secs), enclosure_url = COALESCE(?, enclosure_url), \
            guid = COALESCE(?, guid), source_removed_at = NULL, updated_at = ? \
         WHERE id = ?",
    )
    .bind(series_id)
    .bind(source)
    .bind(e.number.map(i64::from))
    .bind(source)
    .bind(&e.title)
    .bind(&e.synopsis)
    .bind(ms(e.first_broadcast))
    .bind(e.duration_secs.map(i64::from))
    .bind(&e.enclosure_url)
    .bind(&e.guid)
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Marks episodes the source no longer lists (kept, never deleted).
pub async fn mark_removed(db: &Db, ids: &[i64]) -> Result<()> {
    let now = now_ms();
    for id in ids {
        sqlx::query(
            "UPDATE radio_episodes SET source_removed_at = ?, updated_at = ? \
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

/// A number Reelix gave (broadcast order); source numbers are never overwritten.
pub async fn set_derived_number(db: &Db, id: i64, number: u32) -> Result<()> {
    sqlx::query(
        "UPDATE radio_episodes SET number = ?, updated_at = ? \
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

pub async fn set_monitored(db: &Db, id: i64, monitored: bool) -> Result<()> {
    sqlx::query("UPDATE radio_episodes SET monitored = ?, updated_at = ? WHERE id = ? AND monitored <> ?")
        .bind(monitored)
        .bind(now_ms())
        .bind(id)
        .bind(monitored)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Saves a status; true when it changed.
pub async fn set_status(db: &Db, id: i64, status: RadioEpisodeStatus) -> Result<bool> {
    let s = enum_str(&status);
    let res =
        sqlx::query("UPDATE radio_episodes SET status = ?, updated_at = ? WHERE id = ? AND status <> ?")
            .bind(&s)
            .bind(now_ms())
            .bind(id)
            .bind(&s)
            .execute(db.pool())
            .await?;
    Ok(res.rows_affected() > 0)
}

/// Links (or unlinks) a file; `manual` = set by hand, kept by automatic matching.
pub async fn set_file(db: &Db, id: i64, file_id: Option<i64>, manual: bool) -> Result<()> {
    sqlx::query("UPDATE radio_episodes SET file_id = ?, manual_match = ?, updated_at = ? WHERE id = ?")
        .bind(file_id)
        .bind(manual)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// Ids of episodes linked to any of `file_ids`.
pub async fn ids_for_files(db: &Db, file_ids: &[i64]) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT id FROM radio_episodes WHERE file_id IN ({})",
        placeholders(file_ids.len())
    );
    let mut q = sqlx::query_scalar(&sql);
    for f in file_ids {
        q = q.bind(f);
    }
    Ok(q.fetch_all(db.pool()).await?)
}

/// External ids of a show's episodes whose details (length) are already saved.
pub async fn known_details(db: &Db, show_id: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT external_id FROM radio_episodes WHERE show_id = ? AND duration_secs IS NOT NULL",
    )
    .bind(show_id)
    .fetch_all(db.pool())
    .await?)
}

/// Show ids of episodes.
pub async fn show_ids(db: &Db, ids: &[i64]) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT DISTINCT show_id FROM radio_episodes WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query_scalar(&sql);
    for id in ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all(db.pool()).await?)
}
