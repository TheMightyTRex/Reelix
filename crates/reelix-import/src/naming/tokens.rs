//! Token value resolution and title helpers.

use std::sync::LazyLock;

use reelix_parser::model::{AudioCodec, HdrFormat, Language, VideoCodec};
use regex::Regex;

use super::template::{TokenSpec, pad, render_number_list};
use super::{RenderContext, SeriesType};

/// Result of resolving a token key.
pub(crate) enum Resolved {
    /// Known token (possibly empty).
    Value(String),
    /// Unknown token: rendered literally.
    Unknown,
}

fn text(v: impl Into<String>) -> Resolved {
    Resolved::Value(v.into())
}

fn opt<T: ToString>(v: Option<T>) -> Resolved {
    Resolved::Value(v.map(|v| v.to_string()).unwrap_or_default())
}

fn digits_spec(spec: &Option<String>) -> Option<usize> {
    spec.as_deref()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .map(|s| {
            if s.chars().all(|c| c == '0') {
                s.len()
            } else {
                s.parse().unwrap_or(0)
            }
        })
}

/// Truncate text to at most `max` characters; trailing separators/punctuation left by
/// the cut are dropped (an ellipsis would be eaten by the dot/trailing-dot cleanup).
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    cut.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string()
}

pub(crate) fn resolve(ctx: &RenderContext<'_>, t: &TokenSpec) -> Resolved {
    let key = t.key.as_str();
    let resolved = resolve_key(ctx, key, &t.spec);
    // Numeric specs on text tokens mean truncation; numbers handle padding themselves.
    match resolved {
        Resolved::Value(v) if !is_numeric_token(key) => match digits_spec(&t.spec) {
            Some(n) if n > 0 && !t.spec.as_deref().unwrap_or_default().starts_with('0') => {
                Resolved::Value(truncate(&v, n))
            }
            _ => Resolved::Value(v),
        },
        other => other,
    }
}

fn is_numeric_token(key: &str) -> bool {
    matches!(
        key,
        "season" | "episode" | "absolute" | "release year" | "series year"
    )
}

