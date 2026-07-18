# Git LFS Delta

Git LFS Delta is an open, forge-neutral, content-defined-chunking backend for Git
Large File Storage. Existing Git LFS pointer files and clients remain valid;
installing the native `git-lfs-delta` custom transfer agent additionally allows a
client to upload and download only chunks it does not already share with the
server.

The project is a production-candidate `0.1.0-beta.2`. Its design and exercised beta
acceptance criteria are recorded in [the project plan](docs/PROJECT_PLAN.md).

## Compatibility promise

- Git remains the version-control system.
- Repositories retain standard Git LFS SHA-256 pointer files.
- Stock Git LFS clients use the standard basic transfer path.
- Git LFS Delta-aware clients negotiate a chunk-aware transfer path.
- Forgejo is the first reference integration, not a core dependency.

## Implemented beta

- Deterministic streaming FastCDC manifests with SHA-256/BLAKE3 integrity.
- A native `git-lfs` custom-transfer client with resumable uploads, verified
  chunk caching, install/configure/status/uninstall/doctor/cache commands, and
  no shell dependency.
- Standard Git LFS Batch/basic upload/download and locking APIs.
- A chunk-aware CDC upload/download protocol with idempotent sessions.
- PostgreSQL metadata and provider-neutral filesystem, S3/MinIO, Azure, and GCS
  object storage through Apache Arrow's `object_store` crate.
- Forgejo authorization with bounded caching, preview generic OIDC/JWKS validation and
  repository grants, read-only Git reachability reconciliation, and
  conservative grace-period garbage collection.
- Real PostgreSQL, filesystem, MinIO, HTTP client/server, and stock `git-lfs`
  contract coverage. Native CI builds and tests on Linux, macOS, and Windows.

## Develop, build, and test

The common workflows are Cargo commands; no manual environment setup is
required:

```console
cargo dev          # PostgreSQL, migrations, and the local server
cargo build        # all default workspace binaries
cargo test         # fast infrastructure-free tests
cargo ci           # formatting, strict lint, and workspace tests
cargo acceptance   # complete PostgreSQL/MinIO/Forgejo acceptance suite
```

`cargo dev` uses `http://127.0.0.1:8080`, the development bearer token
`git-lfs-delta-local`, filesystem storage under `target/dev`, and a disposable
PostgreSQL container. Press Ctrl-C to stop the server and run `cargo dev-down`
when the database is no longer needed.

The black-box acceptance command provisions disposable dependencies, runs the
complete workspace suite, exercises a private Forgejo repository through both
Git LFS Delta and stock Git LFS, proves incremental transfer and restart
behavior, and verifies PostgreSQL/object-storage recovery.

## Production configuration

Local development deliberately has useful defaults. Production fails closed
and reads its deployment-specific values from the environment. Start from the
documented template instead of entering them individually:

```console
cp .env.example .env
docker compose -f docker-compose.production.yml config
```

Apply schema migrations as a separate deployment step, then start the service:

```console
docker compose -f docker-compose.production.yml run --rm migrate
docker compose -f docker-compose.production.yml up -d git-lfs-delta
```

Native deployments use the equivalent `git-lfs-delta-admin migrate`,
`git-lfs-delta-admin schema-check`, and `git-lfs-delta-server` commands.

`GIT_LFS_DELTA_STORAGE_URL` also accepts `s3://`, `gs://`, and Azure object-store
URLs; the corresponding provider credentials are read from environment
variables supported by `object_store`.

Production defaults bound logical objects to 100 GiB, permit two simultaneous
stock/basic staging operations, and require development authentication to stay
on a loopback bind. The container binds on `0.0.0.0:8080`, runs as non-root,
and expects a staging volume sized to at least 240 GiB at the default limits.
Use `docker-compose.production.yml` with external PostgreSQL and durable
S3-compatible storage; terminate TLS at the reverse proxy and do not expose
`/metrics` publicly. Migration roles, rolling deployment order, rollback, and
backup procedures are defined in [Operations](docs/OPERATIONS.md).

Create repository mappings explicitly with `git-lfs-delta-admin repository-add`. Then
point a repository's LFS endpoint at
`https://host/OWNER/REPOSITORY/info/lfs`, configure authentication through
Git's HTTP configuration or credential machinery, and run:

```console
git-lfs-delta install --scope local
```

The development-token adapter expects a bearer header on the initial Batch
request. For local testing only:

```console
git config http.extraheader "Authorization: Bearer $GIT_LFS_DELTA_DEV_TOKEN"
```

Deployment, provisioning, authentication, reconciliation, garbage collection,
backup, and recovery are documented in [Operations](docs/OPERATIONS.md).
The negotiated transfer and integrity contract is documented in
[Protocol v1](docs/PROTOCOL.md).

## License

Git LFS Delta is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
