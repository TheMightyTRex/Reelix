use chrono::NaiveDate;
use reelix_import::SeriesType;
use reelix_import::matching::{
    EpisodeMatchMethod, EpisodeMismatch, ExpectedEpisode, ExpectedMovie, MovieMismatch, TitleMatch,
    best_movie_match, match_episodes, match_movie, match_parsed_episodes,
};
use reelix_parser::model::{EpisodeInfo, ParsedRelease, ReleaseKind};

fn movie_release(title: &str, year: Option<u16>) -> ParsedRelease {
    let mut p = ParsedRelease::empty(title);
    p.title = title.into();
    p.year = year;
    p.kind = ReleaseKind::Movie;
    p
}

fn expected(title: &str, alts: &[&str], year: u16) -> ExpectedMovie {
    ExpectedMovie {
        title: title.into(),
        alternative_titles: alts.iter().map(|s| s.to_string()).collect(),
        year: Some(year),
    }
}

#[test]
fn movie_matching() {
    let e = expected("Amélie", &["Le Fabuleux Destin d'Amélie Poulain"], 2001);
    let m = match_movie(&movie_release("Amelie", Some(2001)), &e).unwrap();
    assert_eq!((m.title, m.year_exact), (TitleMatch::Exact, Some(true)));
    let m = match_movie(
        &movie_release("Le Fabuleux Destin dAmelie Poulain", Some(2001)),
        &e,
    )
    .unwrap();
    assert_eq!(m.title, TitleMatch::Alternative);
    let m = match_movie(&movie_release("Amelie", Some(2002)), &e).unwrap();
    assert_eq!(m.year_exact, Some(false));
    let m = match_movie(&movie_release("Amelie", None), &e).unwrap();
    assert_eq!(m.year_exact, None);
    assert!(matches!(
        match_movie(&movie_release("Amelie", Some(2010)), &e),
        Err(MovieMismatch::Year {
            found: 2010,
            expected: 2001
        })
    ));
    assert!(matches!(
        match_movie(&movie_release("Taxi Driver", Some(2001)), &e),
        Err(MovieMismatch::Title { .. })
    ));

    let e = expected("The Shawshank Redemption", &[], 1994);
    let m = match_movie(&movie_release("Shawshank Redemtion", Some(1994)), &e).unwrap();
    assert_eq!(m.title, TitleMatch::Fuzzy);
    assert!(match_movie(&movie_release("", Some(1994)), &e).is_err());
}

#[test]
fn best_candidate_prefers_exact_year() {
    let cands = vec![
        expected("Dune", &[], 1984),
        expected("Dune", &[], 2021),
        expected("Dune: Part Two", &[], 2024),
    ];
    let (i, _) = best_movie_match(&movie_release("Dune", Some(2021)), &cands).unwrap();
    assert_eq!(i, 1);
    let (i, _) = best_movie_match(&movie_release("Dune Part Two", Some(2024)), &cands).unwrap();
    assert_eq!(i, 2);
    assert!(best_movie_match(&movie_release("Dune", Some(2000)), &cands).is_none());
}

fn eps() -> Vec<ExpectedEpisode> {
    let mut v = Vec::new();
    let mut abs = 0;
    for season in 0..=2u32 {
        for episode in 1..=4u32 {
            if season > 0 {
                abs += 1;
            }
            v.push(ExpectedEpisode {
                id: i64::from(season * 100 + episode),
                season,
                episode,
                absolute: (season > 0).then_some(abs),
                air_date: NaiveDate::from_ymd_opt(2020, season.max(1), episode),
                ..Default::default()
            });
        }
    }
    v
}

fn info(season: Option<u32>, episodes: &[u32]) -> EpisodeInfo {
    EpisodeInfo {
        season,
        episodes: episodes.to_vec(),
        ..Default::default()
    }
}

#[test]
fn season_episode_and_multi_episode() {
    let m = match_episodes(&info(Some(1), &[2]), SeriesType::Standard, &eps()).unwrap();
    assert_eq!(
        (m.episode_ids.clone(), m.method),
        (vec![102], EpisodeMatchMethod::SeasonEpisode)
    );
    assert!(m.is_complete());
    let m = match_episodes(&info(Some(2), &[3, 4, 5]), SeriesType::Standard, &eps()).unwrap();
    assert_eq!(m.episode_ids, vec![203, 204]);
    assert_eq!(m.unmatched, vec!["S02E05"]);
    assert!(!m.is_complete());
    assert!(matches!(
        match_episodes(&info(Some(9), &[1]), SeriesType::Standard, &eps()),
        Err(EpisodeMismatch::NotFound(_))
    ));
    // Specials are season 0.
    let m = match_episodes(&info(Some(0), &[3]), SeriesType::Standard, &eps()).unwrap();
    assert_eq!(m.episode_ids, vec![3]);
}

