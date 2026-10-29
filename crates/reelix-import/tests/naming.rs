use std::path::Path;

use chrono::NaiveDate;
use reelix_import::naming::{
    ColonReplacement, EpisodeNaming, FileNaming, MovieNaming, MultiEpisodeStyle, NamingConfig, RenderContext,
    render, samples, sanitize, unknown_tokens, validate_template,
};
use reelix_import::{Error, SeriesType};
use reelix_parser::model::{Quality, QualityModel, Revision};

fn movie_ctx<'a>(m: &'a MovieNaming, f: &'a FileNaming) -> RenderContext<'a> {
    RenderContext {
        movie: Some(m),
        file: Some(f),
        ..Default::default()
    }
}

fn r(template: &str) -> String {
    let (m, f) = (samples::movie(), samples::movie_file());
    render(template, &movie_ctx(&m, &f), &NamingConfig::default())
}

fn ep(season: u32, episode: u32, title: &str) -> EpisodeNaming {
    EpisodeNaming {
        season,
        episode,
        absolute: None,
        title: title.into(),
        air_date: None,
    }
}

#[test]
fn default_preview() {
    let p = NamingConfig::default().preview();
    assert_eq!(p.movie_folder, "The Movie Title (2010) {tmdb-345691}");
    assert_eq!(
        p.movie_file,
        "The Movie Title (2010) {edition-Ultimate Extended Edition} [Surround Sound x264][Bluray-1080p Proper][DV HDR10]-RlsGrp"
    );
    assert_eq!(p.series_folder, "The Series Title! (2010) {tvdb-12345}");
    assert_eq!(p.season_folder, "Season 01");
    assert_eq!(
        p.standard_episode,
        "The Series Title! (2010) - S01E01 - Episode Title 1 [WEBDL-1080p]-RlsGrp"
    );
    assert_eq!(
        p.multi_episode,
        "The Series Title! (2010) - S01E01-02 - Episode Title [WEBDL-1080p]-RlsGrp"
    );
    assert_eq!(
        p.daily_episode,
        "The Series Title! (2010) - 2013-10-30 - Episode Title 1 [WEBDL-1080p]-RlsGrp"
    );
    assert_eq!(
        p.anime_episode,
        "The Series Title! (2010) - S01E01 - 001 - Episode Title 1 [WEBDL-1080p]-RlsGrp"
    );
}

#[test]
fn movie_token_variants() {
    assert_eq!(r("{Movie Title}"), "The Movie Title");
    assert_eq!(r("{Movie.Title}"), "The.Movie.Title");
    assert_eq!(r("{Movie_CleanTitle}"), "The_Movie_Title");
    assert_eq!(r("{MOVIE TITLE}"), "THE MOVIE TITLE");
    assert_eq!(r("{movie title}"), "the movie title");
    assert_eq!(r("{Movie TitleThe}"), "Movie Title, The");
    assert_eq!(r("{Movie TitleYear}"), "The Movie Title (2010)");
    assert_eq!(r("{Movie TitleFirstCharacter}/x").replace('+', "/"), "M/x");
    assert_eq!(r("{Movie OriginalTitle}"), "The Original Movie Title");
    assert_eq!(r("{Movie Collection}"), "The Movie Collection");
    assert_eq!(r("{Movie Certification}"), "R");
    assert_eq!(r("{Release Year}"), "2010");
    assert_eq!(r("{imdb-{ImdbId}}"), "{imdb-tt0066921}");
    assert_eq!(r("{Quality Title}"), "Bluray-1080p");
    assert_eq!(r("{Quality Proper}"), "Proper");
    assert_eq!(r("{Release Hash}"), "ABCDEFGH");
    assert_eq!(
        r("{Original Title}"),
        "The.Movie.Title.2010.1080p.BluRay.DTS.x264-RlsGrp"
    );
    assert_eq!(r("{Custom Format:x264}"), "x264");
}

