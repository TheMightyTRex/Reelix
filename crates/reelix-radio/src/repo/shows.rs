//! `radio_shows` rows ⇄ [`RadioShowDto`].

use reelix_api_types::library::SortDir;
use reelix_api_types::radio::{RadioFollow, RadioShowDto, RadioShowListQuery, RadioShowStatisticsDto};
use reelix_db::{Db, from_ms, now_ms};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{page_window, placeholders};
use crate::convert::{clean_title, enum_str, from_json, from_ms_opt, json, ms, parse_enum, sort_title};
use crate::error::{RadioError, Result};
use crate::provider::ShowData;

const COLS: &str = "s.id, s.provider, s.external_id, s.title, s.sort_title, s.synopsis, s.author, s.web_url, \
    s.first_broadcast, s.station_id, st.name AS station_name, s.artwork_url, s.artwork_path, s.feed_url, \
    s.follow, s.followed_at, s.monitored, s.tags, s.root_folder_id, s.abs_item_id, s.quality_profile_id, \
    s.refreshed_at, s.added_at, s.keep_episodes, s.custom, \
    (SELECT COUNT(*) FROM radio_series x WHERE x.show_id = s.id) AS n_series, \
    (SELECT COUNT(*) FROM radio_episodes e WHERE e.show_id = s.id) AS n_eps, \
    (SELECT COUNT(*) FROM radio_episodes e WHERE e.show_id = s.id AND e.status = 'in_library') AS n_have, \
    (SELECT COUNT(*) FROM radio_episodes e WHERE e.show_id = s.id AND e.status = 'wanted') AS n_wanted";

const FROM: &str = "radio_shows s LEFT JOIN radio_stations st ON st.id = s.station_id";

fn count(r: &SqliteRow, col: &str) -> Result<u32> {
    Ok(r.try_get::<i64, _>(col)?.max(0) as u32)
}

fn map(r: &SqliteRow) -> Result<RadioShowDto> {
    Ok(RadioShowDto {
        id: r.try_get("id")?,
        provider: r.try_get("provider")?,
        external_id: r.try_get("external_id")?,
        title: r.try_get("title")?,
        sort_title: r.try_get("sort_title")?,
        synopsis: r.try_get("synopsis")?,
        author: r.try_get("author")?,
        web_url: r.try_get("web_url")?,
        first_broadcast: from_ms_opt(r.try_get("first_broadcast")?),
        station_id: r.try_get("station_id")?,
        station_name: r.try_get("station_name")?,
        artwork_url: r.try_get("artwork_url")?,
        has_artwork: r.try_get::<Option<String>, _>("artwork_path")?.is_some(),
        feed_url: r.try_get("feed_url")?,
        follow: parse_enum(&r.try_get::<String, _>("follow")?).unwrap_or_default(),
        followed_at: from_ms_opt(r.try_get("followed_at")?),
        monitored: r.try_get("monitored")?,
        tags: from_json(r.try_get("tags")?),
        root_folder_id: r.try_get("root_folder_id")?,
        abs_item_id: r.try_get("abs_item_id")?,
        quality_profile_id: r.try_get("quality_profile_id")?,
        refreshed_at: from_ms_opt(r.try_get("refreshed_at")?),
        added_at: from_ms(r.try_get("added_at")?),
        statistics: RadioShowStatisticsDto {
            series_count: count(r, "n_series")?,
            episode_count: count(r, "n_eps")?,
            in_library_count: count(r, "n_have")?,
            wanted_count: count(r, "n_wanted")?,
        },
        abs_web_url: None,
        keep_episodes: r.try_get("keep_episodes")?,
        custom: r.try_get("custom")?,
    })
}

