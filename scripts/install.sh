#!/bin/sh
set -eu

DEFAULT_VERSION='v0.1.0-beta.2'
REPOSITORY='TamedTornado/git-lfs-delta'

version=$DEFAULT_VERSION
install_dir=${HOME:?HOME must be set}/.local/bin
register=true
require_provenance=false
asset_base_url=

usage() {
    cat <<'EOF'
Install the Git LFS Delta client.

Usage: install.sh [OPTIONS]
  --version VERSION          Release tag to install (default: v0.1.0-beta.2)
  --prefix DIRECTORY         Binary destination (default: $HOME/.local/bin)
  --no-register              Do not register the transfer agent in global Git config
  --verify-provenance        Require GitHub CLI provenance verification
  --asset-base-url URL       Override the release asset directory (for mirrors/testing)
  -h, --help                 Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { echo 'install.sh: --version requires a value' >&2; exit 2; }
            version=$2
            shift 2
            ;;
        --prefix)
            [ "$#" -ge 2 ] || { echo 'install.sh: --prefix requires a value' >&2; exit 2; }
            install_dir=$2
            shift 2
            ;;
        --no-register)
            register=false
            shift
            ;;
        --verify-provenance)
            require_provenance=true
            shift
            ;;
        --asset-base-url)
            [ "$#" -ge 2 ] || { echo 'install.sh: --asset-base-url requires a value' >&2; exit 2; }
            asset_base_url=${2%/}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "install.sh: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$version" in
    v*) ;;
    *) version="v$version" ;;
esac

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-musl ;;
    *) echo "install.sh: unsupported client platform: $os $arch" >&2; exit 1 ;;
esac

if [ "$register" = true ]; then
    command -v git >/dev/null 2>&1 || { echo 'install.sh: Git is required' >&2; exit 1; }
    command -v git-lfs >/dev/null 2>&1 || { echo 'install.sh: Git LFS is required' >&2; exit 1; }
fi

archive="git-lfs-delta-${version}-${target}.tar.gz"
if [ -z "$asset_base_url" ]; then
    asset_base_url="https://github.com/$REPOSITORY/releases/download/$version"
    github_release=true
else
    github_release=false
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/git-lfs-delta-install.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

download() {
    source_url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location "$source_url" --output "$destination"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --output-document="$destination" "$source_url"
    else
        echo 'install.sh: curl or wget is required' >&2
        exit 1
    fi
}

download "$asset_base_url/$archive" "$temporary/$archive"
download "$asset_base_url/$archive.sha256" "$temporary/$archive.sha256"
expected=$(awk 'NR == 1 { print $1 }' "$temporary/$archive.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$archive" | awk '{ print $1 }')
else
    actual=$(shasum -a 256 "$temporary/$archive" | awk '{ print $1 }')
fi
[ -n "$expected" ] && [ "$actual" = "$expected" ] || {
    echo "install.sh: SHA-256 verification failed for $archive" >&2
    exit 1
}

if [ "$github_release" = true ] && command -v gh >/dev/null 2>&1; then
    gh attestation verify "$temporary/$archive" --repo "$REPOSITORY" >/dev/null
elif [ "$require_provenance" = true ]; then
    echo 'install.sh: --verify-provenance requires GitHub CLI and official GitHub assets' >&2
    exit 1
fi

mkdir "$temporary/extract"
tar -xzf "$temporary/$archive" -C "$temporary/extract"
[ -f "$temporary/extract/git-lfs-delta" ] || {
    echo 'install.sh: archive does not contain git-lfs-delta' >&2
    exit 1
}

mkdir -p "$install_dir"
staged="$install_dir/.git-lfs-delta.new.$$"
cp "$temporary/extract/git-lfs-delta" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$install_dir/git-lfs-delta"

if [ "$register" = true ]; then
    "$install_dir/git-lfs-delta" install --scope global
fi

case ":${PATH:-}:" in
    *:"$install_dir":*) ;;
    *) echo "warning: add $install_dir to PATH to run git-lfs-delta directly" >&2 ;;
esac

echo "installed git-lfs-delta $version to $install_dir/git-lfs-delta"