#[test]
fn media_info_tokens() {
    assert_eq!(r("{MediaInfo VideoCodec}"), "h264");
    assert_eq!(r("{MediaInfo AudioCodec}"), "DTS");
    assert_eq!(r("{MediaInfo AudioChannels}"), "5.1");
    assert_eq!(r("{MediaInfo VideoBitDepth}"), "8");
    assert_eq!(r("{MediaInfo VideoDynamicRange}"), "HDR");
    assert_eq!(r("{MediaInfo VideoDynamicRangeType}"), "DV HDR10");
    assert_eq!(r("{MediaInfo AudioLanguages}"), "[EN+DE]");
    assert_eq!(r("{MediaInfo Simple}"), "h264 DTS");
    assert_eq!(r("{MediaInfo Full}"), "h264 DTS [EN+DE]");
    assert_eq!(r("{MediaInfo 3D}"), "");
}

#[test]
fn optional_prefix_suffix_and_blocks_vanish_when_empty() {
    let m = MovieNaming {
        title: "Bare".into(),
        ..Default::default()
    };
    let f = FileNaming {
        release_group: None,
        edition: None,
        custom_formats: vec![],
        media: Default::default(),
        ..samples::movie_file()
    };
    let cfg = NamingConfig::default();
    let out = render(&cfg.movie_file_format, &movie_ctx(&m, &f), &cfg);
    assert_eq!(out, "Bare [Bluray-1080p Proper]");
    assert_eq!(
        render("{Movie Title} {tmdb-{TmdbId}}", &movie_ctx(&m, &f), &cfg),
        "Bare"
    );
    assert_eq!(
        render("{Movie Title}{ (Release Year)}", &movie_ctx(&m, &f), &cfg),
        "Bare"
    );
    assert_eq!(r("{Movie Title}{ (Release Year)}"), "The Movie Title (2010)");
}

#[test]
fn unknown_tokens_render_literally_and_fail_validation() {
    assert_eq!(r("{Movie Title} {Bogus Token}"), "The Movie Title {Bogus Token}");
    assert_eq!(
        unknown_tokens("{Movie Title} {Bogus Token}"),
        vec!["{Bogus Token}"]
    );
    assert!(validate_template(&NamingConfig::default().movie_file_format).is_ok());
    assert!(validate_template(&NamingConfig::default().anime_episode_format).is_ok());
    assert!(matches!(
        validate_template("{Movie Title"),
        Err(Error::Template(_))
    ));
    assert!(matches!(validate_template("{Nope}"), Err(Error::Template(_))));
}

#[test]
fn colon_and_illegal_character_handling() {
    let mut cfg = NamingConfig::default();
    assert_eq!(
        sanitize("Mission: Impossible 10:30", &cfg),
        "Mission - Impossible 10-30"
    );
    cfg.colon_replacement = ColonReplacement::Delete;
    assert_eq!(sanitize("Mission: Impossible", &cfg), "Mission Impossible");
    cfg.colon_replacement = ColonReplacement::Dash;
    assert_eq!(sanitize("A:B", &cfg), "A-B");
    cfg.colon_replacement = ColonReplacement::SpaceDash;
    assert_eq!(sanitize("A: B", &cfg), "A - B");
    cfg.colon_replacement = ColonReplacement::SpaceDashSpace;
    assert_eq!(sanitize("A:B", &cfg), "A - B");
    assert_eq!(
        sanitize("AC/DC? <Live> *Rare* \"x\" |y|", &cfg),
        "AC+DC! Live -Rare- x y"
    );
    cfg.replace_illegal_characters = false;
    assert_eq!(sanitize("AC/DC? *x*", &cfg), "ACDC x");
    assert_eq!(sanitize("...hidden.", &cfg), "hidden");

    // Titles with slashes never create sub-folders.
    let m = MovieNaming {
        title: "Face/Off".into(),
        year: Some(1997),
        ..Default::default()
    };
    let f = samples::movie_file();
    let cfg = NamingConfig::default();
    assert_eq!(
        render("{Movie Title} ({Release Year})", &movie_ctx(&m, &f), &cfg),
        "Face+Off (1997)"
    );
}

