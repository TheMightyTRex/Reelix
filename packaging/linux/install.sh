#!/bin/sh
# Installs (or upgrades) Reelix as a systemd service.
#
#   curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh
#   curl -fsSL .../install.sh | sudo sh -s -- --version 0.6.0 --port 8080
#
# Options (or the matching environment variable):
#   --version X     release to install (REELIX_VERSION; default: the newest release)
#   --port N        HTTP port (REELIX_PORT; default: 7700, or the port of an existing install)
#   --prefix DIR    program folder (REELIX_PREFIX; default: /opt/reelix)
#   --data-dir DIR  settings, database and logs (REELIX_DATA_DIR; default: /var/lib/reelix)
#   --archive-dir D use reelix-<version>-linux-<arch>.tar.gz and SHA256SUMS from folder D instead
#                   of downloading them (offline installs, CI); needs --version
#   --uninstall     stop and remove the service and the program folder; keeps the data
#   --purge         with --uninstall: also delete the data folder and the reelix user
#   -h, --help      show this help
#
# Running it again upgrades in place; the port, prefix and data dir of the existing unit are
# kept unless given again. The program folder is owned by the "reelix" user so Reelix can
# update itself from System > Updates. See docs/installers.md.
set -eu

# Everything runs inside main, called on the last line, so a download cut short by `curl | sh`
# runs nothing. (The body isn't indented to keep the unit-file heredoc readable.)
main() {

REPO=demonzoned/Reelix
UNIT=reelix
UNIT_FILE=/etc/systemd/system/$UNIT.service
SVC_USER=reelix

version=${REELIX_VERSION:-}
port=${REELIX_PORT:-}
prefix=${REELIX_PREFIX:-}
data_dir=${REELIX_DATA_DIR:-}
archive_dir=
action=install
purge=no
tmp=

say() { printf '%s\n' "$*"; }
warn() { printf 'reelix-install: %s\n' "$*" >&2; }
die() {
    warn "error: $*"
    exit 1
}
usage() {
    cat << 'EOF'
Usage: install.sh [--version X] [--port N] [--prefix DIR] [--data-dir DIR]
       install.sh --uninstall [--purge]

Installs or upgrades Reelix as the systemd service "reelix" (run as root).
  --version X     release to install (default: the newest release)
  --port N        HTTP port (default: 7700, or the port of an existing install)
  --prefix DIR    program folder (default: /opt/reelix)
  --data-dir DIR  settings, database and logs (default: /var/lib/reelix)
  --archive-dir D install the tarball and SHA256SUMS from folder D (needs --version)
  --uninstall     remove the service and the program folder, keep the data
  --purge         with --uninstall: also delete the data and the reelix user
Environment: REELIX_VERSION, REELIX_PORT, REELIX_PREFIX, REELIX_DATA_DIR.
EOF
}
cleanup() { [ -z "$tmp" ] || rm -rf "$tmp"; }
trap cleanup EXIT
trap 'exit 130' INT TERM

need_arg() { [ "$2" -ge 2 ] || die "$1 needs a value"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) need_arg "$1" $#; version=$2; shift 2 ;;
        --version=*) version=${1#*=}; shift ;;
        --port) need_arg "$1" $#; port=$2; shift 2 ;;
        --port=*) port=${1#*=}; shift ;;
        --prefix) need_arg "$1" $#; prefix=$2; shift 2 ;;
        --prefix=*) prefix=${1#*=}; shift ;;
        --data-dir) need_arg "$1" $#; data_dir=$2; shift 2 ;;
        --data-dir=*) data_dir=${1#*=}; shift ;;
        --archive-dir) need_arg "$1" $#; archive_dir=$2; shift 2 ;;
        --archive-dir=*) archive_dir=${1#*=}; shift ;;
        --uninstall) action=uninstall; shift ;;
        --purge) purge=yes; shift ;;
        -h | --help) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

have() { command -v "$1" > /dev/null 2>&1; }

