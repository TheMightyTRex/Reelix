//! Parsing of file paths, falling back to folder names when the file name is weak
//! (obfuscated names, `S01E05.mkv`, `05 - Title.mkv` inside a season folder…).

use crate::model::{EpisodeInfo, Language, ParsedRelease, Quality, ReleaseKind};
use crate::parse;

re!(
    SEASON_FOLDER,
    r"(?i)^(?:Season|Saison|Staffel|Temporada|Series|S)[ ._-]*(\d{1,4})$|^(Specials?)$"
);
re!(LEADING_EPISODE, r"^(\d{1,3})(?:[ ._-]|$)");

/// Season number when `name` is a plain season folder (`Season 01`, `S2`, `Specials`).
fn season_folder(name: &str) -> Option<u32> {
    let c = SEASON_FOLDER.captures(name.trim())?;
    if c.get(2).is_some() {
        return Some(0);
    }
    c.get(1)?.as_str().parse().ok()
}

/// Parses a path such as `/tv/Show/Season 01/Show.S01E05.mkv`, filling in what the
/// file name lacks (title, year, quality, group, season) from its parent folders.
pub fn parse_path(path: &str) -> ParsedRelease {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.trim().is_empty()).collect();
    let Some((file, dirs)) = parts.split_last() else {
        return ParsedRelease::empty(path);
    };
    let mut out = parse(file);
    out.raw_title = path.to_string();

    // Walk up the folders: season folders give a season, the first other folder is the release/media folder.
    let mut folder_season = None;
    let mut folder: Option<ParsedRelease> = None;
    for dir in dirs.iter().rev() {
        if let Some(s) = season_folder(dir) {
            folder_season.get_or_insert(s);
            continue;
        }
        let parsed = parse(dir);
        if !parsed.title.is_empty() {
            folder = Some(parsed);
            break;
        }
    }

    // `05 - Episode Title.mkv` inside a season folder.
    if let Some(season) = folder_season {
        let needs_episode = match &out.kind {
            ReleaseKind::Episode(e) => e.episodes.is_empty() && e.air_date.is_none() && e.season.is_none(),
            _ => true,
        };
        if needs_episode && let Some(c) = LEADING_EPISODE.captures(file) {
            let n: u32 = c.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            out.kind = ReleaseKind::Episode(EpisodeInfo {
                season: Some(season),
                episodes: vec![n],
                is_special: season == 0,
                ..Default::default()
            });
            out.title.clear();
        }
        if let ReleaseKind::Episode(e) = &mut out.kind
            && e.season.is_none()
            && e.air_date.is_none()
        {
            e.season = Some(season);
            e.is_special = season == 0;
        }
    }

    let Some(dir) = folder else { return out };

    // An obfuscated file name carries nothing useful: take the folder wholesale.
    let file_is_weak = out.title.is_empty() || out.kind == ReleaseKind::Unknown;
    if file_is_weak && out.quality.quality == Quality::Unknown && out.episode().is_none() {
        let mut d = dir;
        d.raw_title = path.to_string();
        return d;
    }
    if out.title.is_empty() {
        out.title = dir.title.clone();
        out.year = out.year.or(dir.year);
    }
    if out.year.is_none() && crate::clean_title(&out.title) == crate::clean_title(&dir.title) {
        out.year = dir.year;
    }
    if out.kind == ReleaseKind::Unknown {
        out.kind = match &dir.kind {
            ReleaseKind::Episode(_) => ReleaseKind::Unknown,
            k => k.clone(),
        };
    }
    if out.quality.quality == Quality::Unknown {
        out.quality = dir.quality;
    }
    if out.release_group.is_none() {
        out.release_group = dir.release_group;
    }
    if out.edition.is_none() {
        out.edition = dir.edition;
    }
    if out.languages == [Language::English] && dir.languages != [Language::English] {
        out.languages = dir.languages;
    }
    if out.video_codec.is_none() {
        out.video_codec = dir.video_codec;
    }
    if out.audio_codecs.is_empty() {
        out.audio_codecs = dir.audio_codecs;
        out.audio_channels = out.audio_channels.or(dir.audio_channels);
    }
    if out.hdr.is_empty() {
        out.hdr = dir.hdr;
    }
    out.streaming_service = out.streaming_service.or(dir.streaming_service);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscated_file_uses_folder() {
        let p = parse_path("/downloads/Movie.Name.2020.1080p.BluRay.x264-GRP/a8f3e2c1d9.mkv");
        assert_eq!(p.title, "Movie Name");
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.quality.quality, Quality::Bluray1080p);
        assert_eq!(p.release_group.as_deref(), Some("GRP"));
    }

    #[test]
    fn season_folder_and_bare_episode() {
        let p = parse_path("/tv/Show Name (2019)/Season 02/S02E05.mkv");
        assert_eq!(p.title, "Show Name");
        assert_eq!(p.year, Some(2019));
        assert_eq!(p.episode().unwrap().episodes, vec![5]);

        let p = parse_path(r"D:\TV\Show Name\Season 1\05 - Pilot.mkv");
        assert_eq!(p.title, "Show Name");
        let e = p.episode().unwrap();
        assert_eq!((e.season, e.episodes.clone()), (Some(1), vec![5]));
    }

    #[test]
    fn good_file_name_wins() {
        let p = parse_path("/dl/whatever/Show.S01E02.720p.HDTV.x264-LOL.mkv");
        assert_eq!(p.title, "Show");
        assert_eq!(p.quality.quality, Quality::Hdtv720p);
    }

    #[test]
    fn empty_paths() {
        assert_eq!(parse_path("").title, "");
        assert_eq!(parse_path("///").title, "");
    }
}
