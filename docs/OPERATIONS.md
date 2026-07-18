# Git LFS Delta Operations

## Authentication modes

The server selects exactly one authentication mode with
`GIT_LFS_DELTA_AUTH_MODE`. There is no fallback between modes.

- `development`: requires `GIT_LFS_DELTA_DEV_TOKEN`; local testing only.
- `forgejo`: requires `GIT_LFS_DELTA_FORGEJO_URL`. Successful repository decisions
  are cached for 30 seconds; raw credentials, denials, and failures are not.
- `oidc` (preview in beta.2): requires `GIT_LFS_DELTA_OIDC_ISSUER` and
  `GIT_LFS_DELTA_OIDC_AUDIENCE`.
  Startup performs discovery and loads JWKS or fails. Tokens require a valid
  signature, issuer, audience, expiry, and a `repository_grants` entry.

The public service URL, database, storage URL, and bind address use
`GIT_LFS_DELTA_BASE_URL`, `GIT_LFS_DELTA_DATABASE_URL`, `GIT_LFS_DELTA_STORAGE_URL`, and
`GIT_LFS_DELTA_BIND` respectively.

## Provisioning

```console
git-lfs-delta-admin repository-add OWNER REPOSITORY
git-lfs-delta-admin grant REPOSITORY_UUID SUBJECT --write
```

The first command prints the stable repository UUID. Forgejo mode obtains
permissions from Forgejo and does not require grants. OIDC grants are
repository-scoped; `--admin` implies read/write and permits forced unlock.

`repository-add` is idempotent: rerunning it for the same owner/name prints
the existing stable UUID.

## Forgejo reference integration

Forgejo 15 LTS is the beta reference forge. Start Git LFS Delta in `forgejo` mode
with the externally reachable Forgejo root URL:

```console
export GIT_LFS_DELTA_AUTH_MODE=forgejo
export GIT_LFS_DELTA_FORGEJO_URL=https://forge.example/
```

Provision the matching owner/repository in Git LFS Delta, then route
`/OWNER/REPOSITORY/info/lfs` to Git LFS Delta at the reverse proxy. A separate LFS
host is also supported; configure it in the Git checkout with:

```console
git-lfs-delta install --scope local
git-lfs-delta configure --scope local --url https://lfs.example/OWNER/REPOSITORY/info/lfs
```

Forgejo personal access tokens used for Git LFS Delta need `read:user` plus
`read:repository` for downloads or `write:repository` for uploads and locks.
Repository-specific tokens are supported. Store credentials through Git's
credential machinery; do not put tokens in the LFS URL. Git LFS Delta forwards the
caller's authorization to Forgejo for both the current-user and repository
permission checks through a bounded cache, so revocation and permission changes
take effect within 30 seconds while large transfers avoid overwhelming Forgejo.

The integration suite creates a real private Forgejo repository, performs a
Git push whose LFS object uses CDC, clones and verifies the bytes through CDC,
uninstalls Git LFS Delta, then cold-fetches and verifies the same standard pointer
with stock Git LFS.

## Reachability and garbage collection

Reconciliation uses an ordinary read-only Git URL and Git credential helpers:

```console
git-lfs-delta-admin reconcile REPOSITORY_UUID GIT_URL
git-lfs-delta-admin gc-dry-run REPOSITORY_UUID
git-lfs-delta-admin gc-stage REPOSITORY_UUID --grace-seconds 604800
git-lfs-delta-admin gc-collect REPOSITORY_UUID
git-lfs-delta-admin uploads-reclaim REPOSITORY_UUID --grace-seconds 86400
```

The reconciler creates a fresh mirror, fingerprints all refs, validates every
reachable standard LFS pointer, and atomically marks the epoch complete. Any
Git, ref, blob, pointer, or database failure publishes no complete epoch.

An object is eligible only when absent from the two newest complete epochs.
It is then staged behind the configured grace period. Metadata deletion and
chunk-provider deletion are separated by a durable cleanup queue, making a
crash or provider outage safe to retry. Without two complete epochs, completed
objects are retained indefinitely. Always inspect `gc-dry-run` before staging.

Expired incomplete uploads are marked separately from completed objects.
`uploads-reclaim` removes only sessions beyond its quarantine grace period and
only chunks with no completed-object reference and no active upload reference.
Provider failures remain in the same durable cleanup queue for retry.

## Health and metrics

- `/healthz` is process liveness.
- `/readyz` checks PostgreSQL plus an object-store write/read/delete probe.
- `/metrics` exposes logical/physical byte and Forgejo authorization counters.
  Restrict it to the internal monitoring network at the reverse proxy.

## Production sizing and shutdown

Set `GIT_LFS_DELTA_STAGING_DIR` to a dedicated filesystem. At the defaults of 100 GiB
per object and two simultaneous basic transfers, provision at least 240 GiB.
`GIT_LFS_DELTA_MAX_BASIC_TRANSFERS` and `GIT_LFS_DELTA_DATABASE_MAX_CONNECTIONS` default to
2 and 20. Remote development-auth binds are rejected unless
`GIT_LFS_DELTA_ALLOW_REMOTE_DEVELOPMENT_AUTH` is explicitly present.

The server handles SIGINT and SIGTERM. The native client uses two chunk workers
by default; configure 1-8 with `GIT_LFS_DELTA_CHUNK_CONCURRENCY`, and tune its
connection/request deadlines with `GIT_LFS_DELTA_HTTP_CONNECT_TIMEOUT_SECONDS` and
`GIT_LFS_DELTA_HTTP_REQUEST_TIMEOUT_SECONDS`.

## Backup and restore

For a consistent backup, temporarily stop write traffic, wait for active
requests to finish, snapshot/copy the configured object-store prefix, and run
`pg_dump` against PostgreSQL. Resume writes only after both finish. Object
storage may safely contain extra immutable chunks; missing chunks are not safe.

Restore into an empty database and empty object-store prefix, restore object
bytes first, then PostgreSQL, run migrations by starting the server, and verify
`/readyz`. Exercise a stock basic download and a CDC download before reopening
write traffic. Never restore metadata without the corresponding object-store
snapshot.
