//! An estimate of the aired (TVDB and broadcast) order of a season, from TMDB's own episodes.
//!
//! Release names often number a season as TVDB and broadcasters do, and TMDB can differ in
//! two common ways:
//!
//! - Specials moved out of the season: TVDB's Archer season 3 starts with the three parts of
//!   "Heart of Archness", which TMDB keeps as specials (season 0), so TVDB's S03E11 is TMDB's
//!   S03E08.
//! - Double-length episodes kept as one: TVDB splits The Good Place's season 2 premiere and
//!   Scrubs' "My Finale" into two numbers each, TMDB keeps one, so TVDB's last number is one
//!   higher than TMDB's.
//!
//! When TMDB has no episode group for the series ([`crate::episode_map`]), [`estimate_episode`]
//! rebuilds the season in each plausible aired order (specials aired just before, during or just
//! after the season, and double-length episodes counted twice) and reads the number in each.
//! It answers only when every order that reaches the number points at the same episode; any
//! disagreement means the match is not clear, and the file is left for Choose the episode.

use chrono::NaiveDate;

/// An episode as the estimate needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeFacts {
    pub id: i64,
    pub season: u32,
    pub episode: u32,
    pub air_date: Option<NaiveDate>,
    pub runtime_minutes: Option<u32>,
}

/// A special shorter than this share of the season's usual runtime (a trailer, a mini-episode,
/// a behind-the-scenes clip) is never counted as an episode of the season.
const SPECIAL_MIN_SHARE: f64 = 0.6;
/// An episode at least this many times the season's usual runtime is a double-length episode
/// that other orders may count as two.
const DOUBLE_SHARE: f64 = 1.7;

/// A season with no later season counts as finished once its last episode aired this many days
/// before today.
const SETTLED_DAYS: i64 = 180;

/// The TMDB episode that `season`/`episode`, read in an aired order, points at, or `None` when
/// no aired order reaches it or the orders disagree. Season 0 is never estimated, and neither is
/// a season that may still be airing (no later season, last episode under 180 days before
/// `today`): there a number TMDB lacks is more likely a new episode TMDB has not listed yet. The
/// caller uses this only for a number TMDB's own numbering does not have.
pub fn estimate_episode(
    season: u32,
    episode: u32,
    episodes: &[EpisodeFacts],
    today: NaiveDate,
) -> Option<i64> {
    if season == 0 || episode == 0 {
        return None;
    }
    let mut regular: Vec<&EpisodeFacts> = episodes.iter().filter(|e| e.season == season).collect();
    if regular.is_empty() {
        return None;
    }
    regular.sort_by_key(|e| e.episode);
    // Positions only mean something when TMDB numbers the season 1, 2, 3 and so on.
    if regular
        .iter()
        .enumerate()
        .any(|(i, e)| e.episode as usize != i + 1)
    {
        return None;
    }
    let usual = median_runtime(&regular);
    let first = regular.iter().filter_map(|e| e.air_date).min();
    let last = regular.iter().filter_map(|e| e.air_date).max();
    let later_season = episodes.iter().any(|e| e.season > season);
    let settled = last.is_some_and(|l| (today - l).num_days() > SETTLED_DAYS);
    if !later_season && !settled {
        return None;
    }
    let previous_end = season_end(episodes, season.checked_sub(1).filter(|s| *s > 0));
    let next_start = season_start(episodes, season + 1);

    // Specials long enough to be an episode, with an air date, sorted by it.
    let mut specials: Vec<&EpisodeFacts> = episodes
        .iter()
        .filter(|e| e.season == 0 && e.air_date.is_some())
        .filter(|e| match (usual, e.runtime_minutes) {
            (Some(u), Some(r)) => f64::from(r) >= SPECIAL_MIN_SHARE * u,
            _ => false,
        })
        .collect();
    specials.sort_by_key(|e| (e.air_date, e.episode));
    let (Some(first), Some(last)) = (first, last) else {
        // Without air dates only the double-length reading is possible.
        return read(&regular, &[], usual, true, episode);
    };
    let before: Vec<&EpisodeFacts> = specials
        .iter()
        .copied()
        .filter(|s| s.air_date < Some(first) && previous_end.is_none_or(|p| s.air_date > Some(p)))
        .collect();
    let during: Vec<&EpisodeFacts> = specials
        .iter()
        .copied()
        .filter(|s| s.air_date >= Some(first) && s.air_date <= Some(last))
        .collect();
    let after: Vec<&EpisodeFacts> = specials
        .iter()
        .copied()
        .filter(|s| s.air_date > Some(last) && next_start.is_none_or(|n| s.air_date < Some(n)))
        .collect();

    let mut found = Vec::new();
    for mask in 0..8u8 {
        let mut extra: Vec<&EpisodeFacts> = Vec::new();
        for (bit, group) in [(1, &before), (2, &during), (4, &after)] {
            if mask & bit != 0 {
                extra.extend(group.iter().copied());
            }
        }
        for doubles in [false, true] {
            if mask == 0 && !doubles {
                // TMDB's own order, which the caller has already tried.
                continue;
            }
            found.push(read(&regular, &extra, usual, doubles, episode));
        }
    }
    agree(&found)
}

