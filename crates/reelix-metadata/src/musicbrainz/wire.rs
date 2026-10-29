//! MusicBrainz / Cover Art Archive JSON wire types (private to the client).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Search scores are numbers today but were strings in older responses.
fn score<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let v = Value::deserialize(d)?;
    let n = v
        .as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0);
    Ok(n.min(100) as u8)
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub(super) struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub(super) struct ArtistCredit {
    pub name: String,
    pub joinphrase: String,
    pub artist: ArtistRef,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Genre {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Url {
    pub resource: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Relation {
    pub url: Option<Url>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct Artist {
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub disambiguation: Option<String>,
    #[serde(rename = "type")]
    pub artist_type: Option<String>,
    pub country: Option<String>,
    #[serde(deserialize_with = "score")]
    pub score: u8,
    pub genres: Vec<Genre>,
    pub relations: Vec<Relation>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct ArtistSearch {
    pub artists: Vec<Artist>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Rating {
    pub value: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ReleaseGroup {
    pub id: String,
    pub title: String,
    pub disambiguation: Option<String>,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub first_release_date: Option<String>,
    pub artist_credit: Vec<ArtistCredit>,
    pub genres: Vec<Genre>,
    pub rating: Option<Rating>,
    #[serde(deserialize_with = "score")]
    pub score: u8,
}

/// Search (`release-groups` + `count`) and browse (`release-group-count`) share this shape.
#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ReleaseGroupList {
    pub release_groups: Vec<ReleaseGroup>,
    pub release_group_count: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Recording {
    pub id: String,
    pub title: String,
    pub length: Option<u64>,
    #[serde(rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct Track {
    pub id: String,
    pub number: String,
    pub position: u32,
    pub title: String,
    pub length: Option<u64>,
    pub recording: Recording,
    pub artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Medium {
    pub position: u32,
    pub format: Option<String>,
    pub title: Option<String>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Label {
    pub name: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct LabelInfo {
    pub catalog_number: Option<String>,
    pub label: Option<Label>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct GroupRef {
    pub id: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct Release {
    pub id: String,
    pub title: String,
    pub status: Option<String>,
    pub country: Option<String>,
    pub date: Option<String>,
    pub barcode: Option<String>,
    pub disambiguation: Option<String>,
    pub label_info: Vec<LabelInfo>,
    pub media: Vec<Medium>,
    pub release_group: Option<GroupRef>,
    pub artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ReleaseList {
    pub releases: Vec<Release>,
    pub release_count: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Thumbnails {
    #[serde(rename = "250")]
    pub t250: Option<String>,
    #[serde(rename = "500")]
    pub t500: Option<String>,
    pub small: Option<String>,
    pub large: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Image {
    pub types: Vec<String>,
    pub front: bool,
    pub image: Option<String>,
    pub thumbnails: Thumbnails,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct CoverIndex {
    pub images: Vec<Image>,
}
