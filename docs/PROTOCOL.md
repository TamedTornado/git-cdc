# Git-CDC Protocol v1

Git-CDC extends the Git LFS Batch API without changing Git history. Pointer
files remain the standard three-line Git LFS v1 format; the pointer SHA-256 and
size are the canonical logical-object identity.

## Negotiation

Clients include `cdc` in the Batch request's `transfers` array. The server
selects `cdc` when offered and otherwise selects standard `basic`. An absent
transfer list means `basic`. Each returned action includes the caller's
authorization and an absolute object URL.

The repository LFS base is:

```text
/OWNER/REPOSITORY/info/lfs
```

Standard Batch, whole-object upload/download, and locking follow the upstream
Git LFS API. CDC-specific requests use the paths below.

## Manifest

A v1 manifest contains:

- `version`: `v1`.
- `profile`: the deterministic FastCDC parameters.
- `object_oid`: the canonical Git LFS SHA-256.
- `object_size`: reconstructed byte length.
- `chunks`: ordered BLAKE3 IDs with contiguous `offset` and `length` values.

The beta profile uses a 512 KiB minimum, 2 MiB target, and 8 MiB maximum.
Servers reject gaps, overlaps, empty chunks, size mismatches, wrong chunk
digests, and a reconstructed SHA-256 that differs from `object_oid`.

## Upload

1. `POST /objects/OID/cdc` with `protocol_version: 1` and the complete
   manifest. The response contains a stable `upload_id`, expiry, and only the
   missing chunk indexes.
2. `PUT /objects/OID/cdc/UPLOAD_ID/chunks/INDEX` with the exact chunk bytes.
3. `POST /objects/OID/cdc/UPLOAD_ID/finalize` after all chunks are present.

Session creation, identical chunk submission, and successful finalization are
idempotent. Chunks may arrive in any order. Concurrent clients uploading the
same OID converge on the same open session. Finalization verifies every chunk,
the reconstructed length, and SHA-256 before atomically publishing metadata.

Expired sessions are quarantined. Administrative reclamation observes a grace
period and cannot remove a chunk referenced by a completed object or active
upload.

## Download

1. `GET /objects/OID/cdc` returns the authorized manifest.
2. `GET /objects/OID/cdc/chunks/INDEX` returns one verified chunk.

The client may reuse a local chunk only after validating its length and BLAKE3
ID. It reconstructs into a temporary file, verifies the final SHA-256 and size,
then atomically publishes the result. Invalid cache entries are removed and
refetched. Provider corruption is never returned as valid data.

## Status and retry behavior

- `401`: missing, invalid, or revoked credentials.
- `403`: valid identity without the required repository operation.
- `404`: repository, object, session, lock, or index is absent.
- `409`: a lock conflict or upload finalization with missing chunks.
- `422`: caller-supplied manifest, length, digest, or SHA-256 integrity error.
- `500`/`502`: database, storage, or upstream authentication service failure;
  no successful logical publication should be inferred.

Clients may retry session creation, chunk PUTs, and finalization after
transport ambiguity. A completed `204` finalization remains `204` on retry.
