# Git-CDC: Usable Beta Project Plan

## Objective

Build a public Rust implementation of chunk-deduplicated Git LFS. Standard
Git LFS pointers and clients remain compatible; installing the native
`git-cdc` transfer agent enables incremental chunk uploads and downloads.

Forgejo is the first reference integration, not a core dependency. The client
runs natively on Windows, macOS, and Linux. The Linux-first server uses
PostgreSQL and pluggable object storage.

## Repository and engineering policy

- Develop publicly on the `master` branch with feature branches and pull
  requests once the bootstrap lands.
- Use a focused Rust workspace with thin binary entrypoints and domain-owned
  modules.
- Apply RED/GREEN TDD to protocols, persistence, lifecycle, and failure paths.
- Treat formatting, compilation, Clippy, tests, dependency policy, and native
  end-to-end tests as separate quality gates.
- Dual-license all original code under Apache-2.0 OR MIT.

## Architecture

The workspace contains:

- `git-cdc`: native Git LFS custom-transfer agent and user CLI.
- `git-cdc-server`: LFS API, CDC transfer API, locks, authentication, and
  administration.
- Shared crates for protocol types, chunking/manifests, storage, and test
  support where a real compile boundary exists.

Git-CDC retains the standard Git LFS pointer format. Whole-object SHA-256 and
byte size remain canonical. The server supports both standard Git LFS
Batch/basic transfers and a negotiated `cdc` custom transfer.

The beta chunking profile is deterministic FastCDC with 512 KiB minimum,
2 MiB target, and 8 MiB maximum chunks. Chunks use 256-bit BLAKE3 identities;
completed objects are always verified against the LFS pointer's SHA-256.
Chunks are loose immutable objects in beta. Manifest and storage versions
reserve a later packed-storage implementation without changing Git history or
the client protocol.

Deduplication is repository-scoped. No API may reveal whether a chunk exists
outside the caller's authorized repository.

## Client

Git-CDC initially extends rather than replaces Git LFS. It implements the Git
LFS custom-transfer protocol over stdin/stdout and provides these commands:

- `git-cdc install`
- `git-cdc uninstall`
- `git-cdc configure`
- `git-cdc doctor`
- `git-cdc status`
- `git-cdc cache prune`
- internal `git-cdc transfer`

The client resolves credentials using Git credential helpers, stores cache and
resumable state in native platform directories, bounds memory independently of
file size, reconstructs into temporary files, and atomically publishes only
fully verified output.

Required targets are Windows x86-64, macOS ARM64 and x86-64, and Linux x86-64
and ARM64. Normal operation may not depend on a shell, Unix utility, daemon, or
platform-specific secret API.

## Server and storage

PostgreSQL stores repositories, logical objects, upload sessions, chunk
presence, locks, authentication mappings, reachability epochs, tombstones, and
GC jobs. Object bytes sit behind Apache Arrow's Rust `object_store` interface.

The beta fully supports and integration-tests local filesystems and
S3-compatible storage, using MinIO as the reference self-hosted target. Azure
Blob and GCS are preview providers from the same runtime-configured binary.
Provider-specific types do not escape the storage adapter boundary.

The server exposes health/readiness endpoints, structured logs, Prometheus
metrics, and administrative reconciliation/GC commands. It ships as an OCI
image with a PostgreSQL/MinIO Compose deployment.

## Authentication and forge integration

Authentication is adapter-driven. The beta officially supports Forgejo and
generic OIDC bearer-token validation. Forgejo credentials are checked for the
specific repository and operation; administrator credentials are never handed
to clients. OIDC token acquisition remains external in beta and credentials
flow through Git's credential machinery.

A reverse proxy can route a forge's normal LFS URL to Git-CDC. Other forges and
bare Git servers can configure a separate LFS endpoint. Pull requests, issues,
reviews, and forge UI concepts remain outside Git-CDC core.

The service implements standard Git LFS locks, including repository-scoped
paths, ownership, conflict reporting, pagination, and authorized forced
unlock.

## Transfer lifecycle

For CDC upload, the agent verifies the source SHA-256, deterministically chunks
the file, submits a versioned manifest, receives the missing chunk indexes,
uploads those chunks concurrently through a resumable session, and finalizes.
The server verifies chunk digests, total length, and reconstructed SHA-256
before atomically publishing the object.

For CDC download, the client obtains the authorized manifest, reuses verified
local chunks, downloads the remainder, reconstructs into a temporary file,
verifies the canonical SHA-256, and atomically publishes the result. Stock LFS
clients upload whole logical objects and receive streamed reconstructions.

Session creation, chunk submission, verification, and finalization are
idempotent. Duplicated or reordered requests and client/server restarts must
not corrupt state or repeat successful work. Expired partial uploads are
quarantined before reclamation.

## Safe garbage collection

Git LFS object traffic does not prove whether Git history still references an
object. Git-CDC therefore never deletes a completed logical object based on
last access or age alone.

A forge-neutral reconciler fetches repository refs through ordinary read-only
Git access, walks commits reachable from branches and tags, identifies LFS
pointer blobs, caches previously scanned commits/blobs, and submits a complete
epoch-tagged live-OID snapshot. Objects must be absent from consecutive
complete snapshots and survive staging and deletion grace periods before being
tombstoned. A chunk is deleted only when no retained manifest references it.

Without a complete reachability source, completed objects are retained
indefinitely. Expired incomplete uploads may always be reclaimed. GC provides
a dry run showing every proposed deletion.

## Test strategy

Golden chunking fixtures must produce identical boundaries and manifests on
every supported OS and architecture. Property and fuzz tests cover empty
files, exact boundaries, large streams, insertions, deletions, corruption, and
exact reconstruction.

Native CI uses the real `git` and `git-lfs` executables on Windows, macOS, and
Linux for add/commit/push, clone/checkout, incremental modification, cache
reuse, stock fallback, locks, Unicode and spaced paths, concurrent transfers,
and uninstall. Compilation alone is not platform support.

Integration tests use real PostgreSQL and MinIO. Failure injection covers
process interruption, duplicated/reordered requests, database rollback,
object-store timeouts, expired/revoked credentials, failed Git push after LFS
upload, concurrent identical upload, corrupt local/remote chunks, and
interrupted GC.

Authorization tests prove read/write separation, lock ownership, token
revocation, repository isolation, and absence of cross-repository chunk
existence leaks. GC tests use real Git histories with branches, tags,
force-pushes, and failed pushes.

Performance tests report chunking throughput, peak memory, request counts,
logical/physical bytes, initial transfer, localized edits, opaque rewritten
assets, and warm/cold cache behavior. The beta promises integrity, bounded
memory, and incremental transfer where data permits it, not a universal
deduplication ratio.

## Beta acceptance

- Native end-to-end CI passes on Windows, macOS, and Linux.
- Stock Git LFS and Git-CDC clients safely share a Forgejo repository.
- Standard pointers remain usable after Git-CDC is uninstalled.
- Filesystem and S3/MinIO storage pass the same behavioral suite.
- Interrupted transfers resume without corruption or duplicate logical state.
- Locks conform to the Git LFS API.
- Repository authorization and token revocation are enforced.
- Completed-object deletion never occurs without proven Git reachability.
- PostgreSQL plus object-storage backup/restore produces a working service.
- Installation, Forgejo integration, generic deployment, protocol,
  operations, and recovery are documented.

## Explicit exclusions

The beta does not provide Lore compatibility, another VCS, a Forgejo-specific
core, a Bro dependency, hosted SaaS, FUSE, cross-repository deduplication,
packed chunks, replacement Git LFS filters/migration tools, or unsafe
time-based logical-object GC.
