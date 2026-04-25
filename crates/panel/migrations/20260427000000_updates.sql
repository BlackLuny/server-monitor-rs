-- M7: agent self-update orchestration tables.
--
-- Two-table design:
--   `update_rollouts`     — admin-initiated rollout campaign.
--   `update_assignments`  — one row per (rollout, agent) describing what
--                           gets pushed and the per-agent execution state.
--
-- The panel-side orchestrator drives state transitions; the agent stream
-- handler updates `state` + `last_status_message` whenever an
-- AgentToPanel::UpdateStatus frame arrives.

-- Default polling target. install-panel.sh leaves this in place; admins
-- can override via /settings/updates if they fork.
INSERT INTO settings (key, value) VALUES
    ('update_repo',     '"BlackLuny/server-monitor-rs"'::jsonb),
    ('update_channel',  '"stable"'::jsonb)
ON CONFLICT (key) DO NOTHING;

CREATE TABLE update_rollouts (
    id           BIGSERIAL PRIMARY KEY,
    version      TEXT NOT NULL,
    created_by   BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at   TIMESTAMPTZ,
    finished_at  TIMESTAMPTZ,
    -- Percentage of eligible agents to ship to. Combined with `agent_ids`:
    --   agent_ids = []  → take percent of every eligible agent
    --   agent_ids = […] → assignments restricted to those, percent applied to the subset
    percent      INTEGER NOT NULL DEFAULT 100 CHECK (percent BETWEEN 1 AND 100),
    agent_ids    JSONB NOT NULL DEFAULT '[]'::jsonb,
    state        TEXT NOT NULL DEFAULT 'pending'
                 CHECK (state IN ('pending','active','paused','completed','aborted')),
    note         TEXT
);

CREATE INDEX idx_update_rollouts_state ON update_rollouts (state);
CREATE INDEX idx_update_rollouts_created_at ON update_rollouts (created_at DESC);

CREATE TABLE update_assignments (
    rollout_id           BIGINT NOT NULL REFERENCES update_rollouts(id) ON DELETE CASCADE,
    -- FK to servers.agent_id (UNIQUE there); agents are identified by uuid
    -- on the wire, server.id is panel-side bookkeeping only.
    agent_id             UUID NOT NULL REFERENCES servers(agent_id) ON DELETE CASCADE,
    -- e.g. x86_64-unknown-linux-musl. Stored explicitly so a target swap
    -- (admin re-imaging an agent host) can't silently change the artefact.
    target               TEXT NOT NULL,
    artefact_url         TEXT NOT NULL,
    artefact_sha256      TEXT NOT NULL,
    state                TEXT NOT NULL DEFAULT 'pending'
                         CHECK (state IN ('pending','sent','succeeded','failed')),
    last_status_message  TEXT,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (rollout_id, agent_id)
);

CREATE INDEX idx_update_assignments_agent ON update_assignments (agent_id);
CREATE INDEX idx_update_assignments_state ON update_assignments (rollout_id, state);
