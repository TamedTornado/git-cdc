# Git-CDC

Git-CDC is an open, forge-neutral, content-defined-chunking backend for Git
Large File Storage. Existing Git LFS pointer files and clients remain valid;
installing the native `git-cdc` custom transfer agent additionally allows a
client to upload and download only chunks it does not already share with the
server.

The project is currently an alpha implementation moving toward its first
usable beta. The design and complete beta acceptance criteria are recorded in
[the project plan](docs/PROJECT_PLAN.md).

## Compatibility promise

- Git remains the version-control system.
- Repositories retain standard Git LFS SHA-256 pointer files.
- Stock Git LFS clients use the standard basic transfer path.
- Git-CDC-aware clients negotiate a chunk-aware transfer path.
- Forgejo is the first reference integration, not a core dependency.

## Implemented alpha

- Deterministic streaming FastCDC manifests with SHA-256/BLAKE3 integrity.
- A native `git-lfs` custom-transfer client with resumable uploads, verified
  chunk caching, install/doctor/cache commands, and no shell dependency.
- Standard Git LFS Batch/basic upload/download and locking APIs.
- A chunk-aware CDC upload/download protocol with idempotent sessions.
- PostgreSQL metadata and provider-neutral filesystem, S3/MinIO, Azure, and GCS
  object storage through Apache Arrow's `object_store` crate.
- Real PostgreSQL, filesystem, MinIO, HTTP client/server, and stock `git-lfs`
  contract coverage. Native CI builds and tests on Linux, macOS, and Windows.

## Build and test

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
docker compose -f docker-compose.test.yml up -d postgres minio
docker compose -f docker-compose.test.yml run --rm minio-init
GIT_CDC_TEST_DATABASE_URL=postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc \
GIT_CDC_TEST_MINIO=1 cargo test --workspace
```

## Run the server

The server fails closed if any required setting is absent:

```console
export GIT_CDC_DATABASE_URL=postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc
export GIT_CDC_BASE_URL=http://127.0.0.1:8080/
export GIT_CDC_DEV_TOKEN=replace-this-development-token
export GIT_CDC_STORAGE_URL=file:///var/lib/git-cdc
cargo run -p git-cdc-server
```

`GIT_CDC_STORAGE_URL` also accepts `s3://`, `gs://`, and Azure object-store
URLs; the corresponding provider credentials are read from environment
variables supported by `object_store`.

Create repository mappings explicitly in PostgreSQL during the alpha. Then
point a repository's LFS endpoint at
`https://host/OWNER/REPOSITORY/info/lfs`, configure authentication through
Git's HTTP configuration or credential machinery, and run:

```console
git-cdc install --scope local
```

The development-token adapter expects a bearer header on the initial Batch
request. For local testing only:

```console
git config http.extraheader "Authorization: Bearer $GIT_CDC_DEV_TOKEN"
```

Production Forgejo and OIDC authentication, reachability reconciliation, and
safe garbage collection remain beta work and are intentionally not replaced
with permissive fallbacks.

## License

Git-CDC is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
