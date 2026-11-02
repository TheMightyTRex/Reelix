# Native Spotify and YouTube downloads

Reelix's direct-download pipeline is implemented in Rust. It does not invoke
Zotify, youtube-dl, yt-dlp, Python, Deno, or a provider helper executable.
Only FFmpeg is used for media conversion and audio/video muxing.

Open **Downloads** (administrator only), enable the feature, and choose an absolute
server destination. Settings apply immediately. Disabling pauses queued work; use
Cancel for a running job. Old Zotify/yt-dlp executable settings are ignored.

## YouTube

Paste a video or playlist URL and select **Audio** or **Video**:

- Audio: original M4A/WebM, MP3, M4A, Opus or FLAC conversion.
- Video: MKV or MP4 with a strict maximum resolution. Video and audio are selected
  and downloaded separately, then muxed with FFmpeg. MP4 does not force H.264.
- Playlist mode requires explicit opt-in; otherwise a video URL downloads one video.
- Preview shows up to 50 titles; downloading follows playlist continuation pages.
- RustyPipe extracts stream metadata in-process. Reelix transfers bytes with
  reqwest, resumes using HTTP ranges, and enforces cancellation itself.

The native extractor uses the iOS/Android player clients. It does not invoke the
optional RustyPipe Botguard executable. Live streams, DRM-protected videos and
videos requiring unavailable authentication/PO tokens are unsupported. Extraction
can fail when YouTube changes its protocol; updating Reelix updates the extractor.
This implementation does not claim full yt-dlp compatibility or support for its
other websites. Stream decoding uses RustyPipe's embedded JavaScript engine;
no separate JavaScript runtime needs to be installed.

## Spotify

Reelix follows Zotify's workflow in native Rust: authenticate, expand track/album/
playlist URLs, select account-supported Vorbis quality, request the authorized audio
key, transfer/decrypt the CDN stream, remove Spotify's container prefix, and write
tags/artwork before publishing the completed file.

Create reusable session credentials with Reelix's native PKCE login command:

```powershell
reelix.exe spotify-login --credentials C:\Reelix\data\spotify-credentials.json
```

On Linux:

```sh
./reelix spotify-login --credentials /config/spotify-credentials.json
```

Open the printed authorization link in a browser on the same machine; the default
callback is `http://127.0.0.1:8898/login`. For a headless server, forward that port
over SSH or authenticate on your workstation and copy the resulting credentials
file to the server. The command does not start the Reelix web server. Optional
`--client-id` and `--redirect-uri` flags let you use a registered Spotify application;
its allowed redirect URI must match exactly.

Set **Spotify credentials file** to that file's absolute server path. Existing
librespot and Zotify reusable session JSON files are supported. Credentials remain
in a local file; Reelix does not ask for Spotify passwords or expose session data
through the API. Unix login files are created with mode 0600.

Paste an `https://open.spotify.com/track/...`, `/album/...`, or `/playlist/...` URL.
Original/OGG preserves Vorbis audio. MP3, M4A and FLAC conversion uses FFmpeg.
Converting lossy audio to FLAC does not improve quality. Normal/High/Very high
quality controls use the files available for the authenticated account; Very high
requires an eligible Premium account. Auto selects the best eligible Vorbis file.
Spotify may restrict playback to eligible accounts regardless of the selection.
Unsupported episodes/local tracks and incomplete playlist responses fail the job
rather than silently producing an incomplete collection. Reauthenticate if Spotify
invalidates your session.

Spotify Web API import-list credentials are separate and do not authenticate
native audio downloads.

## Files, progress and import

Jobs run serially and persist in SQLite. Each uses a `reelix-<job-id>` subfolder.
Changing the destination affects new jobs; retries retain their original folder.
The queue shows the latest 200 jobs. Percentages describe the current stream,
not the whole playlist. Cancellation drops native extraction/HTTP futures and
terminates an active FFmpeg process. Partial and completed files remain on disk.

Retry resumes partial streams when the server supports ranges, or restarts a
stream if ranges are ignored. A completed item is skipped on retry. Final files
are published by renaming a separate staging file after processing; an unfinished
job is requeued on server restart. Duplicate URLs/options are rejected while
queued/running and after completion.

Artist/album music can be matched using **Music → Import Library** after adding
its job folder as a music root. General videos and mixed individual tracks are
not automatically converted into movie/episode/MusicBrainz album records. YouTube
WebM original audio keeps its source metadata; M4A and converted audio get native
title/artist tags. Spotify gets native track, album, artist, disc, track and cover tags.

## Installation and Docker

The native download implementations ship in the Reelix binary. Install FFmpeg for
format conversion/video muxing and configure its executable path if it is not on
PATH. Native original-audio downloads do not need FFmpeg. Docker includes FFmpeg;
no Python environment or separately installed downloader is required.

For container deployments, use a writable persistent destination such as
`/data/downloads/direct` and put session credentials under `/config`. Run the native
login command with a browser-accessible loopback callback, then mount/copy the
credentials file into the container. The Reelix service user must be able to read it.

The administrator-only API is `/api/v1/downloads`, with `/settings`, `/health`,
`/preview`, `/{id}/cancel`, and `/{id}/retry`. Cookie-authenticated mutations require
the usual CSRF header. Health reports compiled-in providers, FFmpeg availability,
and whether the session file parses; account access is checked during use.

## Implementation references

- [Zotify](https://github.com/zotify-dev/zotify) (zlib): track/album/playlist workflow,
  Vorbis quality tiers, directory layout, metadata, and retry/skip behavior.
- [youtube-dl](https://github.com/ytdl-org/youtube-dl) (public domain) and
  [yt-dlp](https://github.com/yt-dlp/yt-dlp) (Unlicense): separate best audio/video
  selection, range resumes, and post-download muxing/conversion.
- [librespot](https://github.com/librespot-org/librespot) (MIT): native Spotify
  authentication, metadata, authorized audio keys and CDN resolution.
- [RustyPipe](https://codeberg.org/ThetaDev/rustypipe) (GPL-3.0): native YouTube
  metadata/player extraction and playlist continuation support.

This is a Rust implementation of the relevant workflows using native protocol
libraries, not a complete line-by-line port of either Python project. Cargo.lock
pins dependencies, including vergen 9.0.6 for librespot 0.8's build compatibility.
