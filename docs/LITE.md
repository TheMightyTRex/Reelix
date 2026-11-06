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
4. pushes the merged branch.

A clash or failure pushes nothing, and GitHub emails the owner. It warns when upstream changes its own
Dockerfile. The image is published only from a run started by hand with "publish" ticked.
