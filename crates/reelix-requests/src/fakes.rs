//! In-memory fakes of every gateway trait, for tests here and in `reelix-api`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reelix_api_types::radio::{RadioPreviewEpisodeDto, RadioShowHitDto, RadioShowPreviewDto};
use reelix_api_types::requests::{
    AvailabilityStatus, ContentRequestKind, NamedIdDto, RadioDiscoverDto, RadioDiscoverListDto,
    RootFolderOptionDto, radio_foreign_id,
};
use reelix_core::{BookFormat, ContentKind, MediaKind};
use reelix_mediaserver::plextv::PlexTv;
use reelix_mediaserver::{
    BuildContext, ExternalUser, MediaServer, MediaServerKind, MsError, MsItem, ServerInfo, WatchedItem,
};
use reelix_metadata::{
    Credits, DiscoverFeed, ExternalIds, Genre, Images, MetaError, MovieMetadata, MovieStatus, MultiHit, Page,
    Ratings, ReleaseDates, SearchHit, SeasonMetadata, SeriesMetadata, SeriesStatus, TmdbId,
};
use reelix_notify::NotificationEvent;

use crate::gateway::{
    AddAlbumRequest, AddBookRequest, AddMovieRequest, AddRadioRequest, AddSeriesRequest, ArtistHit,
    Availability, ContentAdded, ContentHit, DiscoverySource, LibraryGateway, MediaServerDirectory,
    MediaServerEntry, NotificationSink,
};

// ------------------------------------------------------------------ library

#[derive(Default)]
pub struct LibraryState {
    pub movies: HashMap<TmdbId, Availability>,
    pub series: HashMap<TmdbId, Availability>,
    pub added_movies: Vec<AddMovieRequest>,
    pub added_series: Vec<AddSeriesRequest>,
    pub searches: Vec<(MediaKind, Vec<i64>)>,
    pub root_folders: Vec<RootFolderOptionDto>,
    /// When set, add_* fail with this message.
    pub fail_adds: Option<String>,
    /// Book availability per Open Library work id.
    pub books: HashMap<String, Availability>,
    /// Album availability per MusicBrainz release-group id.
    pub albums: HashMap<String, Availability>,
    /// Known works / release groups (lookups search their titles).
    pub book_hits: Vec<ContentHit>,
    pub album_hits: Vec<ContentHit>,
    pub artist_hits: Vec<ArtistHit>,
    /// Discover rows: `(kind, row id)` → hits.
    pub content_rows: HashMap<(ContentRequestKind, String), Vec<ContentHit>>,
    pub added_books: Vec<AddBookRequest>,
    pub added_albums: Vec<AddAlbumRequest>,
    pub content_searches: Vec<(ContentRequestKind, Vec<i64>, Vec<BookFormat>)>,
    /// Followed radio shows per `{provider}:{external_id}`.
    pub radio: HashMap<String, Availability>,
    /// Known radio shows (search matches titles; previews by provider + id).
    pub radio_hits: Vec<RadioShowHitDto>,
    /// Radio Discover rows: row id → hits (listed as rows by `radio_lists`).
    pub radio_rows: HashMap<String, Vec<RadioShowHitDto>>,
    pub added_radio: Vec<AddRadioRequest>,
    next_id: i64,
}

/// Fake library: records adds/searches; availability is set by the test.
#[derive(Default, Clone)]
pub struct FakeLibrary(pub Arc<Mutex<LibraryState>>);

