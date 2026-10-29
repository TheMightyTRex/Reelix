//! Naming engine: Sonarr/Radarr-compatible token templates.
//!
//! Syntax: `{Token Name}` (separator variants `{Movie.Title}`, case variants
//! `{MOVIE TITLE}`), optional prefix/suffix `{-Release Group}` / `{[Quality Full]}`,
//! format specs `{season:00}` / `{Episode Title:30}`, nested id blocks `{tmdb-{TmdbId}}`
//! (outer braces kept, dropped when empty) and `S{season:00}E{episode:00}` units that
//! follow [`MultiEpisodeStyle`]. Unknown tokens render literally; see
//! [`validate_template`].
//!
//! Typical use:
//! ```
//! use reelix_import::naming::{NamingConfig, samples};
//! let cfg = NamingConfig::default();
//! let name = cfg.movie_file_name(&samples::movie(), &samples::movie_file()).unwrap();
//! assert!(name.starts_with("The Movie Title (2010)"));
//! ```

pub mod custom;
mod template;
mod tokens;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use reelix_parser::model::{AudioCodec, HdrFormat, Language, ParsedRelease, QualityModel, VideoCodec};
use serde::{Deserialize, Serialize};

pub use custom::*;
pub use tokens::{
    audio_codec_name, clean_title, dynamic_range_type, strip_year, title_the, title_year, video_codec_name,
};

use crate::mediainfo::MediaInfo;
use crate::{Error, Result, SeriesType};
use template::Node;

/// How `:` in titles is written to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColonReplacement {
    Delete,
    /// `:` → `-`
    Dash,
    /// `:` → ` -`
    SpaceDash,
    /// `:` → ` - `
    SpaceDashSpace,
    /// `Title: Sub` → `Title - Sub`, `10:30` → `10-30`.
    #[default]
    Smart,
}

/// How files containing several episodes are numbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiEpisodeStyle {
    /// `S01E01-02-03`
    #[default]
    Extend,
    /// `S01E01 - S01E02`
    Duplicate,
    /// `S01E01E02E03`
    Repeat,
    /// `S01E01-E02-E03`
    Scene,
    /// `S01E01-03`
    Range,
    /// `S01E01-E03`
    PrefixedRange,
}

/// Naming settings (persisted as JSON by a later wave).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NamingConfig {
    /// When false, imported files keep their original file name.
    pub rename_files: bool,
    /// Replace illegal characters with look-alikes instead of deleting them.
    pub replace_illegal_characters: bool,
    pub colon_replacement: ColonReplacement,
    pub movie_folder_format: String,
    pub movie_file_format: String,
    pub series_folder_format: String,
    pub season_folder_format: String,
    pub specials_folder_format: String,
    pub standard_episode_format: String,
    pub daily_episode_format: String,
    pub anime_episode_format: String,
    pub multi_episode_style: MultiEpisodeStyle,
    /// Per path component (file or folder name), in bytes.
    pub max_file_name_bytes: usize,
    /// Whole destination path, in bytes.
    pub max_path_bytes: usize,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            rename_files: true,
            replace_illegal_characters: true,
            colon_replacement: ColonReplacement::Smart,
            movie_folder_format: "{Movie CleanTitle} ({Release Year}) {tmdb-{TmdbId}}".into(),
            movie_file_format: "{Movie CleanTitle} ({Release Year}) {edition-{Edition Tags}} [{Custom Formats}][{Quality Full}][{MediaInfo VideoDynamicRangeType}]{-Release Group}".into(),
            series_folder_format: "{Series TitleYear} {tvdb-{TvdbId}}".into(),
            season_folder_format: "Season {season:00}".into(),
            specials_folder_format: "Specials".into(),
            standard_episode_format: "{Series TitleYear} - S{season:00}E{episode:00} - {Episode CleanTitle} [{Quality Full}]{-Release Group}".into(),
            daily_episode_format: "{Series TitleYear} - {Air-Date} - {Episode CleanTitle} [{Quality Full}]{-Release Group}".into(),
            anime_episode_format: "{Series TitleYear} - S{season:00}E{episode:00} - {absolute:000} - {Episode CleanTitle} [{Quality Full}]{-Release Group}".into(),
            multi_episode_style: MultiEpisodeStyle::Extend,
            max_file_name_bytes: 255,
            max_path_bytes: if cfg!(windows) { 260 } else { 4096 },
        }
    }
}

