//! Episode titles in release names.
//!
//! Releases named with broadcast or TVDB numbers ("Archer.S03E11.Space.Race.Part.1.720p.HDTV")
//! do not always agree with TMDB's numbers (TMDB has season 3 of Archer as 10 episodes). The
//! episode title after the number does: [`match_episode_title`] finds the one episode of the
//! series whose title the release name carries, so the importer can trust it over the number.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// `S03E11`, `S03E11E12`, `S03E11-E12` or `3x11`: the episode number the title follows.
static EPISODE_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^a-z0-9])(?:s\d{1,4}[ ._-]?e\d{1,4}(?:[ ._-]?e?\d{1,4})*|\d{1,2}x\d{1,3})(?:[^a-z0-9]|$)",
    )
    .expect("valid regex")
});

/// Words that end the episode title: resolution, source, codec and release tags.
const END_WORDS: &[&str] = &[
    "480p", "480i", "576p", "576i", "720p", "720i", "1080p", "1080i", "2160p", "4k", "uhd", "hdtv", "pdtv",
    "sdtv", "dsr", "web", "webdl", "webrip", "bluray", "bdrip", "brrip", "dvdrip", "dvd", "hdrip", "remux",
    "x264", "x265", "h264", "h265", "hevc", "avc", "xvid", "divx", "proper", "repack", "rerip", "internal",
    "multi", "subbed", "dubbed", "amzn", "nf", "hulu", "dsnp", "atvp", "pcok", "itunes", "10bit", "hdr",
    "mkv", "mp4", "avi",
];

/// Words dropped on both sides: articles, `and`, and the "part" of "Part 1" (TMDB writes "(1)").
const IGNORED_WORDS: &[&str] = &["the", "a", "an", "and", "part", "pt"];

/// Titles that do not name one episode ("Episode 3", "TBA").
const GENERIC_WORDS: &[&str] = &["episode", "chapter", "tba", "tbd"];

/// One episode of the series, as the matcher sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeTitle<'a> {
    pub id: i64,
    pub season: u32,
    pub title: &'a str,
}

