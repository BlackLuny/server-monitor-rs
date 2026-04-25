-- M4 network probes (ICMP / TCP / HTTP) with per-agent override.
--
-- The shape is deliberately parallel to `metric_snapshots`:
--   - `probes` defines what to measure
--   - `probe_results` holds time-series data with the same 4 granularity tiers
--   - `probe_agent_overrides` is a sparse exception table that flips a
--     specific (probe, agent) off the default
--
-- Default scope semantics:
--   `probes.default_enabled = true`  → every agent runs this probe unless
--   their (probe_id, agent_id) row in probe_agent_overrides explicitly says
--   `enabled = false`.
--   `probes.default_enabled = false` → no agent runs this probe unless an
--   override row explicitly says `enabled = true`.
--
-- New agents inherit the default automatically — they have no override rows
-- yet, so `COALESCE(o.enabled, p.default_enabled)` returns the default.

CREATE TABLE probes (
    id                 BIGSERIAL PRIMARY KEY,
    name               TEXT NOT NULL,
    kind               TEXT NOT NULL CHECK (kind IN ('icmp', 'tcp', 'http')),
    target             TEXT NOT NULL,             -- host / IP for icmp+tcp, URL for http
    port               INTEGER,                   -- only meaningful for tcp
    interval_s         INTEGER NOT NULL DEFAULT 60 CHECK (interval_s >= 5),
    timeout_ms         INTEGER NOT NULL DEFAULT 3000 CHECK (timeout_ms BETWEEN 100 AND 60000),

    -- HTTP-only options
    http_method        TEXT,                      -- default 'GET' applied at the API layer
    http_expect_code   INTEGER,                   -- 0/null = any 2xx
    http_expect_body   TEXT,                      -- empty/null = skip body check

    -- Default scope: when true (the common case), every registered agent
    -- executes this probe unless excluded via probe_agent_overrides.
    default_enabled    BOOLEAN NOT NULL DEFAULT TRUE,
    -- Global on/off independent of scope. Useful to silence a probe without
    -- losing its config + history.
    enabled            BOOLEAN NOT NULL DEFAULT TRUE,

    created_by         BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_probes_enabled ON probes (enabled) WHERE enabled = TRUE;

-- Sparse override table. One row per *deviation* from the probe's default.
-- Conventions: missing row → use `probes.default_enabled`. Present row →
-- `enabled` field wins. We never store rows that match the default; the API
-- layer is responsible for cleaning those up so the table stays sparse.
CREATE TABLE probe_agent_overrides (
    probe_id   BIGINT NOT NULL REFERENCES probes(id) ON DELETE CASCADE,
    agent_id   UUID   NOT NULL REFERENCES servers(agent_id) ON DELETE CASCADE,
    enabled    BOOLEAN NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (probe_id, agent_id)
);

CREATE INDEX idx_probe_agent_overrides_agent ON probe_agent_overrides (agent_id);

-- Time-series of probe results, mirrors metric_snapshots layout. Latency in
-- microseconds (BIGINT) so we don't lose sub-millisecond precision on
-- localhost-ish targets — JSON serializes it as a regular number.
CREATE TABLE probe_results (
    probe_id      BIGINT NOT NULL REFERENCES probes(id) ON DELETE CASCADE,
    agent_id      UUID   NOT NULL,                 -- soft link; we keep history even after a server is removed
    granularity   TEXT NOT NULL CHECK (granularity IN ('raw', 'm1', 'm5', 'h1')),
    ts            TIMESTAMPTZ NOT NULL,

    ok            BOOLEAN NOT NULL DEFAULT FALSE,
    latency_us    BIGINT NOT NULL DEFAULT 0,       -- 0 on failure or for aggregates with no successes
    -- For aggregated tiers: percentile latency over successful samples.
    -- Raw rows leave these NULL — they coincide with `latency_us`.
    latency_us_p50 BIGINT,
    latency_us_p95 BIGINT,
    -- For aggregated tiers: success rate in [0,1]. Raw rows = 1.0 if ok, 0.0 otherwise.
    success_rate  DOUBLE PRECISION,
    sample_count  INTEGER NOT NULL DEFAULT 1,
    status_code   INTEGER,                          -- HTTP only
    error         TEXT,                             -- last error message in this bucket; NULL when all succeeded

    PRIMARY KEY (probe_id, agent_id, granularity, ts)
);

CREATE INDEX idx_probe_results_granularity_ts
    ON probe_results (granularity, ts);

CREATE INDEX idx_probe_results_probe_agent_ts
    ON probe_results (probe_id, agent_id, granularity, ts DESC);