/// Movie facts used by movie tokens.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MovieNaming {
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub collection: Option<String>,
    pub certification: Option<String>,
}

/// Series facts used by series tokens.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SeriesNaming {
    pub title: String,
    pub year: Option<u16>,
    pub tvdb_id: Option<u32>,
    pub tmdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub tvmaze_id: Option<u32>,
    pub series_type: SeriesType,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EpisodeNaming {
    pub season: u32,
    pub episode: u32,
    pub absolute: Option<u32>,
    pub title: String,
    pub air_date: Option<NaiveDate>,
}

/// Technical facts for `{MediaInfo …}` tokens: from ffprobe when available, else the
/// release name.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MediaFacts {
    pub video_codec: Option<VideoCodec>,
    pub video_bit_depth: Option<u8>,
    pub hdr: Vec<HdrFormat>,
    pub audio_codec: Option<AudioCodec>,
    pub audio_channels: Option<f32>,
    pub is_atmos: bool,
    pub audio_languages: Vec<Language>,
    pub subtitle_languages: Vec<Language>,
    pub is_3d: bool,
}

impl MediaFacts {
    pub fn from_media_info(m: &MediaInfo) -> Self {
        Self {
            video_codec: m.video_codec(),
            video_bit_depth: m.video.as_ref().and_then(|v| v.bit_depth),
            hdr: m.hdr().to_vec(),
            audio_codec: m.audio_codec(),
            audio_channels: m.audio_channels(),
            is_atmos: m.primary_audio().is_some_and(|a| a.is_atmos),
            audio_languages: m.audio_languages(),
            subtitle_languages: m.subtitle_languages(),
            is_3d: m.video.as_ref().is_some_and(|v| v.is_3d),
        }
    }

    pub fn from_parsed(p: &ParsedRelease) -> Self {
        Self {
            video_codec: p.video_codec,
            video_bit_depth: None,
            hdr: p.hdr.clone(),
            audio_codec: p.audio_codecs.first().copied(),
            audio_channels: p.audio_channels,
            is_atmos: p.audio_codecs.contains(&AudioCodec::Atmos),
            audio_languages: p.languages.clone(),
            subtitle_languages: Vec::new(),
            is_3d: p.is_3d,
        }
    }
}

/// Facts about the file being named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileNaming {
    pub quality: QualityModel,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    pub custom_formats: Vec<String>,
    pub media: MediaFacts,
    /// Release (scene) title the file came from.
    pub original_title: Option<String>,
    /// Original file name without extension; used when renaming is disabled.
    pub original_file_name: Option<String>,
    pub release_hash: Option<String>,
}

impl FileNaming {
    /// Build from a parsed release, preferring `media_info` for technical tokens.
    pub fn from_parsed(p: &ParsedRelease, media_info: Option<&MediaInfo>) -> Self {
        Self {
            quality: p.quality,
            release_group: p.release_group.clone(),
            edition: p.edition.clone(),
            custom_formats: Vec::new(),
            media: media_info.map_or_else(|| MediaFacts::from_parsed(p), MediaFacts::from_media_info),
            original_title: Some(p.raw_title.clone()),
            original_file_name: None,
            release_hash: p.hash.clone(),
        }
    }
}

