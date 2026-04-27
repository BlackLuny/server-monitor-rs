-- Add the `panel_public_url` setting alongside `agent_endpoint`. The two
-- serve different lanes — `agent_endpoint` is the gRPC dial URL agents use,
-- `panel_public_url` is the HTTP base browsers / curl use to fetch
-- `install-agent.sh`. Splitting them lets a deployment expose gRPC on
-- :9090 and HTTP on :8080 (or behind a Caddy/Cloudfront vhost) without
-- one setting clobbering the other.
--
-- Seed empty so the install-command builder falls back to `agent_endpoint`
-- on existing installs that haven't been told about the new key yet.

INSERT INTO settings (key, value) VALUES
    ('panel_public_url', '""'::jsonb)
ON CONFLICT (key) DO NOTHING;