/// The one episode every reading that reached the number agrees on.
fn agree(found: &[Option<i64>]) -> Option<i64> {
    let mut ids = found.iter().flatten();
    let first = *ids.next()?;
    ids.all(|id| *id == first).then_some(first)
}

/// Episode number `episode` in one reading: the season's episodes with `extra` specials placed
/// by air date, and double-length episodes counted twice when `doubles` is set.
fn read(
    regular: &[&EpisodeFacts],
    extra: &[&EpisodeFacts],
    usual: Option<f64>,
    doubles: bool,
    episode: u32,
) -> Option<i64> {
    let mut order: Vec<&EpisodeFacts> = regular.to_vec();
    for s in extra {
        // A special goes before the first episode that aired after it.
        let at = order
            .iter()
            .position(|e| e.season != 0 && e.air_date.is_some_and(|d| Some(d) > s.air_date))
            .unwrap_or(order.len());
        order.insert(at, *s);
    }
    let mut numbered: Vec<i64> = Vec::with_capacity(order.len() + 2);
    for e in order {
        numbered.push(e.id);
        let double = doubles
            && e.season != 0
            && matches!((usual, e.runtime_minutes), (Some(u), Some(r)) if f64::from(r) >= DOUBLE_SHARE * u);
        if double {
            numbered.push(e.id);
        }
    }
    numbered.get(episode as usize - 1).copied()
}

fn median_runtime(eps: &[&EpisodeFacts]) -> Option<f64> {
    let mut r: Vec<u32> = eps
        .iter()
        .filter_map(|e| e.runtime_minutes)
        .filter(|r| *r > 0)
        .collect();
    if r.is_empty() {
        return None;
    }
    r.sort_unstable();
    Some(f64::from(r[r.len() / 2]))
}

fn season_end(episodes: &[EpisodeFacts], season: Option<u32>) -> Option<NaiveDate> {
    let season = season?;
    episodes
        .iter()
        .filter(|e| e.season == season)
        .filter_map(|e| e.air_date)
        .max()
}

