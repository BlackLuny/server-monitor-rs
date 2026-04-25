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
  /** Admin-only policy fields. Null in guest responses. */
  terminal_enabled: boolean | null;
  ssh_recording: 'default' | 'on' | 'off' | null;
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
  const res = await fetch(path, {
    headers: { accept: 'application/json' },
    credentials: 'same-origin'
  });
  if (!res.ok) throw new Error(`${path}: ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

// ---------------------------------------------------------------------------
// Setup wizard
// ---------------------------------------------------------------------------

export interface SetupStatus {
  initialized: boolean;
}

export interface SetupResult {
  user_id: number;
  username: string;
  role: string;
}

export interface ApiErrorBody {
  code: string;
  message: string;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string
  ) {
    super(message);
  }
}

export function getSetupStatus(): Promise<SetupStatus> {
  return getJson<SetupStatus>('/api/setup/status');
}

export async function runSetup(username: string, password: string): Promise<SetupResult> {
  return postJson<SetupResult>('/api/setup', { username, password });
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

export interface User {
  user_id: number;
  username: string;
  role: string;
  totp_enabled: boolean;
}

export async function whoami(): Promise<User | null> {
  const res = await fetch('/api/auth/me', {
    headers: { accept: 'application/json' },
    credentials: 'same-origin'
  });
  if (res.status === 401) return null;
  if (!res.ok) throw new Error(`/api/auth/me: ${res.status}`);
  return (await res.json()) as User;
}

export async function login(
  username: string,
  password: string,
  totpCode?: string
): Promise<User> {
  return postJson<User>('/api/auth/login', {
    username,
    password,
    totp_code: totpCode ?? null
  });
}

export async function logout(): Promise<void> {
  const res = await fetch('/api/auth/logout', {
    method: 'POST',
    credentials: 'same-origin',
    // Send Origin so the CSRF middleware accepts the call when the SPA is
    // hosted under a non-default port — fetch already does this on its own
    // for cross-fetch, but spelling it out keeps tests + curl interchange
    // identical.
    headers: { origin: location.origin }
  });
  if (!res.ok && res.status !== 204) {
    throw new Error(`/api/auth/logout: ${res.status}`);
  }
}

// ---------------------------------------------------------------------------
// 2FA management
// ---------------------------------------------------------------------------

export interface TotpEnrollResponse {
  secret: string;
  otpauth_url: string;
  qr_svg_data_url: string;
}

export interface TotpConfirmResponse {
  totp_enabled: boolean;
  backup_codes: string[];
}

export async function totpEnroll(): Promise<TotpEnrollResponse> {
  return postJson<TotpEnrollResponse>('/api/auth/totp/enroll', {});
}

export async function totpConfirm(code: string): Promise<TotpConfirmResponse> {
  return postJson<TotpConfirmResponse>('/api/auth/totp/confirm', { code });
}

export async function totpDisable(password: string): Promise<void> {
  await postNoContent('/api/auth/totp/disable', { password });
}

export async function totpRegenerateBackup(password: string): Promise<{ backup_codes: string[] }> {
  return postJson<{ backup_codes: string[] }>('/api/auth/totp/regenerate-backup', { password });
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export interface SettingRow {
  key: string;
  value: unknown;
}

export function listSettings(): Promise<SettingRow[]> {
  return getJson<SettingRow[]>('/api/settings');
}

export async function putSetting<T>(key: string, value: T): Promise<SettingRow> {
  return putJson<SettingRow>(`/api/settings/${encodeURIComponent(key)}`, { value });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

export interface GroupRow {
  id: number;
  name: string;
  order_idx: number;
  description: string | null;
  color: string | null;
}

export function listGroups(): Promise<GroupRow[]> {
  return getJson<GroupRow[]>('/api/groups');
}
export function createGroup(body: {
  name: string;
  order_idx?: number;
  description?: string | null;
  color?: string | null;
}): Promise<GroupRow> {
  return postJson<GroupRow>('/api/groups', body);
}
export function updateGroup(id: number, body: Partial<GroupRow>): Promise<GroupRow> {
  return patchJson<GroupRow>(`/api/groups/${id}`, body);
}
export function deleteGroup(id: number): Promise<void> {
  return deleteNoBody(`/api/groups/${id}`);
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

export interface AdminUser {
  id: number;
  username: string;
  role: string;
  totp_enabled: boolean;
  created_at: string;
}

export function listUsers(): Promise<AdminUser[]> {
  return getJson<AdminUser[]>('/api/users');
}
export function createUser(username: string, password: string): Promise<AdminUser> {
  return postJson<AdminUser>('/api/users', { username, password });
}
export function deleteUser(id: number): Promise<void> {
  return deleteNoBody(`/api/users/${id}`);
}
export function resetUserPassword(id: number, password: string): Promise<void> {
  return putNoContent(`/api/users/${id}/password`, { password });
}
export function changeOwnPassword(current: string, fresh: string): Promise<void> {
  return putNoContent('/api/auth/password', {
    current_password: current,
    new_password: fresh
  });
}

// ---------------------------------------------------------------------------
// Servers (admin actions)
// ---------------------------------------------------------------------------

export interface CreatedServer {
  id: number;
  agent_id: string;
  display_name: string;
  join_token: string;
  install_command: string;
}

export function adminCreateServer(body: {
  display_name: string;
  group_id?: number | null;
  hidden_from_guest?: boolean;
  location?: string | null;
  flag_emoji?: string | null;
}): Promise<CreatedServer> {
  return postJson<CreatedServer>('/api/servers', body);
}

export function updateServer(id: number, body: Record<string, unknown>): Promise<unknown> {
  return patchJson<unknown>(`/api/servers/${id}`, body);
}

export function deleteServer(id: number): Promise<void> {
  return deleteNoBody(`/api/servers/${id}`);
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

export type ProbeKind = 'icmp' | 'tcp' | 'http';

export interface ProbeRow {
  id: number;
  name: string;
  kind: ProbeKind;
  target: string;
  port: number | null;
  interval_s: number;
  timeout_ms: number;
  http_method: string | null;
  http_expect_code: number | null;
  http_expect_body: string | null;
  default_enabled: boolean;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface ProbeAgentRow {
  agent_id: string;
  display_name: string;
  default_enabled: boolean;
  override_enabled: boolean | null;
  effective_enabled: boolean;
}

export interface ProbeResultPoint {
  ts: string;
  agent_id: string;
  ok: boolean;
  latency_us: number;
  latency_us_p50: number | null;
  latency_us_p95: number | null;
  success_rate: number | null;
  status_code: number | null;
  error: string | null;
}

export interface ProbeResultsSeries {
  probe_id: number;
  range: string;
  granularity: 'raw' | 'm1' | 'm5' | 'h1';
  points: ProbeResultPoint[];
}

export function listProbes(): Promise<ProbeRow[]> {
  return getJson<ProbeRow[]>('/api/probes');
}

export function createProbe(body: Partial<ProbeRow>): Promise<ProbeRow> {
  return postJson<ProbeRow>('/api/probes', body);
}

export function updateProbe(id: number, body: Partial<ProbeRow>): Promise<ProbeRow> {
  return patchJson<ProbeRow>(`/api/probes/${id}`, body);
}

export function deleteProbe(id: number): Promise<void> {
  return deleteNoBody(`/api/probes/${id}`);
}

export function listProbeAgents(id: number): Promise<ProbeAgentRow[]> {
  return getJson<ProbeAgentRow[]>(`/api/probes/${id}/agents`);
}

export function setProbeOverride(
  probeId: number,
  agentId: string,
  enabled: boolean | null
): Promise<void> {
  return putNoContent(`/api/probes/${probeId}/agents/${agentId}`, { enabled });
}

export function fetchProbeResults(
  id: number,
  range: string,
  agentId?: string
): Promise<ProbeResultsSeries> {
  const params = new URLSearchParams({ range });
  if (agentId) params.set('agent_id', agentId);
  return getJson<ProbeResultsSeries>(`/api/probes/${id}/results?${params}`);
}

export interface AgentRow {
  agent_id: string;
  display_name: string;
  online: boolean;
  group_name: string | null;
}

export function listAgents(): Promise<AgentRow[]> {
  return getJson<AgentRow[]>('/api/agents');
}

// ---------------------------------------------------------------------------
// Updates / rollouts (admin only)
// ---------------------------------------------------------------------------

export interface ReleaseAsset {
  name: string;
  url: string;
  size: number;
  sha256: string;
}

export interface LatestRelease {
  tag: string;
  name: string | null;
  html_url: string | null;
  prerelease: boolean;
  published_at: string;
  fetched_at: string;
  assets: ReleaseAsset[];
}

export interface RolloutSummary {
  id: number;
  version: string;
  state: 'pending' | 'active' | 'paused' | 'completed' | 'aborted';
  percent: number;
  created_by: number | null;
  created_at: string;
  note: string | null;
  assignments_total: number;
  assignments_pending: number;
  assignments_sent: number;
  assignments_succeeded: number;
  assignments_failed: number;
}

export interface AssignmentView {
  agent_id: string;
  display_name: string;
  target: string;
  state: 'pending' | 'sent' | 'succeeded' | 'failed';
  last_status_message: string | null;
  updated_at: string;
}

export interface RolloutView {
  summary: RolloutSummary;
  assignments: AssignmentView[];
}

export interface CreateRolloutInput {
  version: string;
  percent?: number;
  agent_ids?: string[];
  note?: string;
}

export function getLatestRelease(): Promise<LatestRelease | null> {
  return getJson<LatestRelease | null>('/api/updates/latest');
}

export function listRecentReleases(): Promise<LatestRelease[]> {
  return getJson<LatestRelease[]>('/api/updates/recent');
}

export function listRollouts(): Promise<RolloutSummary[]> {
  return getJson<RolloutSummary[]>('/api/updates/rollouts');
}

export function getRollout(id: number): Promise<RolloutView> {
  return getJson<RolloutView>(`/api/updates/rollouts/${id}`);
}

export function createRollout(body: CreateRolloutInput): Promise<RolloutView> {
  return postJson<RolloutView>('/api/updates/rollouts', body);
}

export function pauseRollout(id: number): Promise<void> {
  return postNoContent(`/api/updates/rollouts/${id}/pause`, {});
}
export function resumeRollout(id: number): Promise<void> {
  return postNoContent(`/api/updates/rollouts/${id}/resume`, {});
}
export function abortRollout(id: number): Promise<void> {
  return postNoContent(`/api/updates/rollouts/${id}/abort`, {});
}

// ---------------------------------------------------------------------------
// Terminal sessions + recordings
// ---------------------------------------------------------------------------

export interface TerminalSessionRow {
  id: string;
  server_id: number;
  user_id: number | null;
  username: string | null;
  opened_at: string;
  closed_at: string | null;
  exit_code: number | null;
  error: string | null;
  recording_path: string | null;
  recording_size: number | null;
  recording_sha256: string | null;
  client_ip: string | null;
}

export function listTerminalSessions(serverId: number): Promise<TerminalSessionRow[]> {
  return getJson<TerminalSessionRow[]>(`/api/servers/${serverId}/terminal-sessions`);
}

/** Build the URL for streaming a recording. The browser's normal cookie
 *  auth applies; the panel proxies to the agent over the existing gRPC
 *  channel. */
export function recordingDownloadUrl(sessionId: string): string {
  return `/api/recordings/${sessionId}/download`;
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

export interface AuditRow {
  id: number;
  user_id: number | null;
  username: string | null;
  action: string;
  target: string | null;
  ip: string | null;
  user_agent: string | null;
  ts: string;
}

export function listAudit(limit = 100): Promise<AuditRow[]> {
  return getJson<AuditRow[]>(`/api/audit?limit=${limit}`);
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

async function postJson<T>(path: string, body: unknown): Promise<T> {
  return fetchJson<T>('POST', path, body);
}
async function patchJson<T>(path: string, body: unknown): Promise<T> {
  return fetchJson<T>('PATCH', path, body);
}
async function putJson<T>(path: string, body: unknown): Promise<T> {
  return fetchJson<T>('PUT', path, body);
}
async function fetchJson<T>(method: string, path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: {
      'content-type': 'application/json',
      accept: 'application/json',
      origin: location.origin
    },
    credentials: 'same-origin',
    body: JSON.stringify(body)
  });
  if (!res.ok) {
    const err = (await res.json().catch(() => null)) as ApiErrorBody | null;
    throw new ApiError(res.status, err?.code ?? 'unknown', err?.message ?? res.statusText);
  }
  return (await res.json()) as T;
}
async function postNoContent(path: string, body: unknown): Promise<void> {
  return fetchNoContent('POST', path, body);
}
async function putNoContent(path: string, body: unknown): Promise<void> {
  return fetchNoContent('PUT', path, body);
}
async function deleteNoBody(path: string): Promise<void> {
  return fetchNoContent('DELETE', path, undefined);
}
async function fetchNoContent(method: string, path: string, body: unknown): Promise<void> {
  const res = await fetch(path, {
    method,
    headers: body
      ? { 'content-type': 'application/json', origin: location.origin }
      : { origin: location.origin },
    credentials: 'same-origin',
    body: body !== undefined ? JSON.stringify(body) : undefined
  });
  if (!res.ok && res.status !== 204) {
    const err = (await res.json().catch(() => null)) as ApiErrorBody | null;
    throw new ApiError(res.status, err?.code ?? 'unknown', err?.message ?? res.statusText);
  }
}

export function listServers(opts: { guest?: boolean } = {}): Promise<ServerList> {
  const q = opts.guest ? '?guest=true' : '';
  return getJson<ServerList>(`/api/servers${q}`);
}

/** Per-server fixed-length sparkline windows. Used to seed the dashboard
 *  cards with real history at page-load time so the line isn't a flat
 *  seed-and-grow. Server returns one row per server with values ordered
 *  oldest → newest. */
export interface SparklineRow {
  server_id: number;
  cpu_pct: number[];
  mem_pct: number[];
  net_in_bps: number[];
  net_out_bps: number[];
}

export function fetchSparklines(seconds = 60): Promise<SparklineRow[]> {
  return getJson<SparklineRow[]>(`/api/servers/sparklines?seconds=${seconds}`);
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