# Value of an ExecStart flag in the existing unit, e.g. existing_flag --port.
existing_flag() {
    [ -f "$UNIT_FILE" ] || return 0
    sed -n 's/^ExecStart=.*[[:space:]]'"$1"'[[:space:]]\{1,\}\([^[:space:]]*\).*/\1/p' "$UNIT_FILE" | head -n 1
}

existing_prefix() {
    [ -f "$UNIT_FILE" ] || return 0
    sed -n 's|^ExecStart=\([^[:space:]]*\)/reelix\([[:space:]].*\)\{0,1\}$|\1|p' "$UNIT_FILE" | head -n 1
}

check_path() {
    case "$2" in
        /*) ;;
        *) die "$1 must be an absolute path: $2" ;;
    esac
    case "$2" in
        *[[:space:]]* | *[\"\'\\\$\`%]*) die "$1 must not contain spaces or quotes: $2" ;;
    esac
    case "$2" in
        / | /bin | /boot | /dev | /etc | /home | /lib | /lib64 | /opt | /proc | /root | /run | /sbin | /srv | /sys | /tmp | /usr | /var | /var/lib)
            die "refusing to use $2 as $1" ;;
    esac
}

stop_service() {
    if systemctl is-active --quiet "$UNIT" 2> /dev/null; then
        say "Stopping $UNIT..."
        systemctl stop "$UNIT"
    fi
}

# ---- preflight ----------------------------------------------------------------------------

[ "$(id -u)" -eq 0 ] || die "run as root, e.g. curl -fsSL <url> | sudo sh"
if ! have systemctl || [ ! -d /run/systemd/system ]; then
    die "systemd is required (for other setups use the release tarball or Docker)"
fi

[ -n "$prefix" ] || prefix=$(existing_prefix)
[ -n "$prefix" ] || prefix=/opt/reelix
[ -n "$data_dir" ] || data_dir=$(existing_flag --data-dir)
[ -n "$data_dir" ] || data_dir=/var/lib/reelix
prefix=${prefix%/}
data_dir=${data_dir%/}
check_path --prefix "$prefix"
check_path --data-dir "$data_dir"
case "$data_dir/" in
    "$prefix/"*) die "--data-dir must not be inside --prefix (the program folder is replaced on upgrade)" ;;
esac

# ---- uninstall ----------------------------------------------------------------------------

if [ "$action" = uninstall ]; then
    if [ -f "$UNIT_FILE" ]; then
        systemctl disable --now "$UNIT" 2> /dev/null || true
        rm -f "$UNIT_FILE"
        systemctl daemon-reload
        say "Removed $UNIT_FILE"
    fi
    if [ -d "$prefix" ]; then
        [ -f "$prefix/reelix" ] || die "$prefix doesn't look like a Reelix install; remove it yourself"
        rm -rf "$prefix"
        say "Removed $prefix"
    fi
    if [ "$purge" = yes ]; then
        rm -rf "$data_dir"
        say "Removed $data_dir"
        if id "$SVC_USER" > /dev/null 2>&1; then
            if have userdel; then userdel "$SVC_USER" 2> /dev/null || true; else deluser "$SVC_USER" 2> /dev/null || true; fi
            say "Removed user $SVC_USER"
        fi
    else
        say "Kept the data in $data_dir (settings, database, backups, logs)."
        say "Run again with --uninstall --purge to delete it and the $SVC_USER user."
    fi
    exit 0
fi

# ---- install / upgrade --------------------------------------------------------------------

[ -n "$port" ] || port=$(existing_flag --port)
if [ -n "$port" ]; then
    case "$port" in
        *[!0-9]*) die "--port must be a number: $port" ;;
    esac
    if [ "$port" -lt 1 ] || [ "$port" -gt 65535 ]; then
        die "--port must be between 1 and 65535: $port"
    fi
fi

for tool in tar sha256sum mktemp awk; do
    have "$tool" || die "$tool is required"
done
if [ -n "$archive_dir" ]; then
    [ -n "$version" ] || die "--archive-dir needs --version"
    fetch() { die "no download in --archive-dir mode: $1"; }
elif have curl; then
    fetch() { curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"; }
elif have wget; then
    fetch() { wget -q --https-only -O "$2" "$1"; }
else
    die "curl or wget is required"
fi

case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported architecture $(uname -m): Reelix builds exist for x86_64 and aarch64 (or use Docker)" ;;
esac

if have ldd; then
    libc=$(ldd --version 2>&1 | head -n 1 || true)
    case "$libc" in
        *musl*) die "musl libc (e.g. Alpine) isn't supported; use the Docker image" ;;
    esac
    glibc=$(printf '%s\n' "$libc" | sed -n 's/.*[^0-9.]\([0-9][0-9]*\.[0-9][0-9]*\)[^0-9.]*$/\1/p')
    if [ -n "$glibc" ]; then
        major=${glibc%%.*} minor=${glibc#*.}
        if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 17 ]; }; then
            die "glibc $glibc found, Reelix needs 2.17 or newer"
        fi
    fi
fi

tmp=$(mktemp -d)

if [ -z "$version" ]; then
    # GitHub hides pre-releases (all 0.x versions) from /releases/latest, so fall back to the
    # newest entry of the release list.
    if ! fetch "https://api.github.com/repos/$REPO/releases/latest" "$tmp/release.json" 2> /dev/null; then
        fetch "https://api.github.com/repos/$REPO/releases?per_page=1" "$tmp/release.json" ||
            die "couldn't look up the newest release (pass --version)"
    fi
    version=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' "$tmp/release.json" | head -n 1)
    [ -n "$version" ] || die "couldn't find a release (pass --version)"
fi
version=${version#v}
case "$version" in
    *[!0-9A-Za-z.+-]* | '') die "invalid version: $version" ;;
esac

name=reelix-$version-linux-$arch
tarball=$name.tar.gz
base=https://github.com/$REPO/releases/download/v$version

if [ -n "$archive_dir" ]; then
    say "Installing Reelix $version ($arch) from $archive_dir..."
    cp "$archive_dir/$tarball" "$tmp/$tarball" || die "$archive_dir/$tarball not found"
    cp "$archive_dir/SHA256SUMS" "$tmp/SHA256SUMS" || die "$archive_dir/SHA256SUMS not found"
else
    say "Downloading Reelix $version ($arch)..."
    fetch "$base/$tarball" "$tmp/$tarball" || die "download failed: $base/$tarball"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"
fi

expected=$(awk -v f="$tarball" '$2 == f || $2 == "*" f { print $1; exit }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$tarball is not listed in SHA256SUMS"
actual=$(sha256sum "$tmp/$tarball" | awk '{ print $1 }')
[ "$expected" = "$actual" ] || die "checksum mismatch for $tarball (expected $expected, got $actual)"
say "Checksum OK"

mkdir "$tmp/x"
tar -xzf "$tmp/$tarball" -C "$tmp/x"
src=$tmp/x/$name
if [ ! -x "$src/reelix" ] || [ ! -f "$src/web/index.html" ]; then
    die "unexpected archive layout in $tarball"
fi

# System user whose home is the data dir; it owns the program folder so self-update works.
if ! id "$SVC_USER" > /dev/null 2>&1; then
    nologin=/usr/sbin/nologin
    [ -x "$nologin" ] || nologin=/sbin/nologin
    [ -x "$nologin" ] || nologin=/bin/false
    if have useradd; then
        if getent group "$SVC_USER" > /dev/null 2>&1; then
            useradd --system --gid "$SVC_USER" --home-dir "$data_dir" --no-create-home --shell "$nologin" "$SVC_USER"
        else
            useradd --system --user-group --home-dir "$data_dir" --no-create-home --shell "$nologin" "$SVC_USER"
        fi
    elif have adduser; then
        # BusyBox / Debian adduser.
        addgroup --system "$SVC_USER" 2> /dev/null || addgroup -S "$SVC_USER" 2> /dev/null || true
        adduser --system --ingroup "$SVC_USER" --home "$data_dir" --no-create-home --shell "$nologin" "$SVC_USER" 2> /dev/null ||
            adduser -S -G "$SVC_USER" -h "$data_dir" -H -s "$nologin" "$SVC_USER"
    else
        die "useradd or adduser is required to create the $SVC_USER user"
    fi
    say "Created system user $SVC_USER"
fi

upgrade=no
[ -f "$prefix/reelix" ] && upgrade=yes
stop_service

# Replace the program files; install.json and anything else in the folder stay.
mkdir -p "$prefix"
for item in reelix web docs LICENSE.txt THIRD_PARTY_NOTICES.md README.txt start-reelix.sh; do
    rm -rf "${prefix:?}/$item"
    if [ -e "$src/$item" ]; then
        cp -R "$src/$item" "$prefix/$item"
    fi
done
printf '{"kind": "linux-systemd"}\n' > "$prefix/install.json"
chmod 755 "$prefix" "$prefix/reelix"
chown -R "$SVC_USER:$SVC_USER" "$prefix"

mkdir -p "$data_dir"
chown -R "$SVC_USER:$SVC_USER" "$data_dir"
chmod 750 "$data_dir"

exec_start="$prefix/reelix --data-dir $data_dir"
[ -z "$port" ] || exec_start="$exec_start --port $port"

cat > "$UNIT_FILE" << EOF
# Written by the Reelix installer (packaging/linux/install.sh); re-running it overwrites this
# file. Put your own changes in a drop-in instead: systemctl edit $UNIT
[Unit]
Description=Reelix media manager
Documentation=https://github.com/$REPO
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SVC_USER
Group=$SVC_USER
WorkingDirectory=$prefix
Environment=REELIX_WEB_DIR=$prefix/web
# Logs also go to $data_dir/logs. For JSON logs: Environment=REELIX_LOG_FORMAT=json
ExecStart=$exec_start
# Self-update re-executes the binary in place (same PID); this covers crashes.
Restart=on-failure
RestartSec=5

# Hardening. ProtectSystem=full (not strict) keeps /usr, /boot and /etc read-only but leaves
# the rest of the filesystem writable, so media and download folders on /mnt, /srv, /data,
# etc. work without extra configuration. $prefix must stay writable for self-update.
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
# /home, /root and /run/user are read-only to Reelix. To manage media there, add a drop-in
# (systemctl edit $UNIT) with e.g.:
#   [Service]
#   ReadWritePaths=/home/me/Media
ProtectHome=read-only
ReadWritePaths=$prefix $data_dir

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$UNIT" > /dev/null 2>&1
systemctl restart "$UNIT"

say "Waiting for Reelix to start..."
ok=no
i=0
while [ "$i" -lt 30 ]; do
    if "$prefix/reelix" --data-dir "$data_dir" ${port:+--port "$port"} healthcheck > /dev/null 2>&1; then
        ok=yes
        break
    fi
    systemctl is-failed --quiet "$UNIT" && break
    i=$((i + 1))
    sleep 1
done

url_port=${port:-7700}
host=$(hostname -f 2> /dev/null || hostname 2> /dev/null || echo localhost)
say ""
if [ "$ok" = yes ]; then
    if [ "$upgrade" = yes ]; then say "Reelix $version is running (upgraded)."; else say "Reelix $version is running."; fi
else
    warn "Reelix didn't answer its health check within 30 seconds. Check the log:"
    warn "  journalctl -u $UNIT -n 50 --no-pager"
fi
cat << EOF

  Web UI:     http://localhost:$url_port  (from another machine: http://$host:$url_port)
  Program:    $prefix  (updates itself from System > Updates)
  Data:       $data_dir
  Logs:       journalctl -u $UNIT -f
  Service:    systemctl status|restart|stop $UNIT

First run: setting up from this machine needs no token. From another machine the setup
wizard asks for a one-time setup token, printed to the log:
  journalctl -u $UNIT | grep -i token

Media and download folders under /home are read-only to the service; to allow one, run
  systemctl edit $UNIT
and add:
  [Service]
  ReadWritePaths=/home/me/Media
Also make sure the $SVC_USER user can read and write your media folders.

Uninstall (keeps $data_dir):
  curl -fsSL https://raw.githubusercontent.com/$REPO/master/packaging/linux/install.sh | sudo sh -s -- --uninstall
EOF
[ "$ok" = yes ]
}

main "$@" < /dev/null
