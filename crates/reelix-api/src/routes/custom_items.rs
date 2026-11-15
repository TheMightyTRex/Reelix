//! Items with the owner's own details and no outside match (RFX-90): a film, series, author,
//! artist or radio show made from a folder (or an Audiobookshelf podcast item) with a typed
//! title, year, overview and picture.
//!
//! - `POST /customitems` adds one; the answer's `id` works in the kind's own routes.
//! - `PUT /customitems/{kind}/{id}` changes the typed details or the picture later.
//! - `GET /customitems/{kind}/{id}/image` serves the picture Reelix keeps.
//!
//! Every route needs `MANAGE_LIBRARY`, as adding an item of any kind does. Pictures come as an
//! `http(s)` address (fetched once) or a `data:` address, PNG, JPEG or WebP only (recognised
//! from the bytes), up to 5 MB. They are kept in `<data_dir>/custom/{kind}/{id}.{ext}`; a radio
//! show's goes where downloaded show artwork goes (`radio/artwork/{id}.{ext}`), so
//! `/radio/shows/{id}/artwork` serves it too.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use base64::Engine;
use reelix_api_types::ApiErrorBody;
use reelix_api_types::custom::{
    AddCustomItemRequest, CustomDetails, CustomDetailsChange, CustomItemDto, CustomItemEdit, CustomItemKind,
    IMAGE_MAX_BYTES, IMAGE_URL_MAX, OVERVIEW_MAX, STATION_MAX, TITLE_MAX, YEAR_MAX, YEAR_MIN,
};
use reelix_books::BooksService;
use reelix_library::LibraryService;
use reelix_music::MusicService;
use reelix_radio::artwork::{ImageKind, sniff};
use reelix_radio::{RadioService, StoredImage};
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::routes::library::require_lib;
use crate::state::AppState;

/// A 5 MB picture is about 6.7 MB as base64; the default 2 MB body limit would refuse it.
const BODY_LIMIT: usize = 8 * 1024 * 1024;
/// How long fetching a picture from an address may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/customitems", post(add_item))
        .route("/customitems/{kind}/{id}", put(edit_item))
        .route("/customitems/{kind}/{id}/image", get(item_image))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[derive(OpenApi)]
#[openapi(
    paths(add_item, edit_item, item_image),
    components(schemas(
        reelix_api_types::custom::CustomItemKind,
        reelix_api_types::custom::AddCustomItemRequest,
        reelix_api_types::custom::CustomItemEdit,
        reelix_api_types::custom::CustomItemDto,
    )),
    tags((name = "custom items", description = "Films, series, authors, artists and radio shows added with your own details and no outside match")),
)]
pub struct Api;

// ---------------- checks ----------------

/// The typed title: spaces collapsed, 1 to 200 characters.
fn clean_title(t: &str) -> ApiResult<String> {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = t.chars().count();
    if n == 0 || n > TITLE_MAX {
        return Err(ApiError::validation(format!(
            "the title is 1 to {TITLE_MAX} characters"
        )));
    }
    Ok(t)
}

fn check_year(y: Option<i32>) -> ApiResult<Option<i32>> {
    match y {
        Some(y) if !(YEAR_MIN..=YEAR_MAX).contains(&y) => Err(ApiError::validation(format!(
            "the year is between {YEAR_MIN} and {YEAR_MAX}"
        ))),
        y => Ok(y),
    }
}

/// Trimmed text up to `max` characters; empty is none.
fn clean_text(v: Option<&str>, max: usize, what: &str) -> ApiResult<Option<String>> {
    let Some(t) = v.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if t.chars().count() > max {
        return Err(ApiError::validation(format!(
            "the {what} is at most {max} characters"
        )));
    }
    Ok(Some(t.to_owned()))
}

/// A picture Reelix keeps.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Picture {
    kind: ImageKind,
    bytes: Vec<u8>,
}

/// PNG, JPEG or WebP, recognised from the bytes, up to 5 MB.
fn accept_picture(bytes: Vec<u8>) -> ApiResult<Picture> {
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(ApiError::validation("the picture is larger than 5 MB"));
    }
    match sniff(&bytes) {
        Some(kind @ (ImageKind::Png | ImageKind::Jpeg | ImageKind::Webp)) => Ok(Picture { kind, bytes }),
        _ => Err(ApiError::validation(
            "the picture is not a PNG, JPEG or WebP image",
        )),
    }
}

