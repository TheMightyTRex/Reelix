//! Items added with the owner's own details and no outside match (RFX-90): a film, series,
//! author, artist or radio show made from a folder (or an Audiobookshelf podcast item) with a
//! typed title, year, overview and picture. `POST /customitems` adds one, `PUT
//! /customitems/{kind}/{id}` changes its details and `GET /customitems/{kind}/{id}/image`
//! serves its picture.
//!
//! The item is an ordinary item of its kind (`/movies/{id}`, `/series/{id}`, `/authors/{id}`,
//! `/artists/{id}`, `/radio/shows/{id}`) with `custom: true`; metadata refreshes keep the
//! typed details.

use serde::{Deserialize, Deserializer, Serialize};

/// Longest title (or name) in characters.
pub const TITLE_MAX: usize = 200;
/// Longest overview in characters.
pub const OVERVIEW_MAX: usize = 4000;
/// Longest station name in characters.
pub const STATION_MAX: usize = 200;
/// Longest picture address in characters.
pub const IMAGE_URL_MAX: usize = 2000;
/// Largest picture, once decoded or downloaded.
pub const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const YEAR_MIN: i32 = 1800;
pub const YEAR_MAX: i32 = 2100;

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// What a custom item is made as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CustomItemKind {
    /// A film: one folder in a film root folder (`/rootfolders`).
    Movie,
    /// A series: one folder in a series root folder; files are filed by season and episode.
    Series,
    /// An author: one folder in a book root folder (`/bookrootfolders`); its sub-folders and
    /// files become books.
    Author,
    /// An artist: one folder in a music root folder (`/musicrootfolders`); its sub-folders
    /// become albums.
    Artist,
    /// A radio show: a podcast item of the Audiobookshelf library (`/radio/library/review`);
    /// its files become episodes.
    RadioShow,
}

impl CustomItemKind {
    pub const ALL: [CustomItemKind; 5] = [
        Self::Movie,
        Self::Series,
        Self::Author,
        Self::Artist,
        Self::RadioShow,
    ];

    /// `movie`, `series`, `author`, `artist` or `radio_show` (as in the JSON and the URLs).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Author => "author",
            Self::Artist => "artist",
            Self::RadioShow => "radio_show",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// `POST /customitems`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AddCustomItemRequest {
    pub kind: CustomItemKind,
    /// A root folder of the kind (`movie`, `series`, `author` and `artist`): `/rootfolders`,
    /// `/bookrootfolders` (ebook or audiobook) or `/musicrootfolders`.
    #[serde(default)]
    pub root_folder_id: Option<i64>,
    /// An existing folder inside the root folder (not for `radio_show`).
    #[serde(default)]
    pub path: Option<String>,
    /// `radio_show` only: the Audiobookshelf podcast item (`/radio/library/review`).
    #[serde(default)]
    pub abs_item_id: Option<String>,
    /// 1 to 200 characters; the author's or artist's name for those kinds.
    pub title: String,
    /// 1800 to 2100. Kept for films, series and radio shows (as the first broadcast's year);
    /// authors and artists have no year.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(minimum = 1800, maximum = 2100))]
    pub year: Option<i32>,
    /// Up to 4000 characters.
    #[serde(default)]
    pub overview: Option<String>,
    /// `radio_show` only: the station's name, up to 200 characters.
    #[serde(default)]
    pub station: Option<String>,
    /// An `http` or `https` address of a PNG, JPEG or WebP picture; Reelix fetches it once and
    /// keeps its own copy. Give this or `image_data`.
    #[serde(default)]
    pub image_url: Option<String>,
    /// `data:image/png;base64,…` (or `image/jpeg`, `image/webp`), up to 5 MB decoded.
    #[serde(default)]
    pub image_data: Option<String>,
    /// Reelix's first quality profile of the kind when absent.
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    /// Defaults to false: nothing is searched for or downloaded.
    #[serde(default)]
    pub monitored: bool,
}

/// `PUT /customitems/{kind}/{id}` (partial; `null` clears `year`, `overview`, `station` and
/// the picture).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CustomItemEdit {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<i32>, minimum = 1800, maximum = 2100))]
    pub year: Option<Option<i32>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub overview: Option<Option<String>>,
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub station: Option<Option<String>>,
    /// A new picture from an address (as for adding).
    #[serde(default)]
    pub image_url: Option<String>,
    /// A new picture as a `data:` address (as for adding).
    #[serde(default)]
    pub image_data: Option<String>,
    /// True removes the picture.
    #[serde(default)]
    pub remove_image: bool,
}

/// `POST /customitems/{kind}/{id}/match`: the outside match that replaces the typed details.
/// Give the id of the item's kind: `tmdb_id` for a film, `tmdb_id` or `tvdb_id` for a series,
/// `openlibrary_id` (`OL…A`) for an author, `musicbrainz_id` for an artist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CustomItemMatch {
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub tvdb_id: Option<u32>,
    #[serde(default)]
    pub openlibrary_id: Option<String>,
    #[serde(default)]
    pub musicbrainz_id: Option<String>,
}

/// The answer to `POST` and `PUT /customitems` and to `match`: `id` works in the kind's own
/// routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CustomItemDto {
    pub id: i64,
    pub kind: CustomItemKind,
    pub title: String,
    /// The item's folder (`None` for a radio show, whose files are in Audiobookshelf).
    pub path: Option<String>,
}

/// Typed details as the services store them (checked by the API).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomDetails {
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
}

/// A change of typed details (`None` leaves a field; `Some(None)` clears it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomDetailsChange {
    pub title: Option<String>,
    pub year: Option<Option<i32>>,
    pub overview: Option<Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip() {
        for k in CustomItemKind::ALL {
            assert_eq!(CustomItemKind::parse(k.as_str()), Some(k));
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(json, format!("\"{}\"", k.as_str()));
        }
        assert_eq!(CustomItemKind::parse("book"), None);
    }

    #[test]
    fn request_defaults_and_nulls() {
        let r: AddCustomItemRequest =
            serde_json::from_str(r#"{"kind":"radio_show","abs_item_id":"li_1","title":"Show"}"#).unwrap();
        assert!(!r.monitored);
        assert_eq!(r.kind, CustomItemKind::RadioShow);
        let e: CustomItemEdit = serde_json::from_str(r#"{"overview":null,"title":"New"}"#).unwrap();
        assert_eq!(e.overview, Some(None));
        assert_eq!(e.year, None);
        assert_eq!(e.title.as_deref(), Some("New"));
    }
}
