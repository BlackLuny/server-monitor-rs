-- M5: terminal_sessions audit + recording metadata.
--
-- Bookkeeping for every Web SSH session opened through the panel WS bridge.
-- Recording metadata is filled in when the agent reports the .cast finalised;
-- the actual file lives on the agent and is fetched on demand.

CREATE TABLE terminal_sessions (
    id              UUID PRIMARY KEY,
    server_id       BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    user_id         BIGINT REFERENCES users(id) ON DELETE SET NULL,
    opened_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    closed_at       TIMESTAMPTZ,
    exit_code       INTEGER,
    error           TEXT,
    -- Filled when the agent finalises the asciinema recording.
    recording_path  TEXT,
    recording_size  BIGINT,
    recording_sha256 TEXT,
    client_ip       TEXT,
    user_agent      TEXT
);

CREATE INDEX idx_terminal_sessions_server ON terminal_sessions (server_id, opened_at DESC);
CREATE INDEX idx_terminal_sessions_user   ON terminal_sessions (user_id, opened_at DESC);
