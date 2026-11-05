# Soulseek (slskd)

Reelix searches and downloads from the Soulseek network through
[slskd](https://github.com/slskd/slskd), a self-hosted Soulseek client with a REST API.
Soulseek is a third download protocol next to torrent and usenet: releases carry a
**Soulseek** badge, and the protocol has its own delay-profile settings, blocklist entries,
history and queue rows. Reelix uses it for **music, ebooks, audiobooks and radio**. Movie
and TV searches never reach slskd.

The integration was built against slskd 0.26 (`/api/v0`) but has not been tested against a
live instance yet.

## Setup

1. **API key.** Give slskd an API key, either with `SLSKD_API_KEY` /
   `--api-key` or under `web.authentication.api_keys` in `slskd.yml`. A `readonly` key is
   enough, because slskd lets any key search and enqueue.
2. **Download client.** Open **Settings → Download clients → Add**, then pick **slskd
   (Soulseek)**. Fill in the following:
   - Host and port: 5030 by default. SSL and URL base are optional.
   - API key.
   - **Downloads folder**: slskd's `directories.downloads` as **slskd** sees it, for example
     `/app/downloads` in the official image.

   Leave **Also add the slskd indexer** on and saving the client also creates an **slskd**
   indexer with the same address and API key. This is skipped if one for that address
   already exists. You can also add the indexer yourself under **Settings → Indexers**.
3. **Test** both. The checks are:
   - the connection;
   - the API key (`GET /api/v0/session`);
   - whether slskd is logged in to the Soulseek network (`GET /api/v0/application`).

slskd normally runs on your LAN. Its client and indexer may use private addresses,
whatever **Allow private addresses** is set to, just like every other download client.
Like other secrets, the API key is stored encrypted and masked in the API and UI.

### Sharing the downloads folder (Docker)

slskd moves finished files to `<downloads>/<remote folder name>/`. For example, a peer's
`@@abcde\Music\Radiohead\1997 - OK Computer\01 - Airbag.flac` ends up in
`/app/downloads/1997 - OK Computer/01 - Airbag.flac`. Reelix imports from that folder, so it
must be able to read it:

```yaml
services:
  slskd:
    image: slskd/slskd
    volumes:
      - /srv/media/slskd:/app/downloads
  reelix:
    volumes:
      - /srv/media/slskd:/downloads/slskd
```

If the paths differ, as above, add a remote path mapping under **Settings → Download
clients**: host = the slskd host, remote `/app/downloads`, local `/downloads/slskd`. Keep
slskd's default `${SOURCE_DIRECTORY}` download subdirectory pattern. With a custom pattern
or `{}`, Reelix looks in the wrong folder.

## Indexer settings

| Setting | Default | |
|---|---|---|
| Search timeout | 15 s | How long slskd waits for further responses (5–20 s). Reelix cancels the search a few seconds later and reads what came in. |
| Minimum files | 1 | Folders with fewer matching files are skipped. |
| Maximum results | 50 | Folders returned per search, best peers first. |
| File types | flac, mp3, m4a, aac, ogg, opus, wav, m4b, epub, mobi, azw3, pdf | Other files (cover art, cue sheets) are dropped from the release. |
| Require a free upload slot | off | Skip peers that would queue you. |
| Maximum peer queue length | 50 | 0 = no limit. |
| Minimum upload speed | 0 KB/s | |

Searches use the same terms as other indexers:
- music: `artist album`;
- books: `author title`;
- radio: the episode terms.

Punctuation is removed, because Soulseek matches whole words. Each peer folder becomes one
release:
- **Title**: the folder name. When the folder alone lacks the artist or author
  (`Radiohead\1997 - OK Computer`, `Frank Herbert\Dune`), the parent folder is prefixed.
  Disc folders (`CD1`) take the album folder's name.
- **Format tag**: derived from the files and appended, for example `[FLAC]`,
  `[FLAC 24-96]`, `[MP3 320]`, `[MP3 V0]`, `[AAC 256]`, `[OPUS]`, `[M4B]` or `[EPUB]`.
  The normal music and book quality profiles and custom formats score it.
- **Peer details**: the size is the sum of the files. The username, free upload slot and
  queue length show in interactive search. **Free upload slot** is also an indexer flag that
  custom formats can match.

Among equally good releases, Reelix prefers these peers, in order:
1. peers with a free slot;
2. then a shorter queue;
3. then a faster peer.

Searches run concurrently with your other indexers. slskd accepts one search request at a
time, so Reelix retries briefly when it is busy. The usual per-indexer query limits apply.

## Download client settings

**Fail if any file fails** is on by default. When a file fails for good (rejected, peer
offline, cancelled), the release fails at once. It is then blocklisted and searched again.
Files slskd is still retrying (`nextAttemptAt`) don't count. With the setting off, the
release completes with whatever arrived, and the import decides whether that is enough.

The queue shows progress as bytes transferred out of the folder's total. Peers that queue
you appear as **Queued** with your position. **Remove completed** clears the folder's
transfers from slskd after import. slskd's API never deletes downloaded files, so Reelix
moves, hardlinks or copies them per your import mode, and anything left stays in slskd's
downloads folder.

## Delay profiles and blocklist

Soulseek has no publish date. A release counts as published when Reelix first finds it, and
a held (pending) release keeps that time. The **Soulseek delay** is measured from then.
**Enable Soulseek** and **Preferred protocol: Soulseek** work like their torrent and usenet
counterparts.

A Soulseek blocklist entry blocks that **peer's folder** (username + remote folder), not the
title. The same album from another peer can still be grabbed.

## Limitations

- **Slow searches.** Every slskd search takes roughly the search timeout. Automatic
  searches for large wanted lists take correspondingly longer.
- **Peers come and go.** A folder found a minute ago may be offline or refuse the transfer
  by the time it is grabbed. That fails the download and triggers a new search.
- **No seeding.** Nothing is uploaded back automatically. Share your library in slskd if
  you want to give back.
- **No RSS.** Soulseek has no feed of new uploads, so the slskd indexer is search-only: RSS
  sync is off by default and returns nothing.
- **No raw grabs.** Results under **Indexers → Search** can't be sent to a client directly.
  Use interactive search on the album, book or episode page.
- **Not adopted.** Downloads you start in slskd's own UI are ignored, because slskd has no
  categories to tell them apart from Reelix's.
