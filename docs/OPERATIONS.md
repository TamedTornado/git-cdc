# Git-CDC Operations

## Authentication modes

The server selects exactly one authentication mode with
`GIT_CDC_AUTH_MODE`. There is no fallback between modes.

- `development`: requires `GIT_CDC_DEV_TOKEN`; local testing only.
- `forgejo`: requires `GIT_CDC_FORGEJO_URL`. Every request forwards the
  caller credential to `/api/v1/user` and the specific repository API, so
  permission changes and token revocation apply immediately.
- `oidc`: requires `GIT_CDC_OIDC_ISSUER` and `GIT_CDC_OIDC_AUDIENCE`.
  Startup performs discovery and loads JWKS or fails. Tokens require a valid
  signature, issuer, audience, expiry, and a `repository_grants` entry.

The public service URL, database, storage URL, and bind address use
`GIT_CDC_BASE_URL`, `GIT_CDC_DATABASE_URL`, `GIT_CDC_STORAGE_URL`, and
`GIT_CDC_BIND` respectively.

## Provisioning

```console
git-cdc-admin repository-add OWNER REPOSITORY
git-cdc-admin grant REPOSITORY_UUID SUBJECT --write
```

The first command prints the stable repository UUID. Forgejo mode obtains
permissions from Forgejo and does not require grants. OIDC grants are
repository-scoped; `--admin` implies read/write and permits forced unlock.

`repository-add` is idempotent: rerunning it for the same owner/name prints
the existing stable UUID.

## Forgejo reference integration

Forgejo 15 LTS is the beta reference forge. Start Git-CDC in `forgejo` mode
with the externally reachable Forgejo root URL:

```console
export GIT_CDC_AUTH_MODE=forgejo
export GIT_CDC_FORGEJO_URL=https://forge.example/
```

Provision the matching owner/repository in Git-CDC, then route
`/OWNER/REPOSITORY/info/lfs` to Git-CDC at the reverse proxy. A separate LFS
host is also supported; configure it in the Git checkout with:

```console
git-cdc install --scope local
git-cdc configure --scope local --url https://lfs.example/OWNER/REPOSITORY/info/lfs
```

Forgejo personal access tokens used for Git-CDC need `read:user` plus
`read:repository` for downloads or `write:repository` for uploads and locks.
Repository-specific tokens are supported. Store credentials through Git's
credential machinery; do not put tokens in the LFS URL. Git-CDC forwards the
caller's authorization to Forgejo for both the current-user and repository
permission checks on every request, so revocation and permission changes do
not wait for a cache to expire.

The integration suite creates a real private Forgejo repository, performs a
Git push whose LFS object uses CDC, clones and verifies the bytes through CDC,
uninstalls Git-CDC, then cold-fetches and verifies the same standard pointer
with stock Git LFS.

## Reachability and garbage collection

Reconciliation uses an ordinary read-only Git URL and Git credential helpers:

```console
git-cdc-admin reconcile REPOSITORY_UUID GIT_URL
git-cdc-admin gc-dry-run REPOSITORY_UUID
git-cdc-admin gc-stage REPOSITORY_UUID --grace-seconds 604800
git-cdc-admin gc-collect REPOSITORY_UUID
```

The reconciler creates a fresh mirror, fingerprints all refs, validates every
reachable standard LFS pointer, and atomically marks the epoch complete. Any
Git, ref, blob, pointer, or database failure publishes no complete epoch.

An object is eligible only when absent from the two newest complete epochs.
It is then staged behind the configured grace period. Metadata deletion and
chunk-provider deletion are separated by a durable cleanup queue, making a
crash or provider outage safe to retry. Without two complete epochs, completed
objects are retained indefinitely. Always inspect `gc-dry-run` before staging.

## Health and metrics

- `/healthz` is process liveness.
- `/readyz` executes a PostgreSQL query and fails when metadata is unavailable.
- `/metrics` exposes Prometheus counters for logical upload/download bytes and
  received chunk bytes.

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
