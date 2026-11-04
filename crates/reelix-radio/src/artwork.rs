//! Show artwork and station logos: downloaded during refresh (and when a show is added), never
//! on a page view, and served by id.
//!
//! - Files: `<data_dir>/radio/artwork/{show_id}.{ext}` and
//!   `<data_dir>/radio/logos/{station_id}.{ext}`; the columns hold the path relative to the
//!   data folder. Next to each file, `{id}.source` holds the URL it came from, so a file is
//!   downloaded again only when the source URL changes (or the file is gone).
//! - Limits: [`MAX_ARTWORK_BYTES`] (2 MB) and [`MAX_LOGO_BYTES`] (256 KB).
//! - Types: JPEG, PNG, WebP and SVG only, recognised from the bytes (the server's
//!   `Content-Type` is not trusted). SVG is cleaned ([`crate::svg::sanitise`]); one that
//!   cannot be cleaned is refused.
//! - A failed download keeps the previous file (or none) and never fails the refresh.

use std::path::{Path, PathBuf};

use reelix_api_types::radio::RadioStationDto;
use sha2::{Digest, Sha256};

use crate::error::{RadioError, Result};
use crate::providers::http::{ReadError, read_limited};
use crate::{RadioService, entities, repo, svg};

pub const MAX_ARTWORK_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LOGO_BYTES: usize = 256 * 1024;

/// Folders under the data folder.
pub const ARTWORK_DIR: &str = "radio/artwork";
pub const LOGO_DIR: &str = "radio/logos";

/// Image types Reelix keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    Webp,
    Svg,
}

impl ImageKind {
    pub const ALL: [ImageKind; 4] = [Self::Jpeg, Self::Png, Self::Webp, Self::Svg];

    pub fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.ext() == ext)
    }
}

/// The image type from the first bytes; `None` for anything else (HTML, GIF, …).
pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageKind::Jpeg);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageKind::Png);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::Webp);
    }
    // SVG: XML text whose first element is <svg> (after a declaration and comments).
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let mut rest = head.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("<?") {
            rest = r.split_once("?>")?.1.trim_start();
        } else if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.split_once("-->")?.1.trim_start();
        } else {
            break;
        }
    }
    let after = rest.strip_prefix("<svg")?;
    after
        .starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .then_some(ImageKind::Svg)
}

/// A stored image, ready to serve.
#[derive(Debug, Clone)]
pub struct StoredImage {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    /// Strong ETag (quoted) from the content.
    pub etag: String,
}

impl StoredImage {
    pub fn is_svg(&self) -> bool {
        self.content_type == ImageKind::Svg.content_type()
    }
}

/// Why an image was not saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("the image server answered {0}")]
    Status(u16),
    #[error("the image server could not be reached: {0}")]
    Network(String),
    #[error("the image is larger than {0} KB")]
    TooLarge(usize),
    #[error("the file is not a JPEG, PNG, WebP or SVG image")]
    NotAnImage,
    #[error("the SVG was refused: {0}")]
    BadSvg(String),
    #[error("{0}")]
    Io(String),
}

/// Checks downloaded bytes: a known type within `max`, SVG cleaned. Returns what to store.
pub fn accept_image(bytes: &[u8], max: usize) -> std::result::Result<(ImageKind, Vec<u8>), ImageError> {
    if bytes.len() > max {
        return Err(ImageError::TooLarge(max / 1024));
    }
    let kind = sniff(bytes).ok_or(ImageError::NotAnImage)?;
    let data = match kind {
        ImageKind::Svg => svg::sanitise(bytes).map_err(ImageError::BadSvg)?,
        _ => bytes.to_vec(),
    };
    Ok((kind, data))
}

fn etag(bytes: &[u8]) -> String {
    let h = Sha256::digest(bytes);
    format!("\"{}\"", hex::encode(&h[..16]))
}

/// The stored relative path for `id` in `dir`, if `rel` is exactly such a path.
fn checked_rel(rel: &str, dir: &str, id: i64) -> Option<ImageKind> {
    let name = rel.strip_prefix(dir)?.strip_prefix('/')?;
    let (stem, ext) = name.split_once('.')?;
    (stem == id.to_string()).then_some(())?;
    ImageKind::from_ext(ext)
}

