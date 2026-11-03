# Installers

Every release ships the same program in several forms:

| Download | Kind (`install.json`) | Program folder | Data folder | Runs as |
|---|---|---|---|---|
| `reelix-<v>-windows-x86_64.zip`, `reelix-<v>-linux-<arch>.tar.gz` | `portable` | wherever you unpack it | `data` next to the binary (via `start-reelix`) | you, in a console |
| `reelix-<v>-windows-x86_64-setup.exe`, **Install for me only** | `windows-user` | `%LOCALAPPDATA%\Programs\Reelix` | `%LOCALAPPDATA%\Reelix` | you, while signed in |
| `reelix-<v>-windows-x86_64-setup.exe`, **Install for all users** | `windows-service` | `C:\Program Files\Reelix` | `C:\ProgramData\Reelix` | Windows service `Reelix` (LocalSystem) |
| `install.sh` (Linux) | `linux-systemd` | `/opt/reelix` (owned by `reelix`) | `/var/lib/reelix` | systemd unit `reelix.service`, user `reelix` |
| Docker image | — | `/app` in the image | `/config` volume | container |

`install.json` sits next to the binary and tells Reelix how it was installed, so the in-app
updater knows what to replace and how to restart. Installers write it; updates never touch it.

Logs are in `<data folder>/logs`. Settings live in `<data folder>/config.toml`.

## Windows installer

Run `reelix-<version>-windows-x86_64-setup.exe`. Setup first asks **Install for me only** or
**Install for all users**; an upgrade reuses the mode of the existing installation. To switch
modes, uninstall first (your data stays) and copy `%LOCALAPPDATA%\Reelix` to
`C:\ProgramData\Reelix` or back if you want to keep it: the two modes are separate
installations with separate data folders, and both use port 7700.

### Install for me only

- No admin rights needed.
- Start menu: **Reelix** starts it (a console window shows the log; closing the window stops
  Reelix) and **Open Reelix** opens <http://localhost:7700>.
- Optional: **Start Reelix when I sign in** (a `HKCU\...\Run` entry that starts it minimized)
  and a desktop shortcut.
- Windows Firewall may ask for permission the first time Reelix listens; allow it if other
  devices should reach Reelix.

### Install for all users (Windows service)

- Needs admin rights. Setup runs `reelix.exe service install --data-dir "C:\ProgramData\Reelix"`,
  which registers the service `Reelix` (automatic start, LocalSystem, restarts 5 s after a
  failure) and starts it. Manage it in `services.msc` or with `sc stop Reelix` / `sc start Reelix`.
- Optional: a Windows Firewall rule named `Reelix` that allows inbound connections to
  `reelix.exe` on private and domain networks.
- A new `C:\ProgramData\Reelix` gets an ACL with only SYSTEM and Administrators, so other
  local users can't read the database or keys.
- LocalSystem can't open network shares with your credentials. For media on a NAS share,
  either use UNC paths the computer account can reach, or change the service's **Log On**
  account in `services.msc` to a user that can; that account also needs write access to
  `C:\Program Files\Reelix` (for updates) and `C:\ProgramData\Reelix`.

### First run

Open <http://localhost:7700> and complete the setup wizard. From the same PC no token is needed.
From another device the wizard asks for a one-time setup token that Reelix prints to its log
(and, in per-user mode, to its console window): look in `<data folder>\logs`.

### Updates

