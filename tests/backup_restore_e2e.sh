#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
server_pid=""

cleanup() {
  status=$?
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [[ $status -ne 0 && -f "$work/server.log" ]]; then
    sed -n '1,240p' "$work/server.log"
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

stop_server() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid"
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
  fi
}

start_server() {
  "$root/target/debug/git-lfs-delta-server" >"$work/server.log" 2>&1 &
  server_pid=$!
  for _ in {1..30}; do
    if curl --fail --silent http://127.0.0.1:58081/readyz >/dev/null; then
      return
    fi
    sleep 1
  done
  curl --fail --silent http://127.0.0.1:58081/readyz >/dev/null
}

docker compose -f "$root/docker-compose.test.yml" up -d --wait postgres
docker compose -f "$root/docker-compose.test.yml" exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U git_lfs_delta git_lfs_delta \
  -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'

export GIT_LFS_DELTA_DATABASE_URL=postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta
export GIT_LFS_DELTA_BASE_URL=http://127.0.0.1:58081/
export GIT_LFS_DELTA_STORAGE_URL="file://$work/live-storage"
export GIT_LFS_DELTA_AUTH_MODE=development
export GIT_LFS_DELTA_DEV_TOKEN=backup-secret
export GIT_LFS_DELTA_BIND=127.0.0.1:58081
"$root/target/debug/git-lfs-delta-admin" migrate >/dev/null
"$root/target/debug/git-lfs-delta-admin" repository-add team assets >/dev/null
start_server

repository="$work/repository"
cache="$work/cache"
export XDG_CACHE_HOME="$cache"
export LOCALAPPDATA="$cache"
git init -b master "$repository"
git -C "$repository" config user.name "Git CDC Backup Test"
git -C "$repository" config user.email backup@git-lfs-delta.invalid
git -C "$repository" config http.extraheader 'Authorization: Bearer backup-secret'
git -C "$repository" lfs install --local
(cd "$repository" && "$root/target/debug/git-lfs-delta" install --scope local)
(cd "$repository" && "$root/target/debug/git-lfs-delta" configure --scope local --url http://127.0.0.1:58081/team/assets/info/lfs)
git -C "$repository" remote add origin https://invalid.example/repository.git
git -C "$repository" lfs track '*.bin'
head -c 9437201 /dev/urandom >"$repository/asset.bin"
expected="$(openssl dgst -sha256 "$repository/asset.bin" | sed 's/^.*= //')"
git -C "$repository" add .gitattributes asset.bin
git -C "$repository" commit -m 'backup fixture'
git -C "$repository" lfs push --all origin

stop_server
cp -R "$work/live-storage" "$work/storage-backup"
docker compose -f "$root/docker-compose.test.yml" exec -T postgres \
  pg_dump -U git_lfs_delta --clean --if-exists --no-owner git_lfs_delta >"$work/database.sql"

docker compose -f "$root/docker-compose.test.yml" exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U git_lfs_delta git_lfs_delta \
  -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
mv "$work/live-storage" "$work/destroyed-storage"
cp -R "$work/storage-backup" "$work/live-storage"
docker compose -f "$root/docker-compose.test.yml" exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U git_lfs_delta git_lfs_delta <"$work/database.sql"
start_server

objects="$repository/.git/lfs/objects"
mv "$objects" "$repository/.git/lfs/objects-before-restore"
git -C "$repository" lfs fetch origin master
git -C "$repository" lfs fsck --objects
(cd "$repository" && "$root/target/debug/git-lfs-delta" uninstall --scope local)
mv "$objects" "$repository/.git/lfs/objects-before-stock-restore"
git -C "$repository" lfs fetch origin master
git -C "$repository" lfs fsck --objects
rm "$repository/asset.bin"
git -C "$repository" checkout -- asset.bin
actual="$(openssl dgst -sha256 "$repository/asset.bin" | sed 's/^.*= //')"
test "$actual" = "$expected"
