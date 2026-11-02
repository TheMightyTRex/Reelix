# Changelog

All notable changes to Reelix are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

When releasing, rename `Unreleased` to the version and date (`## [0.2.0] - 2026-12-01`) and tag the commit `v0.2.0`. The release workflow uses that section as the GitHub release notes.

## [Unreleased]

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
