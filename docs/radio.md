# Radio

Radio is its own media type (`ContentKind::Radio`): radio programmes and podcasts that Reelix
follows, with their series and episodes, matched against an Audiobookshelf podcast library.
Audiobookshelf is the player; Reelix keeps the list, the statuses and (later) files new
episodes. The design follows GitHub issue #21.

This page describes what exists and where the remaining parts plug in.

## Crate layout

`crates/reelix-radio` owns the radio tables and logic. It depends on `reelix-db`,
`reelix-api-types` (DTOs in `reelix_api_types::radio`), `reelix-jobs`, `reelix-library`
(shared root folders), `reelix-import` (title keys, file-name sanitising) and `reelix-parser`
(quality families).

| File | What it does |
|---|---|
| `src/lib.rs` | Module list, re-exports, job kinds (`kinds`), settings keys (`keys`), event entity names (`entities`) |
| `src/service.rs` | `RadioService`, `RadioDeps`, job queue hand-off, `emit`, `queue_refresh`, `queue_library_sync` |
| `src/provider.rs` | `RadioProvider` trait, `ProviderRegistry`, `ShowData`/`SeriesData`/`EpisodeData`/`StationData`, `ShowFetch`, `SearchPrefs`, `ProviderError` |
| `src/providers/rss.rs` | The generic podcast feed provider (`rss`) |
| `src/providers/{bbc,apple,podcastindex,archive}.rs` | BBC, Apple Podcasts, Podcast Index, Internet Archive (see (b)) |
| `src/providers/http.rs` | Cached, paced JSON reader shared by the providers |
| `src/svg.rs` | SVG cleaner for logos and artwork |
| `src/xml.rs` | Small quick-xml tree (entity handling, depth limit 64) |
| `src/shows.rs` | Add, edit, delete, list, get; episodes; stations; search; preview; `recompute_statuses` |
| `src/refresh.rs` | `refresh_show`, `refresh_shows`, merging provider data, broadcast-order numbering, settling |
| `src/status.rs` | Pure status rules (`monitored_by_follow`, `episode_status`, `is_settled`, `filing_series_number`) |
| `src/matching.rs` | Hand-set file links (`set_episode_file`, `clear_episode_file`), automatic matching (`match_library`, `source_ids`) |
| `src/library.rs` | `RadioLibrarySync` trait, `NoopLibrarySync`, `NewEpisodeFile`, file rows, automatic links, review |
| `src/artwork.rs` | Downloading, storing and reading show artwork and station logos |
| `src/wanted.rs` | `wanted_episodes`, `episode_file_name` (indexer search / filing hand-off) |
| `src/settings.rs` | `radio.settings` and folder checks (`check_library_folder`) |
| `src/root_folders.rs` | Root folders of kind `radio` |
| `src/jobs.rs` | Job handlers and scheduled tasks |
| `src/repo/*.rs` | SQL per table |

Wiring: `crates/reelix-server/src/modules.rs` builds the service (with the provider
registry), registers its jobs and adds it to the API state. Routes are in
`crates/reelix-api/src/routes/radio.rs` and `routes/command.rs`.

## Tables

Migration `migrations/sqlite/0014_radio.sql`. Timestamps are unix ms, including first
broadcasts. It also rebuilds `root_folders` to accept `media_kind = 'radio'`.

- `radio_stations`: `provider` + `external_id` (unique), `name`, `logo_url`, `logo_path`.
- `radio_shows`: `provider` + `external_id` (unique; the feed URL for `rss`), `title`,
  `sort_title`, `clean_title` (matching key), `synopsis`, `author`, `web_url`,
  `first_broadcast`, `station_id` (→ stations, `SET NULL`), `artwork_url`, `artwork_path`,
  `feed_url`, `follow` (`all` | `from_now_on` | `none`), `followed_at`, `monitored`, `tags`
  (JSON ids), `root_folder_id`, `abs_item_id`, `quality_profile_id` (nullable), `refreshed_at`,
  `added_at`.
