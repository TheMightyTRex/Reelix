# Changelog

All notable changes to Reelix are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

When releasing, rename `Unreleased` to the version and date (`## [0.2.0] - 2026-12-01`) and tag the commit `v0.2.0`. The release workflow uses that section as the GitHub release notes.

## [Unreleased]

### Added
- **Films kept as disc images.** A film folder's library scan now takes a Blu-ray or DVD disc image (`.iso` or `.img`) as the film's file, so a film migrated from Radarr as `Name (Year) BR-DISK.iso` no longer shows as missing. The image gets the BR-DISK quality (DVD when its name says so) and no media details, because ffprobe cannot read an image; it is not probed again on each scan. Disc images in an extras folder are left out. Imports from downloads and series folders still ignore disc images (demonzoned/Reelix#60).
- **Built-in custom formats for audio description and British Sign Language.** Reelix now adds two custom formats at start: "Audio description" (release names with Audio Description, Audio Described, Descriptive Video, DVS, or AD beside the audio such as `AAC2.0.AD`) and "BSL" (BSL, WiTH BSL, British Sign Language, SignZone, Signed Version). They score 0, so nothing changes until a quality profile gives one a score to prefer those releases. Each has a stable key, `audio_description` and `bsl`: `GET /api/v1/customformats/builtin` lists them and `GET /api/v1/customformats/builtin/{key}` returns one even after it is renamed, so a client looks it up instead of adding a second. A format that already has the same name becomes the built-in one, and a deleted built-in format is not added again (RFX-107).
- **More book subjects on Discover, and a list of them.** The Books tab keeps its default rows and now offers 25 more Open Library subjects as chips under "More fiction" (humour, adventure, crime, classics, dystopian, space opera, short stories, westerns, war, children's, poetry, plays, graphic novels) and "More non-fiction" (history, true crime, science, philosophy, psychology, self-help, business, nature, travel, sport, cooking, art); a picked subject shows as a row. `GET /api/v1/discover/genres/book` lists every subject with its row id (`subject-humor`), name, group (`fiction` or `non_fiction`), whether it is a default row and whether it suits audiobooks; `GET /api/v1/discover/genres/audiobook` lists only those that suit audiobooks. Every listed subject is a row of `GET /api/v1/discover/content/book/{row}`. The subjects are kept in one list, `BOOK_SUBJECTS` (demonzoned/Reelix#81).
- **Items with your own details and no outside match.** `POST /api/v1/customitems` adds a film, series, author, artist or radio show that TMDB, Open Library, MusicBrainz and the radio sources do not know (a home video, a lecture series, a self-published audiobook, a local radio programme): from an existing folder of a root folder of the kind, or for a radio show an Audiobookshelf podcast item, with a typed title (the name for authors and artists), year, overview, station (radio) and picture (an `http(s)` address Reelix fetches once, or a `data:` PNG, JPEG or WebP up to 5 MB, kept by Reelix). It answers 201 `{id, kind, title, path}`, 400 for a folder outside the root folder or a bad picture, and 409 when the folder or podcast already belongs to an item; it needs Manage library rights. The item is an ordinary item of its kind with `custom: true`: metadata refreshes keep the typed details, and scans make its episodes (series, by season and episode in the file names; radio, one per library file), books (author, one per sub-folder or file) and albums (artist, one per folder, kept without a tracklist). `PUT /api/v1/customitems/{kind}/{id}` changes the details or picture later and `GET /api/v1/customitems/{kind}/{id}/image` serves the picture. Nothing is searched for unless `monitored` is true (RFX-90).
- **History clean-up.** Reelix now keeps 90 days of history by default and removes older rows once a day (the "Clean up history" task). `GET` and `PUT /api/v1/settings/history` read and set `keep_days` (0 keeps everything, at most 36500), and `DELETE /api/v1/history?before=<RFC 3339 time>` removes every row dated before that time. All three need the admin right. Until now history grew without limit.
- **Films and series without a TMDB key.** With no TMDB key set, Reelix now uses open data instead of stopping: films from Wikidata (found by their TMDB or IMDb id) with the summary from Wikipedia, and series from TVmaze (found by their TVDB or IMDb id, or their TMDB id through Wikidata) with every season, episode and air date. Migrations from Radarr and Sonarr no longer skip the library. TMDB is still used when a key is set; posters for films, collections, people and discover feeds still need it. Data: Wikidata (CC0), Wikipedia and TVmaze (CC BY-SA 4.0).
- **Other addresses for TMDB and TVDB.** `metadata.tmdb_base_url` and `metadata.tvdb_base_url` in `config.toml` (or `REELIX_TMDB_BASE_URL` and `REELIX_TVDB_BASE_URL`) point Reelix at a mirror, a caching proxy or a test stand-in. Until now only the test suite could change them.
- **Downloads that hold only RAR archives are imported.** When a finished download has no video files but has RAR archives (one `.rar`, a `.part01.rar` set or a `.rar` and `.r00` set), Reelix unpacks the archives that hold a video into the download's folder and imports the video as usual. The archives are left for the download client to seed; the unpacked copy is removed once it is imported. A password-protected or damaged archive gets a message in the queue that says so, instead of "No video files found in the download". The Docker image now includes `unrar`; other installs need it on the `PATH` (demonzoned/Reelix#80).

### Fixed
- **The *arr health routes now show Reelix's warnings.** `GET /radarr/api/v3/health`, `/sonarr/api/v3/health`, `/readarr/api/v1/health` and `/lidarr/api/v1/health` always answered an empty list, so *arr clients, Homepage widgets and dashboards never saw a problem. They now list every check from `/api/v1/system/health` that is not `ok`, in the *arr shape (`id`, `source`, `type` of `warning` or `error`, `message`, `wikiUrl`), so a healthy Reelix still answers `[]`. They need the Manage library right, as before (RFX-83).
- **Episodes named with broadcast or TVDB numbers are imported by their title.** Reelix numbers episodes as TMDB does, so a download such as `Archer.S03E11.Space.Race.Part.1` (TMDB's season 3 of Archer has 10 episodes) was refused with "Episodes are not part of the series". When a one-episode file or release name carries an episode title after its number, Reelix now imports it to the episode of the series with that title, even when the number differs ("Part 1" matches TMDB's "(1)"; accents, punctuation and articles do not matter). The download then counts as done, instead of waiting with "Not all expected episodes were imported". Generic titles such as "Episode 3" and titles shared within a season are never used. Episode ids from the grab that the series no longer has are ignored, so a file without a title now says "Unable to match episodes" instead.
- **API-key requests no longer fail with "database is locked" while a long job writes.** Every request with an API key wrote the key's last use, so while a big radio show was refreshing (a daily drama has over a thousand episodes), dashboards, Torznab and *arr clients waited past SQLite's 5 second busy timeout and got a 500. A request now notes the key's use in memory, and Reelix writes it at most once a minute (and when it stops). A radio refresh now writes in batches of at most 50 ms, with a pause between them so other writers get in. The media server sync writes many rows per statement.
- **Reelix starts when its data folder is on a mergerfs pool or another file system without shared memory.** It stopped at once with "migration error ... (code: 5386) disk I/O error", because SQLite's write-ahead log needs a memory-mapped `-shm` file, and mergerfs with `cache.files=off` (its default) cannot map one. Reelix now falls back to a rollback journal and logs a warning. A local disk is still faster.
- **Small files in the library are no longer skipped as samples.** A library scan treated any video under 3 MB per minute of running time (and under 70 MB) as a sample, so older low-bitrate episodes and films (20 MB for a 22-minute episode) showed as missing. Files already in a library folder are now skipped only when their name or folder says "sample"; imports from downloads keep the size rule.
- **Books: authors whose works link a series by key now load.** Open Library has started to send some works' series as `{"series": {"key": "/series/OL…L"}, "position": "12"}` instead of text, and Reelix failed the whole author with "invalid type: map, expected a string" (Agatha Christie, Isaac Asimov, Douglas Adams and others). Both forms are read now; the linked form is skipped until Reelix reads series records.
- **Migration: a profile name used by another kind no longer fails.** Quality profile names are unique across films and series, music and books, so migrating Lidarr after Radarr failed on Lidarr's "Any" ("a quality profile named Any already exists") and skipped the artists using it. Such a profile is now created as "Any (Lidarr)", and the preview says so. When Reelix already has a profile of the same name and kind, the preview now says that Reelix's own is used and the app's changes to it aren't copied.

## [0.8.7] - 2026-11-09

### Fixed
- **Docker image name:** the README and `docker/docker-compose.example.yml` now use `ghcr.io/demonzoned/reelix`, where release images are published. The old `ghcr.io/mvanhare/reelix` name doesn't exist, so pulling it failed.

### Changed
- **Repository links:** the crate metadata and the user agent sent to MusicBrainz, ListenBrainz and Open Library now point to `github.com/demonzoned/Reelix`.

## [0.8.6] - 2026-11-06

### Security
- **Changing a provider's address no longer sends its saved password or API key to the new host.** Saving an indexer, download client or other connection with a new host or port now asks you to enter the secret again, as testing it already did (#23).

### Added
- **A new setup wizard** with three ways to start: a new install, restoring a Reelix backup, or migrating from the *arr apps. Each answer is saved to the setting Reelix already has, and every step after the account can be skipped.
  - **New install** asks what to follow, then the TMDB key, library folders, a torrent app and a Usenet app, which to prefer, sites to search, quality, when to get new movies, media servers and requests.
  - **Restore** stages a backup before any account exists (`POST /api/v1/setup/restore`, behind the same setup token as `POST /api/v1/setup`).
  - **Migration** brings the *arr apps over with the System → Migrate preview, inside the wizard.
  - "What is…" help on each step, a progress line, and focus on each new step's heading.
- **`reelix setup`:** the same wizard in a terminal, for a server with no desktop. Boxed (a screen per step, a progress bar and a menu on the bottom line, with help over the step) or plain (`--plain` or `REELIX_PLAIN=1`) for screen readers.
- **Settings → Set-up Wizard** with **Launch wizard**: an admin can run the wizard again (without the account and restore steps), in the browser or with `reelix setup`.
- **Classic onboarding as a build option:** build the web UI with `--features classic-setup` (or the Docker build argument `REELIX_WEB_FEATURES=classic-setup`) to keep the classic onboarding instead of the set-up wizard.
- **Terminal-only instructions** in `docs/terminal-setup.md`: every command from installing Reelix to the last set-up question.
- **Send feedback in the wizard,** with [Snaglist](https://github.com/TheMightyTRex/snaglist) (MIT). Reports name the step and open a GitHub issue. **Show as a QR code** sends a report from a phone when the server is not online yet; in the terminal the code is sized to the window, and the whole report is saved under the data directory's `feedback` folder.
- **Radio: popular shows on each BBC station.** Discover's Radio tab has a row for Radio 4, Radio 4 Extra, Radio 2, Radio 5 Live and the World Service, next to Popular on BBC Sounds (UK only, like the other BBC rows).
- **Radio: 32 BBC Sounds categories, up from 7**, under four headings: Comedy, Drama, Factual, and Sport and music. They include sub-genres such as Science fiction and fantasy, Sitcoms, Panel games and quizzes, and Thrillers.

### Changed
- **Radio search finds older BBC shows.** BBC Sounds search leaves out many shows, such as Cabin Pressure and Old Harry's Game. When none of its results has the name you typed, Reelix also reads the BBC programmes A to Z and lists its radio shows first.
- **Radio searches find more episodes:**
  - **BBC programme ids:** get_iplayer names such as `The_Unbelievable_Truth_-_Series_20_Episode_3_b09zv5m0_original.m4a` are now read, including the BBC programme id (`b09zv5m0`). For a BBC episode the id decides: a release with the episode's id matches even when the show is named differently, and one with another id is refused.
  - **More categories:** radio is now searched in Audio/MP3 and Audio/Audiobook as well as Audio/Other, because indexers often file radio there (MyAnonamouse's Radio categories arrive as Audio/Audiobook through Prowlarr and Jackett). A release still has to match a wanted episode, so music and audiobooks are not grabbed as radio. RSS sync also checks Audio/MP3 releases against wanted episodes before passing them to music.

### Fixed
- **Interrupted jobs:** a job cut off on its last attempt is marked failed instead of being retried after every restart (#24).
- **Folder moves:** moving a movie, series or author folder to another disk no longer stalls the server while the files copy (#37, #41).
- **Huge times:** large time values from indexers and download clients no longer crash Reelix (#27).
- **rTorrent:** it now reports when a download finished (#38).
- **Discord:** notifications no longer repeat the overview as a "Path" field (#39).

### Performance
- **Fewer idle database writes:**
  - the job runner skips kinds with nothing queued (#28);
  - the direct-download worker is woken instead of polling (#28);
  - library pages read tags and alternative titles only for the items shown (#26).
- **Calmer web UI:**
  - library pages bundle bursts of live updates (#34);
  - polling pauses in hidden tabs (#34, #40);
  - Downloads polls slowly when nothing is downloading (#40).
- **SQLite:** filtered history pages use an index, and the WAL file is kept small (#36).
- **Docker:** lower idle memory and a lighter health check (#29).

### Changed
- **Dependencies:**
  - updated: toml_edit 0.25, tower-http 0.7, toml 1, base64 0.23, openidconnect 4.1, encoding_rs and 10 GitHub Actions;
  - Dependabot now groups compatible Cargo updates into one pull request.

## [0.8.5] - 2026-11-06

### Added
- **The Windows installer asks how to install Reelix:**
  - **Fresh install:** a new, empty Reelix.
  - **Recovery:** pick a Reelix backup (`reelix_backup_*.zip`). Setup checks it and stages it, and Reelix restores it on first start, rolling back if the restored database won't open.
  - **Migration:**
    - **From Radarr, Sonarr, Prowlarr, Lidarr or Readarr on this PC.** Setup finds them from their `config.xml`, and you tick the ones to bring over. After you create the admin account, Reelix shows a "Ready to migrate" banner and opens System → Migrate with those apps filled in. Their API keys are stored encrypted and never put on a command line.
    - **From an existing Reelix folder,** such as the zip download. Setup copies its data and updates `config.toml` for the new location.
  - Upgrades skip this question and keep your data.
  - Silent installs can use `/MODE=fresh|recovery|migrate`, `/BACKUP=`, `/MIGRATEFROM=` and `/ARR=`; see `docs/installers.md`.
- New commands:
  - `reelix backup create`
  - `reelix restore check|stage <zip>`
  - `reelix migrate prepare|list`
  - `reelix config relocate`
- API: `GET`/`DELETE /api/v1/system/migrate/pending` (admin).

### Changed
- Release builds time out instead of hanging when a download stalls.

## [0.8.4] - 2026-11-06

### Fixed
- **Opening an Apple Podcasts show from Discover works again.** In 0.8.2 and 0.8.3 it failed with "Request failed (502)" or "this link cannot be read here", because the Apple source's feed reader was missing the public-only client that Discover uses.

### Security
- Feed links read for Discover and requests now also refuse private IP addresses written as numbers (such as `http://127.0.0.1/…`), not only host names that resolve to private addresses.

## [0.8.3] - 2026-11-05

### Added
- **Soulseek support through [slskd](https://github.com/slskd/slskd)**, as an indexer and a download client, for music, books, audiobooks and radio. See `docs/soulseek.md`.
  - **Search:** Reelix searches slskd and turns each peer's folder into one release (for example a whole album), named and tagged with its format, such as FLAC 24-96, MP3 320 or M4B. Releases are scored by your quality profiles and custom formats.
  - **Peers:** you can skip peers with no free upload slot, a long queue or a slow connection. Among otherwise equal results, a free slot ranks first.
  - **Download and import:** grabbing a release queues its files in slskd, and progress shows in the queue. Finished folders are imported from slskd's downloads folder, with remote path mappings for Docker.
  - **Delay profiles and blocklist:** delay profiles gain a Soulseek switch and delay. A failed download blocklists that peer's folder only, so the same album from another peer can still be grabbed.
  - **Setup:** adding the slskd download client also adds the matching indexer.

### Changed
- **Infinite scrolling.** These pages now load more as you scroll, instead of using page buttons or "Show more":
  - library pages: Movies, Series, Music artists, Books authors and Radio shows
  - logs and lists: History, Blocklist, Requests and Issues
  - Discover "see all" grids and Search results

  Every list keeps a **Load more** button for keyboard users, and shows "Showing N of M" where the total is known. Going **Back** to a library page returns you to where you were. Select all on Requests, History and Blocklist applies to the loaded rows and says so. The Queue and Wanted pages keep their page buttons.

## [0.8.2] - 2026-11-05

### Added
- **Radio on Discover:** radio programmes and podcasts now have their own tab on Discover and Search, next to books and music.
  - **Rows:** Apple Podcasts top charts and 19 categories for your country; on BBC Sounds, Popular and seven categories (UK only); and Podcast Index trending, once it's set up.
  - **Show view:** synopsis, station and the newest episodes. Shows you already follow are marked.
  - **Follow or Request:** library managers can follow a show straight away. Other users request it like a book or album, with radio permissions, auto-approve, default profile, root folder and tags, and a radio quota. Approving a request follows the show with your default follow choice.

### Security
- Podcast links read for Discover and requests can only reach public addresses. Private addresses are refused when connecting and on every redirect, so a public link can't lead the server into your network.

## [0.8.1] - 2026-11-05

### Fixed
- **Importing a large library no longer times out.**
  - **Before:** Import selected sent every folder in one request and only answered once all of them were added. With hundreds of folders that took longer than a reverse proxy allows (Cloudflare's 524).
  - **Now:** folders are imported 25 at a time, with an "Importing… N of M" counter, and the errors from every batch are listed together.
  - This applies to movies, series, music and books.
- **Matching and importing movies and series is about five times faster.** Reelix still paced its TMDB lookups to 4 per second, TMDB's old limit, which it dropped in 2019. It now makes up to 20 per second and still backs off if TMDB asks it to. Up to 8 folders are now added at once, up from 4.

## [0.8.0] - 2026-11-05

### Added
- **Radio**, a new media type for radio shows and podcasts, played through Audiobookshelf (#21).
  - **Follow shows:** follow every episode, only new ones, or none. Reelix keeps each show's series and episodes, with numbers, titles, first broadcast and length; episodes without a number are numbered in broadcast order.
  - **Sources:** podcast feeds, BBC programmes and BBC Sounds search, Apple Podcasts, Podcast Index (with your API key) and the Internet Archive. Search follows your country and language.
    - The BBC endpoints are public but undocumented, so requests are spaced out and cached. If the data changes you get a clear message, and nothing saved is lost.
  - **Audiobookshelf:** Reelix reads your Audiobookshelf podcast library and matches files to episodes:
    - by the source's id,
    - then by `S20E03` or the season and episode in Audiobookshelf,
    - then by a title only one file and one episode share.
    - You can set or clear a match by hand. A Review page lists podcasts and files that don't match anything.
  - **Finding and filing episodes:** wanted episodes are searched on your indexers (Audio/Other) with the usual quality, delay and release profiles, and finished downloads are filed into the podcast library.
    - Files are named `{Show}/{Show} - S20E03 - Title`, with radio naming settings.
    - Tags are written on Reelix's copy only. Audio is never re-encoded, existing files are never overwritten, and torrents still seeding are left untouched.
    - Then Audiobookshelf is asked to scan.
  - **Artwork and station logos:** downloaded when a show is refreshed. SVG logos are cleaned before they're shown.
  - **Pages:** Radio shows (with an A–Z list), show pages with a series picker and episode table, an episode panel for matching and searching, Add show (search or paste a link), Stations, Review, and Radio settings.
  - **Activity and history:** radio episodes appear in the queue, history and blocklist.
  - **Tasks:** refresh shows (12 h), sync feeds (1 h), read the Audiobookshelf library (15 min) and search for missing episodes (6 h).
  - **API:** everything is under `/api/v1/radio`. `POST /api/v1/command` is new, with `RefreshRadioShow`, `RescanRadioLibrary` and `RadioMissingSearch`.
- `THIRD_PARTY_NOTICES.md`, which ships with every download. The BBC provider is adapted from Storyarr (MIT).

### Changed
- Root folders can have the kind **Radio**.

### Fixed
- Restoring a backup on Windows waits up to ten seconds for a locked database file, instead of two.

## [0.7.1] - 2026-11-04

### Fixed
- **Import Library on large libraries:** scanning a root folder no longer looks up every folder one by one before showing anything.
  - That could take many minutes, or fail behind a reverse proxy with a timeout (Cloudflare's 524), while the page still said "Pick a root folder".
  - Folders are now listed right away, with a "Scanning folders…" state. Matches are then filled in batches with a progress counter, so no single request runs long.
  - Covers movies, series, music and books.
- **Music import matches every folder:** before, it only matched the first 25 folders on MusicBrainz and left the rest for manual searching. It now matches all of them, about 10 folders per batch because of MusicBrainz's 1-request-per-second limit.
- **Import shows why folders can't be matched**, for example TMDB not set up or a failed lookup, instead of silently leaving them unmatched. When nothing is selected, it explains why there's nothing to import.

### Added
- API:
  - `lookup=false` on the scan endpoints lists folders without metadata lookups.
  - `POST /api/v1/libraryimport/match`, `/musicimport/match` and `/bookimport/match` take up to 25 folders and return their suggestions.
  - Scan results gain `lookup_error`.

## [0.7.0] - 2026-11-04

### Added
- **Windows installer** (`reelix-<version>-windows-x86_64-setup.exe`). The installer offers two modes:
  - **Install for me** needs no admin rights. It installs to `%LOCALAPPDATA%\Programs\Reelix` with data in `%LOCALAPPDATA%\Reelix`, and adds Start menu shortcuts and an optional "start when I sign in".
  - **Install for all users** installs to Program Files and runs Reelix as a Windows service that starts at boot. Data goes in `C:\ProgramData\Reelix`, readable only by SYSTEM and Administrators, and there's an optional firewall rule.
  - Upgrading installs over the existing copy. Uninstalling keeps your data unless you choose to delete it.
- **Linux install script with systemd:** `curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh`.
  - It verifies the download, installs to `/opt/reelix` with a `reelix` system user, and sets up a hardened `reelix.service`.
  - Re-running it upgrades. `--uninstall` removes Reelix and keeps your data; `--purge` also removes the data.
- **In-app updates:** System → Updates now has **Install and restart**. Reelix downloads the release for your platform and checks it against the release's `SHA256SUMS`. Then it takes a backup, swaps in the new files (rolling back if anything fails) and restarts itself.
  - This works for the Windows installer (both modes), the Linux install script, and the zip/tarball downloads.
  - Docker installs keep using `docker compose pull`.
  - An optional **Install updates automatically** setting (off by default) installs new stable releases when the scheduled check finds them.
- `reelix service install|uninstall` to run Reelix as a Windows service without the installer.
- `POST /api/v1/system/updates/install`, and the `self_update` field on `GET /api/v1/system/updates`.

### Fixed
- Re-importing a file that's already hardlinked into the library no longer fails on Windows with "destination exists".
- Restoring a backup on Windows retries when the database file is briefly locked (for example by antivirus).
- The web UI is found next to `reelix.exe` when Reelix isn't started from its own folder (for example as a Windows service).
- Update checks use the repository's new location, `demonzoned/Reelix`.

## [0.6.0] - 2026-11-03

This release fixes the issues found in a full security audit. Upgrading is recommended.

### Security
- **Library paths:** a user with library access could move any directory on the server into the music folder (and then delete it) by editing an artist. Artist, movie, series and author paths must now be inside a root folder of the right kind, source folders are checked before moves, and `..` can no longer escape root checks during library import.
- **First-run setup:** a fresh install no longer lets anyone on the network create the admin account. Setup from another machine needs the one-time setup token printed in the log and console at startup; setup from the server itself works as before.
- **Login:**
  - Concurrent requests can no longer bypass the login rate limit.
  - There is a per-IP limit across usernames, and logins get `429` when the password-hash queue is full.
  - Disabling local login is enforced on the server. Admins can still log in from the server machine itself.
- **Password changes:** changing your own password asks for the current one and signs out your other sessions. An admin resetting someone's password signs out all their sessions and revokes their API keys.
- **User management:** users with "manage users" can no longer change the password or permissions of accounts that have permissions they lack, or import users with extra permissions.
- **WebSocket:** cookie-authenticated connections must come from the same origin. Open connections close when their session or API key is revoked or their permissions change.
- **Server-side requests:** the private-address protection is now enforced when connecting, so DNS rebinding and redirects can't get around it.
  - It also covers notifications and import lists when they're sent, and raw grabs.
  - FlareSolverr and redirected tracker requests no longer send cookies or login bodies to other hosts.
  - More private and special-use address ranges are blocked.
- **Denial of service:**
  - Deeply nested XML from an indexer or rTorrent can no longer crash the server.
  - Responses from import lists, notifiers, FlareSolverr, download clients and *arr migration sources are size-capped.
  - Backup uploads are checked for admin before the body is read, and are streamed to disk instead of held in memory.
  - Restoring a backup caps the size of each extracted file.
- **Secrets:**
  - Tracker cookies, passkeys and API keys of definition-based indexers are now encrypted at rest and masked in the API; existing values are encrypted on first start.
  - Backups and `secret.key` are owner-only, on Windows too.
  - Restoring a backup keeps this installation's `[paths]`, `[database]`, trusted proxies and key-file settings.
- **Other fixes:**
  - The data directory is no longer shown to non-admins.
  - Indexer and metadata links are only rendered for `http`/`https` URLs.
  - The OIDC callback no longer echoes attacker-supplied error text.
  - Recycle-bin cleanup only deletes folders the recycler created.
  - Windows reserved file names (`CON`, `NUL`, …) are escaped.
- **Supply chain:**
  - Docker images pin base images by digest and verify yt-dlp and Deno checksums.
  - Workflow actions are pinned to commit SHAs.
  - Release downloads carry signed build provenance (`gh attestation verify <file> --repo demonzoned/Reelix`).

### Added
- `security.secure_cookies` (`auto`, `always` or `never`; env `REELIX_SECURE_COOKIES`) to force or disable the `Secure` cookie flag. `auto` keeps the current behaviour.
- `security.trusted_proxies` is now used for client IPs (rate limiting and local-access checks) and validated at startup.

### Changed
- Testing an existing indexer, client or notifier after changing its host or port asks you to re-enter its password or API key.
- Raw grabs (after the search cache expires) only accept download URLs on the indexer's own host or its subdomains.
- The indexer definitions source must use `https`, except on loopback.
- API:
  - New fields: `SetupStatus.setup_token_required`, `SetupRequest.setup_token` and `UpdateUserDto.current_password`.
  - `SystemStatus.data_dir` is only sent to admins.

## [0.5.0] - 2026-11-03

### Fixed
- YouTube audio downloads are sorted into `Artist/Album` folders when they complete, like Spotify downloads, instead of being left loose in the job folder. The artist comes from YouTube Music metadata or the channel (without " - Topic"); the album from the track or the playlist name.
- Much lower memory use after library refreshes. Metadata responses (TMDB, TVDB, MusicBrainz, Open Library) are cached as compact JSON instead of parsed trees (about 3.4× smaller), the caches hold fewer entries, and expired entries are swept every 5 minutes instead of lingering until the cache fills.

### Security
- Updated `zip` to 9.x and `quick-xml` to 0.41, fixing RustSec advisories for EPUB/backup archive extraction and XML parsing. Remaining advisories in librespot's and openidconnect's dependencies are documented in `deny.toml`.

## [0.4.0] - 2026-11-02

### Added
- Queue: select rows (or a whole page) and remove them together (#1). Bulk removal leaves the download client untouched unless you choose otherwise.
- Discord notifications use a richer layout: overview, rating, genres, codecs, languages, subtitles, TMDb/IMDb/Trakt/TVDB/YouTube links, the release name and the backdrop image (#2). Other services gain the rating, genre, codec and language fields.
- Discover: the Trailer button plays the YouTube trailer in a popup instead of opening a new tab (#4).

### Changed
- New Reelix logo in the web UI and as the browser icon.

### Fixed
- The queue no longer fills with old, finished downloads from a shared download-client category (for example SABnzbd history imported by Sonarr or Radarr). Unknown downloads are only tracked while in progress or within a day of finishing (#1).
- Spotify downloads retry slow audio-key replies, fall back to Spotify's alternative tracks for tracks not licensed in the account's country, and explain a refusal (including when the account is not Premium).

## [0.3.0] - 2026-11-02

### Added
- Connect Spotify from Download settings: a browser PKCE login that finishes by itself on the Reelix machine, or from the redirected address pasted back from any other device. Disconnect removes the saved session.

## [0.2.0] - 2026-11-02

### Added
- Downloads page with native Rust Spotify tracks/albums/playlists, YouTube audio/video choices, format and quality controls, native YouTube/Spotify preview, and a persistent queue with progress, cancellation and retry.
- Direct-download tool/session checks, administrator-only API routes, native Spotify PKCE login and FFmpeg packaging, and setup documentation.
- YouTube downloads use yt-dlp by default (configurable path; Docker images ship yt-dlp and Deno), with the built-in RustyPipe extractor when the path is empty. Failed YouTube jobs now show the underlying extractor error.

## [0.1.0] - 2026-11-01

First public pre-release. Nothing has been tested against real indexers, download clients or media servers yet.

### Added
- Movies and TV: monitoring, indexer search, grabbing, importing and renaming, with quality profiles, custom formats (TRaSH-guide compatible), delay profiles, release restrictions and a blocklist.
- Books and music: authors, books, artists and albums with Open Library, MusicBrainz and ListenBrainz metadata, EPUB and audio tagging, and requests.
- Requests and Discover for users, with approval, quotas, issues, and Plex, Jellyfin, Emby and OIDC sign-in.
- Indexers: Newznab, Torznab and YAML (Cardigann-style) definitions; download clients for the common torrent and Usenet apps, plus a blackhole.
- Media server, notification and list integrations.
- Calendar with an iCal feed, wanted lists, activity and history.
- Backup and restore, update checks, a log viewer, UI settings and server settings screens.
- Migration from Radarr, Sonarr, Readarr, Lidarr, Prowlarr and Jellyseerr (System → Migrate).
- Torznab/Newznab aggregator feed, and Radarr, Sonarr, Readarr and Lidarr compatible APIs for third-party tools.
- Windows and Linux (glibc 2.17+) downloads, and a Docker image.

### Security
- Threat model and review in `docs/security.md`. Tracker cookies stay on the tracker's own hosts, API keys are stripped from error text, paths are canonicalised before imports, and remote responses are size-capped.
