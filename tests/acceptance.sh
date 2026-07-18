#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
compose=(docker compose -f "$root/docker-compose.test.yml")

cleanup() {
  status=$?
  if [[ $status -ne 0 ]]; then
    "${compose[@]}" logs --no-color || true
  fi
  "${compose[@]}" down --remove-orphans >/dev/null 2>&1 || true
  exit "$status"
}
trap cleanup EXIT

for command in cargo docker git git-lfs curl; do
  command -v "$command" >/dev/null || {
    echo "required acceptance dependency is missing: $command" >&2
    exit 1
  }
done

# Acceptance always starts from disposable infrastructure. This makes local
# reruns exercise provisioning instead of inheriting state from an earlier run.
"${compose[@]}" down --remove-orphans >/dev/null 2>&1 || true
"${compose[@]}" up -d --wait postgres
"${compose[@]}" up -d minio
"${compose[@]}" run --rm minio-init

cargo build --locked -p git-lfs-delta -p git-lfs-delta-server --bins
GIT_LFS_DELTA_DATABASE_URL=postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta \
  "$root/target/debug/git-lfs-delta-admin" migrate
GIT_LFS_DELTA_TEST_DATABASE_URL=postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta \
GIT_LFS_DELTA_TEST_MINIO=1 cargo test --workspace --locked --features git-lfs-delta-server/integration-tests

bash "$root/tests/forgejo_e2e.sh"
bash "$root/tests/backup_restore_e2e.sh"

echo "Git LFS Delta beta acceptance passed"
