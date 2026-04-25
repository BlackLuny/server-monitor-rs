-- M3 auth layer: login sessions + TOTP/backup-code storage.
--
-- Sessions are DB-backed (not JWT) so revocation is immediate and we can
-- inspect who is online. The cookie carries only the opaque session_id;
-- all validation hits this table.

-- ----------------------------------------------------------------------------
-- login_sessions: one row per active browser session.
-- Sliding 7-day expiry: a session is valid while `last_used_at > NOW() - 7d`
-- and `revoked_at IS NULL`.
-- ----------------------------------------------------------------------------
CREATE TABLE login_sessions (
    id            TEXT PRIMARY KEY,
    user_id       BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    ip            TEXT,
    user_agent    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at    TIMESTAMPTZ
);

CREATE INDEX idx_login_sessions_user      ON login_sessions (user_id);
CREATE INDEX idx_login_sessions_last_used ON login_sessions (last_used_at DESC);

-- ----------------------------------------------------------------------------
-- users: extend with TOTP enable flag + backup codes.
-- `totp_secret` already exists from the initial migration.
-- `backup_codes` stores argon2 hashes of one-time recovery codes.
-- ----------------------------------------------------------------------------
ALTER TABLE users
    ADD COLUMN totp_enabled  BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN backup_codes  JSONB   NOT NULL DEFAULT '[]'::jsonb;
