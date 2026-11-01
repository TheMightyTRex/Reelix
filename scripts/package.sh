#!/usr/bin/env bash
# Packages a built Reelix binary and web UI into a release archive.
#
#   scripts/package.sh <linux|windows> <arch> <version> <binary> <web-dist> <out-dir>
#
# Produces <out-dir>/reelix-<version>-<os>-<arch>.tar.gz (Linux) or .zip (Windows)
# plus a matching .sha256 file. Packaging files come from packaging/<os>/.
set -euo pipefail

if [ $# -ne 6 ]; then
    sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

os=$1 arch=$2 version=$3 binary=$4 web=$5 out=$6
root=$(cd "$(dirname "$0")/.." && pwd)
name="reelix-$version-$os-$arch"

case "$os" in
    linux) exe=reelix ;;
    windows) exe=reelix.exe ;;
    *) echo "unknown os: $os" >&2; exit 2 ;;
esac
[ -f "$binary" ] || { echo "binary not found: $binary" >&2; exit 1; }
[ -f "$web/index.html" ] || { echo "web UI not built: $web/index.html missing" >&2; exit 1; }

mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir="$stage/$name"
mkdir -p "$dir/web"

cp "$binary" "$dir/$exe"
# Skip dotfiles such as trunk's .trunk-lock.
(cd "$web" && find . -mindepth 1 -maxdepth 1 ! -name '.*' -exec cp -R {} "$dir/web/" \;)
cp "$root/LICENSE" "$dir/LICENSE.txt"

# README: fill in version and architecture, and keep the title underline the same length.
sed -e "s/@VERSION@/$version/g" -e "s/@ARCH@/$arch/g" "$root/packaging/$os/README.txt" |
    awk 'NR == 1 { title = length($0) } NR == 2 && /^=+$/ { $0 = sprintf("%*s", title, ""); gsub(/ /, "=") } { print }' \
        > "$dir/README.txt"

if [ "$os" = linux ]; then
    cp "$root/packaging/linux/start-reelix.sh" "$dir/"
    chmod 755 "$dir/$exe" "$dir/start-reelix.sh"
    archive="$name.tar.gz"
    tar -C "$stage" --owner=0 --group=0 --sort=name -czf "$out/$archive" "$name"
else
    cp "$root/packaging/windows/start-reelix.cmd" "$dir/"
    # Notepad and cmd.exe expect CRLF line endings.
    for f in README.txt LICENSE.txt start-reelix.cmd; do
        sed -i 's/\r$//; s/$/\r/' "$dir/$f"
    done
    archive="$name.zip"
    rm -f "$out/$archive"
    if command -v zip > /dev/null; then
        (cd "$stage" && zip -q -r -X "$out/$archive" "$name")
    else
        python3 -I - "$stage" "$name" "$out/$archive" << 'PY'
import os, sys, zipfile
base, name, dest = sys.argv[1:]
with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for root, dirs, files in os.walk(os.path.join(base, name)):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(root, f)
            z.write(p, os.path.relpath(p, base))
PY
    fi
fi

(cd "$out" && sha256sum "$archive" > "$archive.sha256")
echo "$out/$archive"
