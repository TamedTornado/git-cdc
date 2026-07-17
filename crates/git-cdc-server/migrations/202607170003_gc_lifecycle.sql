CREATE TABLE object_tombstones (
    repository_id UUID NOT NULL,
    object_oid BYTEA NOT NULL,
    staged_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delete_after TIMESTAMPTZ NOT NULL,
    metadata_deleted_at TIMESTAMPTZ,
    PRIMARY KEY (repository_id, object_oid),
    FOREIGN KEY (repository_id, object_oid)
        REFERENCES objects(repository_id, oid) ON DELETE CASCADE
);

CREATE TABLE chunk_gc_queue (
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    chunk_id BYTEA NOT NULL CHECK (octet_length(chunk_id) = 32),
    queued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, chunk_id)
);
