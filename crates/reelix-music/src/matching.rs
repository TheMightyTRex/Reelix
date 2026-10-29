//! Matching audio files to the tracks of an album's releases.
//!
//! Every candidate release gets a *distance* in `0.0..=1.0` (lower is better) built from the
//! track count, the per-track duration delta (±3 s free, up to ±10 s penalised, beyond that a
//! full penalty), title similarity, the user's preferred countries/formats and MusicBrainz ids
//! found in the tags. Files are assigned to tracks by MusicBrainz track/recording id first,
//! then disc/track numbers (tags, else the file name), then title, then position.
//!
//! The best release is accepted when its distance is ≤ [`ACCEPT_DISTANCE`] and the best
//! release with a *different* tracklist is at least [`MIN_LEAD`] further away; releases with
//! the same tracklist (e.g. the US and UK CD) are interchangeable, and the album's selected
//! release (then the preferred country/format) wins among them.

use std::path::Path;

use reelix_api_types::music::{ReleaseMatchDto, TrackDto};
use reelix_import::matching::similarity;

use crate::tags::AudioInfo;

/// Best distance that is accepted automatically.
pub const ACCEPT_DISTANCE: f32 = 0.2;
/// Required lead over the best release with a different tracklist.
pub const MIN_LEAD: f32 = 0.1;
/// Disk scans accept looser matches (files are already in the library).
pub const SCAN_ACCEPT_DISTANCE: f32 = 0.5;

/// One release that the files may belong to.
#[derive(Debug, Clone)]
pub struct CandidateRelease {
    pub release_id: i64,
    pub musicbrainz_release_id: String,
    pub title: String,
    pub country: Option<String>,
    pub format: Option<String>,
    /// Ordered by disc, then track.
    pub tracks: Vec<TrackDto>,
}

/// User preferences that break ties.
#[derive(Debug, Clone, Default)]
pub struct Preferences {
    pub countries: Vec<String>,
    pub formats: Vec<String>,
    /// The album's selected release.
    pub selected: Option<i64>,
}

/// How well one release fits the files.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseScore {
    pub release_id: i64,
    pub title: String,
    pub track_count: u32,
    pub distance: f32,
    /// `(file index, track id)`.
    pub mapping: Vec<(usize, i64)>,
    pub missing_tracks: u32,
    pub extra_files: u32,
}

impl ReleaseScore {
    pub fn to_dto(&self) -> ReleaseMatchDto {
        ReleaseMatchDto {
            release_id: self.release_id,
            title: self.title.clone(),
            track_count: self.track_count,
            distance: (self.distance * 1000.0).round() / 1000.0,
            missing_tracks: self.missing_tracks,
            extra_files: self.extra_files,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchDecision {
    Matched(ReleaseScore),
    /// Several releases with different tracklists fit about equally well.
    Ambiguous(Vec<ReleaseScore>),
    /// Nothing fits well enough (sorted by distance).
    NoMatch(Vec<ReleaseScore>),
}

/// Disc/track/title of a file from its tags, falling back to the file name
/// (`01 - Title`, `1-03 Title`, `103 Title`, `CD2/04. Title`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileKey {
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub title: Option<String>,
}

pub fn file_key(f: &AudioInfo) -> FileKey {
    let (fd, ft, ftitle) = from_file_name(&f.path);
    FileKey {
        disc: f
            .tags
            .disc
            .filter(|d| *d > 0)
            .or(fd)
            .or_else(|| disc_from_folder(&f.path)),
        track: f.tags.track.filter(|t| *t > 0).or(ft),
        title: f.tags.title.clone().or(ftitle),
    }
}

fn disc_from_folder(path: &Path) -> Option<u32> {
    let parent = path.parent()?.file_name()?.to_str()?.to_ascii_lowercase();
    let rest = ["cd", "disc", "disk", "dvd"]
        .iter()
        .find_map(|p| parent.strip_prefix(p))?;
    rest.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()
}

/// `(disc, track, title)` from a file name.
pub fn from_file_name(path: &Path) -> (Option<u32>, Option<u32>, Option<String>) {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return (None, None, None);
    };
    let s = stem.trim();
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return (None, None, Some(s.to_owned()).filter(|t| !t.is_empty()));
    }
    let mut rest = &s[digits.len()..];
    let mut disc = None;
    let mut track: Option<u32> = digits.parse().ok();
    // "1-03 …" / "1.03 …" (disc-track)
    if let Some(r) = rest.strip_prefix(['-', '.'])
        && digits.len() <= 2
    {
        let d2: String = r.chars().take_while(char::is_ascii_digit).collect();
        if !d2.is_empty() && d2.len() <= 3 {
            disc = digits.parse().ok();
            track = d2.parse().ok();
            rest = &r[d2.len()..];
        }
    } else if digits.len() == 3 {
        // "103 …" = disc 1, track 03
        disc = digits[..1].parse().ok();
        track = digits[1..].parse().ok();
    }
    let title = rest
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '.' | '_' | ')' | ']'))
        .trim();
    // "Artist - Title" remnants: keep the last segment.
    let title = title.rsplit(" - ").next().unwrap_or(title).trim();
    (disc, track, Some(title.to_owned()).filter(|t| !t.is_empty()))
}