- `radio_series`: `show_id` (cascade), `external_id` (unique per show; feeds use `season:N`
  and `specials`), `kind` (`series` | `specials`), `number`, `number_source` (`source` |
  `broadcast_order`), `title`, `synopsis`, `expected_episodes`, `settled_at`,
  `source_removed_at`.
- `radio_episodes`: `show_id`, `series_id` (both cascade), `external_id` (unique per show),
  `number`, `number_source`, `title`, `synopsis`, `first_broadcast`, `duration_secs`,
  `monitored`, `status`, `file_id` (→ files, `SET NULL`, unique when set: one episode per
  file), `manual_match`, `enclosure_url`, `guid`, `source_removed_at`.
- `radio_episode_files`: one row per episode file of the podcast library: `abs_item_id` +
  `abs_episode_id` (unique), `item_title`, `title`, `season`, `episode`, `guid`,
  `enclosure_url`, `published_at`, `duration_secs`, `path`, `size`, `seen_at`.

Nothing is deleted when a source drops a series or episode: it gets `source_removed_at`
(cleared when it comes back). Deleting a show deletes its series and episodes, never the
library files.

## Status rules

`status.rs`, applied by `RadioService::recompute_statuses(show_id)`:

1. A matched file → `in_library`.
2. Show unmonitored, or episode unmonitored → `not_requested`.
3. First broadcast in the future → `not_released`.
4. Otherwise → `wanted` (an undated episode counts as broadcast).

`episodes.monitored` comes from the follow choice when an episode first appears:
`all` → yes; `none` → no; `from_now_on` → only if `first_broadcast >= followed_at` (undated
episodes: only if they appear after the show was added). Changing the follow choice sets
`followed_at` to now and re-applies it to every episode.

Numbering: episodes (and series) with no number from the source get the lowest numbers not
taken by source numbers, in broadcast order (undated last), recomputed on every refresh.
Specials are filed as series 0, an unnumbered series as series 1
(`filing_series_number`).

## Providers

```rust
#[async_trait]
pub trait RadioProvider: Send + Sync + 'static {
    fn id(&self) -> &'static str;               // stored in radio_shows.provider
    fn name(&self) -> &'static str;
    fn supports_search(&self) -> bool { false }
    fn is_feed(&self) -> bool { false }         // re-read hourly by radio_feed_sync
    async fn search(&self, query: &str, prefs: &SearchPrefs) -> Result<Vec<ShowHit>, ProviderError>;
    async fn preview(&self, link: &str) -> Result<Option<ShowData>, ProviderError>;
    async fn show(&self, external_id: &str, fetch: &ShowFetch) -> Result<ShowData, ProviderError>;
}
```

- `preview` returns `Ok(None)` for links that are not the provider's, so the next provider
  is asked. The registry is asked in order; `rss` accepts any http(s) link and must be last.
  `add_show` with a `link` reuses the preview's data (no second read).
- `show` gets a `ShowFetch`: `skip_series` (settled series: leave them out of the result,
  which is not a removal), `known_episodes` (episodes whose length is already saved) and
  `max_lookups` (budget for per-episode requests, from settings, default 30).
- `ShowData.complete = false` tells the merge not to mark missing items removed (e.g. a
  lookup budget ran out).
- `ShowHit` is `reelix_api_types::radio::RadioShowHitDto`; the service fills `show_id`.
- `SearchPrefs` carries the settings' `country` (upper case) and `language` (lower case).

`rss` (`providers/rss.rs`): reads up to 10 MB, follows an HTML page's
`<link rel="alternate" type="application/rss+xml">` once, and parses the channel (title,
description, `itunes:image` or `image/url`, `itunes:author`, link) and items (`guid`, else
enclosure URL, else link, else title, as the id; title; description as plain text;
`pubDate`; enclosure; `itunes:duration` as `HH:MM:SS`, `MM:SS` or seconds; `itunes:season`;
`itunes:episode`). `itunes:episodeType` `trailer`/`bonus` go to the specials; items without a
season go to series 1. Search is not supported.

## Jobs

Registered in `jobs.rs` and listed under System → Tasks.

