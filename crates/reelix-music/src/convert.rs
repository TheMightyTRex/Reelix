//! Conversions between DTO enums, database text columns and metadata strings.

use chrono::NaiveDate;
use reelix_api_types::music::{AlbumType, ReleaseStatus, SecondaryAlbumType};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Serde wire name of a unit enum variant (`"pseudo_release"`).
pub fn enum_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Parses a serde wire name back into the enum.
pub fn parse_enum<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

pub fn json<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn from_json<T: DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn date_str(d: Option<NaiveDate>) -> Option<String> {
    d.map(|d| d.format("%Y-%m-%d").to_string())
}

/// `YYYY-MM-DD`, `YYYY-MM` or `YYYY` (MusicBrainz partial dates; missing parts = 1).
pub fn parse_date(s: Option<&str>) -> Option<NaiveDate> {
    let s = s?.trim();
    let mut parts = s.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    let d: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    NaiveDate::from_ymd_opt(y, m, d)
}

/// Comparison key for names and titles (see [`reelix_import::matching::normalize_title`]).
pub fn clean_name(s: &str) -> String {
    reelix_import::matching::normalize_title(s)
}

/// MusicBrainz primary type → [`AlbumType`] (unknown → `Other`, missing → `Album`).
pub fn album_type(s: Option<&str>) -> AlbumType {
    match s.map(|s| s.trim().to_ascii_lowercase()) {
        None => AlbumType::Album,
        Some(s) => match s.as_str() {
            "album" | "" => AlbumType::Album,
            "ep" => AlbumType::Ep,
            "single" => AlbumType::Single,
            "broadcast" => AlbumType::Broadcast,
            _ => AlbumType::Other,
        },
    }
}

/// MusicBrainz secondary type → [`SecondaryAlbumType`].
pub fn secondary_type(s: &str) -> Option<SecondaryAlbumType> {
    let k: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    Some(match k.as_str() {
        "compilation" => SecondaryAlbumType::Compilation,
        "soundtrack" => SecondaryAlbumType::Soundtrack,
        "spokenword" => SecondaryAlbumType::Spokenword,
        "interview" => SecondaryAlbumType::Interview,
        "audiobook" => SecondaryAlbumType::Audiobook,
        "audiodrama" => SecondaryAlbumType::AudioDrama,
        "live" => SecondaryAlbumType::Live,
        "remix" => SecondaryAlbumType::Remix,
        "djmix" => SecondaryAlbumType::DjMix,
        "mixtape" | "mixtapestreet" => SecondaryAlbumType::Mixtape,
        "demo" => SecondaryAlbumType::Demo,
        "fieldrecording" => SecondaryAlbumType::FieldRecording,
        _ => return None,
    })
}

/// MusicBrainz release status → [`ReleaseStatus`].
pub fn release_status(s: Option<&str>) -> Option<ReleaseStatus> {
    let k: String = s?
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    Some(match k.as_str() {
        "official" => ReleaseStatus::Official,
        "promotion" | "promo" => ReleaseStatus::Promotion,
        "bootleg" => ReleaseStatus::Bootleg,
        "pseudorelease" => ReleaseStatus::PseudoRelease,
        _ => return None,
    })
}

/// Display name of an album type (`{Album Type}` token).
pub fn album_type_name(t: AlbumType) -> &'static str {
    match t {
        AlbumType::Album => "Album",
        AlbumType::Ep => "EP",
        AlbumType::Single => "Single",
        AlbumType::Broadcast => "Broadcast",
        AlbumType::Other => "Other",
    }
}

/// `Artist A feat. Artist B` from a credit list.
pub fn credit_name(credits: &[reelix_metadata::ArtistCredit]) -> Option<String> {
    if credits.is_empty() {
        return None;
    }
    let mut s = String::new();
    for c in credits {
        s.push_str(&c.name);
        s.push_str(&c.join_phrase);
    }
    Some(s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mb_strings() {
        assert_eq!(album_type(Some("EP")), AlbumType::Ep);
        assert_eq!(album_type(None), AlbumType::Album);
        assert_eq!(album_type(Some("Audio drama")), AlbumType::Other);
        assert_eq!(secondary_type("DJ-mix"), Some(SecondaryAlbumType::DjMix));
        assert_eq!(
            secondary_type("Audio drama"),
            Some(SecondaryAlbumType::AudioDrama)
        );
        assert_eq!(
            release_status(Some("Pseudo-Release")),
            Some(ReleaseStatus::PseudoRelease)
        );
        assert_eq!(parse_date(Some("1997")), NaiveDate::from_ymd_opt(1997, 1, 1));
        assert_eq!(
            parse_date(Some("1997-05-21")),
            NaiveDate::from_ymd_opt(1997, 5, 21)
        );
        assert_eq!(parse_date(Some("")), None);
        assert_eq!(enum_str(&ReleaseStatus::PseudoRelease), "pseudo_release");
    }
}
