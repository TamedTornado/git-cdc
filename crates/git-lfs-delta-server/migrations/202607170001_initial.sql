CREATE TABLE repositories (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner, name)
);

CREATE TABLE objects (
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    oid BYTEA NOT NULL CHECK (octet_length(oid) = 32),
    size BIGINT NOT NULL CHECK (size >= 0),
    manifest JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, oid)
);

CREATE TABLE chunks (
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    chunk_id BYTEA NOT NULL CHECK (octet_length(chunk_id) = 32),
    size BIGINT NOT NULL CHECK (size > 0),
    reference_count BIGINT NOT NULL DEFAULT 0 CHECK (reference_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, chunk_id)
);

CREATE TABLE object_chunks (
    repository_id UUID NOT NULL,
    object_oid BYTEA NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    chunk_id BYTEA NOT NULL,
    byte_offset BIGINT NOT NULL CHECK (byte_offset >= 0),
    byte_length INTEGER NOT NULL CHECK (byte_length > 0),
    PRIMARY KEY (repository_id, object_oid, ordinal),
    FOREIGN KEY (repository_id, object_oid)
        REFERENCES objects(repository_id, oid) ON DELETE CASCADE,
    FOREIGN KEY (repository_id, chunk_id)
        REFERENCES chunks(repository_id, chunk_id)
);

CREATE TABLE upload_sessions (
    id UUID PRIMARY KEY,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    object_oid BYTEA NOT NULL CHECK (octet_length(object_oid) = 32),
    object_size BIGINT NOT NULL CHECK (object_size >= 0),
    manifest JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'finalized', 'expired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE UNIQUE INDEX one_open_upload_per_object
    ON upload_sessions(repository_id, object_oid)
    WHERE state = 'open';

CREATE TABLE lfs_locks (
    id UUID PRIMARY KEY,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    owner_subject TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repository_id, path)
);

CREATE TABLE reachability_epochs (
    id UUID PRIMARY KEY,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    ref_fingerprint TEXT NOT NULL,
    complete BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

CREATE TABLE reachable_objects (
    epoch_id UUID NOT NULL REFERENCES reachability_epochs(id) ON DELETE CASCADE,
    object_oid BYTEA NOT NULL CHECK (octet_length(object_oid) = 32),
    PRIMARY KEY (epoch_id, object_oid)
);