/// `data:image/png;base64,…` (or `image/jpeg`, `image/webp`).
pub(crate) fn decode_data_url(s: &str) -> ApiResult<Picture> {
    let bad = || ApiError::validation("the picture is a data: address of a PNG, JPEG or WebP image");
    let rest = s.trim().strip_prefix("data:").ok_or_else(bad)?;
    let (mime, data) = rest.split_once(";base64,").ok_or_else(bad)?;
    if !matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/webp"
    ) {
        return Err(bad());
    }
    // Refuse what cannot fit before decoding it.
    if data.len() > IMAGE_MAX_BYTES / 3 * 4 + 1024 * 1024 {
        return Err(ApiError::validation("the picture is larger than 5 MB"));
    }
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| ApiError::validation("the picture could not be read"))?;
    accept_picture(bytes)
}

/// Fetches a picture once from an `http` or `https` address.
async fn fetch_picture(address: &str) -> ApiResult<Picture> {
    let address = address.trim();
    let url = url::Url::parse(address)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
        .filter(|_| address.len() <= IMAGE_URL_MAX)
        .ok_or_else(|| ApiError::validation("the picture's address starts with https:// or http://"))?;
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(ApiError::internal)?;
    let fail = |e: String| ApiError::validation(format!("the picture could not be fetched: {e}"));
    let mut res = client.get(url).send().await.map_err(|e| fail(e.to_string()))?;
    if !res.status().is_success() {
        return Err(fail(format!("the server answered {}", res.status().as_u16())));
    }
    if res.content_length().is_some_and(|n| n > IMAGE_MAX_BYTES as u64) {
        return Err(ApiError::validation("the picture is larger than 5 MB"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| fail(e.to_string()))? {
        if bytes.len() + chunk.len() > IMAGE_MAX_BYTES {
            return Err(ApiError::validation("the picture is larger than 5 MB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    accept_picture(bytes)
}

/// The picture of a request, checked (and fetched) before anything is saved.
async fn picture_of(image_url: Option<&str>, image_data: Option<&str>) -> ApiResult<Option<Picture>> {
    let url = image_url.map(str::trim).filter(|s| !s.is_empty());
    let data = image_data.map(str::trim).filter(|s| !s.is_empty());
    match (url, data) {
        (Some(_), Some(_)) => Err(ApiError::validation("give image_url or image_data, not both")),
        (Some(u), None) => fetch_picture(u).await.map(Some),
        (None, Some(d)) => decode_data_url(d).map(Some),
        (None, None) => Ok(None),
    }
}

// ---------------- pictures on disk ----------------

const PICTURE_KINDS: [ImageKind; 3] = [ImageKind::Png, ImageKind::Jpeg, ImageKind::Webp];

/// Folder (relative to the data folder) of a kind's pictures.
fn picture_dir(kind: CustomItemKind) -> String {
    match kind {
        CustomItemKind::RadioShow => reelix_radio::artwork::ARTWORK_DIR.to_owned(),
        k => format!("custom/{}", k.as_str()),
    }
}

/// Path of the item's picture of `ext` relative to the data folder.
fn picture_rel(kind: CustomItemKind, id: i64, ext: &str) -> String {
    format!("{}/{id}.{ext}", picture_dir(kind))
}

/// The item's picture of `ext` inside the data folder. The name is built from a fixed kind and a
/// number, so it cannot leave the folder; this checks it anyway, so a later change cannot either:
/// every part must be a plain name (no `..`, no leading `/`) and the result must sit in `data_dir`.
fn picture_path(data_dir: &Path, kind: CustomItemKind, id: i64, ext: &str) -> Option<PathBuf> {
    let rel = picture_rel(kind, id, ext);
    let plain = Path::new(&rel)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    let path = data_dir.join(&rel);
    (plain && path.starts_with(data_dir)).then_some(path)
}

fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", hex::encode(&Sha256::digest(bytes)[..16]))
}

/// The address the item's picture is served at; `v` changes with the picture so a changed
/// picture is not served from a cache.
fn picture_address(kind: CustomItemKind, id: i64, bytes: &[u8]) -> String {
    let v = &etag(bytes)[1..9];
    format!("/api/v1/customitems/{}/{id}/image?v={v}", kind.as_str())
}

/// Removes every stored picture of the item (any type).
async fn remove_pictures(data_dir: &Path, kind: CustomItemKind, id: i64) {
    for k in PICTURE_KINDS {
        let Some(p) = picture_path(data_dir, kind, id, k.ext()) else {
            continue;
        };
        if p.exists()
            && let Err(e) = tokio::fs::remove_file(&p).await
        {
            tracing::warn!(path = %p.display(), error = %e, "old picture not removed");
        }
    }
}

/// Saves the picture (replacing any other type) and points the item at it.
async fn store_picture(state: &AppState, kind: CustomItemKind, id: i64, pic: &Picture) -> ApiResult<()> {
    let data_dir = state.config().data_dir.clone();
    remove_pictures(&data_dir, kind, id).await;
    let rel = picture_rel(kind, id, pic.kind.ext());
    let path: PathBuf = picture_path(&data_dir, kind, id, pic.kind.ext())
        .ok_or_else(|| ApiError::internal("picture path outside the data folder"))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(ApiError::internal)?;
    }
    tokio::fs::write(&path, &pic.bytes)
        .await
        .map_err(ApiError::internal)?;
    let address = picture_address(kind, id, &pic.bytes);
    set_picture(state, kind, id, Some(address), Some(&rel)).await
}

/// Points the item at its picture (`None` removes it).
async fn set_picture(
    state: &AppState,
    kind: CustomItemKind,
    id: i64,
    address: Option<String>,
    rel: Option<&str>,
) -> ApiResult<()> {
    match kind {
        CustomItemKind::Movie => {
            state
                .ext::<LibraryService>()?
                .set_custom_movie_poster(id, address)
                .await?
        }
        CustomItemKind::Series => {
            state
                .ext::<LibraryService>()?
                .set_custom_series_poster(id, address)
                .await?
        }
        CustomItemKind::Author => {
            state
                .ext::<BooksService>()?
                .set_custom_author_picture(id, address)
                .await?
        }
        CustomItemKind::Artist => {
            state
                .ext::<MusicService>()?
                .set_custom_artist_picture(id, address)
                .await?
        }
        CustomItemKind::RadioShow => {
            state
                .ext::<RadioService>()?
                .set_custom_show_artwork(id, rel)
                .await?
        }
    }
    Ok(())
}

/// The stored picture of the item, if any.
async fn read_picture(data_dir: &Path, kind: CustomItemKind, id: i64) -> Option<StoredImage> {
    for k in PICTURE_KINDS {
        let Some(path) = picture_path(data_dir, kind, id, k.ext()) else {
            continue;
        };
        if let Ok(bytes) = tokio::fs::read(path).await {
            return Some(StoredImage {
                etag: etag(&bytes),
                bytes,
                content_type: k.content_type(),
            });
        }
    }
    None
}

fn kind_of(s: &str) -> ApiResult<CustomItemKind> {
    CustomItemKind::parse(s).ok_or_else(|| {
        ApiError::not_found(format!(
            "{s} is not a kind of custom item (movie, series, author, artist or radio_show)"
        ))
    })
}

// ---------------- routes ----------------

/// Adds a film, series, author, artist or radio show with your own details and no outside
/// match. Films, series, authors and artists come from an existing folder of a root folder of
/// the kind (an author's sub-folders and files become books, an artist's sub-folders albums,
/// a series' files episodes by season and episode); a radio show comes from an Audiobookshelf
/// podcast item (its files become episodes). Metadata refreshes keep the typed details.
#[utoipa::path(post, path = "/api/v1/customitems", tag = "custom items", request_body = AddCustomItemRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added; `id` works in the kind's own routes", body = CustomItemDto),
        (status = 400, description = "A folder outside the root folder, a missing field or a bad picture", body = ApiErrorBody),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 409, description = "The folder (or podcast) already belongs to an item", body = ApiErrorBody),
    ))]
