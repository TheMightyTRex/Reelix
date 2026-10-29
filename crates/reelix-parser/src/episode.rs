//! Season / episode / absolute / air-date detection.

use chrono::NaiveDate;
use regex::Regex;

use crate::model::EpisodeInfo;

/// An episode marker found in the release body; `start` is where the title ends.
#[derive(Debug, Clone)]
pub(crate) struct EpMatch {
    pub start: usize,
    pub info: EpisodeInfo,
}

re!(STD, r"(?i)\bS(\d{1,4})[ .-]?E(\d{1,4})");
re!(
    STD_CONT,
    r"(?i)^(?:[ .]?([-&+~])[ .]?|[ .]?)(?:S(\d{1,4})[ .]?)?E(\d{1,4})"
);
re!(NUM_RANGE, r"^[ .]?-[ .]?(\d{1,4})\b");
re!(
    SEASON_EPISODE_WORDS,
    r"(?i)\bSeason[ .]?(\d{1,2})[ .]?(?:-[ .]?)?(?:Episode|Ep)[ .]?(\d{1,3})\b"
);
re!(CROSS, r"(?i)\b(\d{1,2})x(\d{2,3})\b");
re!(CROSS_CONT, r"(?i)^[ .]?([-&])[ .]?(?:\d{1,2}x)?(\d{2,3})\b");
re!(DAILY_YMD, r"\b((?:19|20)\d{2})[ .-](\d{2})[ .-](\d{2})\b");
re!(DAILY_DMY, r"\b(\d{2})[ .-](\d{2})[ .-]((?:19|20)\d{2})\b");
re!(
    ANIME_SEASON,
    r"(?i)\b(?:S|Season )(\d{1,2}) - (\d{2,4})(?:v(\d))?\b"
);
re!(
    ANIME_DASH,
    r"(?i) - (\d{1,4})(?:v\d)?(?: ?[-~] ?(\d{2,4})(?:v\d)?)?(?:$|[ \[(.])"
);
re!(ANIME_NODASH, r"(?i) (\d{2,4})(?:v\d)? ?[\[(]");
re!(ANIME_BATCH, r"[\[(](\d{2,4}) ?[-~] ?(\d{2,4})[\])]");
re!(ANIME_EP, r"(?i)\b(?:Episode|Ep\.?)[ ]?(\d{1,4})\b");
re!(
    ANIME_SPECIAL,
    r"(?i)\b(?:OVA|OAD|ONA|OVD|SP|Specials?)(?:[ ]?(\d{1,3}))?\b"
);
re!(SEASON_S, r"(?i)\bS(\d{1,2})(?:[ .]?-[ .]?S?(\d{1,2}))?\b");
re!(
    SEASON_WORD,
    r"(?i)\b(?:Complete[ .])?(?:Seasons?|Saison|Staffel|Temporada|Stagione)[ .]?(\d{1,2})(?:[ .]?(?:-|to|&)[ .]?(?:(?:Seasons?|S)[ .]?)?(\d{1,2}))?\b"
);
re!(COMPLETE_SERIES, r"(?i)\bComplete[ .]Series\b");
re!(PART, r"(?i)\b(?:Part|Pt)[ .]?(\d{1,2})\b");
re!(BARE_EP, r"(?i)(?:^|[ .\-])(?:E|Ep|Episode)[ .]?(\d{1,3})\b");

fn num(s: &str) -> Option<u32> {
    s.parse().ok()
}

fn cap_u32(c: &regex::Captures<'_>, i: usize) -> Option<u32> {
    c.get(i).and_then(|m| num(m.as_str()))
}

/// Pushes `from..=to` (bounded, so garbage never allocates huge vectors).
fn push_range(out: &mut Vec<u32>, from: u32, to: u32) {
    if to > from && to - from <= 200 {
        out.extend(from + 1..=to);
    } else if to != from && !out.contains(&to) {
        out.push(to);
    }
}

fn is_year_like(n: u32) -> bool {
    (1900..=2099).contains(&n)
}

/// Finds the episode marker in `body`.
/// `anime` enables bracket-group conventions; `allow_weak` enables `Part N` / bare `E05`.
pub(crate) fn detect(body: &str, anime: bool, allow_weak: bool) -> Option<EpMatch> {
    standard(body)
        .or_else(|| season_episode_words(body))
        .or_else(|| cross(body))
        .or_else(|| daily(body))
        .or_else(|| anime_season(body))
        .or_else(|| anime_dash(body, anime))
        .or_else(|| if anime { anime_nodash(body) } else { None })
        .or_else(|| anime_batch(body))
        .or_else(|| if anime { anime_ep(body) } else { None })
        .or_else(|| if anime { anime_special(body) } else { None })
        .or_else(|| season_pack(body))
        .or_else(|| complete_series(body))
        .or_else(|| if allow_weak { weak(body) } else { None })
}

