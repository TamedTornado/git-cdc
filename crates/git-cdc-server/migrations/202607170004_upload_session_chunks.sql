CREATE TABLE upload_session_chunks (
    session_id UUID NOT NULL REFERENCES upload_sessions(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    chunk_id BYTEA NOT NULL CHECK (octet_length(chunk_id) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, chunk_id),
    FOREIGN KEY (repository_id, chunk_id)
        REFERENCES chunks(repository_id, chunk_id)
);

CREATE INDEX upload_session_chunks_by_chunk
    ON upload_session_chunks(repository_id, chunk_id);