pub async fn add_item(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddCustomItemRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let details = CustomDetails {
        title: clean_title(&req.title)?,
        year: check_year(req.year)?,
        overview: clean_text(req.overview.as_deref(), OVERVIEW_MAX, "overview")?,
    };
    let station = clean_text(req.station.as_deref(), STATION_MAX, "station")?;
    let place = || -> ApiResult<(i64, String)> {
        let root = req
            .root_folder_id
            .ok_or_else(|| ApiError::validation("root_folder_id is required"))?;
        let path = req
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ApiError::validation("path is required"))?;
        Ok((root, path.to_owned()))
    };
    let (profile, monitored) = (req.quality_profile_id, req.monitored);
    // Checked and fetched before the item is made, so a bad picture adds nothing.
    let picture = picture_of(req.image_url.as_deref(), req.image_data.as_deref()).await?;
    let kind = req.kind;
    let item = match kind {
        CustomItemKind::Movie => {
            let (root, path) = place()?;
            let m = state
                .ext::<LibraryService>()?
                .add_custom_movie(&details, root, &path, profile, monitored)
                .await?;
            CustomItemDto {
                id: m.id,
                kind,
                title: m.title,
                path: Some(m.path),
            }
        }
        CustomItemKind::Series => {
            let (root, path) = place()?;
            let s = state
                .ext::<LibraryService>()?
                .add_custom_series(&details, root, &path, profile, monitored)
                .await?;
            CustomItemDto {
                id: s.id,
                kind,
                title: s.title,
                path: Some(s.path),
            }
        }
        CustomItemKind::Author => {
            let (root, path) = place()?;
            let a = state
                .ext::<BooksService>()?
                .add_custom_author(&details, root, &path, profile, monitored)
                .await?;
            CustomItemDto {
                id: a.id,
                kind,
                title: a.name,
                path: a.path.or(a.audio_path),
            }
        }
        CustomItemKind::Artist => {
            let (root, path) = place()?;
            let a = state
                .ext::<MusicService>()?
                .add_custom_artist(&details, root, &path, profile, monitored)
                .await?;
            CustomItemDto {
                id: a.id,
                kind,
                title: a.name,
                path: Some(a.path),
            }
        }
        CustomItemKind::RadioShow => {
            let item = req
                .abs_item_id
                .as_deref()
                .map(str::trim)
                .filter(|i| !i.is_empty())
                .ok_or_else(|| ApiError::validation("abs_item_id is required"))?;
            let s = state
                .ext::<RadioService>()?
                .add_custom_show(&details, item, station.as_deref(), profile, monitored)
                .await?;
            CustomItemDto {
                id: s.id,
                kind,
                title: s.title,
                path: None,
            }
        }
    };
    if let Some(p) = &picture
        && let Err(e) = store_picture(&state, kind, item.id, p).await
    {
        // The item is added; only its picture is missing.
        tracing::warn!(kind = kind.as_str(), id = item.id, error = %e, "custom item picture not saved");
    }
    Ok((StatusCode::CREATED, ApiJson(item)).into_response())
}