/// Everything a template can reference.
#[derive(Debug, Clone, Default)]
pub struct RenderContext<'a> {
    pub movie: Option<&'a MovieNaming>,
    pub series: Option<&'a SeriesNaming>,
    pub episodes: &'a [EpisodeNaming],
    pub file: Option<&'a FileNaming>,
    pub series_type: SeriesType,
    pub multi_episode_style: MultiEpisodeStyle,
    /// Maximum characters for episode titles (set by the length guard).
    pub title_budget: Option<usize>,
}

/// Render `template` into a single sanitised path component.
pub fn render(template: &str, ctx: &RenderContext<'_>, config: &NamingConfig) -> String {
    let nodes = template::parse(template);
    let mut episodes: Vec<EpisodeNaming> = ctx.episodes.to_vec();
    episodes.sort_by_key(|e| (e.season, e.episode));
    let ctx = RenderContext {
        episodes: &episodes,
        ..ctx.clone()
    };
    let (raw, _) = render_nodes(&nodes, &ctx);
    sanitize(&template::tidy(&raw), config)
}

/// Unknown tokens in `template` (rendered literally); empty when the template is valid.
pub fn unknown_tokens(template: &str) -> Vec<String> {
    let (m, s, eps, f) = (
        samples::movie(),
        samples::series(),
        samples::episodes(),
        samples::episode_file(),
    );
    let ctx = RenderContext {
        movie: Some(&m),
        series: Some(&s),
        episodes: &eps,
        file: Some(&f),
        ..Default::default()
    };
    let mut out = Vec::new();
    collect_unknown(&template::parse(template), &ctx, &mut out);
    out
}

/// Validate a template: balanced braces and only known tokens.
pub fn validate_template(template: &str) -> Result<()> {
    let opens = template.matches('{').count();
    let closes = template.matches('}').count();
    if opens != closes {
        return Err(Error::Template(format!("unbalanced braces in {template:?}")));
    }
    if template.trim().is_empty() {
        return Err(Error::Template("template is empty".into()));
    }
    match unknown_tokens(template) {
        u if u.is_empty() => Ok(()),
        u => Err(Error::Template(format!("unknown token(s): {}", u.join(", ")))),
    }
}

fn collect_unknown(nodes: &[Node], ctx: &RenderContext<'_>, out: &mut Vec<String>) {
    for n in nodes {
        match n {
            Node::Token(t) => {
                if matches!(tokens::resolve(ctx, t), tokens::Resolved::Unknown) {
                    out.push(format!("{{{}}}", t.raw));
                }
            }
            Node::Block(children) => collect_unknown(children, ctx, out),
            Node::Literal(_) | Node::SeasonEpisode(_) => {}
        }
    }
}

/// Returns the rendered text and whether any token produced a non-empty value.
fn render_nodes(nodes: &[Node], ctx: &RenderContext<'_>) -> (String, bool) {
    let mut out = String::new();
    let mut any_value = false;
    for n in nodes {
        match n {
            Node::Literal(l) => out.push_str(l),
            Node::Token(t) => match tokens::resolve(ctx, t) {
                tokens::Resolved::Value(v) => {
                    if !v.is_empty() {
                        any_value = true;
                    }
                    out.push_str(&template::decorate(t, &v));
                }
                tokens::Resolved::Unknown => out.push_str(&format!("{{{}}}", t.raw)),
            },
            Node::Block(children) => {
                let (inner, has) = render_nodes(children, ctx);
                if has {
                    any_value = true;
                    out.push('{');
                    out.push_str(&inner);
                    out.push('}');
                }
            }
            Node::SeasonEpisode(spec) => {
                let season = ctx.episodes.first().map_or(0, |e| e.season);
                let eps: Vec<u32> = ctx.episodes.iter().map(|e| e.episode).collect();
                if !eps.is_empty() {
                    any_value = true;
                }
                out.push_str(&template::render_season_episode(
                    spec,
                    season,
                    &eps,
                    ctx.multi_episode_style,
                ));
            }
        }
    }
    (out, any_value)
}

