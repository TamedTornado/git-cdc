ALTER TABLE upload_sessions
    ADD CONSTRAINT upload_sessions_id_repository_unique UNIQUE (id, repository_id);

ALTER TABLE upload_session_chunks
    DROP CONSTRAINT upload_session_chunks_session_id_fkey;

ALTER TABLE upload_session_chunks
    ADD CONSTRAINT upload_session_chunks_session_repository_fkey
    FOREIGN KEY (session_id, repository_id)
    REFERENCES upload_sessions(id, repository_id)
    ON DELETE CASCADE;

CREATE INDEX upload_sessions_expiry
    ON upload_sessions(repository_id, expires_at)
    WHERE state = 'open';

CREATE INDEX object_tombstones_due
    ON object_tombstones(repository_id, delete_after);

CREATE INDEX chunk_gc_queue_ordered
    ON chunk_gc_queue(repository_id, queued_at);