/// Changes the typed details or the picture of an item added with `POST /customitems` (items
/// with an outside match are refused). Authors and artists have no year; `station` is for
/// radio shows only.
#[utoipa::path(put, path = "/api/v1/customitems/{kind}/{id}", tag = "custom items", request_body = CustomItemEdit,
    params(
        ("kind" = String, Path, description = "movie, series, author, artist or radio_show"),
        ("id" = i64, Path, description = "The item's id in its kind's routes"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Changed", body = CustomItemDto),
        (status = 400, description = "Bad details or picture, or the item has an outside match", body = ApiErrorBody),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn edit_item(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((kind, id)): ApiPath<(String, i64)>,
    ApiJson(e): ApiJson<CustomItemEdit>,
) -> ApiResult<ApiJson<CustomItemDto>> {
    require_lib(&user)?;
    let kind = kind_of(&kind)?;
    let change = CustomDetailsChange {
        title: e.title.as_deref().map(clean_title).transpose()?,
        year: e.year.map(check_year).transpose()?,
        overview: e
            .overview
            .map(|o| clean_text(o.as_deref(), OVERVIEW_MAX, "overview"))
            .transpose()?,
    };
    let station = e
        .station
        .map(|s| clean_text(s.as_deref(), STATION_MAX, "station"))
        .transpose()?;
    if e.remove_image && (e.image_url.is_some() || e.image_data.is_some()) {
        return Err(ApiError::validation(
            "give a new picture or remove_image, not both",
        ));
    }
    let picture = picture_of(e.image_url.as_deref(), e.image_data.as_deref()).await?;
    let item = match kind {
        CustomItemKind::Movie => {
            let m = state
                .ext::<LibraryService>()?
                .change_custom_movie(id, &change)
                .await?;
            CustomItemDto {
                id,
                kind,
                title: m.title,
                path: Some(m.path),
            }
        }
        CustomItemKind::Series => {
            let s = state
                .ext::<LibraryService>()?
                .change_custom_series(id, &change)
                .await?;
            CustomItemDto {
                id,
                kind,
                title: s.title,
                path: Some(s.path),
            }
        }
        CustomItemKind::Author => {
            let a = state
                .ext::<BooksService>()?
                .change_custom_author(id, &change)
                .await?;
            CustomItemDto {
                id,
                kind,
                title: a.name,
                path: a.path.or(a.audio_path),
            }
        }
        CustomItemKind::Artist => {
            let a = state
                .ext::<MusicService>()?
                .change_custom_artist(id, &change)
                .await?;
            CustomItemDto {
                id,
                kind,
                title: a.name,
                path: Some(a.path),
            }
        }
        CustomItemKind::RadioShow => {
            let s = state
                .ext::<RadioService>()?
                .change_custom_show(id, &change, station.as_ref().map(|s| s.as_deref()))
                .await?;
            CustomItemDto {
                id,
                kind,
                title: s.title,
                path: None,
            }
        }
    };
    if let Some(p) = &picture {
        store_picture(&state, kind, id, p).await?;
    } else if e.remove_image {
        remove_pictures(&state.config().data_dir, kind, id).await;
        set_picture(&state, kind, id, None, None).await?;
    }
    Ok(ApiJson(item))
}

/// The picture of an item added with `POST /customitems`.
#[utoipa::path(get, path = "/api/v1/customitems/{kind}/{id}/image", tag = "custom items",
    params(
        ("kind" = String, Path, description = "movie, series, author, artist or radio_show"),
        ("id" = i64, Path, description = "The item's id in its kind's routes"),
    ),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "The picture (PNG, JPEG or WebP)"),
        (status = 304, description = "Not modified (If-None-Match)"),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
        (status = 404, description = "No picture", body = ApiErrorBody),
    ))]