fn duration_penalty(file_ms: Option<u64>, track_ms: Option<u64>) -> f32 {
    match (file_ms, track_ms) {
        (Some(f), Some(t)) => {
            let delta = (f as f64 - t as f64).abs() / 1000.0;
            if delta <= 3.0 {
                0.0
            } else if delta <= 10.0 {
                ((delta - 3.0) / 7.0 * 0.5) as f32
            } else {
                1.0
            }
        }
        (None, Some(_)) => 0.2,
        (_, None) => 0.1,
    }
}

fn title_penalty(file_title: Option<&str>, track_title: &str) -> f32 {
    match file_title {
        Some(t) => (1.0 - similarity(t, track_title)).clamp(0.0, 1.0) as f32,
        None => 0.3,
    }
}

fn eq_ci(a: Option<&str>, list: &[String]) -> bool {
    list.is_empty() || a.is_some_and(|a| list.iter().any(|l| l.eq_ignore_ascii_case(a.trim())))
}

fn format_matches(format: Option<&str>, list: &[String]) -> bool {
    list.is_empty()
        || format.is_some_and(|f| {
            let f = f.to_ascii_lowercase();
            list.iter().any(|l| f.contains(&l.to_ascii_lowercase()))
        })
}

/// Preference penalty (0 = preferred country and format).
fn preference_penalty(r: &CandidateRelease, p: &Preferences) -> f32 {
    let c = if eq_ci(r.country.as_deref(), &p.countries) {
        0.0
    } else {
        0.5
    };
    let f = if format_matches(r.format.as_deref(), &p.formats) {
        0.0
    } else {
        0.5
    };
    c + f
}

