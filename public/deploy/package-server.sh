#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
if [[ $# != 1 ]]; then printf 'Usage: %s OUTPUT_DIRECTORY\n' "$0" >&2; exit 2; fi
mkdir -p "$1"
output=$(cd "$1" && pwd)
archive="$output/Colposcopy-Server-Linux-x64.tar.gz"
if [[ -e "$archive" ]]; then printf 'Server package already exists\n' >&2; exit 2; fi
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/public/deploy" "$stage/public/docs"
for file in .gitignore compose.yaml configure.py init-db.sh migration_sql.py; do
    cp "$root/public/deploy/$file" "$stage/public/deploy/"
done
cp -r "$root/migrations" "$stage/"
cp "$root/public/docs/ADMIN.md" "$stage/public/docs/"
if [[ -f "$root/LICENSE" ]]; then cp "$root/LICENSE" "$stage/"; else cp "$root/public/LICENSE" "$stage/LICENSE"; fi
if [[ -d "$output/ServerNotices" ]]; then cp -r "$output/ServerNotices" "$stage/licenses"; fi
if [[ $(docker image inspect --format '{{.Os}}/{{.Architecture}}' colposcopy-public:local) != linux/amd64 ]]; then
    printf 'The server image must be built for Linux x86-64\n' >&2
    exit 2
fi
docker image save colposcopy-public:local --output "$stage/colposcopy-server-image.tar"
tar -C "$stage" -czf "$archive" .
printf '%s\n' "$archive"
