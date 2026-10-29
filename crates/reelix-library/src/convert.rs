//! Conversions between engine types, DTOs and database text columns.

use chrono::{NaiveDate, TimeZone, Utc};
use reelix_api_types::library::{
    ImagesDto, MediaInfoDto, MinimumAvailability, MovieStatus, NamingSettings, RatingsDto, SeriesStatus,
    SeriesType,
};
use reelix_import::mediainfo::MediaInfo;
use reelix_import::naming::{NamingConfig, audio_codec_name, video_codec_name};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Serde wire name of a unit enum variant (`"in_cinemas"`).
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

pub fn date_str(d: Option<NaiveDate>) -> Option<String> {
    d.map(|d| d.format("%Y-%m-%d").to_string())
}

pub fn parse_date(s: Option<String>) -> Option<NaiveDate> {
    s.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
}

pub fn date_utc(d: NaiveDate) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
}

pub fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn from_json<T: DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn images(i: &reelix_metadata::Images) -> ImagesDto {
    ImagesDto {
        poster: i.poster.clone(),
        backdrop: i.backdrop.clone(),
        logo: i.logo.clone(),
    }
}

pub fn ratings(r: &reelix_metadata::Ratings) -> RatingsDto {
    RatingsDto {
        vote_average: r.vote_average,
        vote_count: r.vote_count,
        popularity: r.popularity,
    }
}

pub fn movie_status(m: &reelix_metadata::MovieMetadata, today: NaiveDate) -> MovieStatus {
    use reelix_metadata::MovieStatus as S;
    let rd = &m.release_dates;
    let released = [rd.digital, rd.physical]
        .into_iter()
        .flatten()
        .any(|d| d <= today);
    let cinema = rd.cinema.or(m.release_date);
    if released {
        return MovieStatus::Released;
    }
    if cinema.is_some_and(|d| d <= today) {
        // Old titles often lack digital/physical dates: treat as released after ~3 months.
        if cinema.is_some_and(|d| (today - d).num_days() > 90)
            || m.status == S::Released && rd.cinema.is_none()
        {
            return MovieStatus::Released;
        }
        return MovieStatus::InCinemas;
    }
    match m.status {
        S::Rumored | S::Unknown if cinema.is_none() => MovieStatus::Tba,
        S::Canceled => MovieStatus::Tba,
        _ if cinema.is_some() => MovieStatus::Announced,
        S::Planned | S::InProduction | S::PostProduction => MovieStatus::Announced,
        _ => MovieStatus::Tba,
    }
}

/// Whether a movie with `status` satisfies `min`.
pub fn is_available(status: MovieStatus, min: MinimumAvailability) -> bool {
    match min {
        MinimumAvailability::Announced => {
            matches!(
                status,
                MovieStatus::Announced | MovieStatus::InCinemas | MovieStatus::Released
            )
        }
        MinimumAvailability::InCinemas => matches!(status, MovieStatus::InCinemas | MovieStatus::Released),
        MinimumAvailability::Released => status == MovieStatus::Released,
    }
}

pub fn series_status(s: reelix_metadata::SeriesStatus) -> SeriesStatus {
    match s {
        reelix_metadata::SeriesStatus::Continuing | reelix_metadata::SeriesStatus::Unknown => {
            SeriesStatus::Continuing
        }
        reelix_metadata::SeriesStatus::Ended => SeriesStatus::Ended,
        reelix_metadata::SeriesStatus::Upcoming => SeriesStatus::Upcoming,
    }
}

pub fn series_type(t: SeriesType) -> reelix_import::SeriesType {
    match t {
        SeriesType::Standard => reelix_import::SeriesType::Standard,
        SeriesType::Daily => reelix_import::SeriesType::Daily,
        SeriesType::Anime => reelix_import::SeriesType::Anime,
    }
}

pub fn naming_to_config(s: &NamingSettings) -> NamingConfig {
    serde_json::to_value(s)
        .and_then(serde_json::from_value)
        .unwrap_or_default()
}

pub fn naming_from_config(c: &NamingConfig) -> NamingSettings {
    serde_json::to_value(c)
        .and_then(serde_json::from_value)
        .expect("NamingSettings mirrors NamingConfig")
}

pub fn media_info_dto(m: &MediaInfo) -> MediaInfoDto {
    let audio = m.primary_audio();
    MediaInfoDto {
        container: m.container.clone(),
        duration_secs: m.duration_secs,
        overall_bitrate: m.overall_bitrate,
        video_codec: m
            .video_codec()
            .map(|c| video_codec_name(c).to_owned())
            .or_else(|| {
                m.video
                    .as_ref()
                    .map(|v| v.codec_name.clone())
                    .filter(|s| !s.is_empty())
            }),
        width: m.video.as_ref().map(|v| v.width),
        height: m.video.as_ref().map(|v| v.height),
        bit_depth: m.video.as_ref().and_then(|v| v.bit_depth),
        fps: m.video.as_ref().and_then(|v| v.fps),
        video_bitrate: m.video.as_ref().and_then(|v| v.bitrate),
        hdr: m.hdr().iter().map(|h| hdr_name(*h).to_owned()).collect(),
        audio_codec: audio.map(|a| match a.codec {
            Some(c) => audio_codec_name(c, a.is_atmos).to_owned(),
            None => a.codec_name.clone(),
        }),
        audio_channels: m.audio_channels(),
        audio_stream_count: m.audio.len() as u32,
        audio_languages: m.audio_languages().iter().map(|l| l.name().to_owned()).collect(),
        subtitle_languages: m
            .subtitle_languages()
            .iter()
            .map(|l| l.name().to_owned())
            .collect(),
    }
}

fn hdr_name(h: reelix_parser::HdrFormat) -> &'static str {
    use reelix_parser::HdrFormat as H;
    match h {
        H::DolbyVision => "DV",
        H::Hdr10 => "HDR10",
        H::Hdr10Plus => "HDR10+",
        H::Hlg => "HLG",
        H::Pq => "PQ",
    }
}

/// Lower-case alphanumeric title for matching (`"The Matrix: Reloaded"` → `"matrixreloaded"`).
pub fn clean_title(title: &str) -> String {
    reelix_import::matching::normalize_title(title).replace(' ', "")
}

pub fn year_u16(y: Option<i32>) -> Option<u16> {
    y.and_then(|y| u16::try_from(y).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_settings_mirror_config() {
        let c = NamingConfig::default();
        let s = naming_from_config(&c);
        assert_eq!(naming_to_config(&s), c);
    }

    #[test]
    fn availability_rules() {
        assert!(is_available(MovieStatus::Released, MinimumAvailability::Released));
        assert!(!is_available(
            MovieStatus::InCinemas,
            MinimumAvailability::Released
        ));
        assert!(is_available(
            MovieStatus::InCinemas,
            MinimumAvailability::InCinemas
        ));
        assert!(is_available(
            MovieStatus::Announced,
            MinimumAvailability::Announced
        ));
        assert!(!is_available(MovieStatus::Tba, MinimumAvailability::Announced));
        assert_eq!(enum_str(&MovieStatus::InCinemas), "in_cinemas");
        assert_eq!(parse_enum::<MovieStatus>("released"), Some(MovieStatus::Released));
    }
}