pub async fn item_image(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath((kind, id)): ApiPath<(String, i64)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_lib(&user)?;
    let kind = kind_of(&kind)?;
    let img = read_picture(&state.config().data_dir, kind, id)
        .await
        .ok_or_else(|| ApiError::not_found(format!("{} {id} has no picture", kind.as_str())))?;
    Ok(crate::routes::radio::image_response(img, &headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn data_url(mime: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn titles_years_and_text() {
        assert_eq!(clean_title("  Home   videos ").unwrap(), "Home videos");
        assert!(clean_title("   ").is_err());
        assert!(clean_title(&"x".repeat(201)).is_err());
        assert!(clean_title(&"é".repeat(200)).is_ok());
        assert_eq!(check_year(Some(2004)).unwrap(), Some(2004));
        assert!(check_year(Some(1799)).is_err());
        assert!(check_year(Some(2101)).is_err());
        assert_eq!(clean_text(Some("  "), 10, "overview").unwrap(), None);
        assert!(clean_text(Some(&"a".repeat(11)), 10, "overview").is_err());
    }

    #[test]
    fn data_addresses() {
        let p = decode_data_url(&data_url("image/png", PNG)).unwrap();
        assert_eq!(p.kind, ImageKind::Png);
        assert_eq!(p.bytes, PNG);
        // The type comes from the bytes, but only the three types are accepted at all.
        assert!(decode_data_url(&data_url("image/gif", PNG)).is_err());
        assert!(decode_data_url(&data_url("image/png", b"<html>no</html>")).is_err());
        assert!(decode_data_url(&data_url("image/png", b"<svg></svg>")).is_err());
        assert!(decode_data_url("data:image/png;base64,***").is_err());
        assert!(decode_data_url("https://example.com/a.png").is_err());
        let mut big = PNG.to_vec();
        big.resize(IMAGE_MAX_BYTES + 1, 0);
        assert!(decode_data_url(&data_url("image/png", &big)).is_err());
    }

    #[test]
    fn picture_paths_stay_in_the_data_folder() {
        let data = Path::new("/data");
        assert_eq!(
            picture_path(data, CustomItemKind::Series, -7, "webp"),
            Some(PathBuf::from("/data/custom/series/-7.webp"))
        );
        assert_eq!(
            picture_path(data, CustomItemKind::Movie, 4, "png/../../../etc/x"),
            None
        );
        assert_eq!(picture_path(data, CustomItemKind::Movie, 4, "png/../.."), None);
    }

    #[test]
    fn picture_places() {
        assert_eq!(picture_rel(CustomItemKind::Movie, 4, "png"), "custom/movie/4.png");
        assert_eq!(
            picture_rel(CustomItemKind::RadioShow, 4, "jpg"),
            "radio/artwork/4.jpg"
        );
        let a = picture_address(CustomItemKind::RadioShow, 4, PNG);
        assert!(a.starts_with("/api/v1/customitems/radio_show/4/image?v="));
        assert_ne!(a, picture_address(CustomItemKind::RadioShow, 4, b"other"));
    }
}