/// Apply colon handling and illegal-character replacement to one path component.
pub fn sanitize(name: &str, config: &NamingConfig) -> String {
    let colon_fixed = match config.colon_replacement {
        ColonReplacement::Delete => name.replace(':', ""),
        ColonReplacement::Dash => name.replace(':', "-"),
        ColonReplacement::SpaceDash => name.replace(':', " -"),
        ColonReplacement::SpaceDashSpace => name
            .replace(" : ", " - ")
            .replace(": ", " - ")
            .replace(':', " - "),
        ColonReplacement::Smart => name.replace(": ", " - ").replace(':', "-"),
    };
    let mut out = String::with_capacity(colon_fixed.len());
    for c in colon_fixed.chars() {
        match c {
            c if c.is_control() => {}
            '\\' | '/' if config.replace_illegal_characters => out.push('+'),
            '?' if config.replace_illegal_characters => out.push('!'),
            '*' if config.replace_illegal_characters => out.push('-'),
            '\\' | '/' | '?' | '*' | '<' | '>' | '|' | '"' => {}
            c => out.push(c),
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_end_matches(['.', ' '])
        .trim_start_matches(['.', ' ']);
    trimmed.to_string()
}

impl NamingConfig {
    fn ctx<'a>(&self, series_type: SeriesType) -> RenderContext<'a> {
        RenderContext {
            series_type,
            multi_episode_style: self.multi_episode_style,
            ..Default::default()
        }
    }

    pub fn movie_folder(&self, movie: &MovieNaming) -> Result<String> {
        let ctx = RenderContext {
            movie: Some(movie),
            ..self.ctx(SeriesType::Standard)
        };
        self.component("movie folder", render(&self.movie_folder_format, &ctx, self))
    }

    /// Movie file name without extension.
    pub fn movie_file_name(&self, movie: &MovieNaming, file: &FileNaming) -> Result<String> {
        if !self.rename_files
            && let Some(orig) = &file.original_file_name
        {
            return Ok(orig.clone());
        }
        let ctx = RenderContext {
            movie: Some(movie),
            file: Some(file),
            ..self.ctx(SeriesType::Standard)
        };
        self.component("movie file name", render(&self.movie_file_format, &ctx, self))
    }

    pub fn series_folder(&self, series: &SeriesNaming) -> Result<String> {
        let ctx = RenderContext {
            series: Some(series),
            ..self.ctx(series.series_type)
        };
        self.component("series folder", render(&self.series_folder_format, &ctx, self))
    }

    /// Season folder; season 0 uses the specials format.
    pub fn season_folder(&self, series: &SeriesNaming, season: u32) -> Result<String> {
        let ep = [EpisodeNaming {
            season,
            ..Default::default()
        }];
        let ctx = RenderContext {
            series: Some(series),
            episodes: &ep,
            ..self.ctx(series.series_type)
        };
        let fmt = if season == 0 {
            &self.specials_folder_format
        } else {
            &self.season_folder_format
        };
        self.component("season folder", render(fmt, &ctx, self))
    }

    /// Episode file name (without extension) for one or more episodes of one file.
    /// Long episode titles are truncated to respect `max_file_name_bytes`
    /// (`extension_len` bytes are reserved for the extension incl. the dot).
    pub fn episode_file_name(
        &self,
        series: &SeriesNaming,
        episodes: &[EpisodeNaming],
        file: &FileNaming,
        extension_len: usize,
    ) -> Result<String> {
        if !self.rename_files
            && let Some(orig) = &file.original_file_name
        {
            return Ok(orig.clone());
        }
        let has_date = episodes.first().is_some_and(|e| e.air_date.is_some());
        let fmt = match series.series_type {
            SeriesType::Daily if has_date => &self.daily_episode_format,
            SeriesType::Anime if episodes.iter().any(|e| e.absolute.is_some()) => &self.anime_episode_format,
            _ => &self.standard_episode_format,
        };
        let mut ctx = RenderContext {
            series: Some(series),
            episodes,
            file: Some(file),
            ..self.ctx(series.series_type)
        };
        let limit = self.max_file_name_bytes.saturating_sub(extension_len);
        let mut name = render(fmt, &ctx, self);
        // Shrink the episode title until the name fits (or the title is gone).
        let mut budget = episodes.iter().map(|e| e.title.chars().count()).sum::<usize>() + 3 * episodes.len();
        while name.len() > limit && budget > 0 {
            let over = name.len() - limit;
            budget = budget.saturating_sub(over.max(1));
            ctx.title_budget = Some(budget);
            name = render(fmt, &ctx, self);
        }
        if name.len() > limit {
            return Err(Error::PathTooLong {
                what: "episode file name",
                len: name.len() + extension_len,
                max: self.max_file_name_bytes,
            });
        }
        self.component("episode file name", name)
    }

    /// `root/<movie folder>/<file>.<ext>`, checked against the length limits.
    pub fn movie_path(
        &self,
        root: &Path,
        movie: &MovieNaming,
        file: &FileNaming,
        extension: &str,
    ) -> Result<PathBuf> {
        let ext = extension.trim_start_matches('.');
        let name = self.movie_file_name(movie, file)?;
        let full = format!("{name}.{ext}");
        self.check_len("movie file name", &full, self.max_file_name_bytes)?;
        let path = root.join(self.movie_folder(movie)?).join(full);
        self.check_path(&path)?;
        Ok(path)
    }

    /// `root/<series folder>/<season folder>/<file>.<ext>`. An empty season folder
    /// format places files directly in the series folder.
    pub fn episode_path(
        &self,
        root: &Path,
        series: &SeriesNaming,
        episodes: &[EpisodeNaming],
        file: &FileNaming,
        extension: &str,
    ) -> Result<PathBuf> {
        let ext = extension.trim_start_matches('.');
        let name = self.episode_file_name(series, episodes, file, ext.len() + 1)?;
        let mut path = root.join(self.series_folder(series)?);
        let season = episodes.first().map_or(1, |e| e.season);
        let season_fmt = if season == 0 {
            &self.specials_folder_format
        } else {
            &self.season_folder_format
        };
        if !season_fmt.trim().is_empty() {
            path.push(self.season_folder(series, season)?);
        }
        path.push(format!("{name}.{ext}"));
        self.check_path(&path)?;
        Ok(path)
    }

    fn component(&self, what: &'static str, name: String) -> Result<String> {
        if name.is_empty() {
            return Err(Error::Template(format!("{what} rendered empty")));
        }
        self.check_len(what, &name, self.max_file_name_bytes)?;
        Ok(name)
    }

    fn check_len(&self, what: &'static str, s: &str, max: usize) -> Result<()> {
        if s.len() > max {
            return Err(Error::PathTooLong {
                what,
                len: s.len(),
                max,
            });
        }
        Ok(())
    }

    fn check_path(&self, path: &Path) -> Result<()> {
        self.check_len("path", &path.to_string_lossy(), self.max_path_bytes)
    }

    /// Example names for the settings preview.
    pub fn preview(&self) -> NamingPreview {
        let show = |r: Result<String>| r.unwrap_or_else(|e| format!("<{e}>"));
        let (movie, mfile) = (samples::movie(), samples::movie_file());
        let (series, eps, efile) = (samples::series(), samples::episodes(), samples::episode_file());
        let daily = samples::daily_series();
        let anime = samples::anime_series();
        NamingPreview {
            movie_folder: show(self.movie_folder(&movie)),
            movie_file: show(self.movie_file_name(&movie, &mfile)),
            series_folder: show(self.series_folder(&series)),
            season_folder: show(self.season_folder(&series, 1)),
            standard_episode: show(self.episode_file_name(&series, &eps[..1], &efile, 4)),
            multi_episode: show(self.episode_file_name(&series, &eps, &efile, 4)),
            daily_episode: show(self.episode_file_name(&daily, &samples::daily_episodes(), &efile, 4)),
            anime_episode: show(self.episode_file_name(&anime, &samples::anime_episodes(), &efile, 4)),
        }
    }
}

/// Output of [`NamingConfig::preview`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamingPreview {
    pub movie_folder: String,
    pub movie_file: String,
    pub series_folder: String,
    pub season_folder: String,
    pub standard_episode: String,
    pub multi_episode: String,
    pub daily_episode: String,
    pub anime_episode: String,
}

/// Sample data for previews and template validation.
pub mod samples {
    use reelix_parser::model::{Quality, Revision};

    use super::*;

    pub fn movie() -> MovieNaming {
        MovieNaming {
            title: "The Movie Title".into(),
            original_title: Some("The Original Movie Title".into()),
            year: Some(2010),
            tmdb_id: Some(345691),
            imdb_id: Some("tt0066921".into()),
            collection: Some("The Movie Collection".into()),
            certification: Some("R".into()),
        }
    }

    pub fn movie_file() -> FileNaming {
        FileNaming {
            quality: QualityModel {
                quality: Quality::Bluray1080p,
                revision: Revision {
                    version: 2,
                    real: 0,
                    is_repack: false,
                },
            },
            release_group: Some("RlsGrp".into()),
            edition: Some("Ultimate Extended Edition".into()),
            custom_formats: vec!["Surround Sound".into(), "x264".into()],
            media: MediaFacts {
                video_codec: Some(VideoCodec::H264),
                video_bit_depth: Some(8),
                hdr: vec![HdrFormat::DolbyVision, HdrFormat::Hdr10],
                audio_codec: Some(AudioCodec::Dts),
                audio_channels: Some(5.1),
                is_atmos: false,
                audio_languages: vec![Language::English, Language::German],
                subtitle_languages: vec![Language::English],
                is_3d: false,
            },
            original_title: Some("The.Movie.Title.2010.1080p.BluRay.DTS.x264-RlsGrp".into()),
            original_file_name: None,
            release_hash: Some("ABCDEFGH".into()),
        }
    }

    pub fn series() -> SeriesNaming {
        SeriesNaming {
            title: "The Series Title!".into(),
            year: Some(2010),
            tvdb_id: Some(12345),
            tmdb_id: Some(11223),
            imdb_id: Some("tt12345".into()),
            tvmaze_id: Some(54321),
            series_type: SeriesType::Standard,
        }
    }

    pub fn daily_series() -> SeriesNaming {
        SeriesNaming {
            series_type: SeriesType::Daily,
            ..series()
        }
    }

    pub fn anime_series() -> SeriesNaming {
        SeriesNaming {
            series_type: SeriesType::Anime,
            ..series()
        }
    }

    pub fn episodes() -> Vec<EpisodeNaming> {
        vec![
            EpisodeNaming {
                season: 1,
                episode: 1,
                absolute: Some(1),
                title: "Episode Title (1)".into(),
                air_date: NaiveDate::from_ymd_opt(2013, 10, 30),
            },
            EpisodeNaming {
                season: 1,
                episode: 2,
                absolute: Some(2),
                title: "Episode Title (2)".into(),
                air_date: NaiveDate::from_ymd_opt(2013, 10, 30),
            },
        ]
    }

    pub fn daily_episodes() -> Vec<EpisodeNaming> {
        episodes().into_iter().take(1).collect()
    }

    pub fn anime_episodes() -> Vec<EpisodeNaming> {
        episodes().into_iter().take(1).collect()
    }

    pub fn episode_file() -> FileNaming {
        FileNaming {
            quality: QualityModel::new(Quality::WebDl1080p),
            edition: None,
            custom_formats: vec![],
            original_title: Some("The.Series.Title.S01E01.1080p.WEB-DL.DDP5.1.H.264-RlsGrp".into()),
            ..movie_file()
        }
    }
}
