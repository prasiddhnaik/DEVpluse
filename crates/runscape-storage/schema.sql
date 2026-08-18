-- Runscape persistence schema, baseline version 1.
--
-- This file is the *baseline* only: it is applied verbatim to a brand new
-- database and is then frozen. A schema change adds a numbered migration in
-- `schema.rs` and bumps `SCHEMA_VERSION`; it never edits the statements below,
-- because an existing database has already run them and `CREATE TABLE IF NOT
-- EXISTS` would silently skip the new shape.
--
-- Conventions:
--   * Timestamps are INTEGER Unix *milliseconds*, signed, so a `SystemTime`
--     before the epoch stores as a negative number instead of panicking.
--   * Enum-shaped columns are TEXT: a bare serde tag for unit variants
--     (`healthy`, `git_repository`) and JSON for structured ones
--     (`{"kind":"container",...}`). See `codec.rs`.
--   * Paths are TEXT, lossily UTF-8 encoded, matching how runscape-core
--     already derives `ProjectId` from `Path::to_string_lossy`.
--   * Tables are STRICT so a wrong-typed bind is an error at write time
--     rather than a surprise at read time.

CREATE TABLE IF NOT EXISTS projects (
    id         TEXT    PRIMARY KEY,
    root       TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    confidence REAL    NOT NULL,
    evidence   TEXT    NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
) STRICT;

-- `instances` and `endpoints` are JSON arrays. They are a record of the last
-- observation, not a claim of liveness: a persisted PID is meaningless after a
-- reboot (AGENTS rule 5), which is why `last_seen` sits next to them.
CREATE TABLE IF NOT EXISTS services (
    id            TEXT    PRIMARY KEY,
    project_id    TEXT    REFERENCES projects (id) ON DELETE SET NULL,
    name          TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    runtime       TEXT    NOT NULL,
    fingerprint   TEXT    NOT NULL,
    health        TEXT    NOT NULL,
    instances     TEXT    NOT NULL,
    endpoints     TEXT    NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    restart_count INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS services_project ON services (project_id);

-- No foreign key on `project_id`: events are the audit trail and must be
-- recordable even for a project row that has not been upserted yet (or has
-- since been deleted). Losing an event to a write ordering accident would be
-- worse than holding a dangling id.
CREATE TABLE IF NOT EXISTS events (
    id         TEXT    PRIMARY KEY,
    at         INTEGER NOT NULL,
    project_id TEXT,
    kind       TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS events_at ON events (at);
CREATE INDEX IF NOT EXISTS events_project_at ON events (project_id, at);

CREATE TABLE IF NOT EXISTS warnings (
    id             TEXT    PRIMARY KEY,
    rule           TEXT    NOT NULL,
    severity       TEXT    NOT NULL,
    project_id     TEXT,
    service_id     TEXT,
    message        TEXT    NOT NULL,
    first_seen     INTEGER NOT NULL,
    last_seen      INTEGER NOT NULL,
    related_events TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS warnings_project ON warnings (project_id);

-- Aliases are the one thing in this database the developer typed by hand, so
-- they deliberately outlive the service row they name.
CREATE TABLE IF NOT EXISTS aliases (
    service_id TEXT PRIMARY KEY,
    alias      TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS resource_samples (
    service_id   TEXT    NOT NULL,
    at           INTEGER NOT NULL,
    cpu_percent  REAL    NOT NULL,
    memory_bytes INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS resource_samples_service_at
    ON resource_samples (service_id, at);