| Kind | Interval | What it does |
|---|---|---|
| `refresh_radio_shows` | 12 h | `refresh_shows`; scheduled runs skip feed shows (they are read hourly); a payload `{show_ids, manual}` refreshes those shows |
| `radio_feed_sync` | 1 h | Refreshes shows whose provider `is_feed()` |
| `radio_library_sync` | 15 min | Calls the attached `RadioLibrarySync` (the Audiobookshelf reader, see (a)) |

## API

All under `/api/v1`. Reads and writes need `MANAGE_LIBRARY` (as music); settings and
root-folder changes need `ADMIN`.

| Method and path | What |
|---|---|
| `GET /radio/shows` | Paged list; `search`, `station_id`, `followed`, `monitored`, `sort`, `dir`, `page`, `page_size` |
| `POST /radio/shows` | Add: `{provider, external_id}` or `{link}`, plus `follow`, `monitored`, `root_folder_id`, `quality_profile_id`, `tags` |
| `GET/PUT/DELETE /radio/shows/{id}` | Read, edit (follow, monitored, tags, root folder, quality profile, `abs_item_id`; `null` clears), delete |
| `GET /radio/shows/{id}/episodes` | `{series, episodes}` |
| `POST /radio/shows/{id}/refresh` | Queues `refresh_radio_shows` for the show |
| `GET /radio/episodes/{id}` | Episode with its matched `file` |
| `PUT /radio/episodes/{id}/file` | `{file_id}`: hand-set match |
| `DELETE /radio/episodes/{id}/file` | Clear; `?auto=true` hands it back to automatic matching |
| `GET /radio/search?q=&provider=` | Search by name (a link is previewed); saves nothing |
| `GET /radio/preview?link=` | Show + newest 10 episodes; saves nothing |
| `GET /radio/stations`, `GET /radio/providers` | Stations; sources with `supports_search`/`enabled` |
| `GET /radio/shows/{id}/artwork`, `GET /radio/stations/{id}/logo` | Downloaded image, ETag/304 (see (c)) |
| `GET /radio/library/review` | `{unmatched_shows, unmatched_files, synced_at}` from `radio_episode_files` |
| `GET/POST /radio/rootfolders`, `DELETE /radio/rootfolders/{id}` | Root folders of kind `radio` |
| `GET/PUT /radio/settings` | `RadioSettingsDto` |
| `POST /command` | `{name: "RefreshRadioShow", show_ids}` or `{name: "RescanRadioLibrary"}` |

Events: `entity.changed` with entity `radio_show`, `radio_episode`, `radio_episode_file`
(also `radio_station`, `rootfolder`, `settings`).

Settings (`radio.settings`): `abs_library_id`, `abs_library_folder` (as Audiobookshelf sees
it), `library_folder` (as Reelix sees it; must exist and be writable), `default_follow`,
`enabled_providers` (empty = all), `country`, `language`, `max_lookups_per_refresh`,
`podcastindex_api_key`, `podcastindex_api_secret` (stored encrypted with the server's secret
key as `enc:…`; `GET` returns `********`, and sending `********` back keeps it; saving a secret
fails if no key is attached with `RadioService::set_secrets`).

## Extension points

### (a) Audiobookshelf podcast library sync and matching

Built. Three parts:

