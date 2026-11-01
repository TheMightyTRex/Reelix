#!/bin/sh
# Starts Reelix from this folder; settings and the database go in ./data.
cd "$(dirname "$0")" || exit 1
echo "Reelix is starting on http://localhost:7700  (press Ctrl+C to stop it)"
exec ./reelix --data-dir "$PWD/data" "$@"
