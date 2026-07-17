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