**Reading** (`crates/reelix-mediaserver/src/audiobookshelf.rs`): `Audiobookshelf::podcast_libraries()`
(`GET /api/libraries`, `mediaType` `podcast`; the book refresh still leaves them out),
`podcast_episodes(library_id)` (summary pages of 100 from
`GET /api/libraries/{id}/items?limit=100&page=N&minified=1`, stopping at a short page, the
announced `total` or a page with nothing new; then details 25 items at a time from
`POST /api/items/batch/get {libraryItemIds}`; each response read up to 32 MB) and
`scan_library(library_id)` (`POST /api/libraries/{id}/scan`). Only episodes with an
`audioFile` become an `AbsPodcastEpisode` (item id, episode id, item title, title,
season/episode as number or string, guid, enclosure URL, `publishedAt`, duration, the audio
file's path and size). Tests: `crates/reelix-mediaserver/tests/audiobookshelf_podcasts.rs`.

**The sync** (`crates/reelix-server/src/integrations/radio_library.rs`, `AbsRadioLibrary`):
attached in `modules.rs` with `radio.set_library_sync(...)`. It uses the enabled
Audiobookshelf rows of the media servers (same URL and API token), finds the one holding the
radio settings' `abs_library_id`, reads it and calls
`RadioService::sync_library_files(&[NewEpisodeFile])`, which upserts with one `seen_at`,
`remove_unseen_files` (episodes return to Wanted/Not requested from the follow choice),
`match_library()` and `mark_library_synced`. If the library cannot be read nothing is removed
and the job fails. `request_scan()` (a default no-op method on `RadioLibrarySync`, for filing)
scans that library. The `radio_library_sync` task and
`POST /command {"name": "RescanRadioLibrary"}` run the sync.

Health (`HealthSource`, source `radio`, refreshed by each sync, only while some show's follow
choice is not `none`): no Audiobookshelf server; no podcast library chosen (warnings); the
library is missing or Audiobookshelf cannot be reached (errors); `abs_library_folder` is not
one of the library's folders; `library_folder` is not mounted or not writable
(`check_library_folder`) (warnings).

**Matching** (`crates/reelix-radio/src/matching.rs`, `RadioService::match_library`): at most
one file per episode and one episode per file, in order:

1. Source id: the episode's guid or enclosure address equals the file's, or one of
   `source_ids(provider, episode external id)` occurs as a whole word (lower case) in the
   file's guid, enclosure address or file name. `source_ids` is per provider: `bbc` gives the
   programme pid (`b0abc123`, `p0abc123`, `m001abcd`, `w3…`), `archive` the audio file's stem
   (8+ characters); Apple (saved as `rss`) and Podcast Index rely on the exact guid/enclosure.
2. Series + episode number: `S20E03` (also `s20e03`, `S20 E03`) in the file's title, else its
   file name, else Audiobookshelf's season and episode (no season = series 1), compared with
   `(filing_series_number, number)`. Only files of the show's `abs_item_id`, or when it has
   none, of items whose title has the show's clean title (no case, accents, punctuation, "The").
   A number two files or two episodes share matches nothing.
3. Title: the clean title shared by exactly one file and one episode in the whole library.

Hand-set links are kept and their files offered to no other episode; "Not this file"
(`manual_match` with no file) is never matched until `DELETE …/file?auto=true`. An automatic
link that no rule now gives elsewhere is kept. A show without `abs_item_id` gets one when its
linked files and title-matched items are a single item no other show uses.

Review (`GET /radio/library/review`): `unmatched_shows` = items no show maps to, with no
linked file and no show of the same clean title; `unmatched_files` = files linked to no
episode. Tests: `crates/reelix-radio/tests/library_matching.rs`.

### (b) Sources (providers)

Built: registered in `modules.rs` in this order (links are offered to each in turn):

| Id | Module | Search | Links it recognises | Shows |
|---|---|---|---|---|
| `bbc` | `providers/bbc.rs` | BBC Sounds | `bbc.co.uk/programmes/{pid}`, `bbc.co.uk/sounds/brand\|series\|play/{pid}`, a bare pid | Its own (external id = brand pid, or the top series) |
| `apple` | `providers/apple.rs` | iTunes Search API | `podcasts.apple.com/…/id{n}` | Handed to `rss` |
| `podcastindex` | `providers/podcastindex.rs` | `search/byterm` | `podcastindex.org/podcast/{id}` | Its own (external id = feed id) |
| `archive` | `providers/archive.rs` | `advancedsearch.php` | `archive.org/details/{identifier}` | Its own (external id = identifier) |
| `rss` | `providers/rss.rs` | no | any http(s) feed or page advertising one | Its own (external id = feed URL) |

Trait additions (all defaulted): `countries()` / `languages()` (empty = everywhere/any),
`configure(&RadioSettingsDto)` (called on every settings read, e.g. credentials),
`is_configured()` (unconfigured = treated as turned off: left out of `enabled()`, `enabled:
false` in `GET /radio/providers`). `ShowData.provider` lets a directory hand a show to
another provider (`resolve_link` and `add_show` save it under that one).
`ShowFetch.quick` marks the first read when a show is added by id: a provider that needs many
requests may return a partial view (`complete = false`); `add_show` then queues a refresh.

Search (`GET /radio/search` without `provider`): `ProviderRegistry::for_search` takes the
enabled, configured providers that can search, drops those for another country or language
than the settings name (the BBC is `GB`, `en`/`cy`/`gd`; the others are for everywhere) and
puts country-specific ones first; hits are de-duplicated by `(provider, external_id)` and by
feed URL (first wins). `?provider=bbc` asks one source regardless of country.

Shared plumbing: `providers/http.rs` (`JsonReader`): per-URL in-memory cache (bounded),
spacing between requests, `429`/`503` with `Retry-After` (a wait up to 5 s is slept and
retried once; longer fails with `RateLimited` and every request until then fails without
reaching the source), body size limit.

- **BBC — undocumented endpoints.** `www.bbc.co.uk/programmes/{pid}.json` and
  `/programmes/{pid}/children.json?limit=50&page=N` (up to 20 pages) and BBC Sounds search
  `rms.api.bbc.co.uk/v2/programmes/search/container?q=` are public but undocumented and may
  change: every answer is checked, and a changed one fails with "the BBC programme data has
  changed …; saved series and episodes are kept" (a failed read changes nothing). Programmes
  cached 1 h, search 5 min, requests 1.5 s apart. Any pid walks up to six parent levels to the
  brand (or topmost series). Brand children: series (number = `position`, `expected_child_count`)
  and episodes straight under the brand (→ `specials`); a series inside a series is folded in
  (positions dropped, numbered by broadcast order). Settled series are neither listed nor read.
  Lengths: one `/programmes/{episode}.json` (`versions`, canonical first) per episode not in
  `known_episodes`, newest broadcast first, at most `max_lookups`; a rate limit stops the
  lookups, a missing page is skipped. A rate limit while reading series keeps what was read
  with `complete = false`. Station from `ownership.service` (logo
  `sounds.files.bbci.co.uk/3.12.0/networks/{id}/blocks-colour_default.svg`), artwork from
  `image.pid` (`ichef.bbci.co.uk/images/ic/640x640/{pid}.jpg`). Previews and quick adds read
  the newest series only. The walk follows Storyarr's BBC adapter (MIT, attribution in the
  module).