**System → Updates** downloads the new release, replaces
`reelix.exe`, `web\`, `docs\`, `LICENSE.txt` and `README.txt` in the program folder, and
restarts Reelix (in service mode the service exits and the service manager restarts it).
Running a newer setup also upgrades in place: it stops the running copy (per-user: only the
`reelix.exe` started from this install folder; service: `Stop-Service Reelix`, up to 30 s),
replaces the files and starts the service again.

### Uninstalling

**Settings → Apps → Reelix → Uninstall** (or `unins000.exe` in the program folder). It stops
Reelix (service mode: `reelix.exe service uninstall`, and removes the firewall rule), removes
the program folder and shortcuts, then asks whether to delete the data folder. The default is
**No**; silent uninstalls (`/VERYSILENT /SUPPRESSMSGBOXES`) always keep it.

### Unattended installs

```
reelix-<version>-windows-x86_64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER
reelix-<version>-windows-x86_64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /ALLUSERS
```

Add `/TASKS="autostart"` (per-user) or `/TASKS="firewall"` (all users) to pick tasks, and
`/LOG=setup.log` for a log.

### Building the installer

The release workflow builds it from the Windows zip with Inno Setup 6:

```
iscc /DAppVersion=0.7.0 /DSourceDir=C:\path\to\reelix-0.7.0-windows-x86_64 /Odist packaging\windows\reelix.iss
```

## Linux (systemd)

```bash
curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh
```

`install.sh` is also attached to each release (and listed in `SHA256SUMS`) if you'd rather
download and read it first. It needs root, systemd, `curl` or `wget`, `tar` and `sha256sum`,
and runs on x86_64 and aarch64 with glibc 2.17 or newer.

What it does:

1. Picks the newest release (or `--version`), downloads the tarball and `SHA256SUMS` over
   HTTPS and refuses to continue if the checksum doesn't match.
2. Creates the system user and group `reelix` (no login shell, home `/var/lib/reelix`).
3. Stops `reelix.service` if it is running, replaces the program files in `/opt/reelix`, writes
   `install.json`, and makes `reelix` the owner of `/opt/reelix` (so self-update can replace the
   binary) and of `/var/lib/reelix` (mode 750).
4. Writes `/etc/systemd/system/reelix.service`, enables and starts it, waits up to 30 s for
   `reelix healthcheck` to pass, and prints the URL and next steps.

Options (after `sh -s --` when piped, or as `REELIX_*` environment variables):

| Option | Default | |
|---|---|---|
| `--version X` | newest release | release to install |
| `--port N` | 7700 (or the existing unit's port) | HTTP port |
| `--prefix DIR` | `/opt/reelix` | program folder |
| `--data-dir DIR` | `/var/lib/reelix` | settings, database, backups, logs |
| `--archive-dir DIR` | | install `reelix-<v>-linux-<arch>.tar.gz` and `SHA256SUMS` from a local folder (needs `--version`) |
| `--uninstall` | | stop and remove the service and the program folder; keep the data |
| `--purge` | | with `--uninstall`: also delete the data folder and the `reelix` user |

Running it again is an upgrade; the port, prefix and data folder of the existing unit are kept
unless you pass them again.

### Media and download folders

The unit uses `ProtectSystem=full`: `/usr`, `/boot` and `/etc` are read-only to Reelix, the rest
of the filesystem is as usual, so folders under `/mnt`, `/srv`, `/data` etc. work. `/home` is
read-only (`ProtectHome=read-only`). To let Reelix manage media there, add a drop-in:

```bash
sudo systemctl edit reelix
```

```ini
[Service]
ReadWritePaths=/home/me/Media /home/me/Downloads
```

Drop-ins survive upgrades; `install.sh` rewrites `reelix.service` itself on every run. In all
cases the `reelix` user needs read/write permission on the folders, for example by adding it
to the group that owns them (`sudo usermod -aG media reelix`) and restarting the service.

### First run, logs, service control

```bash
journalctl -u reelix -f            # log (the setup token is printed here)
sudo systemctl restart reelix
systemctl status reelix
```

From the same machine the setup wizard needs no token; from another machine it asks for the
one-time setup token from the log.

### Updates

**System → Updates** replaces the files in `/opt/reelix` and re-executes the binary in place,
so systemd keeps tracking the same process. `Restart=on-failure` restarts Reelix if it ever
crashes. Running `install.sh` again also upgrades (to the newest release, or `--version`).

### Uninstalling

```bash
curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh -s -- --uninstall
# also delete /var/lib/reelix and the reelix user:
curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh -s -- --uninstall --purge
```

## Portable archives

Unpack and run `start-reelix.cmd` / `start-reelix.sh`; data goes in `data` next to the binary.
To upgrade by hand, stop Reelix, back up `data`, replace everything except `data` (and
`install.json`) with the new version, and start it again.
