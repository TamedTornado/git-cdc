#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo 'usage: client_installer.sh ASSET_DIRECTORY TARGET' >&2
    exit 2
fi

assets=$(CDPATH= cd -- "$1" && pwd)
target=$2
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
tag=v0.1.0-beta.2
archive="git-lfs-delta-${tag}-${target}.tar.gz"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/git-lfs-delta-installer-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

export HOME="$temporary/home"
mkdir -p "$HOME" "$temporary/bin"

install_client() {
    asset_directory=$1
    shift
    sh "$root/scripts/install.sh" \
        --asset-base-url "file://$asset_directory" \
        --prefix "$temporary/bin" \
        "$@"
}

binary_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

install_client "$assets"
"$temporary/bin/git-lfs-delta" --version | grep "0.1.0-beta.2"
registered=$(git config --global --get lfs.customtransfer.cdc.path)
test -x "$registered"
"$temporary/bin/git-lfs-delta" doctor

# Reinstalling exercises the atomic replacement path and must preserve a valid client.
install_client "$assets"
before=$(binary_hash "$temporary/bin/git-lfs-delta")

mkdir "$temporary/corrupt"
cp "$assets/$archive" "$assets/$archive.sha256" "$temporary/corrupt/"
printf 'corrupt' >>"$temporary/corrupt/$archive"
if install_client "$temporary/corrupt"; then
    echo 'corrupt archive unexpectedly installed' >&2
    exit 1
fi
after=$(binary_hash "$temporary/bin/git-lfs-delta")
test "$before" = "$after"

if sh "$root/scripts/install.sh" \
    --asset-base-url "file://$assets" \
    --prefix "$temporary/bin" \
    --verify-provenance; then
    echo 'mandatory provenance unexpectedly accepted a mirror' >&2
    exit 1
fi

no_register="$temporary/no-register"
sh "$root/scripts/install.sh" \
    --asset-base-url "file://$assets" \
    --prefix "$no_register" \
    --no-register
test -x "$no_register/git-lfs-delta"

echo "client installer contracts passed for $target"