- **Apple Podcasts**: `itunes.apple.com/search?media=podcast&entity=podcast&term=&country=`
  (settings country, else `US`), `lookup?id=` for a link's `feedUrl`. Apple only knows where
  the feed is, so hits are `provider: "rss"` with the feed URL, and previews/adds return the
  feed (read by `RssProvider`) with `ShowData.provider = "rss"`: the show is then refreshed
  hourly as a feed, de-duplicated with other directories, and keeps working if Apple's API
  changes. Cached 5 min.
- **Podcast Index**: needs `podcastindex_api_key` and `podcastindex_api_secret` in the
  settings (off until both are set). Requests carry `X-Auth-Key`, `X-Auth-Date` (unix
  seconds) and `Authorization` = hex `sha1(key + secret + date)`. Search hits in the settings'
  language first. `podcasts/byfeedid` + `episodes/byfeedid?max=1000` map like feed items
  (season → `season:N`, trailer/bonus → specials; id = guid, else the episode id).
  Refreshed every 12 h (not a feed provider).
- **Internet Archive**: search limited to
  `mediatype:audio AND (collection:oldtimeradio OR subject:radio)` (query syntax stripped from
  the user's words). `/metadata/{identifier}`: one episode per original audio recording (an MP3
  derived from a FLAC is the same episode, the MP3 kept), in series 1, numbered by `track`,
  length from `length`, enclosure `archive.org/download/{id}/{file}`, artwork
  `archive.org/services/img/{id}`. `source_ids("archive", file)` gives the file stem for
  matching.