/// Shows (all, or `ids`), by sort title.
pub async fn load(db: &Db, ids: Option<&[i64]>) -> Result<Vec<RadioShowDto>> {
    let sql = match ids {
        Some(ids) => format!(
            "SELECT {COLS} FROM {FROM} WHERE s.id IN ({}) ORDER BY s.sort_title COLLATE NOCASE, s.id",
            placeholders(ids.len())
        ),
        None => format!("SELECT {COLS} FROM {FROM} ORDER BY s.sort_title COLLATE NOCASE, s.id"),
    };
    let mut q = sqlx::query(&sql);
    for id in ids.unwrap_or_default() {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await?.iter().map(map).collect()
}

pub async fn get(db: &Db, id: i64) -> Result<Option<RadioShowDto>> {
    Ok(load(db, Some(&[id])).await?.pop())
}

pub async fn require(db: &Db, id: i64) -> Result<RadioShowDto> {
    get(db, id)
        .await?
        .ok_or_else(|| RadioError::NotFound(format!("radio show {id} not found")))
}

pub async fn by_external(db: &Db, provider: &str, external_id: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM radio_shows WHERE provider = ? AND external_id = ?")
            .bind(provider)
            .bind(external_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// What a search hit or Discover item can be matched by: every show's `(provider,
/// external_id)` and feed URL, and the aliases it was added under (`radio_show_aliases`).
#[derive(Debug, Clone, Default)]
pub struct KnownShows {
    /// `(provider, external_id) → show id`, aliases included.
    pub by_id: std::collections::HashMap<(String, String), i64>,
    /// Feed URL ([`feed_key`]) → show id.
    pub by_feed: std::collections::HashMap<String, i64>,
}

impl KnownShows {
    /// The followed show behind a hit: its own id, an alias, else its feed URL (or the
    /// external id of an `rss` hit, which is the feed URL).
    pub fn find(&self, provider: &str, external_id: &str, feed_url: Option<&str>) -> Option<i64> {
        if let Some(id) = self.by_id.get(&(provider.to_owned(), external_id.to_owned())) {
            return Some(*id);
        }
        let feed = feed_url.or((provider == crate::providers::rss::ID).then_some(external_id))?;
        self.by_feed.get(&feed_key(feed)).copied()
    }
}

/// A feed URL for comparison: trimmed, lower case, without a trailing `/` or the scheme.
pub fn feed_key(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').to_ascii_lowercase();
    u.strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .map(str::to_owned)
        .unwrap_or(u)
}

pub async fn known(db: &Db) -> Result<KnownShows> {
    let mut k = KnownShows::default();
    let rows = sqlx::query("SELECT id, provider, external_id, feed_url FROM radio_shows")
        .fetch_all(db.pool())
        .await?;
    for r in &rows {
        let id: i64 = r.try_get("id")?;
        k.by_id
            .insert((r.try_get("provider")?, r.try_get("external_id")?), id);
        if let Some(f) = r.try_get::<Option<String>, _>("feed_url")? {
            k.by_feed.entry(feed_key(&f)).or_insert(id);
        }
    }
    let aliases = sqlx::query("SELECT provider, external_id, show_id FROM radio_show_aliases")
        .fetch_all(db.pool())
        .await?;
    for r in &aliases {
        k.by_id
            .entry((r.try_get("provider")?, r.try_get("external_id")?))
            .or_insert(r.try_get("show_id")?);
    }
    Ok(k)
}

/// Remembers that `(provider, external_id)` leads to `show_id` (replacing an older alias).
pub async fn add_alias(db: &Db, provider: &str, external_id: &str, show_id: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO radio_show_aliases (provider, external_id, show_id) VALUES (?, ?, ?)
         ON CONFLICT (provider, external_id) DO UPDATE SET show_id = excluded.show_id",
    )
    .bind(provider)
    .bind(external_id)
    .bind(show_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Ids of shows from these providers (all providers when `None`).
pub async fn ids(db: &Db, providers: Option<&[&str]>) -> Result<Vec<i64>> {
    let sql = match providers {
        Some(p) => format!(
            "SELECT id FROM radio_shows WHERE provider IN ({}) ORDER BY id",
            placeholders(p.len())
        ),
        None => "SELECT id FROM radio_shows ORDER BY id".to_owned(),
    };
    let mut q = sqlx::query_scalar(&sql);
    for p in providers.unwrap_or_default() {
        q = q.bind(*p);
    }
    Ok(q.fetch_all(db.pool()).await?)
}

/// Filtered, sorted, paged list: `(items, total, page, page_size)`.
pub async fn page(db: &Db, q: &RadioShowListQuery) -> Result<(Vec<RadioShowDto>, u64, u32, u32)> {
    let mut wh: Vec<&str> = Vec::new();
    let like = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));
    if like.is_some() {
        wh.push("(s.title LIKE ? OR s.sort_title LIKE ?)");
    }
    if q.station_id.is_some() {
        wh.push("s.station_id = ?");
    }
    match q.followed {
        Some(true) => wh.push("s.follow <> 'none'"),
        Some(false) => wh.push("s.follow = 'none'"),
        None => {}
    }
    if q.monitored.is_some() {
        wh.push("s.monitored = ?");
    }
    let where_sql = if wh.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", wh.join(" AND "))
    };
    let order = match q.sort.as_deref().unwrap_or("sort_title") {
        "title" => "s.title COLLATE NOCASE",
        "added" | "added_at" => "s.added_at",
        "refreshed" | "refreshed_at" => "s.refreshed_at",
        "first_broadcast" => "s.first_broadcast",
        _ => "s.sort_title COLLATE NOCASE",
    };
    let dir = match q.dir.unwrap_or_default() {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    };
    let (page, size, window) = page_window(q.page, q.page_size);
    let limit = window.map_or(String::new(), |(o, l)| format!(" LIMIT {l} OFFSET {o}"));
    let list_sql = format!("SELECT {COLS} FROM {FROM}{where_sql} ORDER BY {order} {dir}, s.id{limit}");
    let count_sql = format!("SELECT COUNT(*) FROM {FROM}{where_sql}");
    let mut lq = sqlx::query(&list_sql);
    let mut cq = sqlx::query_scalar::<_, i64>(&count_sql);
    if let Some(l) = &like {
        lq = lq.bind(l.clone()).bind(l.clone());
        cq = cq.bind(l.clone()).bind(l.clone());
    }
    if let Some(st) = q.station_id {
        lq = lq.bind(st);
        cq = cq.bind(st);
    }
    if let Some(m) = q.monitored {
        lq = lq.bind(m);
        cq = cq.bind(m);
    }
    let items = lq
        .fetch_all(db.pool())
        .await?
        .iter()
        .map(map)
        .collect::<Result<Vec<_>>>()?;
    let total = cq.fetch_one(db.pool()).await?.max(0) as u64;
    Ok((items, total, page, size))
}

/// Settings chosen when a show is added.
pub struct NewShow<'a> {
    pub provider: &'a str,
    pub follow: RadioFollow,
    pub monitored: bool,
    pub root_folder_id: Option<i64>,
    pub quality_profile_id: Option<i64>,
    pub tags: &'a [i64],
    pub station_id: Option<i64>,
    /// Unix ms: `added_at` and `followed_at`.
    pub now: i64,
}