#[test]
fn packs() {
    let full = EpisodeInfo {
        season: Some(2),
        full_season: true,
        ..Default::default()
    };
    let m = match_episodes(&full, SeriesType::Standard, &eps()).unwrap();
    assert_eq!(
        (m.episode_ids.len(), m.method),
        (4, EpisodeMatchMethod::FullSeason)
    );
    let range = EpisodeInfo {
        season_range: Some((1, 2)),
        ..Default::default()
    };
    let m = match_episodes(&range, SeriesType::Standard, &eps()).unwrap();
    assert_eq!(
        (m.episode_ids.len(), m.method),
        (8, EpisodeMatchMethod::SeasonRange)
    );
    assert_eq!(
        match_episodes(&EpisodeInfo::default(), SeriesType::Standard, &eps()),
        Err(EpisodeMismatch::NoNumbering)
    );
}

#[test]
fn daily_by_air_date() {
    let daily = EpisodeInfo {
        air_date: NaiveDate::from_ymd_opt(2020, 2, 3),
        ..Default::default()
    };
    let m = match_episodes(&daily, SeriesType::Daily, &eps()).unwrap();
    assert_eq!(
        (m.episode_ids.clone(), m.method),
        (vec![203], EpisodeMatchMethod::AirDate)
    );
    let none = EpisodeInfo {
        air_date: NaiveDate::from_ymd_opt(1999, 1, 1),
        ..Default::default()
    };
    assert!(matches!(
        match_episodes(&none, SeriesType::Daily, &eps()),
        Err(EpisodeMismatch::NotFound(_))
    ));
}

#[test]
fn anime_absolute_and_scene_numbering() {
    let abs = EpisodeInfo {
        absolute_episodes: vec![5, 6],
        ..Default::default()
    };
    let m = match_episodes(&abs, SeriesType::Anime, &eps()).unwrap();
    assert_eq!(
        (m.episode_ids.clone(), m.method),
        (vec![201, 202], EpisodeMatchMethod::Absolute)
    );

    // Anime with both: absolute wins.
    let both = EpisodeInfo {
        season: Some(1),
        episodes: vec![1],
        absolute_episodes: vec![8],
        ..Default::default()
    };
    assert_eq!(
        match_episodes(&both, SeriesType::Anime, &eps())
            .unwrap()
            .episode_ids,
        vec![204]
    );
    // Standard series with both: season/episode wins.
    assert_eq!(
        match_episodes(&both, SeriesType::Standard, &eps())
            .unwrap()
            .episode_ids,
        vec![101]
    );

    // Scene mapping: scene S03E01 is metadata S02E01.
    let mut list = eps();
    let e = list.iter_mut().find(|e| e.id == 201).unwrap();
    e.scene_season = Some(3);
    e.scene_episode = Some(1);
    e.scene_absolute = Some(50);
    let m = match_episodes(&info(Some(3), &[1]), SeriesType::Standard, &list).unwrap();
    assert_eq!(
        (m.episode_ids.clone(), m.method),
        (vec![201], EpisodeMatchMethod::SceneSeasonEpisode)
    );
    // The mapped episode is no longer addressable by its metadata numbers.
    assert!(match_episodes(&info(Some(2), &[1]), SeriesType::Standard, &list).is_err());
    let scene_abs = EpisodeInfo {
        absolute_episodes: vec![50],
        ..Default::default()
    };
    let m = match_episodes(&scene_abs, SeriesType::Anime, &list).unwrap();
    assert_eq!(
        (m.episode_ids.clone(), m.method),
        (vec![201], EpisodeMatchMethod::SceneAbsolute)
    );
}

#[test]
fn parsed_release_wrapper() {
    let mut p = ParsedRelease::empty("Show.S01E01");
    assert_eq!(
        match_parsed_episodes(&p, SeriesType::Standard, &eps()),
        Err(EpisodeMismatch::NotAnEpisode)
    );
    p.kind = ReleaseKind::Episode(info(Some(1), &[1]));
    assert_eq!(
        match_parsed_episodes(&p, SeriesType::Standard, &eps())
            .unwrap()
            .episode_ids,
        vec![101]
    );
}