fn standard(body: &str) -> Option<EpMatch> {
    let c = STD.captures(body)?;
    let m = c.get(0)?;
    let season = cap_u32(&c, 1)?;
    let first = cap_u32(&c, 2)?;
    let mut episodes = vec![first];
    let mut pos = m.end();
    loop {
        let rest = body.get(pos..).unwrap_or("");
        if let Some(cc) = STD_CONT.captures(rest) {
            let Some(n) = cap_u32(&cc, 3) else { break };
            if cc
                .get(2)
                .and_then(|s| num(s.as_str()))
                .is_some_and(|s| s != season)
            {
                break;
            }
            let last = *episodes.last().unwrap_or(&n);
            if cc.get(1).is_some_and(|d| d.as_str() == "-") {
                push_range(&mut episodes, last, n);
            } else if !episodes.contains(&n) {
                episodes.push(n);
            }
            pos += cc.get(0).map_or(0, |x| x.end());
            continue;
        }
        if let Some(cc) = NUM_RANGE.captures(rest)
            && let Some(n) = cap_u32(&cc, 1)
        {
            let last = *episodes.last().unwrap_or(&n);
            if n > last && n - last < 100 {
                push_range(&mut episodes, last, n);
                pos += cc.get(0).map_or(0, |x| x.end());
                continue;
            }
        }
        break;
    }
    Some(EpMatch {
        start: m.start(),
        info: EpisodeInfo {
            season: Some(season),
            episodes,
            is_special: season == 0,
            ..Default::default()
        },
    })
}

fn season_episode_words(body: &str) -> Option<EpMatch> {
    let c = SEASON_EPISODE_WORDS.captures(body)?;
    let season = cap_u32(&c, 1)?;
    Some(EpMatch {
        start: c.get(0)?.start(),
        info: EpisodeInfo {
            season: Some(season),
            episodes: vec![cap_u32(&c, 2)?],
            is_special: season == 0,
            ..Default::default()
        },
    })
}

fn cross(body: &str) -> Option<EpMatch> {
    let c = CROSS.captures(body)?;
    let m = c.get(0)?;
    let season = cap_u32(&c, 1)?;
    let mut episodes = vec![cap_u32(&c, 2)?];
    let mut pos = m.end();
    while let Some(cc) = CROSS_CONT.captures(body.get(pos..).unwrap_or("")) {
        let Some(n) = cap_u32(&cc, 2) else { break };
        let last = *episodes.last().unwrap_or(&n);
        if n <= last {
            break;
        }
        if cc.get(1).is_some_and(|d| d.as_str() == "-") {
            push_range(&mut episodes, last, n);
        } else {
            episodes.push(n);
        }
        pos += cc.get(0).map_or(0, |x| x.end());
    }
    Some(EpMatch {
        start: m.start(),
        info: EpisodeInfo {
            season: Some(season),
            episodes,
            is_special: season == 0,
            ..Default::default()
        },
    })
}

fn daily(body: &str) -> Option<EpMatch> {
    let date = |start: usize, y: u32, m: u32, d: u32| {
        NaiveDate::from_ymd_opt(y as i32, m, d).map(|date| EpMatch {
            start,
            info: EpisodeInfo {
                air_date: Some(date),
                ..Default::default()
            },
        })
    };
    for c in DAILY_YMD.captures_iter(body) {
        let (Some(y), Some(m), Some(d)) = (cap_u32(&c, 1), cap_u32(&c, 2), cap_u32(&c, 3)) else {
            continue;
        };
        if let Some(found) = date(c.get(0)?.start(), y, m, d) {
            return Some(found);
        }
    }
    for c in DAILY_DMY.captures_iter(body) {
        let (Some(a), Some(b), Some(y)) = (cap_u32(&c, 1), cap_u32(&c, 2), cap_u32(&c, 3)) else {
            continue;
        };
        // US order (MM.DD.YYYY) unless the first number can't be a month.
        let (m, d) = if a > 12 { (b, a) } else { (a, b) };
        if let Some(found) = date(c.get(0)?.start(), y, m, d) {
            return Some(found);
        }
    }
    None
}

