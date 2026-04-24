// REST + WebSocket client for the panel.
//
// All fetches go through relative paths so Vite's dev proxy + SvelteKit's
// static build both hit the panel on the same origin.

export interface ServerRow {
  id: number;
  agent_id: string;
  display_name: string;
  group_id: number | null;
  group_name: string | null;
  tags: string[];
  hidden_from_guest: boolean;
  last_seen_at: string | null;
  online: boolean;
  hardware: Hardware | null;
  latest: LatestSample | null;
  agent_version: string | null;
  location: string | null;
  flag_emoji: string | null;
}

export interface Hardware {
  os: string | null;
  os_version: string | null;
  kernel: string | null;
  arch: string | null;
  cpu_model: string | null;
  cpu_cores: number | null;
  mem_bytes: number | null;
  swap_bytes: number | null;
  disk_bytes: number | null;
  virtualization: string | null;
}

export interface LatestSample {
  ts: string;
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
  swap_used: number;
  swap_total: number;
  load_1: number;
  disk_used: number;
  disk_total: number;
  net_in_bps: number;
  net_out_bps: number;
  process_count: number;
}

export interface ServerList {
  servers: ServerRow[];
  updated_at: string;
}

export interface MetricPoint {
  ts: string;
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
  swap_used: number;
  swap_total: number;
  load_1: number;
  load_5: number;
  load_15: number;
  disk_used: number;
  disk_total: number;
  net_in_bps: number;
  net_out_bps: number;
  process_count: number;
  tcp_conn: number;
  udp_conn: number;
  temperature_c: number;
}

export interface MetricsSeries {
  server_id: number;
  range: string;
  granularity: 'raw' | 'm1' | 'm5' | 'h1';
  points: MetricPoint[];
}

export interface LiveUpdate {
  type: 'metric';
  server_id: number;
  hidden_from_guest: boolean;
  ts: string;
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
  net_in_bps: number;
  net_out_bps: number;
  load_1: number;
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: 'application/json' } });
  if (!res.ok) throw new Error(`${path}: ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

export function listServers(opts: { guest?: boolean } = {}): Promise<ServerList> {
  const q = opts.guest ? '?guest=true' : '';
  return getJson<ServerList>(`/api/servers${q}`);
}

export function fetchMetrics(serverId: number, range: string): Promise<MetricsSeries> {
  return getJson<MetricsSeries>(
    `/api/servers/${serverId}/metrics?range=${encodeURIComponent(range)}`
  );
}

/**
 * Subscribe to live metric updates. Reconnects automatically with expon.
 * backoff; surfaces updates to `onMessage`. Returns a disposer the caller
 * invokes to stop reconnecting + close the current socket.
 */
export function subscribeLive(
  onMessage: (u: LiveUpdate) => void,
  opts: { guest?: boolean } = {}
): () => void {
  let closed = false;
  let ws: WebSocket | null = null;
  let backoff = 1000;

  const connect = () => {
    if (closed) return;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const q = opts.guest ? '?guest=true' : '';
    const url = `${proto}//${location.host}/ws/live${q}`;
    ws = new WebSocket(url);

    ws.addEventListener('message', (ev) => {
      try {
        const payload = JSON.parse(ev.data) as LiveUpdate;
        onMessage(payload);
      } catch {
        /* ignore malformed frames */
      }
    });
    ws.addEventListener('open', () => {
      backoff = 1000;
    });
    ws.addEventListener('close', () => {
      if (closed) return;
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 30_000);
    });
    ws.addEventListener('error', () => {
      try {
        ws?.close();
      } catch {
        /* noop */
      }
    });
  };

  connect();

  return () => {
    closed = true;
    ws?.close();
  };
}
