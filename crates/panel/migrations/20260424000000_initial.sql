-- Initial schema for server-monitor-rs panel.
-- Scope: M1 Register + Heartbeat, M2 metrics, M3 auth + groups + tags,
-- M5 terminal flags, M7 update tracking columns are added in later migrations.

-- Ensure uuid generation is available.
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ----------------------------------------------------------------------------
-- users: admin accounts.
-- ----------------------------------------------------------------------------
CREATE TABLE users (
    id             BIGSERIAL PRIMARY KEY,
    username       TEXT NOT NULL UNIQUE,
    password_hash  TEXT NOT NULL,
    role           TEXT NOT NULL DEFAULT 'admin' CHECK (role IN ('admin')),
    totp_secret    TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ----------------------------------------------------------------------------
-- settings: global KV store.
-- ----------------------------------------------------------------------------
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value JSONB NOT NULL
);

-- Seed baseline settings. `agent_endpoint` starts empty to force the admin to
-- configure it before any server can be added (see `servers` API).
INSERT INTO settings (key, value) VALUES
    ('site_name',              '"server-monitor"'::jsonb),
    ('guest_enabled',           'true'::jsonb),
    ('agent_endpoint',          '""'::jsonb),
    -- SSH session recording is on by default. Operators who'd rather not
    -- keep per-session .cast files on the agent's disk can flip this in
    -- /settings/general, or override per server via the ssh_recording
    -- column.
    ('ssh_recording_enabled',   'true'::jsonb)
ON CONFLICT (key) DO NOTHING;

-- ----------------------------------------------------------------------------
-- server_groups: organizational grouping for servers.
-- ----------------------------------------------------------------------------
CREATE TABLE server_groups (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL,
    order_idx   INTEGER NOT NULL DEFAULT 0,
    description TEXT,
    color       TEXT
);

-- ----------------------------------------------------------------------------
-- servers: one row per monitored agent.
--
-- Flow:
--   1. Admin adds server → row inserted with `join_token` set, `server_token`
--      and `agent_id` NULL.
--   2. Agent Registers → generates `agent_id`, we store hardware/version,
--      clear `join_token`, issue `server_token`.
--   3. Agent Streams → updates `last_seen_at` on every heartbeat.
-- ----------------------------------------------------------------------------
CREATE TABLE servers (
    id                  BIGSERIAL PRIMARY KEY,
    -- Assigned at row creation; visible in the admin UI even before the
    -- agent has Registered. Remains stable for the lifetime of the row.
    agent_id            UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    display_name        TEXT NOT NULL,
    group_id            BIGINT REFERENCES server_groups(id) ON DELETE SET NULL,
    tags                JSONB NOT NULL DEFAULT '[]'::jsonb,
    join_token          TEXT UNIQUE,                     -- NULL after Register
    server_token        TEXT UNIQUE,                     -- NULL before Register
    hidden_from_guest   BOOLEAN NOT NULL DEFAULT FALSE,
    location            TEXT,
    flag_emoji          TEXT,
    order_idx           INTEGER NOT NULL DEFAULT 0,
    last_seen_at        TIMESTAMPTZ,

    -- Hardware snapshot (filled on Register, may refresh on reconnect).
    hw_cpu_model        TEXT,
    hw_cpu_cores        INTEGER,
    hw_mem_bytes        BIGINT,
    hw_swap_bytes       BIGINT,
    hw_disk_bytes       BIGINT,
    hw_os               TEXT,
    hw_os_version       TEXT,
    hw_kernel           TEXT,
    hw_arch             TEXT,
    hw_virtualization   TEXT,
    hw_boot_id          TEXT,
    agent_version       TEXT,

    -- Per-server SSH / terminal policy.
    terminal_enabled    BOOLEAN NOT NULL DEFAULT TRUE,
    ssh_recording       TEXT NOT NULL DEFAULT 'default'
                        CHECK (ssh_recording IN ('default', 'on', 'off')),

    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_servers_group_id    ON servers (group_id);
CREATE INDEX idx_servers_last_seen   ON servers (last_seen_at DESC NULLS LAST);

-- ----------------------------------------------------------------------------
-- audit_log: admin action trail (logins, server CRUD, SSH sessions, updates).
-- ----------------------------------------------------------------------------
CREATE TABLE audit_log (
    id         BIGSERIAL PRIMARY KEY,
    user_id    BIGINT REFERENCES users(id) ON DELETE SET NULL,
    action     TEXT NOT NULL,
    target     TEXT,
    ip         TEXT,
    user_agent TEXT,
    ts         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_log_ts ON audit_log (ts DESC);