fn season_start(episodes: &[EpisodeFacts], season: u32) -> Option<NaiveDate> {
    episodes
        .iter()
        .filter(|e| e.season == season)
        .filter_map(|e| e.air_date)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 19).unwrap()
    }

    fn d(s: &str) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    }

    fn ep(id: i64, season: u32, episode: u32, air: &str, runtime: u32) -> EpisodeFacts {
        EpisodeFacts {
            id,
            season,
            episode,
            air_date: d(air),
            runtime_minutes: Some(runtime),
        }
    }

    /// Weekly episodes from `start`, `runtime` minutes each, ids `season * 100 + episode`.
    fn season(season: u32, count: u32, start: &str, runtime: u32) -> Vec<EpisodeFacts> {
        let start = d(start).unwrap();
        (1..=count)
            .map(|e| EpisodeFacts {
                id: i64::from(season * 100 + e),
                season,
                episode: e,
                air_date: Some(start + chrono::Duration::weeks(i64::from(e - 1))),
                runtime_minutes: Some(runtime),
            })
            .collect()
    }

    /// Archer as the owner's library has it (19 November 2026): season 3 is 10 episodes from
    /// 19 January 2012; "Heart of Archness" is three specials aired in September 2011, after
    /// season 2 ended; "L'Espion Mal Fait" is a 5-minute special.
    fn archer() -> Vec<EpisodeFacts> {
        let mut v = season(2, 13, "2011-01-27", 21);
        v.extend(season(3, 10, "2012-01-19", 21));
        v.extend(season(4, 13, "2013-01-17", 21));
        v.extend([
            ep(3, 0, 3, "2011-09-15", 22),
            ep(4, 0, 4, "2011-09-22", 22),
            ep(5, 0, 5, "2011-09-29", 22),
            ep(6, 0, 6, "2011-12-27", 5),
            ep(7, 0, 7, "2013-10-10", 22),
        ]);
        v
    }

    #[test]
    fn archer_tvdb_numbers_after_the_specials() {
        let eps = archer();
        assert_eq!(estimate_episode(3, 11, &eps, today()), Some(308));
        assert_eq!(estimate_episode(3, 12, &eps, today()), Some(309));
        assert_eq!(estimate_episode(3, 13, &eps, today()), Some(310));
        // Nothing reaches a number beyond every reading.
        assert_eq!(estimate_episode(3, 14, &eps, today()), None);
    }

    /// The Good Place: each season opens with a double-length premiere TVDB counts as two.
    fn good_place() -> Vec<EpisodeFacts> {
        let mut v = season(1, 13, "2016-09-19", 22);
        let mut s2 = season(2, 12, "2017-09-20", 22);
        s2[0].runtime_minutes = Some(43);
        let mut s3 = season(3, 12, "2018-09-27", 22);
        s3[0].runtime_minutes = Some(44);
        v.extend(s2);
        v.extend(s3);
        // Short web specials are not episodes.
        v.push(ep(1, 0, 1, "2018-09-20", 3));
        v
    }

    #[test]
    fn good_place_finale_after_a_double_premiere() {
        let eps = good_place();
        assert_eq!(
            estimate_episode(2, 13, &eps, today()),
            Some(212),
            "Somewhere Else"
        );
        assert_eq!(estimate_episode(3, 13, &eps, today()), Some(312), "Pandemonium");
    }

    #[test]
    fn scrubs_my_finale_counts_twice() {
        let mut eps = season(8, 18, "2009-01-06", 24);
        eps[17].runtime_minutes = Some(48);
        // Scrubs' specials have no air dates and are left out.
        eps.push(EpisodeFacts {
            id: 2,
            season: 0,
            episode: 2,
            air_date: None,
            runtime_minutes: Some(29),
        });
        assert_eq!(estimate_episode(8, 19, &eps, today()), Some(818), "My Finale");
    }

    #[test]
    fn friday_night_dinner_christmas_after_season_two() {
        let mut eps = season(2, 6, "2012-10-07", 24);
        eps.extend(season(3, 6, "2014-01-03", 24));
        eps.push(ep(1, 0, 1, "2012-12-24", 30));
        assert_eq!(estimate_episode(2, 7, &eps, today()), Some(1));
    }

    #[test]
    fn disagreeing_orders_give_no_answer() {
        // A double-length second episode and a special aired after the season: E06 is the
        // finale when E02 counts twice, but the special when it follows the season.
        let mut eps = season(1, 5, "2020-01-08", 30);
        eps[1].runtime_minutes = Some(60);
        eps.push(ep(9, 0, 1, "2020-02-20", 30));
        assert_eq!(estimate_episode(1, 6, &eps, today()), None);
        // E07 is reached only when both apply, so that reading stands alone.
        assert_eq!(estimate_episode(1, 7, &eps, today()), Some(9));
    }

    #[test]
    fn season_zero_and_unknown_seasons_are_not_estimated() {
        let eps = archer();
        assert_eq!(estimate_episode(0, 9, &eps, today()), None);
        assert_eq!(estimate_episode(9, 1, &eps, today()), None);
    }

    #[test]
    fn a_season_still_airing_is_not_estimated() {
        // E11 of a season whose ten episodes aired in the last weeks, with a double-length
        // premiere: most likely a new episode TMDB has not listed yet, not the tenth.
        let mut eps = season(1, 10, "2026-09-01", 22);
        eps[0].runtime_minutes = Some(44);
        assert_eq!(estimate_episode(1, 11, &eps, today()), None);
        // The same season once a later one exists, or long after it aired.
        let mut with_next = eps.clone();
        with_next.extend(season(2, 2, "2027-09-01", 22));
        assert_eq!(estimate_episode(1, 11, &with_next, today()), Some(110));
        let later = NaiveDate::from_ymd_opt(2027, 11, 19).unwrap();
        assert_eq!(estimate_episode(1, 11, &eps, later), Some(110));
    }
}
