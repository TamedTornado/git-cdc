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
  "$root/target/debug/git-cdc-server" >"$work/server.log" 2>&1 &
  server_pid=$!
  for _ in {1..30}; do
    if curl --fail --silent http://127.0.0.1:58080/readyz >/dev/null; then
      curl --fail --silent http://127.0.0.1:58080/healthz >/dev/null
      return
    fi
    sleep 1
  done
  curl --fail --silent http://127.0.0.1:58080/readyz >/dev/null
}

metric() {
  curl --fail --silent http://127.0.0.1:58080/metrics |
    awk -v name="$1" '$1 == name { print $2 }'
}

docker compose -f "$root/docker-compose.test.yml" up -d --wait postgres forgejo
docker compose -f "$root/docker-compose.test.yml" exec -T forgejo forgejo admin user create \
  --username alice \
  --password integration-password \
  --email alice@git-cdc.invalid \
  --admin \
  --must-change-password=false
token="$(docker compose -f "$root/docker-compose.test.yml" exec -T forgejo forgejo admin user generate-access-token \
  --username alice \
  --token-name git-cdc-integration \
  --scopes write:repository,write:user \
  --raw | tr -d '\r')"

curl --fail --silent --show-error \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: application/json" \
  -d '{"name":"assets","private":true,"default_branch":"master"}' \
  http://127.0.0.1:53000/api/v1/user/repos >/dev/null

export GIT_CDC_DATABASE_URL=postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc
export GIT_CDC_BASE_URL=http://127.0.0.1:58080/
export GIT_CDC_STORAGE_URL="file://$work/storage"
export GIT_CDC_AUTH_MODE=forgejo
export GIT_CDC_FORGEJO_URL=http://127.0.0.1:53000/
export GIT_CDC_BIND=127.0.0.1:58080
"$root/target/debug/git-cdc-admin" repository-add alice assets >/dev/null
start_server

credentials="$work/credentials"
printf 'http://alice:%s@127.0.0.1:53000\nhttp://alice:%s@127.0.0.1:58080\n' "$token" "$token" >"$credentials"
export GIT_CONFIG_GLOBAL="$work/gitconfig"
git config --global credential.helper ""
git config --global --add credential.helper "store --file=$credentials"
credential="$(printf 'protocol=http\nhost=127.0.0.1:53000\n\n' | git credential fill)"
grep -q '^username=alice$' <<<"$credential"
grep -q "^password=$token$" <<<"$credential"

source_repo="$work/source"
git init -b master "$source_repo"
git -C "$source_repo" config user.name "Git CDC Integration"
git -C "$source_repo" config user.email integration@git-cdc.invalid
git -C "$source_repo" remote add origin http://127.0.0.1:53000/alice/assets.git
git -C "$source_repo" lfs install --local
(cd "$source_repo" && "$root/target/debug/git-cdc" install --scope local)
(cd "$source_repo" && "$root/target/debug/git-cdc" configure --scope local --url http://127.0.0.1:58080/alice/assets/info/lfs)
(cd "$source_repo" && "$root/target/debug/git-cdc" doctor)
(cd "$source_repo" && "$root/target/debug/git-cdc" status) | grep -Fq 'lfs.customtransfer.cdc.args=transfer'
(cd "$source_repo" && "$root/target/debug/git-cdc" status) | grep -Fq 'lfs.url=http://127.0.0.1:58080/alice/assets/info/lfs'
git -C "$source_repo" lfs track '*.bin'
asset_bytes="${GIT_CDC_ACCEPTANCE_ASSET_BYTES:-268435456}"
head -c "$asset_bytes" /dev/urandom >"$source_repo/asset.bin"
git -C "$source_repo" add .gitattributes asset.bin
git -C "$source_repo" commit -m 'Forgejo CDC fixture'
git -C "$source_repo" push --set-upstream origin master

logical_before="$(metric git_cdc_logical_upload_bytes_total)"
physical_before="$(metric git_cdc_received_chunk_bytes_total)"
printf 'localized Git-CDC edit' | dd of="$source_repo/asset.bin" bs=1 seek=$((asset_bytes / 2)) conv=notrunc status=none
git -C "$source_repo" add asset.bin
git -C "$source_repo" commit -m 'Localized asset edit'
git -C "$source_repo" push
logical_after="$(metric git_cdc_logical_upload_bytes_total)"
physical_after="$(metric git_cdc_received_chunk_bytes_total)"
logical_delta=$((logical_after - logical_before))
physical_delta=$((physical_after - physical_before))
test "$logical_delta" -eq "$asset_bytes"
test "$physical_delta" -gt 0
test "$physical_delta" -lt "$logical_delta"

# Completed metadata and chunks must survive an ordinary service restart.
stop_server
start_server

clone="$work/clone"
GIT_LFS_SKIP_SMUDGE=1 git clone http://127.0.0.1:53000/alice/assets.git "$clone"
git -C "$clone" lfs install --local
(cd "$clone" && "$root/target/debug/git-cdc" install --scope local)
(cd "$clone" && "$root/target/debug/git-cdc" configure --scope local --url http://127.0.0.1:58080/alice/assets/info/lfs)
git -C "$clone" lfs pull
git -C "$clone" lfs fsck --objects
cmp "$source_repo/asset.bin" "$clone/asset.bin"
(cd "$clone" && "$root/target/debug/git-cdc" uninstall --scope local)
mv "$clone/.git/lfs/objects" "$clone/.git/lfs/objects-cdc"
git -C "$clone" lfs fetch origin master
git -C "$clone" lfs fsck --objects
rm "$clone/asset.bin"
git -C "$clone" checkout -- asset.bin
cmp "$source_repo/asset.bin" "$clone/asset.bin"
