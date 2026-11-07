# The lite version (fork only)

This branch, `fork/lite`, is TheMightyTRex's lite build of Reelix: upstream Reelix with two build options
switched off and a slimmer image. It is not part of upstream Reelix.

| | Upstream image | Lite image |
|---|---|---|
| Direct downloads (YouTube, Spotify) | Yes | No (`direct` feature off) |
| `/api/docs` page | Yes | No (`docs` feature off; `/api/v1/openapi.json` is kept) |
| ffmpeg | Debian's full package (479 MB) | A stand-alone `ffprobe`, and a 3 MB `ffmpeg` that only rewraps radio captures (MPEG-TS to Matroska audio) |
| Deno and yt-dlp | Yes (138 MB) | No |
| Web app | Yes | Yes (set `REELIX_WEB_DIR` to an empty folder for headless use) |

Build it: `docker build -f docker/Dockerfile.lite -t reelix:lite .`

## How Starboard uses it

Starboard's Install apps offers three installs (7 November 2026). **Basics** uses this lite image; **Full install**
and **Everything** use the full image below, and fall back to Lite when it cannot be downloaded. Settings, Your install
in Starboard switches between them either way, keeping Reelix's settings folder. Starboard sets Reelix up over its
API: the admin account, a manager key for itself, the library folders, qBittorrent and NZBGet (the default Usenet
app; SABnzbd is a choice in Full install and Everything).

## The full image, for Starboard

Starboard offers the lite image by default and the full version as a choice. A run of the Lite workflow started by
hand with **publish_full** ticked builds the fork's main branch (upstream plus the fork's fixes waiting in upstream
pull requests) with upstream's own `docker/Dockerfile`, so it has YouTube and Spotify downloads, and publishes it as
`ghcr.io/themightytrex/reelix:full`. Lite and the full version then carry the same fixes.

## What differs from upstream

Only these, so each upstream release merges cleanly:

- `docker/Dockerfile.lite`, `.github/workflows/lite.yml`, `.github/workflows/codeql.yml` and this file:
  new files.
- The `direct` and `docs` features: `Cargo.toml` (`reelix-api` without default features), the
  `[features]` blocks in `crates/reelix-api/Cargo.toml` and `crates/reelix-server/Cargo.toml`, and
  `#[cfg(feature = ...)]` lines in `crates/reelix-api/src/lib.rs`, `crates/reelix-api/src/routes/mod.rs`,
  `crates/reelix-api/tests/direct.rs`, `crates/reelix-server/src/main.rs` and
  `crates/reelix-server/src/server.rs`.

The features are offered upstream in demonzoned/Reelix#33. If they are accepted, only the new files are
left.

## How it stays current

`.github/workflows/lite.yml` runs every day. It:

1. merges upstream's newest release tag;
2. tests the lite build;
3. builds the image with the release in its version label (`org.opencontainers.image.version`, such as
   0.7.1), and checks it starts healthy, has ffprobe and carries that label;
4. pushes a new merge to a review branch, `lite-auto/<tag>` (such as `lite-auto/v0.8.7`), not to
   `fork/lite`.

A clash or failure pushes nothing, and GitHub emails the owner. It warns when upstream changes its own
Dockerfile.

Upstream's code (its build scripts, tests and Dockerfile) runs only in jobs with a read-only token and no
secrets. Pushing and publishing happen in separate jobs that run no repository code, and the tag name is
checked against `^v[0-9]+\.[0-9]+\.[0-9]+$` before any script uses it.

To take in a new upstream release:

1. Review `lite-auto/<tag>` (the run summary says when one is waiting).
2. Merge it into `fork/lite`: `git merge --ff-only origin/lite-auto/<tag>`, then push `fork/lite`.
3. Run the Lite workflow by hand with **publish** ticked.

The image is published only from a run started by hand with "publish" ticked, and only for a release
`fork/lite` already has: a run that would have to merge a new release first refuses to publish, so only
reviewed code reaches `ghcr.io/themightytrex/reelix:lite`.
