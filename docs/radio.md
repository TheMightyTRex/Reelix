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
| `src/xml.rs` | Small quick-xml tree (entity handling, depth limit 64) |
| `src/shows.rs` | Add, edit, delete, list, get; episodes; stations; search; preview; `recompute_statuses` |
| `src/refresh.rs` | `refresh_show`, `refresh_shows`, merging provider data, broadcast-order numbering, settling |
| `src/status.rs` | Pure status rules (`monitored_by_follow`, `episode_status`, `is_settled`, `filing_series_number`) |
| `src/matching.rs` | Hand-set file links (`set_episode_file`, `clear_episode_file`) |
| `src/library.rs` | `RadioLibrarySync` trait, `NoopLibrarySync`, `NewEpisodeFile`, file rows, automatic links, review |
| `src/artwork.rs` | Where downloaded artwork and logos are recorded |
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
| `radio_library_sync` | 15 min | Calls the attached `RadioLibrarySync` (a no-op until one is attached) |

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
| `GET /radio/library/review` | `{unmatched_shows, unmatched_files, synced_at}` from `radio_episode_files` |
| `GET/POST /radio/rootfolders`, `DELETE /radio/rootfolders/{id}` | Root folders of kind `radio` |
| `GET/PUT /radio/settings` | `RadioSettingsDto` |
| `POST /command` | `{name: "RefreshRadioShow", show_ids}` or `{name: "RescanRadioLibrary"}` |

Events: `entity.changed` with entity `radio_show`, `radio_episode`, `radio_episode_file`
(also `radio_station`, `rootfolder`, `settings`).

Settings (`radio.settings`): `abs_library_id`, `abs_library_folder` (as Audiobookshelf sees
it), `library_folder` (as Reelix sees it; must exist and be writable), `default_follow`,
`enabled_providers` (empty = all), `country`, `language`, `max_lookups_per_refresh`.

## Extension points

### (a) Audiobookshelf podcast library sync and matching

Implement `reelix_radio::RadioLibrarySync` (`src/library.rs`), e.g. in a new
`crates/reelix-radio/src/abs.rs` or next to the connector in
`crates/reelix-mediaserver/src/audiobookshelf.rs`, and attach it in
`crates/reelix-server/src/modules.rs` with `radio.set_library_sync(Arc::new(...))` (pass it
whatever finds the Audiobookshelf server, e.g. the providers' media servers; read
`RadioService::settings().abs_library_id`). The `radio_library_sync` task and
`POST /command {"name": "RescanRadioLibrary"}` then run it.

A sync should, with one `seen_at = now` for the whole run:

1. Read the podcast items/episodes and call `upsert_episode_files(&[NewEpisodeFile], seen_at)`
   for episodes with an audio file (returns row ids).
2. Call `remove_unseen_files(seen_at)`: deletes rows not seen this run; their episodes drop
   the link (also hand-set ones) and return to Wanted/Not requested.
3. Match (issue order: source id in guid/enclosure/file name; series + episode number within
   a show with the same `clean_title` or `abs_item_id`; unique title) using `episode_files()`,
   `show_match_keys()`, `clean_title()`, `show_episodes(show_id)`, then `link_file_auto(episode,
   file)`. It refuses (returns false) when the episode or the file has a hand-set link
   (`manual_match`); an episode with `manual_match` and no file was cleared by hand ("Not this
   file") and must stay unmatched.
4. Optionally set `abs_item_id` on shows (`update_show` with `RadioShowEdit { abs_item_id }`).
5. `mark_library_synced(now)`; return a `LibrarySyncSummary`.

`GET /radio/library/review` already reads the table. `check_library_folder(path)` gives the
plain mounted/writable message for health checks.

### (b) More providers

Add a module under `crates/reelix-radio/src/providers/` (`bbc.rs`, `podcastindex.rs`,
`apple.rs`, `archive.rs`), `pub mod` it in `providers/mod.rs`, implement `RadioProvider`, and
register it in `modules.rs` with `ProviderRegistry::with(...)` before `RssProvider`. Take the
shared `reqwest::Client` (`deps.http`). Directories that only find feeds (Podcast Index,
Apple) can return hits with `provider: "rss"` and `external_id` = feed URL, so the show is
then followed through the feed provider. Honour `ShowFetch` (skip settled series, spend at
most `max_lookups` per-episode requests, set `complete = false` when cut short), map 429 to
`ProviderError::RateLimited`, and keep caching/pacing inside the provider. Test against
saved fixtures with wiremock (see `tests/radio.rs`). New provider ids become valid values of
`enabled_providers` automatically.

### (c) Artwork and logos

`artwork_url` (shows) and `logo_url` (stations) are saved on refresh; `artwork_path` and
`logo_path` are empty. Download during refresh (hook at the end of `RadioService::refresh_show`
in `src/refresh.rs`, and after `add_show`), store under the data folder, then record with
`set_show_artwork_path` / `set_station_logo_path` (`src/artwork.rs`); read back with
`show_artwork_path` / `station_logo_path` / `get_station`. DTOs already expose `has_artwork`
and `has_logo`. Add the serving routes (e.g. `GET /radio/shows/{id}/artwork`,
`GET /radio/stations/{id}/logo`) to `routes/radio.rs` with ETags, the size limits (2 MB
artwork, 256 KB logos) and SVG cleaning with a strict CSP.

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