fn multi(style: MultiEpisodeStyle, template: &str) -> String {
    let s = samples::series();
    let eps = vec![ep(1, 3, "Part 3"), ep(1, 1, "Part 1"), ep(1, 2, "Part 2")];
    let f = samples::episode_file();
    let cfg = NamingConfig {
        multi_episode_style: style,
        ..Default::default()
    };
    let ctx = RenderContext {
        series: Some(&s),
        episodes: &eps,
        file: Some(&f),
        multi_episode_style: style,
        ..Default::default()
    };
    render(template, &ctx, &cfg)
}

#[test]
fn multi_episode_styles() {
    let t = "{Series Title} - S{season:00}E{episode:00}";
    assert_eq!(
        multi(MultiEpisodeStyle::Extend, t),
        "The Series Title! - S01E01-02-03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Duplicate, t),
        "The Series Title! - S01E01 - S01E02 - S01E03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Repeat, t),
        "The Series Title! - S01E01E02E03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Scene, t),
        "The Series Title! - S01E01-E02-E03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Range, t),
        "The Series Title! - S01E01-03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::PrefixedRange, t),
        "The Series Title! - S01E01-E03"
    );
    assert_eq!(
        multi(
            MultiEpisodeStyle::Extend,
            "{Series.CleanTitle}.{season}x{episode:00}"
        ),
        "The.Series.Title.1x01-02-03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Repeat, "{season}x{episode:00}"),
        "1x01x02x03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Duplicate, "Show.S{season:00}E{episode:00}"),
        "Show.S01E01.S01E02.S01E03"
    );
    assert_eq!(
        multi(MultiEpisodeStyle::Extend, "{Episode Title}"),
        "Part 1 + Part 2 + Part 3"
    );
    assert_eq!(multi(MultiEpisodeStyle::Range, "E{episode:000}"), "E001-003");
}

#[test]
fn series_tokens_and_air_date() {
    let s = samples::series();
    let eps = vec![EpisodeNaming {
        air_date: NaiveDate::from_ymd_opt(2024, 3, 5),
        ..ep(2, 7, "Pilot: Redux")
    }];
    let ctx = RenderContext {
        series: Some(&s),
        episodes: &eps,
        ..Default::default()
    };
    let cfg = NamingConfig::default();
    let r = |t: &str| render(t, &ctx, &cfg);
    assert_eq!(r("{Series CleanTitle}"), "The Series Title");
    assert_eq!(r("{Series TitleThe}"), "Series Title!, The");
    assert_eq!(r("{Series TitleYear}"), "The Series Title! (2010)");
    assert_eq!(r("{Series CleanTitleYear}"), "The Series Title (2010)");
    assert_eq!(r("{Series TitleWithoutYear}"), "The Series Title!");
    assert_eq!(
        r("{tvdb-{TvdbId}} {tmdb-{TmdbId}} {imdb-{ImdbId}}"),
        "{tvdb-12345} {tmdb-11223} {imdb-tt12345}"
    );
    assert_eq!(r("{Air-Date}"), "2024-03-05");
    assert_eq!(r("{Air.Date}"), "2024.03.05");
    assert_eq!(r("{Air Date}"), "2024 03 05");
    assert_eq!(r("{Episode Title}"), "Pilot - Redux");
    assert_eq!(r("{Episode CleanTitle}"), "Pilot Redux");
    assert_eq!(r("{Episode Title:8} [x]"), "Pilot - R [x]");
    assert_eq!(r("{Episode Title:6} [x]"), "Pilot [x]");
    assert_eq!(r("{Episode Title:30}"), "Pilot - Redux");
    assert_eq!(r("Season {season:00}"), "Season 02");
    assert_eq!(r("{season}"), "2");
}

#[test]
fn quality_full_revisions() {
    let cfg = NamingConfig::default();
    let m = samples::movie();
    let mk = |version, real, is_repack| FileNaming {
        quality: QualityModel {
            quality: Quality::WebDl2160p,
            revision: Revision {
                version,
                real,
                is_repack,
            },
        },
        ..samples::movie_file()
    };
    let f = mk(2, 1, true);
    assert_eq!(
        render("{Quality Full}", &movie_ctx(&m, &f), &cfg),
        "WEBDL-2160p Repack REAL"
    );
    assert_eq!(render("{Quality Real}", &movie_ctx(&m, &f), &cfg), "REAL");
    let f = mk(1, 0, false);
    assert_eq!(render("{Quality Full}", &movie_ctx(&m, &f), &cfg), "WEBDL-2160p");
    let f = mk(3, 0, false);
    let anime = RenderContext {
        series_type: SeriesType::Anime,
        ..movie_ctx(&m, &f)
    };
    assert_eq!(render("{Quality Full}", &anime, &cfg), "WEBDL-2160p v3");
}