fn anime_season(body: &str) -> Option<EpMatch> {
    let c = ANIME_SEASON.captures(body)?;
    let season = cap_u32(&c, 1)?;
    let ep = cap_u32(&c, 2)?;
    if is_year_like(ep) {
        return None;
    }
    Some(EpMatch {
        start: c.get(0)?.start(),
        info: EpisodeInfo {
            season: Some(season),
            episodes: vec![ep],
            is_special: season == 0,
            ..Default::default()
        },
    })
}

fn absolute(start: usize, from: u32, to: Option<u32>) -> EpMatch {
    let mut abs = vec![from];
    if let Some(to) = to {
        push_range(&mut abs, from, to);
    }
    EpMatch {
        start,
        info: EpisodeInfo {
            absolute_episodes: abs,
            ..Default::default()
        },
    }
}

fn anime_dash(body: &str, anime: bool) -> Option<EpMatch> {
    for c in ANIME_DASH.captures_iter(body) {
        let Some(n) = cap_u32(&c, 1) else { continue };
        let digits = c.get(1).map_or(0, |m| m.as_str().len());
        if is_year_like(n) && digits == 4 {
            continue;
        }
        // Single digits only count for bracket-group (anime) releases.
        if digits < 2 && !anime {
            continue;
        }
        return Some(absolute(c.get(0)?.start(), n, cap_u32(&c, 2)));
    }
    None
}

fn anime_nodash(body: &str) -> Option<EpMatch> {
    for c in ANIME_NODASH.captures_iter(body) {
        let Some(n) = cap_u32(&c, 1) else { continue };
        if is_year_like(n) || matches!(n, 480 | 576 | 720 | 1080 | 2160) {
            continue;
        }
        return Some(absolute(c.get(0)?.start(), n, None));
    }
    None
}

fn anime_batch(body: &str) -> Option<EpMatch> {
    let c = ANIME_BATCH.captures(body)?;
    let (from, to) = (cap_u32(&c, 1)?, cap_u32(&c, 2)?);
    if to <= from || is_year_like(from) {
        return None;
    }
    Some(absolute(c.get(0)?.start(), from, Some(to)))
}

fn anime_ep(body: &str) -> Option<EpMatch> {
    let c = ANIME_EP.captures(body)?;
    Some(absolute(c.get(0)?.start(), cap_u32(&c, 1)?, None))
}

fn anime_special(body: &str) -> Option<EpMatch> {
    let c = ANIME_SPECIAL.captures(body)?;
    let m = c.get(0)?;
    if m.start() == 0 {
        return None;
    }
    Some(EpMatch {
        start: m.start(),
        info: EpisodeInfo {
            season: Some(0),
            episodes: cap_u32(&c, 1).into_iter().collect(),
            is_special: true,
            ..Default::default()
        },
    })
}

fn pack(start: usize, from: u32, to: Option<u32>) -> EpMatch {
    let range = to.filter(|t| *t > from).map(|t| (from, t));
    EpMatch {
        start,
        info: EpisodeInfo {
            season: Some(from),
            full_season: true,
            season_range: range,
            is_special: from == 0,
            ..Default::default()
        },
    }
}

fn season_pack(body: &str) -> Option<EpMatch> {
    let s = SEASON_S
        .captures(body)
        .and_then(|c| Some((c.get(0)?.start(), cap_u32(&c, 1)?, cap_u32(&c, 2))));
    let w = SEASON_WORD
        .captures(body)
        .and_then(|c| Some((c.get(0)?.start(), cap_u32(&c, 1)?, cap_u32(&c, 2))));
    let best = match (s, w) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }?;
    Some(pack(best.0, best.1, best.2))
}

fn complete_series(body: &str) -> Option<EpMatch> {
    let m = COMPLETE_SERIES.find(body)?;
    Some(EpMatch {
        start: m.start(),
        info: EpisodeInfo {
            full_season: true,
            ..Default::default()
        },
    })
}

fn weak(body: &str) -> Option<EpMatch> {
    let find = |re: &Regex| -> Option<(usize, u32)> {
        let c = re.captures(body)?;
        let m = c.get(0)?;
        let start = m.start() + m.as_str().len() - m.as_str().trim_start_matches([' ', '.', '-']).len();
        Some((start, cap_u32(&c, 1)?))
    };
    // "Part 2" of a mini-series is S01E02; a bare "E05" has no season.
    let (start, n, season) = match find(&PART) {
        Some((s, n)) => (s, n, Some(1)),
        None => {
            let (s, n) = find(&BARE_EP)?;
            (s, n, None)
        }
    };
    if n == 0 {
        return None;
    }
    Some(EpMatch {
        start,
        info: EpisodeInfo {
            season,
            episodes: vec![n],
            ..Default::default()
        },
    })
}
