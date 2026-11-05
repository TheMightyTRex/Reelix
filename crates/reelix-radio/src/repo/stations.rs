//! `radio_stations` rows ⇄ [`RadioStationDto`].

use reelix_api_types::radio::RadioStationDto;
use reelix_db::{Db, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::error::Result;
use crate::provider::StationData;

const COLS: &str = "st.id, st.provider, st.external_id, st.name, st.logo_url, st.logo_path, \
    (SELECT COUNT(*) FROM radio_shows s WHERE s.station_id = st.id) AS n_shows";

fn map(r: &SqliteRow) -> Result<RadioStationDto> {
    Ok(RadioStationDto {
        id: r.try_get("id")?,
        provider: r.try_get("provider")?,
        external_id: r.try_get("external_id")?,
        name: r.try_get("name")?,
        logo_url: r.try_get("logo_url")?,
        has_logo: r.try_get::<Option<String>, _>("logo_path")?.is_some(),
        show_count: r.try_get::<i64, _>("n_shows")?.max(0) as u32,
    })
}

/// Every station, by name.
pub async fn list(db: &Db) -> Result<Vec<RadioStationDto>> {
    sqlx::query(&format!(
        "SELECT {COLS} FROM radio_stations st ORDER BY st.name COLLATE NOCASE, st.id"
    ))
    .fetch_all(db.pool())
    .await?
    .iter()
    .map(map)
    .collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<RadioStationDto>> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM radio_stations st WHERE st.id = ?"))
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
    row.as_ref().map(map).transpose()
}

/// Inserts or updates the station (`provider`, `external_id`) and returns its id. A missing
/// logo URL keeps the saved one.
pub async fn upsert(db: &Db, provider: &str, s: &StationData) -> Result<i64> {
    let now = now_ms();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO radio_stations (provider, external_id, name, logo_url, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (provider, external_id) DO UPDATE SET name = excluded.name, \
            logo_url = COALESCE(excluded.logo_url, radio_stations.logo_url), updated_at = excluded.updated_at \
         RETURNING id",
    )
    .bind(provider)
    .bind(&s.external_id)
    .bind(&s.name)
    .bind(&s.logo_url)
    .bind(now)
    .bind(now)
    .fetch_one(db.pool())
    .await?;
    Ok(id)
}

/// Records where a downloaded logo was stored (relative to the data folder; `None` clears).
pub async fn set_logo_path(db: &Db, id: i64, path: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE radio_stations SET logo_path = ?, updated_at = ? WHERE id = ?")
        .bind(path)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `logo_path` of a station.
pub async fn logo_path(db: &Db, id: i64) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT logo_path FROM radio_stations WHERE id = ?")
            .bind(id)
            .fetch_optional(db.pool())
            .await?
            .flatten(),
    )
}
