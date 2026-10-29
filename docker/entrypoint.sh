#!/bin/sh
# Drops privileges to PUID:PGID (linuxserver.io convention) and applies UMASK before starting Reelix.
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
UMASK="${UMASK:-022}"
CONFIG_DIR="${REELIX_DATA_DIR:-/config}"

umask "$UMASK"

if [ "$(id -u)" = "0" ]; then
    mkdir -p "$CONFIG_DIR"
    # Only walk the tree when ownership differs, so restarts stay fast on big config dirs.
    if [ "$(stat -c '%u:%g' "$CONFIG_DIR")" != "$PUID:$PGID" ]; then
        chown -R "$PUID:$PGID" "$CONFIG_DIR"
    fi
    exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups --inh-caps=-all \
        /app/reelix "$@"
fi

# Already running as a non-root user (e.g. `docker run --user`).
exec /app/reelix "$@"
