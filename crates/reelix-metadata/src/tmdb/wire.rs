//! Raw TMDB v3 JSON shapes and their conversion into the provider-neutral model.

use serde::Deserialize;

use crate::model::*;
use reelix_core::MediaKind;

/// Builds absolute image URLs for one image base.
#[derive(Debug, Clone)]
pub(crate) struct Img<'a> {
    pub base: &'a str,
}

pub(crate) const POSTER: &str = "w500";
pub(crate) const BACKDROP: &str = "w1280";
pub(crate) const LOGO: &str = "w500";
pub(crate) const STILL: &str = "w300";
pub(crate) const PROFILE: &str = "w185";

impl Img<'_> {
    pub fn url(&self, size: &str, path: Option<&str>) -> Option<String> {
        let path = path.map(str::trim).filter(|p| !p.is_empty())?;
        Some(crate::tmdb::image_url(self.base, size, path))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WResults<T> {
    pub results: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WPage<T> {
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCompany {
    pub id: u32,
    pub name: String,
    pub logo_path: Option<String>,
    pub origin_country: Option<String>,
}

impl WCompany {
    fn into_model(self, img: &Img) -> Company {
        Company {
            id: self.id,
            logo_url: img.url(LOGO, self.logo_path.as_deref()),
            name: self.name,
            origin_country: non_empty(self.origin_country),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCollectionRef {
    pub id: u32,
    pub name: String,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WRelease {
    pub certification: Option<String>,
    pub note: Option<String>,
    pub release_date: Option<String>,
    #[serde(rename = "type")]
    pub kind: u8,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCountryReleases {
    pub iso_3166_1: String,
    pub release_dates: Vec<WRelease>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WAlt {
    pub iso_3166_1: Option<String>,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// Movies use `titles`, series use `results`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WAltTitles {
    pub titles: Vec<WAlt>,
    pub results: Vec<WAlt>,
}

impl WAltTitles {
    fn into_model(self) -> Vec<AlternativeTitle> {
        self.titles
            .into_iter()
            .chain(self.results)
            .filter(|a| !a.title.trim().is_empty())
            .map(|a| AlternativeTitle {
                title: a.title,
                country: non_empty(a.iso_3166_1),
                kind: non_empty(a.kind),
            })
            .collect()
    }
}

/// Movies use `keywords`, series use `results`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WKeywords {
    pub keywords: Vec<Keyword>,
    pub results: Vec<Keyword>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WExternalIds {
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub wikidata_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCast {
    pub id: u32,
    pub name: String,
    pub character: Option<String>,
    pub order: u32,
    pub profile_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCrew {
    pub id: u32,
    pub name: String,
    pub job: String,
    pub department: String,
    pub profile_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCredits {
    pub cast: Vec<WCast>,
    pub crew: Vec<WCrew>,
}

impl WCredits {
    fn into_model(self, img: &Img) -> Credits {
        let mut cast: Vec<_> = self
            .cast
            .into_iter()
            .map(|c| CastMember {
                tmdb_id: c.id,
                profile_url: img.url(PROFILE, c.profile_path.as_deref()),
                name: c.name,
                character: non_empty(c.character),
                order: c.order,
            })
            .collect();
        cast.sort_by_key(|c| c.order);
        let crew = self
            .crew
            .into_iter()
            .map(|c| CrewMember {
                tmdb_id: c.id,
                profile_url: img.url(PROFILE, c.profile_path.as_deref()),
                name: c.name,
                job: c.job,
                department: c.department,
            })
            .collect();
        Credits { cast, crew }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WImage {
    pub file_path: String,
    pub width: u32,
    pub height: u32,
    pub iso_639_1: Option<String>,
    pub vote_average: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WImages {
    pub posters: Vec<WImage>,
    pub backdrops: Vec<WImage>,
    pub logos: Vec<WImage>,
}

fn images_list(v: Vec<WImage>, img: &Img) -> Vec<Image> {
    v.into_iter()
        .filter(|i| !i.file_path.is_empty())
        .map(|i| Image {
            url: crate::tmdb::image_url(img.base, "original", &i.file_path),
            path: i.file_path,
            width: i.width,
            height: i.height,
            language: non_empty(i.iso_639_1),
            vote_average: i.vote_average,
        })
        .collect()
}

pub(crate) fn build_images(
    poster: Option<&str>,
    backdrop: Option<&str>,
    extra: Option<WImages>,
    img: &Img,
) -> Images {
    let extra = extra.unwrap_or_default();
    let logos = images_list(extra.logos, img);
    Images {
        poster: img.url(POSTER, poster),
        backdrop: img.url(BACKDROP, backdrop),
        // Logos only come from the images append; prefer the first (language-filtered) one.
        logo: logos
            .first()
            .map(|l| crate::tmdb::image_url(img.base, LOGO, &l.path)),
        posters: images_list(extra.posters, img),
        backdrops: images_list(extra.backdrops, img),
        logos,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WVideo {
    pub key: String,
    pub site: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub iso_639_1: Option<String>,
    pub official: bool,
}

fn videos(v: Option<WResults<WVideo>>) -> Vec<Video> {
    v.unwrap_or_default()
        .results
        .into_iter()
        .map(|v| {
            let url = match v.site.as_str() {
                "YouTube" => Some(format!("https://www.youtube.com/watch?v={}", v.key)),
                "Vimeo" => Some(format!("https://vimeo.com/{}", v.key)),
                _ => None,
            };
            Video {
                url,
                key: v.key,
                site: v.site,
                kind: v.kind,
                name: v.name,
                language: non_empty(v.iso_639_1),
                official: v.official,
            }
        })
        .collect()
}

/// A search/list/discover result (movie, series or person).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WHit {
    pub id: u32,
    pub media_type: Option<String>,
    pub title: Option<String>,
    pub name: Option<String>,
    pub original_title: Option<String>,
    pub original_name: Option<String>,
    pub release_date: Option<String>,
    pub first_air_date: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub profile_path: Option<String>,
    pub known_for_department: Option<String>,
    pub genre_ids: Vec<u32>,
    pub original_language: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
    pub adult: bool,
}

impl WHit {
    fn kind(&self, default: Option<MediaKind>) -> Option<MediaKind> {
        match self.media_type.as_deref() {
            Some("movie") => Some(MediaKind::Movie),
            Some("tv") => Some(MediaKind::Series),
            Some(_) => None,
            None => default,
        }
    }

    /// `None` for persons (or unknown media types).
    pub fn into_hit(self, default: Option<MediaKind>, img: &Img) -> Option<SearchHit> {
        let kind = self.kind(default)?;
        let (title, original, date) = match kind {
            MediaKind::Movie => (self.title.or(self.name), self.original_title, self.release_date),
            MediaKind::Series => (self.name.or(self.title), self.original_name, self.first_air_date),
        };
        let release_date = parse_date(date.as_deref());
        Some(SearchHit {
            kind,
            tmdb_id: Some(self.id),
            tvdb_id: None,
            imdb_id: None,
            title: title.unwrap_or_default(),
            original_title: non_empty(original),
            year: year_of(release_date),
            release_date,
            overview: non_empty(self.overview),
            poster: img.url(POSTER, self.poster_path.as_deref()),
            backdrop: img.url(BACKDROP, self.backdrop_path.as_deref()),
            genre_ids: self.genre_ids,
            original_language: non_empty(self.original_language),
            vote_average: self.vote_average,
            vote_count: self.vote_count,
            popularity: self.popularity,
            adult: self.adult,
        })
    }

    pub fn into_multi(self, img: &Img) -> Option<MultiHit> {
        if self.media_type.as_deref() == Some("person") {
            return Some(MultiHit::Person(PersonSummary {
                tmdb_id: self.id,
                name: self.name.unwrap_or_default(),
                known_for_department: non_empty(self.known_for_department),
                profile_url: img.url(PROFILE, self.profile_path.as_deref()),
                popularity: self.popularity,
            }));
        }
        self.into_hit(None, img).map(MultiHit::Media)
    }
}

pub(crate) fn hits(v: Vec<WHit>, default: Option<MediaKind>, img: &Img) -> Vec<SearchHit> {
    v.into_iter().filter_map(|h| h.into_hit(default, img)).collect()
}

pub(crate) fn page_of(p: WPage<WHit>, default: Option<MediaKind>, img: &Img) -> Page<SearchHit> {
    Page {
        page: p.page.max(1),
        total_pages: p.total_pages,
        total_results: p.total_results,
        results: hits(p.results, default, img),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WMovie {
    pub id: u32,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: String,
    pub original_language: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<u32>,
    pub status: Option<String>,
    pub genres: Vec<Genre>,
    pub production_companies: Vec<WCompany>,
    pub belongs_to_collection: Option<WCollectionRef>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
    pub adult: bool,
    pub homepage: Option<String>,
    pub release_dates: Option<WResults<WCountryReleases>>,
    pub alternative_titles: Option<WAltTitles>,
    pub external_ids: Option<WExternalIds>,
    pub credits: Option<WCredits>,
    pub images: Option<WImages>,
    pub videos: Option<WResults<WVideo>>,
    pub keywords: Option<WKeywords>,
    pub recommendations: Option<WPage<WHit>>,
    pub similar: Option<WPage<WHit>>,
}

fn movie_status(s: Option<&str>) -> MovieStatus {
    match s {
        Some("Rumored") => MovieStatus::Rumored,
        Some("Planned") => MovieStatus::Planned,
        Some("In Production") => MovieStatus::InProduction,
        Some("Post Production") => MovieStatus::PostProduction,
        Some("Released") => MovieStatus::Released,
        Some("Canceled") => MovieStatus::Canceled,
        _ => MovieStatus::Unknown,
    }
}

/// Picks cinema/digital/physical dates and certification for `region`
/// (falling back to the earliest date worldwide when the region has none).
fn release_dates(raw: Vec<WCountryReleases>, region: &str) -> (ReleaseDates, Option<String>) {
    let mut all = Vec::new();
    for c in raw {
        for r in c.release_dates {
            let (Some(kind), Some(date)) = (
                ReleaseType::from_tmdb(r.kind),
                parse_date(r.release_date.as_deref()),
            ) else {
                continue;
            };
            all.push(RegionalRelease {
                country: c.iso_3166_1.clone(),
                kind,
                date,
                certification: non_empty(r.certification),
                note: non_empty(r.note),
            });
        }
    }
    all.sort_by_key(|r| r.date);
    let pick = |kinds: &[ReleaseType]| {
        let of_kind = |r: &&RegionalRelease| kinds.contains(&r.kind);
        all.iter()
            .filter(of_kind)
            .find(|r| r.country.eq_ignore_ascii_case(region))
            .or_else(|| all.iter().find(of_kind))
            .map(|r| r.date)
    };
    let cinema = pick(&[ReleaseType::Theatrical]).or_else(|| pick(&[ReleaseType::TheatricalLimited]));
    let digital = pick(&[ReleaseType::Digital]);
    let physical = pick(&[ReleaseType::Physical]);
    // Prefer the theatrical certification, then any certification in the region.
    let in_region = || all.iter().filter(|r| r.country.eq_ignore_ascii_case(region));
    let certification = in_region()
        .filter(|r| matches!(r.kind, ReleaseType::Theatrical | ReleaseType::TheatricalLimited))
        .find_map(|r| r.certification.clone())
        .or_else(|| in_region().find_map(|r| r.certification.clone()));
    (
        ReleaseDates {
            cinema,
            digital,
            physical,
            all,
        },
        certification,
    )
}

impl WMovie {
    pub fn into_model(self, img: &Img, region: &str) -> MovieMetadata {
        let release_date = parse_date(self.release_date.as_deref());
        let (dates, certification) =
            release_dates(self.release_dates.map(|r| r.results).unwrap_or_default(), region);
        let ext = self.external_ids.unwrap_or_default();
        let imdb_id = non_empty(self.imdb_id).or(non_empty(ext.imdb_id));
        let kw = self.keywords.unwrap_or_default();
        MovieMetadata {
            tmdb_id: self.id,
            external_ids: ExternalIds {
                tmdb: Some(self.id),
                imdb: imdb_id.clone(),
                tvdb: None,
                wikidata: non_empty(ext.wikidata_id),
            },
            imdb_id,
            sort_title: sort_title(&self.title),
            original_title: if self.original_title.is_empty() {
                self.title.clone()
            } else {
                self.original_title
            },
            title: self.title,
            original_language: non_empty(self.original_language),
            overview: non_empty(self.overview),
            tagline: non_empty(self.tagline),
            year: year_of(release_date).or_else(|| year_of(dates.cinema)),
            release_date,
            release_dates: dates,
            certification,
            status: movie_status(self.status.as_deref()),
            runtime_minutes: self.runtime.filter(|r| *r > 0),
            genres: self.genres,
            keywords: kw.keywords.into_iter().chain(kw.results).collect(),
            studios: self
                .production_companies
                .into_iter()
                .map(|c| c.into_model(img))
                .collect(),
            collection: self.belongs_to_collection.map(|c| CollectionRef {
                tmdb_id: c.id,
                poster: img.url(POSTER, c.poster_path.as_deref()),
                backdrop: img.url(BACKDROP, c.backdrop_path.as_deref()),
                name: c.name,
            }),
            alternative_titles: self.alternative_titles.unwrap_or_default().into_model(),
            credits: self.credits.unwrap_or_default().into_model(img),
            images: build_images(
                self.poster_path.as_deref(),
                self.backdrop_path.as_deref(),
                self.images,
                img,
            ),
            videos: videos(self.videos),
            ratings: Ratings {
                vote_average: self.vote_average,
                vote_count: self.vote_count,
                popularity: self.popularity,
            },
            adult: self.adult,
            homepage: non_empty(self.homepage),
            recommendations: hits(
                self.recommendations.unwrap_or_default().results,
                Some(MediaKind::Movie),
                img,
            ),
            similar: hits(
                self.similar.unwrap_or_default().results,
                Some(MediaKind::Movie),
                img,
            ),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WCollection {
    pub id: u32,
    pub name: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub parts: Vec<WHit>,
}

impl WCollection {
    pub fn into_model(self, img: &Img) -> CollectionMetadata {
        let mut parts = hits(self.parts, Some(MediaKind::Movie), img);
        // Undated (announced) parts go last.
        parts.sort_by_key(|p| (p.release_date.is_none(), p.release_date));
        CollectionMetadata {
            tmdb_id: self.id,
            name: self.name,
            overview: non_empty(self.overview),
            images: build_images(
                self.poster_path.as_deref(),
                self.backdrop_path.as_deref(),
                None,
                img,
            ),
            parts,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WEpisode {
    pub id: u32,
    pub season_number: u32,
    pub episode_number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub runtime: Option<u32>,
    pub still_path: Option<String>,
    pub vote_average: Option<f64>,
    pub episode_type: Option<String>,
}

impl WEpisode {
    fn into_model(self, img: &Img) -> EpisodeMetadata {
        EpisodeMetadata {
            season_number: self.season_number,
            episode_number: self.episode_number,
            absolute_number: None,
            title: non_empty(self.name),
            overview: non_empty(self.overview),
            air_date: parse_date(self.air_date.as_deref()),
            runtime_minutes: self.runtime.filter(|r| *r > 0),
            still: img.url(STILL, self.still_path.as_deref()),
            tmdb_id: Some(self.id),
            tvdb_id: None,
            finale: self.episode_type.filter(|t| t != "standard" && !t.is_empty()),
            vote_average: self.vote_average,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WSeason {
    pub id: u32,
    pub season_number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub poster_path: Option<String>,
    pub episode_count: Option<u32>,
    pub episodes: Vec<WEpisode>,
}

impl WSeason {
    pub fn into_model(self, img: &Img) -> SeasonMetadata {
        let episodes: Vec<_> = self.episodes.into_iter().map(|e| e.into_model(img)).collect();
        SeasonMetadata {
            season_number: self.season_number,
            name: non_empty(self.name),
            overview: non_empty(self.overview),
            air_date: parse_date(self.air_date.as_deref()),
            poster: img.url(POSTER, self.poster_path.as_deref()),
            episode_count: self.episode_count.unwrap_or(episodes.len() as u32),
            episodes,
            tmdb_id: Some(self.id),
            tvdb_id: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WContentRating {
    pub iso_3166_1: String,
    pub rating: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WTv {
    pub id: u32,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub first_air_date: Option<String>,
    pub last_air_date: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub show_type: Option<String>,
    pub networks: Vec<WCompany>,
    pub genres: Vec<Genre>,
    pub episode_run_time: Vec<u32>,
    pub seasons: Vec<WSeason>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub origin_country: Vec<String>,
    pub original_language: Option<String>,
    pub vote_average: f64,
    pub vote_count: u32,
    pub popularity: f64,
    pub next_episode_to_air: Option<WEpisode>,
    pub last_episode_to_air: Option<WEpisode>,
    pub external_ids: Option<WExternalIds>,
    pub alternative_titles: Option<WAltTitles>,
    pub credits: Option<WCredits>,
    pub images: Option<WImages>,
    pub videos: Option<WResults<WVideo>>,
    pub keywords: Option<WKeywords>,
    pub content_ratings: Option<WResults<WContentRating>>,
    pub recommendations: Option<WPage<WHit>>,
    pub similar: Option<WPage<WHit>>,
}

fn series_status(s: Option<&str>) -> SeriesStatus {
    match s {
        Some("Returning Series") => SeriesStatus::Continuing,
        Some("Ended" | "Canceled") => SeriesStatus::Ended,
        Some("In Production" | "Planned" | "Pilot") => SeriesStatus::Upcoming,
        _ => SeriesStatus::Unknown,
    }
}

impl WTv {
    /// `seasons` are the full seasons (with episodes); summaries from the details call fill gaps.
    pub fn into_model(self, img: &Img, region: &str, mut seasons: Vec<SeasonMetadata>) -> SeriesMetadata {
        // Keep summary-only seasons (e.g. not yet returned) so season counts stay right.
        for summary in self.seasons {
            if !seasons.iter().any(|s| s.season_number == summary.season_number) {
                seasons.push(summary.into_model(img));
            }
        }
        seasons.sort_by_key(|s| s.season_number);
        // TMDB has no absolute numbering; derive it from regular seasons in order.
        let mut abs = 0;
        for s in seasons.iter_mut().filter(|s| s.season_number > 0) {
            s.episodes.sort_by_key(|e| e.episode_number);
            for e in &mut s.episodes {
                abs += 1;
                e.absolute_number = Some(abs);
            }
        }
        let ext = self.external_ids.unwrap_or_default();
        let first = parse_date(self.first_air_date.as_deref());
        let kw = self.keywords.unwrap_or_default();
        let ratings = self.content_ratings.unwrap_or_default().results;
        let certification = ratings
            .iter()
            .find(|r| r.iso_3166_1.eq_ignore_ascii_case(region))
            .map(|r| r.rating.clone())
            .filter(|r| !r.is_empty());
        let runtime = self
            .episode_run_time
            .first()
            .copied()
            .or_else(|| self.last_episode_to_air.as_ref().and_then(|e| e.runtime))
            .filter(|r| *r > 0);
        let networks: Vec<_> = self.networks.into_iter().map(|n| n.into_model(img)).collect();
        SeriesMetadata {
            tmdb_id: Some(self.id),
            tvdb_id: ext.tvdb_id.filter(|i| *i > 0),
            imdb_id: non_empty(ext.imdb_id.clone()),
            external_ids: ExternalIds {
                tmdb: Some(self.id),
                imdb: non_empty(ext.imdb_id),
                tvdb: ext.tvdb_id.filter(|i| *i > 0),
                wikidata: non_empty(ext.wikidata_id),
            },
            sort_title: sort_title(&self.name),
            original_title: non_empty(self.original_name),
            title: self.name,
            overview: non_empty(self.overview),
            year: year_of(first),
            first_air_date: first,
            last_air_date: parse_date(self.last_air_date.as_deref()),
            next_air_date: self
                .next_episode_to_air
                .and_then(|e| parse_date(e.air_date.as_deref())),
            status: series_status(self.status.as_deref()),
            show_type: non_empty(self.show_type),
            network: networks.first().map(|n| n.name.clone()),
            networks,
            genres: self.genres,
            keywords: kw.keywords.into_iter().chain(kw.results).collect(),
            runtime_minutes: runtime,
            certification,
            original_language: non_empty(self.original_language),
            origin_country: self.origin_country,
            alternative_titles: self.alternative_titles.unwrap_or_default().into_model(),
            credits: self.credits.unwrap_or_default().into_model(img),
            images: build_images(
                self.poster_path.as_deref(),
                self.backdrop_path.as_deref(),
                self.images,
                img,
            ),
            videos: videos(self.videos),
            ratings: Ratings {
                vote_average: self.vote_average,
                vote_count: self.vote_count,
                popularity: self.popularity,
            },
            seasons,
            recommendations: hits(
                self.recommendations.unwrap_or_default().results,
                Some(MediaKind::Series),
                img,
            ),
            similar: hits(
                self.similar.unwrap_or_default().results,
                Some(MediaKind::Series),
                img,
            ),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WFind {
    pub movie_results: Vec<WHit>,
    pub tv_results: Vec<WHit>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WGenres {
    pub genres: Vec<Genre>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WPerson {
    pub id: u32,
    pub imdb_id: Option<String>,
    pub name: String,
    pub also_known_as: Vec<String>,
    pub biography: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub place_of_birth: Option<String>,
    pub known_for_department: Option<String>,
    pub profile_path: Option<String>,
    pub popularity: f64,
}

impl WPerson {
    pub fn into_model(self, img: &Img) -> Person {
        Person {
            tmdb_id: self.id,
            imdb_id: non_empty(self.imdb_id),
            name: self.name,
            also_known_as: self.also_known_as,
            biography: non_empty(self.biography),
            birthday: parse_date(self.birthday.as_deref()),
            deathday: parse_date(self.deathday.as_deref()),
            place_of_birth: non_empty(self.place_of_birth),
            known_for_department: non_empty(self.known_for_department),
            profile_url: img.url(PROFILE, self.profile_path.as_deref()),
            popularity: self.popularity,
        }
    }
}

/// TMDB error body: `{"status_code": 34, "status_message": "...", "success": false}`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WError {
    pub status_code: i64,
    pub status_message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn rel(kind: u8, date: &str, cert: &str) -> WRelease {
        WRelease {
            certification: Some(cert.into()),
            note: None,
            release_date: Some(date.into()),
            kind,
        }
    }

    #[test]
    fn release_dates_prefer_region_and_fall_back() {
        let raw = vec![
            WCountryReleases {
                iso_3166_1: "US".into(),
                release_dates: vec![
                    rel(3, "2020-05-01T00:00:00.000Z", "R"),
                    rel(4, "2020-06-15T00:00:00.000Z", ""),
                ],
            },
            WCountryReleases {
                iso_3166_1: "DE".into(),
                release_dates: vec![
                    rel(3, "2020-04-20T00:00:00.000Z", "16"),
                    rel(5, "2020-09-01T00:00:00.000Z", "16"),
                ],
            },
        ];
        let (d, cert) = release_dates(raw, "US");
        assert_eq!(d.cinema, NaiveDate::from_ymd_opt(2020, 5, 1));
        assert_eq!(d.digital, NaiveDate::from_ymd_opt(2020, 6, 15));
        // No US physical release: earliest worldwide.
        assert_eq!(d.physical, NaiveDate::from_ymd_opt(2020, 9, 1));
        assert_eq!(cert.as_deref(), Some("R"));
        assert_eq!(d.all.len(), 4);
    }
}
