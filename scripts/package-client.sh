#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    echo 'usage: package-client.sh TARGET TAG OUTPUT_DIRECTORY' >&2
    exit 2
fi

target=$1
tag=$2
output=$3
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
binary="$root/target/$target/release/git-lfs-delta"
archive="git-lfs-delta-${tag}-${target}.tar.gz"
stage=$(mktemp -d "${TMPDIR:-/tmp}/git-lfs-delta-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM

[ -x "$binary" ] || { echo "missing release binary: $binary" >&2; exit 1; }
mkdir -p "$output"
cp "$binary" "$stage/git-lfs-delta"
chmod 755 "$stage/git-lfs-delta"
strip "$stage/git-lfs-delta"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/CLIENT-README.md" "$stage/"
tar -C "$stage" -czf "$output/$archive" .

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$output" && sha256sum "$archive" >"$archive.sha256")
else
    (cd "$output" && shasum -a 256 "$archive" >"$archive.sha256")
fi