impl RadioService {
    pub async fn get_station(&self, id: i64) -> Result<RadioStationDto> {
        repo::stations::get(&self.db, id)
            .await?
            .ok_or_else(|| RadioError::NotFound(format!("radio station {id} not found")))
    }

    pub async fn show_artwork_path(&self, show_id: i64) -> Result<Option<String>> {
        repo::shows::artwork_path(&self.db, show_id).await
    }

    pub async fn set_show_artwork_path(&self, show_id: i64, path: Option<&str>) -> Result<()> {
        repo::shows::set_artwork_path(&self.db, show_id, path).await?;
        self.emit(entities::SHOW, "updated", show_id);
        Ok(())
    }

    pub async fn station_logo_path(&self, station_id: i64) -> Result<Option<String>> {
        repo::stations::logo_path(&self.db, station_id).await
    }

    pub async fn set_station_logo_path(&self, station_id: i64, path: Option<&str>) -> Result<()> {
        repo::stations::set_logo_path(&self.db, station_id, path).await?;
        self.emit("radio_station", "updated", station_id);
        Ok(())
    }

    /// The show's downloaded artwork; `None` when there is none (or the file is gone).
    pub async fn show_artwork(&self, show_id: i64) -> Result<Option<StoredImage>> {
        repo::shows::require(&self.db, show_id).await?;
        let rel = self.show_artwork_path(show_id).await?;
        Ok(self.read_image(rel.as_deref(), ARTWORK_DIR, show_id).await)
    }

    /// The station's downloaded logo; `None` when there is none.
    pub async fn station_logo(&self, station_id: i64) -> Result<Option<StoredImage>> {
        self.get_station(station_id).await?;
        let rel = self.station_logo_path(station_id).await?;
        Ok(self.read_image(rel.as_deref(), LOGO_DIR, station_id).await)
    }

    async fn read_image(&self, rel: Option<&str>, dir: &str, id: i64) -> Option<StoredImage> {
        let rel = rel?;
        let kind = checked_rel(rel, dir, id)?;
        let path = self.data_dir.get()?.join(rel);
        let bytes = tokio::fs::read(&path).await.ok()?;
        Some(StoredImage {
            etag: etag(&bytes),
            bytes,
            content_type: kind.content_type(),
        })
    }

