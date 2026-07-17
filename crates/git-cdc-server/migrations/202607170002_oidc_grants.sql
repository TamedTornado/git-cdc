CREATE TABLE repository_grants (
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    subject TEXT NOT NULL,
    can_read BOOLEAN NOT NULL DEFAULT FALSE,
    can_write BOOLEAN NOT NULL DEFAULT FALSE,
    can_admin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, subject),
    CHECK (NOT can_write OR can_read),
    CHECK (NOT can_admin OR can_write)
);