impl FakeLibrary {
    pub fn set(&self, kind: MediaKind, tmdb: TmdbId, a: Availability) {
        let mut s = self.0.lock().unwrap();
        match kind {
            MediaKind::Movie => s.movies.insert(tmdb, a),
            MediaKind::Series => s.series.insert(tmdb, a),
        };
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, LibraryState> {
        self.0.lock().unwrap()
    }

    /// Sets the availability of a book (work id) or album (release-group id).
    pub fn set_content(&self, kind: ContentRequestKind, id: &str, a: Availability) {
        let mut s = self.0.lock().unwrap();
        match kind {
            ContentRequestKind::Book => s.books.insert(id.into(), a),
            ContentRequestKind::Music => s.albums.insert(id.into(), a),
            ContentRequestKind::Radio => s.radio.insert(id.into(), a),
        };
    }

    /// Registers a radio show for search and previews, and in the Discover row `row`.
    pub fn with_radio(self, row: Option<&str>, hit: RadioShowHitDto) -> Self {
        {
            let mut s = self.0.lock().unwrap();
            if let Some(row) = row {
                s.radio_rows.entry(row.to_owned()).or_default().push(hit.clone());
            }
            s.radio_hits.push(hit);
        }
        self
    }

    /// Registers a work / release group for lookups.
    pub fn with_content(self, kind: ContentRequestKind, hit: ContentHit) -> Self {
        {
            let mut s = self.0.lock().unwrap();
            match kind {
                ContentRequestKind::Book => s.book_hits.push(hit),
                ContentRequestKind::Music => s.album_hits.push(hit),
                ContentRequestKind::Radio => {}
            }
        }
        self
    }
}

/// A book (Open Library work) for [`FakeLibrary::with_content`].
pub fn book(work_id: &str, title: &str, author: &str) -> ContentHit {
    ContentHit {
        foreign_id: work_id.into(),
        title: title.into(),
        subtitle: Some(author.into()),
        subtitle_id: Some(format!("{work_id}-author")),
        year: Some(1965),
        cover: Some(format!("https://covers.openlibrary.org/w/{work_id}.jpg")),
        album_type: None,
        overview: Some(format!("{title} overview")),
        in_library: None,
    }
}

/// A radio show hit for [`FakeLibrary::with_radio`].
pub fn radio_show(provider: &str, external_id: &str, title: &str) -> RadioShowHitDto {
    RadioShowHitDto {
        provider: provider.into(),
        external_id: external_id.into(),
        title: title.into(),
        synopsis: Some(format!("{title} synopsis")),
        author: None,
        station_name: Some("Radio 4".into()),
        artwork_url: Some(format!(
            "https://ichef.bbci.co.uk/images/ic/640x640/{external_id}.jpg"
        )),
        web_url: None,
        feed_url: None,
        episode_count: None,
        show_id: None,
    }
}

/// An album (MusicBrainz release group) for [`FakeLibrary::with_content`].
pub fn album(rg_id: &str, title: &str, artist: &str) -> ContentHit {
    ContentHit {
        foreign_id: rg_id.into(),
        title: title.into(),
        subtitle: Some(artist.into()),
        subtitle_id: Some(format!("{rg_id}-artist")),
        year: Some(1997),
        cover: None,
        album_type: Some("Album".into()),
        overview: None,
        in_library: None,
    }
}

fn matching(hits: &[ContentHit], term: &str) -> Vec<ContentHit> {
    let t = term.to_lowercase();
    hits.iter()
        .filter(|h| h.title.to_lowercase().contains(&t) || h.foreign_id == term)
        .cloned()
        .collect()
}

#[async_trait]
impl LibraryGateway for FakeLibrary {
    async fn availability(
        &self,
        kind: MediaKind,
        ids: &[TmdbId],
    ) -> anyhow::Result<HashMap<TmdbId, Availability>> {
        let s = self.0.lock().unwrap();
        let map = match kind {
            MediaKind::Movie => &s.movies,
            MediaKind::Series => &s.series,
        };
        Ok(ids
            .iter()
            .filter_map(|i| map.get(i).map(|a| (*i, a.clone())))
            .collect())
    }

    async fn add_movie(&self, req: AddMovieRequest) -> anyhow::Result<i64> {
        let mut s = self.0.lock().unwrap();
        if let Some(m) = &s.fail_adds {
            anyhow::bail!("{m}");
        }
        s.next_id += 1;
        let id = s.next_id;
        s.movies.entry(req.tmdb_id).or_insert(Availability {
            status: AvailabilityStatus::Monitored,
            library_id: Some(id),
            seasons: BTreeMap::new(),
        });
        s.added_movies.push(req);
        Ok(id)
    }

