# Changelog

All notable changes to Reelix are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

When releasing, rename `Unreleased` to the version and date (`## [0.2.0] - 2026-12-01`) and tag the commit `v0.2.0`. The release workflow uses that section as the GitHub release notes.

## [Unreleased]

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