#[test]
fn episode_paths_specials_and_anime() {
    let cfg = NamingConfig::default();
    let root = Path::new("/tv");
    let s = samples::series();
    let f = samples::episode_file();
    let p = cfg
        .episode_path(root, &s, &[ep(0, 4, "Behind It All")], &f, ".mkv")
        .unwrap();
    assert_eq!(
        p,
        Path::new(
            "/tv/The Series Title! (2010) {tvdb-12345}/Specials/The Series Title! (2010) - S00E04 - Behind It All [WEBDL-1080p]-RlsGrp.mkv"
        )
    );
    let anime = samples::anime_series();
    let eps = [EpisodeNaming {
        absolute: Some(1052),
        ..ep(21, 1, "A New Era")
    }];
    let name = cfg.episode_file_name(&anime, &eps, &f, 4).unwrap();
    assert_eq!(
        name,
        "The Series Title! (2010) - S21E01 - 1052 - A New Era [WEBDL-1080p]-RlsGrp"
    );
    // Daily series without an air date falls back to the standard format.
    let daily = samples::daily_series();
    let name = cfg
        .episode_file_name(&daily, &[ep(2024, 15, "Guest")], &f, 4)
        .unwrap();
    assert!(name.contains("S2024E15"), "{name}");
}

#[test]
fn movie_path_and_rename_disabled() {
    let mut cfg = NamingConfig::default();
    let (m, mut f) = (samples::movie(), samples::movie_file());
    let p = cfg.movie_path(Path::new("/movies"), &m, &f, "mkv").unwrap();
    assert!(p.starts_with("/movies/The Movie Title (2010) {tmdb-345691}"));
    assert_eq!(p.extension().unwrap(), "mkv");
    cfg.rename_files = false;
    f.original_file_name = Some("the.movie.2010.1080p-grp".into());
    assert_eq!(cfg.movie_file_name(&m, &f).unwrap(), "the.movie.2010.1080p-grp");
}

#[test]
fn length_guard_truncates_episode_titles_then_errors() {
    let cfg = NamingConfig {
        max_file_name_bytes: 90,
        ..Default::default()
    };
    let s = samples::series();
    let f = samples::episode_file();
    let long = "An Extraordinarily Long Episode Title That Goes On And On Without Any End In Sight";
    let name = cfg.episode_file_name(&s, &[ep(1, 1, long)], &f, 4).unwrap();
    assert!(name.len() + 4 <= 90, "{} bytes: {name}", name.len());
    assert!(name.contains("An Extraordinarily"), "{name}");
    assert!(name.ends_with("[WEBDL-1080p]-RlsGrp"), "{name}");

    let tiny = NamingConfig {
        max_file_name_bytes: 30,
        ..Default::default()
    };
    assert!(matches!(
        tiny.episode_file_name(&s, &[ep(1, 1, long)], &f, 4),
        Err(Error::PathTooLong { .. })
    ));
    let short_path = NamingConfig {
        max_path_bytes: 40,
        ..Default::default()
    };
    assert!(matches!(
        short_path.movie_path(
            Path::new("/movies"),
            &samples::movie(),
            &samples::movie_file(),
            "mkv"
        ),
        Err(Error::PathTooLong { .. })
    ));
}

#[test]
fn config_serde_roundtrip_with_defaults() {
    let cfg: NamingConfig = serde_json::from_str(r#"{"colon_replacement":"dash"}"#).unwrap();
    assert_eq!(cfg.colon_replacement, ColonReplacement::Dash);
    assert_eq!(cfg.season_folder_format, "Season {season:00}");
    let back: NamingConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(back, cfg);
}