    async fn add_series(&self, req: AddSeriesRequest) -> anyhow::Result<i64> {
        let mut s = self.0.lock().unwrap();
        if let Some(m) = &s.fail_adds {
            anyhow::bail!("{m}");
        }
        s.next_id += 1;
        let id = s.next_id;
        let entry = s.series.entry(req.tmdb_id).or_insert(Availability {
            status: AvailabilityStatus::Monitored,
            library_id: Some(id),
            seasons: BTreeMap::new(),
        });
        for n in &req.seasons {
            entry.seasons.entry(*n).or_insert(AvailabilityStatus::Monitored);
        }
        let id = entry.library_id.unwrap_or(id);
        s.added_series.push(req);
        Ok(id)
    }

    async fn request_search(&self, kind: MediaKind, ids: &[i64]) -> anyhow::Result<()> {
        self.0.lock().unwrap().searches.push((kind, ids.to_vec()));
        Ok(())
    }

    async fn root_folders(&self, _: MediaKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        Ok(self.0.lock().unwrap().root_folders.clone())
    }

    async fn content_availability(
        &self,
        kind: ContentRequestKind,
        ids: &[String],
    ) -> anyhow::Result<HashMap<String, Availability>> {
        let s = self.0.lock().unwrap();
        let map = match kind {
            ContentRequestKind::Book => &s.books,
            ContentRequestKind::Music => &s.albums,
            ContentRequestKind::Radio => &s.radio,
        };
        Ok(ids
            .iter()
            .filter_map(|i| map.get(i).map(|a| (i.clone(), a.clone())))
            .collect())
    }

    async fn lookup_books(&self, term: &str) -> anyhow::Result<Vec<ContentHit>> {
        Ok(matching(&self.0.lock().unwrap().book_hits, term))
    }

    async fn lookup_albums(&self, term: &str) -> anyhow::Result<Vec<ContentHit>> {
        Ok(matching(&self.0.lock().unwrap().album_hits, term))
    }

    async fn lookup_artists(&self, term: &str) -> anyhow::Result<Vec<ArtistHit>> {
        let t = term.to_lowercase();
        Ok(self
            .0
            .lock()
            .unwrap()
            .artist_hits
            .iter()
            .filter(|a| a.name.to_lowercase().contains(&t))
            .cloned()
            .collect())
    }