/// Assigns files to the release's tracks and scores the result.
pub fn score_release(files: &[AudioInfo], r: &CandidateRelease, prefs: &Preferences) -> ReleaseScore {
    let keys: Vec<FileKey> = files.iter().map(file_key).collect();
    let mut assigned: Vec<Option<usize>> = vec![None; files.len()]; // file → track index
    let mut taken = vec![false; r.tracks.len()];
    let multi_disc = r.tracks.iter().any(|t| t.disc_number > 1);

    let assign = |fi: usize, ti: usize, assigned: &mut Vec<Option<usize>>, taken: &mut Vec<bool>| {
        assigned[fi] = Some(ti);
        taken[ti] = true;
    };

    // 1. MusicBrainz track id, then recording id.
    for (fi, f) in files.iter().enumerate() {
        let by_track = f.tags.musicbrainz_track_id.as_deref().and_then(|id| {
            r.tracks
                .iter()
                .position(|t| t.musicbrainz_track_id.as_deref() == Some(id))
        });
        let by_recording = || {
            let id = f.tags.musicbrainz_recording_id.as_deref()?;
            let hits: Vec<usize> = r
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, t)| !taken[*i] && t.musicbrainz_recording_id.as_deref() == Some(id))
                .map(|(i, _)| i)
                .collect();
            (hits.len() == 1).then(|| hits[0])
        };
        if let Some(ti) = by_track.filter(|ti| !taken[*ti]).or_else(by_recording) {
            assign(fi, ti, &mut assigned, &mut taken);
        }
    }
    // 2. Disc/track numbers (absolute number when a multi-disc file has no disc).
    for fi in 0..files.len() {
        if assigned[fi].is_some() {
            continue;
        }
        let k = &keys[fi];
        let Some(track) = k.track else { continue };
        let disc = k.disc.unwrap_or(1);
        let mut hit = r
            .tracks
            .iter()
            .enumerate()
            .find(|(i, t)| !taken[*i] && t.disc_number == disc && t.track_number == track)
            .map(|(i, _)| i);
        if hit.is_none() && multi_disc && k.disc.is_none() {
            hit = r
                .tracks
                .iter()
                .enumerate()
                .find(|(i, t)| !taken[*i] && t.absolute_number == Some(track))
                .map(|(i, _)| i);
        }
        if hit.is_none() && !multi_disc {
            hit = r
                .tracks
                .iter()
                .enumerate()
                .find(|(i, t)| !taken[*i] && t.track_number == track)
                .map(|(i, _)| i);
        }
        if let Some(ti) = hit {
            assign(fi, ti, &mut assigned, &mut taken);
        }
    }
    // 3. Title similarity.
    for fi in 0..files.len() {
        if assigned[fi].is_some() {
            continue;
        }
        let Some(title) = keys[fi].title.as_deref() else {
            continue;
        };
        let best = r
            .tracks
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .map(|(i, t)| (i, similarity(title, &t.title)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((ti, sim)) = best
            && sim >= 0.8
        {
            assign(fi, ti, &mut assigned, &mut taken);
        }
    }
    // 4. Position, when exactly the same number of files and tracks remain.
    let free_files: Vec<usize> = (0..files.len()).filter(|i| assigned[*i].is_none()).collect();
    let free_tracks: Vec<usize> = (0..r.tracks.len()).filter(|i| !taken[*i]).collect();
    if !free_files.is_empty() && free_files.len() == free_tracks.len() {
        let mut ff = free_files.clone();
        ff.sort_by(|a, b| files[*a].path.cmp(&files[*b].path));
        for (fi, ti) in ff.into_iter().zip(free_tracks) {
            assign(fi, ti, &mut assigned, &mut taken);
        }
    }

    let mut mapping = Vec::new();
    let (mut dur, mut title) = (0.0f32, 0.0f32);
    for (fi, ti) in assigned.iter().enumerate() {
        let Some(ti) = *ti else { continue };
        let t = &r.tracks[ti];
        dur += duration_penalty(files[fi].duration_ms, t.duration_ms);
        title += title_penalty(keys[fi].title.as_deref(), &t.title);
        mapping.push((fi, t.id));
    }
    let n = mapping.len().max(1) as f32;
    let extra = assigned.iter().filter(|a| a.is_none()).count() as u32;
    let missing = taken.iter().filter(|t| !**t).count() as u32;
    let total = r.tracks.len().max(files.len()).max(1) as f32;
    let count_term = (missing + extra) as f32 / total;
    let mut d = if mapping.is_empty() {
        1.0
    } else {
        0.45 * count_term + 0.3 * (dur / n) + 0.2 * (title / n) + 0.05 * preference_penalty(r, prefs)
    };
    let tagged_release = files
        .iter()
        .filter(|f| f.tags.musicbrainz_release_id.as_deref() == Some(r.musicbrainz_release_id.as_str()))
        .count();
    if !files.is_empty() && tagged_release * 2 > files.len() {
        d *= 0.25;
    }
    mapping.sort_by_key(|(fi, _)| *fi);
    ReleaseScore {
        release_id: r.release_id,
        title: r.title.clone(),
        track_count: r.tracks.len() as u32,
        distance: d.clamp(0.0, 1.0),
        mapping,
        missing_tracks: missing,
        extra_files: extra,
    }
}

/// Same number of discs/tracks and durations within ±3 s at every position.
fn same_tracklist(a: &CandidateRelease, b: &CandidateRelease) -> bool {
    a.tracks.len() == b.tracks.len()
        && a.tracks.iter().zip(&b.tracks).all(|(x, y)| {
            x.disc_number == y.disc_number
                && x.track_number == y.track_number
                && match (x.duration_ms, y.duration_ms) {
                    (Some(p), Some(q)) => p.abs_diff(q) <= 3000,
                    _ => true,
                }
        })
}

/// Scores every candidate and decides; `accept` is the maximum accepted distance.
pub fn decide(
    files: &[AudioInfo],
    candidates: &[CandidateRelease],
    prefs: &Preferences,
    accept: f32,
    require_lead: bool,
) -> MatchDecision {
    let mut scored: Vec<(ReleaseScore, &CandidateRelease)> = candidates
        .iter()
        .map(|c| (score_release(files, c, prefs), c))
        .collect();
    scored.sort_by(|a, b| {
        a.0.distance
            .total_cmp(&b.0.distance)
            .then_with(|| preference_penalty(a.1, prefs).total_cmp(&preference_penalty(b.1, prefs)))
            .then_with(|| a.0.release_id.cmp(&b.0.release_id))
    });
    let Some((best, best_c)) = scored.first().cloned() else {
        return MatchDecision::NoMatch(Vec::new());
    };
    if best.distance > accept {
        return MatchDecision::NoMatch(scored.into_iter().map(|s| s.0).collect());
    }
    if require_lead
        && let Some((rival, _)) = scored.iter().find(|(_, c)| !same_tracklist(c, best_c))
        && rival.distance - best.distance < MIN_LEAD
    {
        return MatchDecision::Ambiguous(scored.into_iter().map(|s| s.0).collect());
    }
    // Among interchangeable releases (same tracklist, about the same distance) keep the
    // selected one.
    let chosen = scored
        .iter()
        .filter(|(s, c)| same_tracklist(c, best_c) && s.distance - best.distance <= 0.02)
        .find(|(s, _)| Some(s.release_id) == prefs.selected)
        .map_or(best, |(s, _)| s.clone());
    MatchDecision::Matched(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::FileTags;

    fn track(id: i64, disc: u32, n: u32, title: &str, secs: u64) -> TrackDto {
        TrackDto {
            id,
            album_id: 1,
            release_id: 1,
            musicbrainz_track_id: None,
            musicbrainz_recording_id: None,
            disc_number: disc,
            track_number: n,
            absolute_number: Some(n),
            title: title.into(),
            duration_ms: Some(secs * 1000),
            artist_credit: None,
            track_file_id: None,
            track_file: None,
        }
    }

    fn file(name: &str, tags: FileTags, secs: u64) -> AudioInfo {
        AudioInfo {
            path: format!("/dl/{name}").into(),
            ext: "flac".into(),
            size: 1,
            duration_ms: Some(secs * 1000),
            bitrate_kbps: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            tags,
        }
    }

    fn release(id: i64, tracks: Vec<TrackDto>) -> CandidateRelease {
        CandidateRelease {
            release_id: id,
            musicbrainz_release_id: format!("rel-{id}"),
            title: format!("Release {id}"),
            country: None,
            format: None,
            tracks,
        }
    }

    #[test]
    fn file_names() {
        let p = |s: &str| from_file_name(Path::new(s));
        assert_eq!(p("/a/01 - Airbag.flac"), (None, Some(1), Some("Airbag".into())));
        assert_eq!(p("/a/1-03 Lucky.flac"), (Some(1), Some(3), Some("Lucky".into())));
        assert_eq!(p("/a/203. Song.mp3"), (Some(2), Some(3), Some("Song".into())));
        assert_eq!(
            p("/a/07 - Radiohead - No Surprises.mp3"),
            (None, Some(7), Some("No Surprises".into()))
        );
        assert_eq!(p("/a/Untitled.mp3"), (None, None, Some("Untitled".into())));
        let f = file("CD2/04 Song.flac", FileTags::default(), 1);
        assert_eq!(file_key(&f).disc, Some(2));
    }

    #[test]
    fn picks_matching_tracklist_and_flags_ambiguity() {
        let standard = release(1, vec![track(1, 1, 1, "One", 200), track(2, 1, 2, "Two", 180)]);
        let deluxe = release(
            2,
            vec![
                track(11, 1, 1, "One", 200),
                track(12, 1, 2, "Two", 180),
                track(13, 1, 3, "Bonus", 240),
            ],
        );
        let files = vec![
            file("01 - One.flac", FileTags::default(), 201),
            file("02 - Two.flac", FileTags::default(), 179),
        ];
        let prefs = Preferences::default();
        match decide(
            &files,
            &[deluxe.clone(), standard.clone()],
            &prefs,
            ACCEPT_DISTANCE,
            true,
        ) {
            MatchDecision::Matched(s) => {
                assert_eq!(s.release_id, 1);
                assert_eq!(s.mapping, vec![(0, 1), (1, 2)]);
                assert!(s.distance < 0.05);
            }
            other => panic!("{other:?}"),
        }
        // Same tracklist twice: interchangeable, the selected one wins.
        let twin = release(3, standard.tracks.clone());
        let prefs = Preferences {
            selected: Some(3),
            ..Default::default()
        };
        match decide(&files, &[standard.clone(), twin], &prefs, ACCEPT_DISTANCE, true) {
            MatchDecision::Matched(s) => assert_eq!(s.release_id, 3),
            other => panic!("{other:?}"),
        }
        // Different tracklists that both fit (no numbers, durations in between) → ambiguous.
        let alt = release(4, vec![track(21, 1, 1, "One", 205), track(22, 1, 2, "Two", 186)]);
        let untitled = vec![
            file("One.flac", FileTags::default(), 202),
            file("Two.flac", FileTags::default(), 183),
        ];
        assert!(matches!(
            decide(
                &untitled,
                &[standard, alt],
                &Preferences::default(),
                ACCEPT_DISTANCE,
                true
            ),
            MatchDecision::Ambiguous(_)
        ));
    }

    #[test]
    fn wrong_durations_do_not_match() {
        let r = release(1, vec![track(1, 1, 1, "One", 200), track(2, 1, 2, "Two", 180)]);
        let files = vec![
            file("01 - Foo.flac", FileTags::default(), 400),
            file("02 - Bar.flac", FileTags::default(), 30),
        ];
        assert!(matches!(
            decide(&files, &[r], &Preferences::default(), ACCEPT_DISTANCE, true),
            MatchDecision::NoMatch(_)
        ));
    }
}