/// Lower-case words: diacritics stripped, `&` as `and`, apostrophes dropped, ignored words removed.
fn words(s: &str) -> Vec<String> {
    s.nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
        .replace('&', " and ")
        .replace(['\'', '’', '`', '´'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !IGNORED_WORDS.contains(w))
        .map(str::to_owned)
        .collect()
}

/// The words between the episode number and the first resolution, source or codec tag.
/// `None` when the name has no `SxxEyy` (or `1x02`) number or nothing follows it.
pub fn title_after_episode_number(release: &str) -> Option<Vec<String>> {
    let m = EPISODE_NUMBER.find(release)?;
    let title: Vec<String> = words(&release[m.end()..])
        .into_iter()
        .take_while(|w| !END_WORDS.contains(&w.as_str()))
        .collect();
    (!title.is_empty()).then_some(title)
}

/// A title that can name one episode: some letters, and not only "Episode 3" or "TBA".
fn is_specific(title: &[String]) -> bool {
    let letters: usize = title
        .iter()
        .map(|w| w.chars().filter(|c| c.is_alphabetic()).count())
        .sum();
    letters >= 3
        && !title
            .iter()
            .all(|w| GENERIC_WORDS.contains(&w.as_str()) || w.chars().all(|c| c.is_ascii_digit()))
}

/// The one episode whose title follows the episode number in `release`.
///
/// An exact title wins; otherwise the longest title the release's words start with (the name may
/// carry more words before its tags, such as a language). When two episodes tie, the one in
/// `season` (the season in the name) is taken; any other tie gives `None`, so the caller falls
/// back to the numbers.
pub fn match_episode_title(release: &str, season: Option<u32>, episodes: &[EpisodeTitle<'_>]) -> Option<i64> {
    let named = title_after_episode_number(release)?;
    let titles: Vec<(EpisodeTitle<'_>, Vec<String>)> = episodes
        .iter()
        .map(|e| (*e, words(e.title)))
        .filter(|(_, t)| is_specific(t))
        .collect();
    let exact: Vec<EpisodeTitle<'_>> = titles
        .iter()
        .filter(|(_, t)| *t == named)
        .map(|(e, _)| *e)
        .collect();
    if !exact.is_empty() {
        return one(&exact, season);
    }
    let longest = titles
        .iter()
        .filter(|(_, t)| named.starts_with(t))
        .map(|(_, t)| t.len())
        .max()?;
    let prefix: Vec<EpisodeTitle<'_>> = titles
        .iter()
        .filter(|(_, t)| t.len() == longest && named.starts_with(t))
        .map(|(e, _)| *e)
        .collect();
    one(&prefix, season)
}

fn one(found: &[EpisodeTitle<'_>], season: Option<u32>) -> Option<i64> {
    match found {
        [only] => Some(only.id),
        _ => {
            let in_season: Vec<&EpisodeTitle<'_>> =
                found.iter().filter(|e| Some(e.season) == season).collect();
            match in_season.as_slice() {
                [only] => Some(only.id),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: i64, season: u32, title: &str) -> EpisodeTitle<'_> {
        EpisodeTitle { id, season, title }
    }

    /// Archer's season 3 as TMDB has it: 10 episodes, "Space Race" as one two-part episode pair.
    fn archer() -> Vec<EpisodeTitle<'static>> {
        vec![
            ep(301, 3, "Heart of Archness (1)"),
            ep(302, 3, "Heart of Archness (2)"),
            ep(303, 3, "Heart of Archness (3)"),
            ep(304, 3, "The Man from Jupiter"),
            ep(305, 3, "El Contador"),
            ep(306, 3, "The Limited"),
            ep(307, 3, "Drift Problem"),
            ep(308, 3, "Lo Scandalo"),
            ep(309, 3, "Bloody Ferlin"),
            ep(310, 3, "Crossing Over"),
            ep(311, 3, "Skin Game"),
            ep(312, 3, "Space Race (1)"),
            ep(313, 3, "Space Race (2)"),
        ]
    }

    #[test]
    fn title_after_the_number_stops_at_the_tags() {
        assert_eq!(
            title_after_episode_number("Archer.S03E11.Space.Race.Part.1.720p.HDTV.x264-ORENJI"),
            Some(vec!["space".into(), "race".into(), "1".into()])
        );
        assert_eq!(
            title_after_episode_number("Archer (2009) - 3x07 - Drift Problem [WEB-DL 1080p]"),
            Some(vec!["drift".into(), "problem".into()])
        );
        assert_eq!(title_after_episode_number("Archer.S03E07.1080p.WEB-DL"), None);
        assert_eq!(title_after_episode_number("Archer.2009.Space.Race.1080p"), None);
    }

    #[test]
    fn broadcast_numbers_match_the_tmdb_episode_by_title() {
        let eps = archer();
        assert_eq!(
            match_episode_title(
                "Archer.S03E11.Space.Race.Part.1.720p.HDTV.x264-ORENJI",
                Some(3),
                &eps
            ),
            Some(312)
        );
        assert_eq!(
            match_episode_title(
                "Archer.S03E12.Space.Race.Part.2.720p.HDTV.x264-ORENJI",
                Some(3),
                &eps
            ),
            Some(313)
        );
        assert_eq!(
            match_episode_title(
                "archer.s03e04.the.man.from.jupiter.1080p.bluray.x264",
                Some(3),
                &eps
            ),
            Some(304)
        );
        assert_eq!(
            match_episode_title("Archer.S03E05.El.Contador.German.720p", Some(3), &eps),
            Some(305),
            "extra words before the tags"
        );
    }

    #[test]
    fn no_title_or_an_unknown_title_gives_none() {
        let eps = archer();
        assert_eq!(
            match_episode_title("Archer.S03E07.1080p.WEB-DL", Some(3), &eps),
            None
        );
        assert_eq!(
            match_episode_title("Archer.S03E07.Something.Else.1080p", Some(3), &eps),
            None
        );
        assert_eq!(
            match_episode_title("Archer.S03E07.Space.1080p", Some(3), &eps),
            None
        );
    }

    #[test]
    fn same_title_in_two_seasons_uses_the_season_in_the_name() {
        let eps = vec![ep(1, 1, "Pilot"), ep(2, 0, "Pilot")];
        assert_eq!(
            match_episode_title("Show.S01E01.Pilot.720p", Some(1), &eps),
            Some(1)
        );
        assert_eq!(match_episode_title("Show.S05E01.Pilot.720p", Some(5), &eps), None);
    }

    #[test]
    fn generic_titles_are_never_matched() {
        let eps = vec![ep(1, 1, "Episode 3"), ep(2, 1, "TBA"), ep(3, 1, "Go")];
        assert_eq!(
            match_episode_title("Show.S01E05.Episode.3.720p", Some(1), &eps),
            None
        );
        assert_eq!(match_episode_title("Show.S01E05.TBA.720p", Some(1), &eps), None);
        assert_eq!(
            match_episode_title("Show.S01E05.Go.720p", Some(1), &eps),
            None,
            "too short"
        );
    }

    #[test]
    fn punctuation_accents_and_articles_do_not_matter() {
        let eps = vec![ep(7, 2, "Café & Crème: The Return"), ep(8, 2, "Who's There?")];
        assert_eq!(
            match_episode_title(
                "Show.S02E09.Cafe.and.Creme.Return.1080p.AMZN.WEB-DL",
                Some(2),
                &eps
            ),
            Some(7)
        );
        assert_eq!(
            match_episode_title("Show.S02E10.Whos.There.720p", Some(2), &eps),
            Some(8)
        );
    }
}
