-- Time-series table for per-server metric samples and their roll-up aggregates.
--
-- Shape rationale:
-- * One table, keyed by (server_id, granularity, ts). `granularity` marks which
--   aggregation level a row represents: 'raw' for 1-Hz agent samples, 'm1' for
--   1-minute buckets, 'm5' for 5-minute buckets, 'h1' for 1-hour buckets.
-- * Raw rows and each aggregation tier coexist in the same table so we can
--   write one set of query helpers and query planners can pick the right
--   granularity via the composite primary key.
-- * Nested structures (per-core CPU, per-mount disk usage, per-interface net
--   usage) are stored as JSONB — avoids extra tables and lets the frontend
--   unpack only what it needs. Aggregated rows keep these null because the
--   value of averaging per-component detail is minimal for M2.

CREATE TABLE metric_snapshots (
    server_id       BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    granularity     TEXT   NOT NULL CHECK (granularity IN ('raw', 'm1', 'm5', 'h1')),
    ts              TIMESTAMPTZ NOT NULL,

    -- CPU
    cpu_pct         DOUBLE PRECISION NOT NULL DEFAULT 0,
    cpu_per_core    JSONB,

    -- Memory
    mem_used        BIGINT NOT NULL DEFAULT 0,
    mem_total       BIGINT NOT NULL DEFAULT 0,
    swap_used       BIGINT NOT NULL DEFAULT 0,
    swap_total      BIGINT NOT NULL DEFAULT 0,

    -- Load
    load_1          DOUBLE PRECISION NOT NULL DEFAULT 0,
    load_5          DOUBLE PRECISION NOT NULL DEFAULT 0,
    load_15         DOUBLE PRECISION NOT NULL DEFAULT 0,

    -- Disk
    disk_used       BIGINT NOT NULL DEFAULT 0,
    disk_total      BIGINT NOT NULL DEFAULT 0,
    disks           JSONB,

    -- Network
    net_in_bps      BIGINT NOT NULL DEFAULT 0,
    net_out_bps     BIGINT NOT NULL DEFAULT 0,
    net_in_total    BIGINT NOT NULL DEFAULT 0,
    net_out_total   BIGINT NOT NULL DEFAULT 0,
    nets            JSONB,

    -- Counters
    process_count   INTEGER NOT NULL DEFAULT 0,
    tcp_conn        INTEGER NOT NULL DEFAULT 0,
    udp_conn        INTEGER NOT NULL DEFAULT 0,

    -- Optional sensors (-1 = unavailable on this platform).
    temperature_c   DOUBLE PRECISION NOT NULL DEFAULT -1,
    gpu_pct         DOUBLE PRECISION NOT NULL DEFAULT -1,

    PRIMARY KEY (server_id, granularity, ts)
);

-- Roll-up task needs to scan each tier by ts; this index covers the query
-- `WHERE granularity=$1 AND ts >= $2 AND ts < $3`.
CREATE INDEX idx_metric_snapshots_granularity_ts
    ON metric_snapshots (granularity, ts);

-- The UI's "latest sample per server" lookup is `ORDER BY ts DESC LIMIT 1`
-- constrained by server_id + granularity; PK already covers it.