    async fn discover_content(&self, kind: ContentRequestKind, row: &str) -> anyhow::Result<Vec<ContentHit>> {
        let s = self.0.lock().unwrap();
        Ok(s.content_rows
            .get(&(kind, row.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    async fn content(&self, kind: ContentRequestKind, id: &str) -> anyhow::Result<Option<ContentHit>> {
        let s = self.0.lock().unwrap();
        let hits = match kind {
            ContentRequestKind::Book => &s.book_hits,
            ContentRequestKind::Music => &s.album_hits,
            ContentRequestKind::Radio => return Ok(None),
        };
        Ok(hits.iter().find(|h| h.foreign_id == id).cloned())
    }

    async fn radio_lists(&self) -> anyhow::Result<RadioDiscoverDto> {
        let s = self.0.lock().unwrap();
        let mut rows: Vec<RadioDiscoverListDto> = s
            .radio_rows
            .keys()
            .map(|id| RadioDiscoverListDto {
                id: id.clone(),
                name: id.clone(),
                provider: id.split('-').next().unwrap_or_default().to_owned(),
                provider_name: "Fake".into(),
            })
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(RadioDiscoverDto {
            rows,
            categories: Vec::new(),
        })
    }

    async fn radio_discover(&self, row: &str) -> anyhow::Result<Vec<RadioShowHitDto>> {
        let s = self.0.lock().unwrap();
        s.radio_rows
            .get(row)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown radio discover row: {row}"))
    }

    async fn lookup_radio(&self, term: &str) -> anyhow::Result<Vec<RadioShowHitDto>> {
        let t = term.to_lowercase();
        Ok(self
            .0
            .lock()
            .unwrap()
            .radio_hits
            .iter()
            .filter(|h| h.title.to_lowercase().contains(&t))
            .cloned()
            .collect())
    }

    async fn radio_preview(
        &self,
        provider: &str,
        external_id: &str,
    ) -> anyhow::Result<Option<RadioShowPreviewDto>> {
        let s = self.0.lock().unwrap();
        let Some(mut show) = s
            .radio_hits
            .iter()
            .find(|h| h.provider == provider && h.external_id == external_id)
            .cloned()
        else {
            return Ok(None);
        };
        show.show_id = s
            .radio
            .get(&radio_foreign_id(provider, external_id))
            .and_then(|a| a.library_id);
        Ok(Some(RadioShowPreviewDto {
            show,
            first_broadcast: None,
            series_count: 1,
            episode_count: 1,
            latest_episodes: vec![RadioPreviewEpisodeDto {
                title: "Episode 1".into(),
                series_number: Some(1),
                number: Some(1),
                first_broadcast: None,
                duration_secs: Some(1800),
            }],
        }))
    }

    async fn add_radio(&self, req: AddRadioRequest) -> anyhow::Result<ContentAdded> {
        let mut s = self.0.lock().unwrap();
        if let Some(m) = &s.fail_adds {
            anyhow::bail!("{m}");
        }
        s.next_id += 1;
        let id = s.next_id;
        let id = s
            .radio
            .entry(radio_foreign_id(&req.provider, &req.external_id))
            .or_insert(Availability {
                status: AvailabilityStatus::Available,
                library_id: Some(id),
                seasons: BTreeMap::new(),
            })
            .library_id
            .unwrap_or(id);
        s.added_radio.push(req);
        Ok(ContentAdded {
            item_id: id,
            parent_id: None,
        })
    }

    async fn add_book(&self, req: AddBookRequest) -> anyhow::Result<ContentAdded> {
        let mut s = self.0.lock().unwrap();
        if let Some(m) = &s.fail_adds {
            anyhow::bail!("{m}");
        }
        s.next_id += 1;
        let id = s.next_id;
        let id = s
            .books
            .entry(req.work_id.clone())
            .or_insert(Availability {
                status: AvailabilityStatus::Monitored,
                library_id: Some(id),
                seasons: BTreeMap::new(),
            })
            .library_id
            .unwrap_or(id);
        s.added_books.push(req);
        Ok(ContentAdded {
            item_id: id,
            parent_id: Some(1000 + id),
        })
    }

    async fn add_album(&self, req: AddAlbumRequest) -> anyhow::Result<ContentAdded> {
        let mut s = self.0.lock().unwrap();
        if let Some(m) = &s.fail_adds {
            anyhow::bail!("{m}");
        }
        s.next_id += 1;
        let id = s.next_id;
        let id = s
            .albums
            .entry(req.release_group_id.clone())
            .or_insert(Availability {
                status: AvailabilityStatus::Monitored,
                library_id: Some(id),
                seasons: BTreeMap::new(),
            })
            .library_id
            .unwrap_or(id);
        s.added_albums.push(req);
        Ok(ContentAdded {
            item_id: id,
            parent_id: Some(2000 + id),
        })
    }

    async fn content_search(
        &self,
        kind: ContentRequestKind,
        ids: &[i64],
        formats: &[BookFormat],
    ) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .content_searches
            .push((kind, ids.to_vec(), formats.to_vec()));
        Ok(())
    }

    async fn content_profiles(&self, kind: ContentKind) -> anyhow::Result<Vec<NamedIdDto>> {
        Ok(vec![NamedIdDto {
            id: 100,
            name: format!("{kind:?} default"),
        }])
    }

    async fn content_root_folders(&self, _: ContentKind) -> anyhow::Result<Vec<RootFolderOptionDto>> {
        Ok(self.0.lock().unwrap().root_folders.clone())
    }
}

// ------------------------------------------------------------------ metadata

/// Minimal movie metadata.
pub fn movie(tmdb_id: TmdbId, title: &str, year: i32) -> MovieMetadata {
    MovieMetadata {
        tmdb_id,
        imdb_id: Some(format!("tt{tmdb_id:07}")),
        title: title.into(),
        original_title: title.into(),
        original_language: Some("en".into()),
        sort_title: title.to_lowercase(),
        overview: Some(format!("{title} overview")),
        tagline: None,
        year: Some(year),
        release_date: chrono::NaiveDate::from_ymd_opt(year, 1, 1),
        release_dates: ReleaseDates::default(),
        certification: None,
        status: MovieStatus::Released,
        runtime_minutes: Some(120),
        genres: vec![Genre {
            id: 28,
            name: "Action".into(),
        }],
        keywords: Vec::new(),
        studios: Vec::new(),
        collection: None,
        alternative_titles: Vec::new(),
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images {
            poster: Some(format!("https://image.tmdb.org/t/p/w500/{tmdb_id}.jpg")),
            ..Default::default()
        },
        videos: Vec::new(),
        ratings: Ratings::default(),
        adult: false,
        homepage: None,
        recommendations: Vec::new(),
        similar: Vec::new(),
    }
}

/// Minimal series metadata with `seasons` regular seasons (plus specials).
pub fn series(tmdb_id: TmdbId, title: &str, seasons: u32) -> SeriesMetadata {
    let season = |n: u32| SeasonMetadata {
        season_number: n,
        name: Some(format!("Season {n}")),
        overview: None,
        air_date: None,
        poster: None,
        episode_count: 10,
        episodes: Vec::new(),
        tmdb_id: None,
        tvdb_id: None,
    };
    SeriesMetadata {
        tmdb_id: Some(tmdb_id),
        tvdb_id: Some(tmdb_id + 70000),
        imdb_id: None,
        title: title.into(),
        original_title: None,
        sort_title: title.to_lowercase(),
        overview: None,
        year: Some(2020),
        first_air_date: None,
        last_air_date: None,
        next_air_date: None,
        status: SeriesStatus::Continuing,
        show_type: None,
        network: None,
        networks: Vec::new(),
        genres: Vec::new(),
        keywords: Vec::new(),
        runtime_minutes: None,
        certification: None,
        original_language: None,
        origin_country: Vec::new(),
        alternative_titles: Vec::new(),
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images::default(),
        videos: Vec::new(),
        ratings: Ratings::default(),
        seasons: (0..=seasons).map(season).collect(),
        recommendations: Vec::new(),
        similar: Vec::new(),
    }
}

/// A search hit for a movie or series.
pub fn hit(kind: MediaKind, tmdb_id: TmdbId, title: &str) -> SearchHit {
    SearchHit {
        kind,
        tmdb_id: Some(tmdb_id),
        tvdb_id: None,
        imdb_id: None,
        title: title.into(),
        original_title: None,
        year: Some(2020),
        release_date: None,
        overview: None,
        poster: None,
        backdrop: None,
        genre_ids: Vec::new(),
        original_language: None,
        vote_average: 7.0,
        vote_count: 100,
        popularity: 1.0,
        adult: false,
    }
}

/// Fake metadata: titles registered by the test; every feed returns `feed_hits`.
#[derive(Default)]
pub struct FakeMetadata {
    pub movies: Mutex<HashMap<TmdbId, MovieMetadata>>,
    pub series: Mutex<HashMap<TmdbId, SeriesMetadata>>,
    pub feed_hits: Mutex<Vec<SearchHit>>,
}

impl FakeMetadata {
    pub fn with_movie(self, m: MovieMetadata) -> Self {
        self.movies.lock().unwrap().insert(m.tmdb_id, m);
        self
    }
    pub fn with_series(self, s: SeriesMetadata) -> Self {
        self.series
            .lock()
            .unwrap()
            .insert(s.tmdb_id.unwrap_or_default(), s);
        self
    }
    pub fn with_hits(self, hits: Vec<SearchHit>) -> Self {
        *self.feed_hits.lock().unwrap() = hits;
        self
    }
    fn page(&self) -> Page<SearchHit> {
        let results = self.feed_hits.lock().unwrap().clone();
        Page {
            page: 1,
            total_pages: 1,
            total_results: results.len() as u32,
            results,
        }
    }
}

#[async_trait]
impl DiscoverySource for FakeMetadata {
    async fn discover(&self, _: &DiscoverFeed, _: u32) -> Result<Page<SearchHit>, MetaError> {
        Ok(self.page())
    }
    async fn search(&self, q: &str, _: u32) -> Result<Page<MultiHit>, MetaError> {
        let q = q.to_lowercase();
        Ok(self.page().map(MultiHit::Media)).map(|mut p| {
            p.results
                .retain(|h| matches!(h, MultiHit::Media(m) if m.title.to_lowercase().contains(&q)));
            p.total_results = p.results.len() as u32;
            p
        })
    }
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError> {
        self.movies
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }
    async fn series(&self, id: TmdbId) -> Result<SeriesMetadata, MetaError> {
        self.series
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }
    async fn related(&self, _: MediaKind, _: TmdbId, _: bool, _: u32) -> Result<Page<SearchHit>, MetaError> {
        Ok(self.page())
    }
    async fn genres(&self, _: MediaKind) -> Result<Vec<Genre>, MetaError> {
        Ok(vec![Genre {
            id: 28,
            name: "Action".into(),
        }])
    }
}

// ------------------------------------------------------------------ media servers

/// A fake media server with fixed users and passwords.
pub struct FakeMediaServer {
    pub kind: MediaServerKind,
    /// `(user, password)`; password `None` = cannot log in with a password.
    pub users: Vec<(ExternalUser, Option<String>)>,
}

impl FakeMediaServer {
    pub fn user(id: &str, name: &str) -> ExternalUser {
        ExternalUser {
            id: id.into(),
            username: name.into(),
            email: Some(format!("{name}@example.com")),
            is_admin: false,
            avatar_url: None,
            access_token: None,
        }
    }
}

#[async_trait]
impl MediaServer for FakeMediaServer {
    fn kind(&self) -> MediaServerKind {
        self.kind
    }
    async fn test(&self) -> Result<ServerInfo, MsError> {
        Ok(ServerInfo {
            kind: self.kind,
            id: "fake".into(),
            name: "Fake".into(),
            version: "1".into(),
        })
    }
    async fn authenticate(&self, username: &str, password: &str) -> Result<ExternalUser, MsError> {
        self.users
            .iter()
            .find(|(u, p)| u.username.eq_ignore_ascii_case(username) && p.as_deref() == Some(password))
            .map(|(u, _)| ExternalUser {
                access_token: Some("token".into()),
                ..u.clone()
            })
            .ok_or(MsError::InvalidCredentials)
    }
    async fn users(&self) -> Result<Vec<ExternalUser>, MsError> {
        Ok(self.users.iter().map(|(u, _)| u.clone()).collect())
    }
    async fn refresh_library(&self, _: &[PathBuf]) -> Result<(), MsError> {
        Ok(())
    }
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError> {
        Ok(Vec::new())
    }
    async fn watch_history(&self, _: &str) -> Result<Vec<WatchedItem>, MsError> {
        Ok(Vec::new())
    }
}

/// Fixed list of servers plus a plex.tv client pointed at `plex_tv_url`.
pub struct FakeDirectory {
    pub servers: Vec<MediaServerEntry>,
    pub plex_tv_url: String,
}

impl FakeDirectory {
    pub fn new(plex_tv_url: &str) -> Self {
        Self {
            servers: Vec::new(),
            plex_tv_url: plex_tv_url.into(),
        }
    }

    pub fn with(mut self, id: i64, name: &str, server: FakeMediaServer) -> Self {
        self.servers.push(MediaServerEntry {
            id,
            name: name.into(),
            kind: server.kind,
            server: Arc::new(server),
        });
        self
    }
}

#[async_trait]
impl MediaServerDirectory for FakeDirectory {
    async fn servers(&self) -> Vec<MediaServerEntry> {
        self.servers.clone()
    }
    fn plex_tv(&self) -> PlexTv {
        PlexTv::new(&BuildContext::new(reqwest::Client::new(), "reelix-test"))
            .with_base_urls(&self.plex_tv_url, &self.plex_tv_url)
    }
}

// ------------------------------------------------------------------ notifications

/// A recorded notification and its recipients.
pub type Sent = (NotificationEvent, Vec<i64>);

/// Records every notification.
#[derive(Default, Clone)]
pub struct FakeNotifications(pub Arc<Mutex<Vec<Sent>>>);

impl FakeNotifications {
    pub fn take(&self) -> Vec<Sent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

#[async_trait]
impl NotificationSink for FakeNotifications {
    async fn notify(&self, event: NotificationEvent, recipients: Vec<i64>) {
        self.0.lock().unwrap().push((event, recipients));
    }
}
