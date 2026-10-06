-- conary-core/src/db/current_schema/sql/source_root.sql
-- Pinned source identity of a source-root database.
--
-- A source root's database records exactly one source identity in the same
-- transaction that creates its schema. The host database has no row and is
-- never a source root. The identity grammar mirrors
-- `source_root::SourceRootName`: 1 to 64 bytes of `[a-z0-9._-]`, starting
-- with `[a-z0-9]`. The pin is immutable: updates and deletes are refused.

CREATE TABLE source_root_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    identity TEXT NOT NULL
        CHECK(
            length(identity) BETWEEN 1 AND 64
            AND substr(identity, 1, 1) GLOB '[a-z0-9]'
            AND identity NOT GLOB '*[^a-z0-9._-]*'
        ),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER source_root_identity_immutable_update
BEFORE UPDATE ON source_root_identity
BEGIN
    SELECT RAISE(ABORT, 'source root identity is immutable');
END;

CREATE TRIGGER source_root_identity_immutable_delete
BEFORE DELETE ON source_root_identity
BEGIN
    SELECT RAISE(ABORT, 'source root identity is immutable');
END;
