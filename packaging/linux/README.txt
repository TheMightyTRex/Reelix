Reelix @VERSION@ for Linux (@ARCH@)
====================================

Pre-alpha build. Nothing has been tested against real indexers, download
clients or media servers yet.

Run
---
1. Extract this folder anywhere (keep "reelix" and the "web" folder together):
     tar xzf reelix-@VERSION@-linux-@ARCH@.tar.gz
2. Run ./start-reelix.sh
3. Open http://localhost:7700 and complete the setup wizard.

Settings, the database and logs are stored in the "data" folder next to
the reelix binary. Back it up before upgrading.

Options
-------
  ./reelix --port 8080            use another port
  ./reelix --bind 127.0.0.1       only accept connections from this machine
                                  (default: all network interfaces)
  ./reelix --data-dir /srv/reelix keep data somewhere else
  ./reelix --help                 all options

When running ./reelix directly, run it from inside this folder: the web UI
is loaded from ./web relative to the current directory (or set
REELIX_WEB_DIR).

Running as a service (systemd)
------------------------------
Example unit, assuming the folder was extracted to /opt/reelix and a
"reelix" user exists:

  [Unit]
  Description=Reelix
  After=network-online.target
  Wants=network-online.target

  [Service]
  User=reelix
  WorkingDirectory=/opt/reelix
  ExecStart=/opt/reelix/reelix --data-dir /var/lib/reelix
  Restart=on-failure

  [Install]
  WantedBy=multi-user.target

Coming from Radarr, Sonarr, Readarr, Lidarr, Prowlarr or Jellyseerr?
--------------------------------------------------------------------
Open System > Migrate, enter the app's URL and API key, and preview what
will be imported (tags, root folders, quality profiles, indexers, download
clients, movies, series, authors, artists, users and requests). Nothing
changes until you start the run, and running it again won't create
duplicates. Use path rewrites if the old app saw your media under different
paths.

Using Reelix with other apps
----------------------------
- Torznab/Newznab: add http://<host>:7700/api/torznab/all (torrents) or
  /api/newznab/all (Usenet) to another app as a single indexer that searches
  every indexer in Reelix. The links are shown in Settings > Indexers.
- *arr-compatible APIs for Homepage, Bazarr, LunaSea, nzb360, Recyclarr and
  similar tools: use http://<host>:7700/radarr, /sonarr, /readarr or
  /lidarr as the server address, with a Reelix API key (Settings > General).

Upgrading
---------
Stop Reelix, back up the "data" folder (or use System > Backup), replace
everything except "data" with the new version, and start it again.
Database changes are applied automatically on startup.

Requirements
------------
- Spotify/YouTube downloads are native Rust. FFmpeg is needed for conversion
  and video muxing. Use reelix spotify-login to create session credentials,
  then configure Downloads. See docs/direct-downloads.md for setup details.
- @ARCH@ Linux with glibc 2.17 or newer (any mainstream distro since 2014,
  e.g. CentOS 7, Ubuntu 16.04, Debian 9). Not Alpine or other musl-based
  distros.
- ffprobe (from FFmpeg) on PATH, for reading codec / resolution / audio info
  of imported files (e.g. "apt install ffmpeg").