fn resolve_key(ctx: &RenderContext<'_>, key: &str, spec: &Option<String>) -> Resolved {
    let movie = ctx.movie;
    let series = ctx.series;
    let file = ctx.file;
    let width = digits_spec(spec).unwrap_or(1);
    match key {
        // Movie
        "movie title" => opt(movie.map(|m| m.title.clone())),
        "movie cleantitle" => opt(movie.map(|m| clean_title(&m.title))),
        "movie titlethe" => opt(movie.map(|m| title_the(&m.title))),
        "movie cleantitlethe" => opt(movie.map(|m| clean_title(&title_the(&m.title)))),
        "movie titleyear" => opt(movie.map(|m| title_year(&m.title, m.year))),
        "movie cleantitleyear" => opt(movie.map(|m| title_year(&clean_title(&m.title), m.year))),
        "movie titlethe year" | "movie titletheyear" => {
            opt(movie.map(|m| title_year(&title_the(&m.title), m.year)))
        }
        "movie titlefirstcharacter" => opt(movie.map(|m| first_character(&m.title))),
        "movie originaltitle" => opt(movie.and_then(|m| m.original_title.clone())),
        "movie cleanoriginaltitle" => opt(movie.and_then(|m| m.original_title.as_deref().map(clean_title))),
        "movie collection" => opt(movie.and_then(|m| m.collection.clone())),
        "movie collectionthe" => opt(movie.and_then(|m| m.collection.as_deref().map(title_the))),
        "movie certification" => opt(movie.and_then(|m| m.certification.clone())),
        "release year" => opt(movie.and_then(|m| m.year).or(series.and_then(|s| s.year))),
        "edition tags" => opt(file.and_then(|f| f.edition.clone())),
        // Ids: the series' when naming an episode, else the movie's.
        "tmdbid" => opt(series.and_then(|s| s.tmdb_id).or(movie.and_then(|m| m.tmdb_id))),
        "imdbid" => opt(series
            .and_then(|s| s.imdb_id.clone())
            .or(movie.and_then(|m| m.imdb_id.clone()))),
        "tvdbid" => opt(series.and_then(|s| s.tvdb_id)),
        "tvmazeid" => opt(series.and_then(|s| s.tvmaze_id)),
        // Series
        "series title" => opt(series.map(|s| s.title.clone())),
        "series cleantitle" => opt(series.map(|s| clean_title(&s.title))),
        "series titleyear" => opt(series.map(|s| title_year(&s.title, s.year))),
        "series cleantitleyear" => opt(series.map(|s| title_year(&clean_title(&s.title), s.year))),
        "series titlewithoutyear" => opt(series.map(|s| strip_year(&s.title))),
        "series cleantitlewithoutyear" => opt(series.map(|s| clean_title(&strip_year(&s.title)))),
        "series titlethe" => opt(series.map(|s| title_the(&s.title))),
        "series cleantitlethe" => opt(series.map(|s| clean_title(&title_the(&s.title)))),
        "series titletheyear" => opt(series.map(|s| title_year(&title_the(&s.title), s.year))),
        "series titlefirstcharacter" => opt(series.map(|s| first_character(&s.title))),
        "series year" => opt(series.and_then(|s| s.year)),
        // Episodes
        "season" => opt(ctx.episodes.first().map(|e| pad(e.season, width))),
        "episode" => {
            let nums: Vec<u32> = ctx.episodes.iter().map(|e| e.episode).collect();
            text(render_number_list(&nums, width, ctx.multi_episode_style))
        }
        "absolute" => {
            let nums: Vec<u32> = ctx.episodes.iter().filter_map(|e| e.absolute).collect();
            text(render_number_list(&nums, width, ctx.multi_episode_style))
        }
        "episode title" => text(episode_title(ctx, false)),
        "episode cleantitle" => text(episode_title(ctx, true)),
        "air date" => opt(ctx
            .episodes
            .first()
            .and_then(|e| e.air_date)
            .map(|d| d.format("%Y %m %d").to_string())),
        // File / release
        "quality full" => opt(file.map(|f| {
            let mut parts = vec![f.quality.quality.name().to_string()];
            parts.extend(proper_tags(ctx));
            parts.join(" ")
        })),
        "quality title" => opt(file.map(|f| f.quality.quality.name())),
        "quality proper" => text(
            proper_tags(ctx)
                .into_iter()
                .filter(|p| p != "REAL")
                .collect::<Vec<_>>()
                .join(" "),
        ),
        "quality real" => opt(file.filter(|f| f.quality.revision.real > 0).map(|_| "REAL")),
        "release group" => opt(file.and_then(|f| f.release_group.clone())),
        "release hash" => opt(file.and_then(|f| f.release_hash.clone())),
        "original title" => opt(file.and_then(|f| f.original_title.clone())),
        "original filename" => opt(file.and_then(|f| f.original_file_name.clone())),
        "custom formats" => opt(file.map(|f| f.custom_formats.join(" "))),
        "custom format" => opt(file.and_then(|f| match spec.as_deref() {
            Some(name) => f
                .custom_formats
                .iter()
                .find(|c| c.eq_ignore_ascii_case(name))
                .cloned(),
            None => Some(f.custom_formats.join(" ")),
        })),
        // Media info
        k if k.starts_with("mediainfo ") => match file {
            None => text(""),
            Some(f) => media_info_token(&k["mediainfo ".len()..], &f.media),
        },
        _ => Resolved::Unknown,
    }
}

fn proper_tags(ctx: &RenderContext<'_>) -> Vec<String> {
    let Some(f) = ctx.file else { return Vec::new() };
    let rev = f.quality.revision;
    let mut tags = Vec::new();
    if rev.version > 1 {
        if ctx.series_type == SeriesType::Anime {
            tags.push(format!("v{}", rev.version));
        } else if rev.is_repack {
            tags.push("Repack".to_string());
        } else {
            tags.push("Proper".to_string());
        }
    }
    if rev.real > 0 {
        tags.push("REAL".to_string());
    }
    tags
}

static PART_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*\((?:part\s*)?\d{1,2}\)$").expect("part regex"));