    /// Downloads the show's artwork and its station's logo when their source URL changed.
    /// Failures are logged and leave the saved files as they were.
    pub(crate) async fn download_artwork(&self, show_id: i64) {
        let Some(data_dir) = self.data_dir.get().cloned() else {
            return;
        };
        let Ok(show) = repo::shows::require(&self.db, show_id).await else {
            return;
        };
        if let Some(url) = show.artwork_url.as_deref() {
            let current = self.show_artwork_path(show_id).await.ok().flatten();
            match self
                .refresh_image(
                    &data_dir,
                    ARTWORK_DIR,
                    show_id,
                    url,
                    current.as_deref(),
                    MAX_ARTWORK_BYTES,
                )
                .await
            {
                Ok(Some(rel)) => {
                    if let Err(e) = self.set_show_artwork_path(show_id, Some(&rel)).await {
                        tracing::warn!(show_id, error = %e, "radio artwork path not saved");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(show_id, url, error = %e, "radio show artwork not saved"),
            }
        }
        let Some(station_id) = show.station_id else {
            return;
        };
        let Ok(station) = self.get_station(station_id).await else {
            return;
        };
        if let Some(url) = station.logo_url.as_deref() {
            let current = self.station_logo_path(station_id).await.ok().flatten();
            match self
                .refresh_image(
                    &data_dir,
                    LOGO_DIR,
                    station_id,
                    url,
                    current.as_deref(),
                    MAX_LOGO_BYTES,
                )
                .await
            {
                Ok(Some(rel)) => {
                    if let Err(e) = self.set_station_logo_path(station_id, Some(&rel)).await {
                        tracing::warn!(station_id, error = %e, "radio logo path not saved");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(station_id, url, error = %e, "radio station logo not saved"),
            }
        }
    }

    /// Downloads `url` into `<data_dir>/<dir>/<id>.<ext>` unless the saved file came from the
    /// same URL. `Ok(Some(rel))` = a new file was saved (relative path).
    async fn refresh_image(
        &self,
        data_dir: &Path,
        dir: &str,
        id: i64,
        url: &str,
        current: Option<&str>,
        max: usize,
    ) -> std::result::Result<Option<String>, ImageError> {
        let folder = data_dir.join(dir);
        let source_file = folder.join(format!("{id}.source"));
        if let Some(rel) = current
            && checked_rel(rel, dir, id).is_some()
            && tokio::fs::try_exists(data_dir.join(rel)).await.unwrap_or(false)
            && tokio::fs::read_to_string(&source_file)
                .await
                .is_ok_and(|s| s.trim() == url)
        {
            return Ok(None);
        }
        let parsed = url::Url::parse(url).map_err(|_| ImageError::NotAnImage)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ImageError::NotAnImage);
        }
        let res = self
            .http
            .get(parsed)
            .header(
                reqwest::header::ACCEPT,
                "image/jpeg, image/png, image/webp, image/svg+xml;q=0.9",
            )
            .send()
            .await
            .map_err(|e| ImageError::Network(e.without_url().to_string()))?;
        if !res.status().is_success() {
            return Err(ImageError::Status(res.status().as_u16()));
        }
        let bytes = read_limited(res, max).await.map_err(|e| match e {
            ReadError::TooLarge => ImageError::TooLarge(max / 1024),
            ReadError::Network(e) => ImageError::Network(e.without_url().to_string()),
        })?;
        let (kind, data) = accept_image(&bytes, max)?;
        let rel = format!("{dir}/{id}.{}", kind.ext());
        save(&folder, id, kind, &data, &source_file, url)
            .await
            .map_err(|e| ImageError::Io(format!("cannot save the image: {e}")))?;
        Ok(Some(rel))
    }
}

/// Writes the file atomically, removes the same id's files of other types, records the URL.
async fn save(
    folder: &PathBuf,
    id: i64,
    kind: ImageKind,
    data: &[u8],
    source_file: &Path,
    url: &str,
) -> std::io::Result<()> {
    tokio::fs::create_dir_all(folder).await?;
    let target = folder.join(format!("{id}.{}", kind.ext()));
    let tmp = folder.join(format!(".{id}.{}.part", std::process::id()));
    tokio::fs::write(&tmp, data).await?;
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    for other in ImageKind::ALL.into_iter().filter(|k| *k != kind) {
        let _ = tokio::fs::remove_file(folder.join(format!("{id}.{}", other.ext()))).await;
    }
    tokio::fs::write(source_file, url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Some(ImageKind::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(ImageKind::Png));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageKind::Webp));
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?>\n<!-- logo --><svg xmlns=\"x\"/>"),
            Some(ImageKind::Svg)
        );
        assert_eq!(sniff(b"<svg>"), Some(ImageKind::Svg));
        assert_eq!(sniff(b"<svgfoo>"), None);
        assert_eq!(sniff(b"<html><svg/></html>"), None);
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn size_caps() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF];
        jpeg.resize(MAX_LOGO_BYTES + 1, 0);
        assert_eq!(
            accept_image(&jpeg, MAX_LOGO_BYTES),
            Err(ImageError::TooLarge(256))
        );
        assert!(accept_image(&jpeg, MAX_ARTWORK_BYTES).is_ok());
        assert_eq!(
            accept_image(b"<html></html>", MAX_ARTWORK_BYTES),
            Err(ImageError::NotAnImage)
        );
        assert!(matches!(
            accept_image(b"<svg><script>", MAX_LOGO_BYTES),
            Err(ImageError::BadSvg(_))
        ));
    }

    #[test]
    fn stored_paths_are_checked() {
        assert_eq!(
            checked_rel("radio/artwork/7.jpg", ARTWORK_DIR, 7),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(
            checked_rel("radio/logos/3.svg", LOGO_DIR, 3),
            Some(ImageKind::Svg)
        );
        assert_eq!(checked_rel("radio/artwork/8.jpg", ARTWORK_DIR, 7), None);
        assert_eq!(checked_rel("radio/artwork/../../etc/7.jpg", ARTWORK_DIR, 7), None);
        assert_eq!(checked_rel("radio/artwork/7.gif", ARTWORK_DIR, 7), None);
        assert_eq!(checked_rel("/abs/7.jpg", ARTWORK_DIR, 7), None);
    }
}
