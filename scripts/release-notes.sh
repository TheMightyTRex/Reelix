#!/usr/bin/env bash
# Prints the release notes for a version: its section of CHANGELOG.md, or, when the
# changelog has none, the commit subjects since the previous tag.
#
#   scripts/release-notes.sh <version>
set -euo pipefail

version=${1:?usage: scripts/release-notes.sh <version>}
root=$(cd "$(dirname "$0")/.." && pwd)

notes=$(awk -v v="$version" '
    /^## \[/ { if (found) exit; found = index($0, "## [" v "]") == 1; next }
    found { print }
' "$root/CHANGELOG.md" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')

if [ -z "$notes" ]; then
    previous=$(git -C "$root" describe --tags --abbrev=0 "v$version^" 2> /dev/null || true)
    range=${previous:+$previous..}v$version
    git -C "$root" rev-parse -q --verify "v$version^{commit}" > /dev/null || range=${previous:+$previous..}HEAD
    notes="### Changes"$'\n'$(git -C "$root" log --no-merges --format='- %s' "$range")
fi

printf '%s\n' "$notes"