/// Episode titles for one or more episodes: identical bases (`Title (1)`, `Title (2)`)
/// collapse to `Title`, distinct titles join with ` + `.
fn episode_title(ctx: &RenderContext<'_>, clean: bool) -> String {
    let titles: Vec<String> = ctx
        .episodes
        .iter()
        .map(|e| e.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let joined = if titles.len() > 1 {
        let bases: Vec<String> = titles
            .iter()
            .map(|t| PART_SUFFIX.replace(t, "").to_string())
            .collect();
        if bases.iter().all(|b| b == &bases[0]) {
            bases[0].clone()
        } else {
            let mut uniq: Vec<String> = Vec::new();
            for t in titles {
                if !uniq.contains(&t) {
                    uniq.push(t);
                }
            }
            uniq.join(" + ")
        }
    } else {
        titles.into_iter().next().unwrap_or_default()
    };
    let joined = if clean { clean_title(&joined) } else { joined };
    match ctx.title_budget {
        Some(max) => truncate(&joined, max),
        None => joined,
    }
}

fn media_info_token(name: &str, m: &super::MediaFacts) -> Resolved {
    match name {
        "videocodec" => opt(m.video_codec.map(video_codec_name)),
        "videobitdepth" => opt(m.video_bit_depth),
        "videodynamicrange" => text(if m.hdr.is_empty() { "" } else { "HDR" }),
        "videodynamicrangetype" => text(dynamic_range_type(&m.hdr)),
        "audiocodec" => opt(m.audio_codec.map(|c| audio_codec_name(c, m.is_atmos))),
        "audiochannels" => opt(m.audio_channels.filter(|c| *c > 0.0).map(|c| format!("{c:.1}"))),
        "audiolanguages" => text(lang_list(&m.audio_languages)),
        "subtitlelanguages" => text(lang_list(&m.subtitle_languages)),
        "3d" => text(if m.is_3d { "3D" } else { "" }),
        "simple" => text(join_nonempty(&[
            m.video_codec
                .map(video_codec_name)
                .unwrap_or_default()
                .to_string(),
            m.audio_codec
                .map(|c| audio_codec_name(c, m.is_atmos))
                .unwrap_or_default()
                .to_string(),
        ])),
        "full" => text(join_nonempty(&[
            m.video_codec
                .map(video_codec_name)
                .unwrap_or_default()
                .to_string(),
            m.audio_codec
                .map(|c| audio_codec_name(c, m.is_atmos))
                .unwrap_or_default()
                .to_string(),
            lang_list(&m.audio_languages),
        ])),
        _ => Resolved::Unknown,
    }
}

fn join_nonempty(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `[EN+DE]`; empty when no languages are known.
fn lang_list(langs: &[Language]) -> String {
    let codes: Vec<String> = langs
        .iter()
        .filter(|l| !l.iso639_1().is_empty())
        .map(|l| l.iso639_1().to_uppercase())
        .collect();
    if codes.is_empty() {
        String::new()
    } else {
        format!("[{}]", codes.join("+"))
    }
}

pub fn video_codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
        VideoCodec::Av1 => "AV1",
        VideoCodec::Vp9 => "VP9",
        VideoCodec::Vc1 => "VC1",
        VideoCodec::Mpeg2 => "MPEG2",
        VideoCodec::Xvid => "XviD",
        VideoCodec::Divx => "DivX",
    }
}

pub fn audio_codec_name(c: AudioCodec, is_atmos: bool) -> &'static str {
    match (c, is_atmos) {
        (AudioCodec::Aac, _) => "AAC",
        (AudioCodec::Ac3, _) => "AC3",
        (AudioCodec::Eac3, true) => "EAC3 Atmos",
        (AudioCodec::Eac3, false) => "EAC3",
        (AudioCodec::Dts, _) => "DTS",
        (AudioCodec::DtsHdMa, _) => "DTS-HD MA",
        (AudioCodec::DtsX, _) => "DTS-X",
        (AudioCodec::TrueHd, true) | (AudioCodec::Atmos, _) => "TrueHD Atmos",
        (AudioCodec::TrueHd, false) => "TrueHD",
        (AudioCodec::Flac, _) => "FLAC",
        (AudioCodec::Mp3, _) => "MP3",
        (AudioCodec::Opus, _) => "Opus",
        (AudioCodec::Pcm, _) => "PCM",
    }
}

