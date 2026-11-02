# Spotify and YouTube downloads

Reelix's direct-download pipeline is implemented in Rust. Spotify is native and
does not invoke Zotify or Python. YouTube uses yt-dlp when its path is set (the
default) and falls back to the built-in RustyPipe extractor when the path is empty.
FFmpeg is used for media conversion and audio/video muxing.

Open **Downloads** (administrator only), enable the feature, and choose an absolute
server destination. Settings apply immediately. Disabling pauses queued work; use
Cancel for a running job. Old Zotify executable settings are ignored.

## YouTube

Paste a video or playlist URL and select **Audio** or **Video**:

- Audio: original M4A/WebM, MP3, M4A, Opus or FLAC conversion.
- Video: MKV or MP4 with a strict maximum resolution. Video and audio are selected
  and downloaded separately, then muxed with FFmpeg. MP4 does not force H.264.
- Playlist mode requires explicit opt-in; otherwise a video URL downloads one video.
- Preview shows up to 50 titles; downloading follows playlist continuation pages.

### yt-dlp (default)

YouTube changes its protocol often, and yt-dlp is updated within days, so it is
the default backend. Set **yt-dlp executable** to its name on PATH or an absolute
path, and keep it current with `yt-dlp -U` (or your package manager). Current
yt-dlp needs a JavaScript runtime for YouTube; install [Deno](https://deno.com)
on PATH, or some formats or videos will be missing. Distribution packages are often
too old to work; prefer the standalone release from
[yt-dlp's releases](https://github.com/yt-dlp/yt-dlp/releases).

yt-dlp lists playlists (`--flat-playlist`) and downloads the best audio stream and,
for video, the best video stream within the resolution limit. Reelix then converts,
muxes, tags and publishes the files exactly as with the built-in extractor, and
stops the yt-dlp process on cancellation. Reelix runs it with `--ignore-config`, its
own cache under the data directory, and FFmpeg's path when that is absolute; only
normalized YouTube URLs are passed. yt-dlp's error message is shown on failed jobs.

### Built-in extractor

Clear the yt-dlp path to use RustyPipe in-process instead. Reelix transfers bytes
with reqwest, resumes using HTTP ranges, and enforces cancellation itself. It uses
the iOS/Android player clients and does not invoke the optional RustyPipe Botguard
executable, so it breaks when YouTube requires PO tokens or changes its player;
failures then show RustyPipe's error and only a Reelix update fixes them.

Live streams and DRM-protected videos are unsupported with either backend. Neither
supports yt-dlp's other websites.

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

The Spotify client and built-in YouTube extractor ship in the Reelix binary. Install
FFmpeg for format conversion/video muxing and configure its executable path if it
is not on PATH. Original-audio downloads do not need FFmpeg. For YouTube, install
yt-dlp and Deno (see above). The Docker images include FFmpeg, the standalone
yt-dlp and Deno, taken from their latest releases when the image is built; pull a
newer image when YouTube downloads start failing.

For container deployments, use a writable persistent destination such as
`/data/downloads/direct` and put session credentials under `/config`. Run the native
login command with a browser-accessible loopback callback, then mount/copy the
credentials file into the container. The Reelix service user must be able to read it.

The administrator-only API is `/api/v1/downloads`, with `/settings`, `/health`,
`/preview`, `/{id}/cancel`, and `/{id}/retry`. Cookie-authenticated mutations require
the usual CSRF header. Health reports the YouTube backend (and whether yt-dlp runs),
FFmpeg availability, and whether the session file parses; account access is checked during use.

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