Image hosts of search hits allowed by the page CSP (`crates/reelix-server/src/web.rs`):
`ichef.bbci.co.uk`, `*.mzstatic.com`, `archive.org`. Other hosts (Podcast Index, feeds) show a
placeholder in search results; saved shows use the downloaded copy.

Tests: `tests/providers.rs` with trimmed real responses in `tests/fixtures/providers/`
(Podcast Index fixtures are written to its documented format).

### (c) Artwork and logos

Built (`src/artwork.rs`, `src/svg.rs`). At the end of `refresh_show` and of `add_show`,
`download_artwork(show_id)` fetches the show's `artwork_url` and its station's `logo_url`
into `<data_dir>/radio/artwork/{show_id}.{ext}` and `<data_dir>/radio/logos/{station_id}.{ext}`
(data folder from `RadioService::set_data_dir`; unset = no downloads), then records them with
`set_show_artwork_path` / `set_station_logo_path`. `{id}.source` next to the file holds the
URL it came from; a file is fetched again only when the URL changes or the file is gone. Never
on a page view.

- Limits: 2 MB artwork, 256 KB logos (streamed; bigger is refused).
- Types from the bytes, not `Content-Type`: JPEG, PNG, WebP, SVG; anything else is refused.
- SVG (`svg::sanitise`, quick-xml, allow-list): drawing elements and presentation attributes
  only; drops `<script>`, `<foreignObject>`, `<image>`, `<a>`, animation and unknown elements
  with their content, `on*` attributes, `href`/`xlink:href` other than `#id`, values with
  `javascript:`/`data:`/`expression(`/`url()` other than `url(#id)`, `<style>` with `url()` or
  `@import`, comments and other namespaces. DOCTYPE, malformed XML, nesting over 64 or a
  non-`<svg>` root is refused.
- A failed download is logged and keeps the previous file.

Routes (same permission as the other radio reads, `MANAGE_LIBRARY`):
`GET /radio/shows/{id}/artwork` and `GET /radio/stations/{id}/logo` → the file with a strong
`ETag` (content hash), `Cache-Control: private, max-age=3600`, `X-Content-Type-Options:
nosniff`, and for SVG `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline';
sandbox`. `If-None-Match` (lists, weak tags, `*`) → `304`. No file → `404`. Query strings
(the UI's `?v=`) are ignored. Deleting a show leaves its file on disk (overwritten if the id is
reused). Tests: `crates/reelix-api/tests/radio_artwork.rs`, `tests/providers.rs`.

### (d) Indexer search, release parsing and filing

- Categories: `ContentKind::Radio` searches Audio/Other (`reelix_indexers::categories::DEFAULT_RADIO`)
  and excludes Audio/Video. `content_kind_of` still maps Audio/* to music; adjust it if radio
  releases must be told apart by category.
- Quality: radio uses the audiobook family (`QualityFamily::of_content(ContentKind::Radio)` →
  `Audiobook`); `radio_shows.quality_profile_id` must be an audiobook profile. Download
  clients put radio in the book category.
- What to search for: `RadioService::wanted_episodes(Option<&[i64]>)` → `WantedEpisode`
  (show title, station, filing series number, episode number, title, date, profile, root
  folder). Add a job kind (e.g. `radio_episode_search`) handled by the pipeline, as
  `album_search` is for music (`crates/reelix-pipeline/src/search/`), and a
  `RadioService::request_search` that enqueues it.
- Release parsing (`Show S20E03`, `Show - Episode title`) belongs in `crates/reelix-parser`.
- Filing: `episode_file_name(show, series, episode, title, ext)` gives
  `{Show}/{Show} - S{series:00}E{episode:00} - {Title}.{ext}`; put it under the radio root
  folder (`root_folder_id`, or `settings().library_folder`), copy with `ffmpeg -c copy` or
  hard link (never overwrite, never touch a seeding file), tag it, ask Audiobookshelf to scan,
  then let the next library sync match it (or call `link_file_auto`). The pipeline hand-off
  for finished downloads follows `MusicService::import_api` in
  `crates/reelix-music/src/import_api.rs`.