/// Sonarr/Radarr `VideoDynamicRangeType` values: `DV HDR10`, `HDR10Plus`, `HLG`…
pub fn dynamic_range_type(hdr: &[HdrFormat]) -> String {
    let dv = hdr.contains(&HdrFormat::DolbyVision);
    let base = if hdr.contains(&HdrFormat::Hdr10Plus) {
        "HDR10Plus"
    } else if hdr.contains(&HdrFormat::Hdr10) {
        "HDR10"
    } else if hdr.contains(&HdrFormat::Hlg) {
        "HLG"
    } else if hdr.contains(&HdrFormat::Pq) {
        "PQ"
    } else {
        ""
    };
    match (dv, base) {
        (true, "") => "DV".into(),
        (true, b) => format!("DV {b}"),
        (false, b) => b.into(),
    }
}

static CLEAN_REMOVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"['"`´’:;,!?*<>|~@#$%^=()\[\]{}]|\.(?:\s|$)"#).expect("clean regex"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("spaces regex"));
static ARTICLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i)(the|an|a)\s+(.+?)(\s+\([^)]*\))?$").expect("article regex"));
static TRAILING_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\((?:19|20)\d{2}\)$").expect("year regex"));

/// `{… CleanTitle}`: `&` → `and`, slashes → spaces, punctuation removed.
pub fn clean_title(title: &str) -> String {
    let s = title.replace('&', "and").replace(['/', '\\'], " ");
    let s = CLEAN_REMOVE.replace_all(&s, |c: &regex::Captures| {
        if c[0].starts_with('.') {
            " ".to_string()
        } else {
            String::new()
        }
    });
    SPACES.replace_all(s.trim(), " ").to_string()
}

/// `The Matrix` → `Matrix, The`; a trailing `(US)` stays last: `Office, The (US)`.
pub fn title_the(title: &str) -> String {
    match ARTICLE.captures(title) {
        Some(c) => format!("{}, {}{}", &c[2], &c[1], c.get(3).map_or("", |m| m.as_str())),
        None => title.to_string(),
    }
}

/// `Title (2010)`, unless the title already carries that year.
pub fn title_year(title: &str, year: Option<u16>) -> String {
    match year {
        Some(y) if y > 0 && !title.ends_with(&format!("({y})")) => format!("{title} ({y})"),
        _ => title.to_string(),
    }
}

pub fn strip_year(title: &str) -> String {
    TRAILING_YEAR.replace(title, "").to_string()
}

fn first_character(title: &str) -> String {
    title_the(title)
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_helpers() {
        assert_eq!(
            clean_title("Mission: Impossible - Fallout"),
            "Mission Impossible - Fallout"
        );
        assert_eq!(clean_title("Law & Order: S.V.U."), "Law and Order S.V.U");
        assert_eq!(clean_title("Mr. Robot"), "Mr Robot");
        assert_eq!(clean_title("AC/DC: Let There Be Rock"), "AC DC Let There Be Rock");
        assert_eq!(clean_title("What's Up?"), "Whats Up");
        assert_eq!(title_the("The Matrix"), "Matrix, The");
        assert_eq!(title_the("The Office (US)"), "Office, The (US)");
        assert_eq!(title_the("Theodore"), "Theodore");
        assert_eq!(title_year("Show (2019)", Some(2019)), "Show (2019)");
        assert_eq!(title_year("Show", Some(2019)), "Show (2019)");
        assert_eq!(strip_year("Show (2019)"), "Show");
        assert_eq!(first_character("The Matrix"), "M");
        assert_eq!(truncate("A Very Long Episode Title", 10), "A Very Lon");
        assert_eq!(truncate("Pilot - Redux", 8), "Pilot");
    }

    #[test]
    fn dynamic_range_types() {
        use HdrFormat::*;
        assert_eq!(dynamic_range_type(&[DolbyVision, Hdr10]), "DV HDR10");
        assert_eq!(dynamic_range_type(&[DolbyVision]), "DV");
        assert_eq!(dynamic_range_type(&[Hdr10Plus]), "HDR10Plus");
        assert_eq!(dynamic_range_type(&[Hlg]), "HLG");
        assert_eq!(dynamic_range_type(&[]), "");
    }
}
