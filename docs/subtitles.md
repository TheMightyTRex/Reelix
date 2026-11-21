# Subtitles

Reelix can fetch subtitles for films and episodes and save them beside the video, named as Jellyfin reads them. It is off until you activate it in **Settings, Subtitles** (fork issue #62, plan task RLX-09).

## Sources

- **OpenSubtitles.com**, through its REST API, with your own API key and account. Create the key at opensubtitles.com under Profile, API consumers. The account gives the daily download allowance: a free account allows a small number of downloads a day. Without a username, only the API key is sent and OpenSubtitles applies its allowance for unsigned requests.
- **Stremio subtitle add-ons**: enter the add-on's address, with or without `/manifest.json` (`stremio://` addresses work too). Reelix asks for `{address}/subtitles/movie/tt0113277.json` for a film and `{address}/subtitles/series/tt0903747:2:5.json` for an episode, adding the file's hash, size and name (`/videoHash=…&videoSize=…&filename=….json`) for add-ons that use them. Add-ons need the IMDb id of the film or series, and they do not mark SDH or forced subtitles, so Reelix uses them for normal subtitles only.

## Which subtitle is taken

1. **Made for this file**: the OpenSubtitles file hash matches. The hash is the file size plus the sum of the first and the last 64 KiB of the file, read as little-endian 64-bit numbers.
2. **The same film or episode**: found by IMDb or TMDb id, with the season and episode for an episode.
3. **The release name**: for an item with no ids (an item added with your own details), only when the subtitle's release name shares at least half its words with the file's.

Within a level, Reelix prefers the release name closest to the file's, then subtitles translated by people over machine translations, then the most downloaded, then the sources in the order of the settings. Subtitles split over several files (CD1, CD2) are left out.

## Languages and file names

Each language is a two-letter code (`en`) or a code with a region (`pt-BR`). Each can have any of:

| Variant | File name |
|---|---|
| Normal | `Heat (1995).en.srt` |
| SDH, for people who are deaf or hard of hearing | `Heat (1995).en.sdh.srt` |
| Forced, only the parts in another language | `Heat (1995).en.forced.srt` |

A subtitle file that is already there and that Reelix did not save (from Bazarr or by hand, for example) is left alone, and counts as there.

## When Reelix searches

- **After an import**: a new film or episode file goes to the front of the subtitle schedule and a run starts.
- **On a schedule**: the task "Search for missing subtitles" runs every 6 hours and searches at most 50 files. A file is searched again after a day while a subtitle is missing, after a week while only a better match could come, and after a month when every subtitle is made for the file. Changing the languages or add-ons starts every file afresh.
- **Upgrades**: when a subtitle made for the file appears later, it replaces one matched by id or release name. Upgrades can be deactivated in the settings.
- **Now**: `POST /api/v1/subtitles/{kind}/{file_id}/search` (`kind` is `movie` or `episode`; the id is the movie file or episode file id) queues a search for one file.

## Checks

- **Timing**: a subtitle whose last cue ends more than 10% of the video's length (at least 2 minutes) after the video's end is turned down, and the next candidate is tried. The video's length comes from its media details; without them the check is skipped.
- A download that holds no subtitle cues is turned down. WebVTT is changed to SRT; other SRT files are saved as they arrive, in their own encoding, which Jellyfin detects.
- Subtitle files larger than 4 MB are refused. Reelix tries at most 3 candidates for each language and variant in one search.

## Rate limits

Reelix sends one request a second to OpenSubtitles and to each add-on. A "slow down" answer (HTTP 429) is waited out once, for the time its `Retry-After` gives (at most a minute). When the OpenSubtitles download allowance runs out, Reelix stops downloading from OpenSubtitles until the time OpenSubtitles gives for the reset, and searches continue with the add-ons.

## What is recorded

The `subtitle_files` table (migration `0024_subtitles.sql`) has one row for each video file, language and variant: where the subtitle is saved, its source and source id, the match level (`hash`, `id` or `release`), the release name and when it was fetched. `subtitle_searches` holds when each file was last searched and when it is due again. Rows go when their video file's row goes.

## API

| Method and path | Right | What it does |
|---|---|---|
| `GET /api/v1/settings/subtitles` | Admin | The settings; the API key and password come back masked |
| `PUT /api/v1/settings/subtitles` | Admin | Saves them; sending the mask back keeps the stored secret |
| `GET /api/v1/subtitles/{kind}/{file_id}` | Manage library | The subtitles Reelix fetched for a file |
| `POST /api/v1/subtitles/{kind}/{file_id}/search` | Manage library | Queues a search now (409 while subtitles are off) |

The OpenSubtitles API key and password are stored encrypted, like other provider secrets.

## Not covered yet

- The Lite variant (`fork/lite`) does not have subtitles yet.
- Making Bazarr optional in Starboard's installs is a separate change in Starboard.
- When an upgrade imports a file with a different name, subtitles saved for the old name stay beside it.

Mediarium (AGPL) was read for ideas only; none of its code is used.