pub async fn insert(db: &Db, data: &ShowData, n: &NewShow<'_>) -> Result<i64> {
    let now = n.now;
    let res = sqlx::query(
        "INSERT INTO radio_shows (provider, external_id, title, sort_title, clean_title, synopsis, author, \
            web_url, first_broadcast, station_id, artwork_url, feed_url, follow, followed_at, monitored, tags, \
            root_folder_id, quality_profile_id, added_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(n.provider)
    .bind(&data.external_id)
    .bind(&data.title)
    .bind(sort_title(&data.title))
    .bind(clean_title(&data.title))
    .bind(&data.synopsis)
    .bind(&data.author)
    .bind(&data.web_url)
    .bind(ms(data.first_broadcast))
    .bind(n.station_id)
    .bind(&data.artwork_url)
    .bind(&data.feed_url)
    .bind(enum_str(&n.follow))
    .bind(now)
    .bind(n.monitored)
    .bind(json(n.tags))
    .bind(n.root_folder_id)
    .bind(n.quality_profile_id)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            RadioError::Conflict(format!("{} is already in the library", data.title))
        }
        _ => e.into(),
    })?;
    Ok(res.last_insert_rowid())
}

/// Copies the provider's current show fields; values the provider no longer gives are kept.
pub async fn update_from_source(db: &Db, id: i64, data: &ShowData, station_id: Option<i64>) -> Result<()> {
    sqlx::query(
        "UPDATE radio_shows SET title = ?, sort_title = ?, clean_title = ?, \
            synopsis = COALESCE(?, synopsis), author = COALESCE(?, author), web_url = COALESCE(?, web_url), \
            first_broadcast = COALESCE(?, first_broadcast), station_id = COALESCE(?, station_id), \
            artwork_url = COALESCE(?, artwork_url), feed_url = COALESCE(?, feed_url), \
            refreshed_at = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&data.title)
    .bind(sort_title(&data.title))
    .bind(clean_title(&data.title))
    .bind(&data.synopsis)
    .bind(&data.author)
    .bind(&data.web_url)
    .bind(ms(data.first_broadcast))
    .bind(station_id)
    .bind(&data.artwork_url)
    .bind(&data.feed_url)
    .bind(now_ms())
    .bind(now_ms())
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Saves the user-editable fields of `s`.
pub async fn save_settings(db: &Db, s: &RadioShowDto) -> Result<()> {
    sqlx::query(
        "UPDATE radio_shows SET follow = ?, followed_at = ?, monitored = ?, tags = ?, root_folder_id = ?, \
            quality_profile_id = ?, abs_item_id = ?, keep_episodes = ?, updated_at = ? WHERE id = ?",
    )
    .bind(enum_str(&s.follow))
    .bind(ms(s.followed_at))
    .bind(s.monitored)
    .bind(json(&s.tags))
    .bind(s.root_folder_id)
    .bind(s.quality_profile_id)
    .bind(&s.abs_item_id)
    .bind(s.keep_episodes)
    .bind(now_ms())
    .bind(s.id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Ids of shows that keep only their newest episodes (`keep_episodes` set).
pub async fn with_keep_limit(db: &Db) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM radio_shows WHERE keep_episodes IS NOT NULL ORDER BY id")
            .fetch_all(db.pool())
            .await?,
    )
}

/// Records where downloaded artwork was stored (relative to the data folder; `None` clears).
pub async fn set_artwork_path(db: &Db, id: i64, path: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE radio_shows SET artwork_path = ?, updated_at = ? WHERE id = ?")
        .bind(path)
        .bind(now_ms())
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `artwork_path` of a show.
pub async fn artwork_path(db: &Db, id: i64) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT artwork_path FROM radio_shows WHERE id = ?")
            .bind(id)
            .fetch_optional(db.pool())
            .await?
            .flatten(),
    )
}

/// `(id, clean_title, abs_item_id)` of every show (for matching library items to shows).
pub async fn match_keys(db: &Db) -> Result<Vec<(i64, String, Option<String>)>> {
    let rows = sqlx::query("SELECT id, clean_title, abs_item_id FROM radio_shows")
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("id")?,
                r.try_get("clean_title")?,
                r.try_get("abs_item_id")?,
            ))
        })
        .collect()
}

pub async fn count_using(db: &Db, column: &str, id: i64) -> Result<i64> {
    debug_assert!(matches!(column, "root_folder_id" | "quality_profile_id"));
    Ok(
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM radio_shows WHERE {column} = ?"))
            .bind(id)
            .fetch_one(db.pool())
            .await?,
    )
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let res = sqlx::query("DELETE FROM radio_shows WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(res.rows_affected() > 0)
}
