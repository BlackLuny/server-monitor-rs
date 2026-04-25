<!--
  Server detail view. Shows hardware, current state, and time-series
  charts for CPU / memory / network over a selectable range.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { page } from '$app/state';
  import {
    Badge,
    Panel,
    SectionHeader,
    StatusDot,
    TimeSeriesChart
  } from '$lib/primitives';
  import {
    fetchMetrics,
    listServers,
    subscribeLive,
    type LiveUpdate,
    type MetricsSeries,
    type ServerRow
  } from '$lib/api';
  import {
    ageFromIso,
    bitsPerSec,
    bytes,
    percent,
    usagePct
  } from '$lib/format';
  import { authStore } from '$lib/auth.svelte';

  const RANGES = [
    { key: '1h', label: '1 hour' },
    { key: '6h', label: '6 hours' },
    { key: '24h', label: '24 hours' },
    { key: '7d', label: '7 days' },
    { key: '30d', label: '30 days' }
  ] as const;

  type RangeKey = (typeof RANGES)[number]['key'];

  let server = $state<ServerRow | null>(null);
  let series = $state<MetricsSeries | null>(null);
  let rangeKey = $state<RangeKey>('1h');
  let error = $state<string | null>(null);
  let loading = $state(true);
  let nowTick = $state(Date.now());

  let liveUnsub: (() => void) | null = null;
  let poll: ReturnType<typeof setInterval> | null = null;
  let agePoll: ReturnType<typeof setInterval> | null = null;

  const serverId = $derived(Number(page.params.id));

  async function loadServer() {
    const all = await listServers();
    server = all.servers.find((s) => s.id === serverId) ?? null;
  }

  async function loadMetrics() {
    try {
      series = await fetchMetrics(serverId, rangeKey);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  onMount(async () => {
    try {
      await Promise.all([loadServer(), loadMetrics()]);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }

    liveUnsub = subscribeLive((u: LiveUpdate) => {
      if (u.server_id !== serverId) return;
      // Patch the live state so header stats stay current; the chart is
      // redrawn every 10s via the poll below (smoother than per-sample
      // reshape for long ranges).
      if (!server) return;
      const next: ServerRow = {
        ...server,
        online: true,
        last_seen_at: u.ts,
        latest: {
          ts: u.ts,
          cpu_pct: u.cpu_pct,
          mem_used: u.mem_used,
          mem_total: u.mem_total,
          swap_used: server.latest?.swap_used ?? 0,
          swap_total: server.latest?.swap_total ?? 0,
          load_1: u.load_1,
          disk_used: server.latest?.disk_used ?? 0,
          disk_total: server.latest?.disk_total ?? 0,
          net_in_bps: u.net_in_bps,
          net_out_bps: u.net_out_bps,
          process_count: server.latest?.process_count ?? 0
        }
      };
      server = next;
    });

    // 10s chart refresh. For long ranges raw/m1/m5 buckets don't change
    // faster than that anyway; for the 1h view it keeps the tail fresh.
    poll = setInterval(loadMetrics, 10_000);
    agePoll = setInterval(() => (nowTick = Date.now()), 1000);
  });

  onDestroy(() => {
    liveUnsub?.();
    if (poll) clearInterval(poll);
    if (agePoll) clearInterval(agePoll);
  });

  async function selectRange(key: RangeKey) {
    rangeKey = key;
    await loadMetrics();
  }

  // --- chart data derivation ---

  const timestamps = $derived.by(() => {
    if (!series) return [] as number[];
    return series.points.map((p) => Math.floor(new Date(p.ts).getTime() / 1000));
  });
  const cpuValues = $derived.by(() => series?.points.map((p) => p.cpu_pct) ?? []);
  const memPctValues = $derived.by(
    () => series?.points.map((p) => usagePct(p.mem_used, p.mem_total)) ?? []
  );
  const netInValues = $derived.by(() => series?.points.map((p) => p.net_in_bps) ?? []);
  const netOutValues = $derived.by(() => series?.points.map((p) => p.net_out_bps) ?? []);
  const loadValues = $derived.by(() => series?.points.map((p) => p.load_1) ?? []);

  function color(token: string) {
    // Resolve CSS custom property to a real string so uPlot's canvas API
    // can paint with it. Safe inside a browser-only SPA.
    return getComputedStyle(document.documentElement).getPropertyValue(token).trim() || token;
  }
</script>

<svelte:head>
  <title>{server?.display_name ?? 'server'} · server-monitor</title>
</svelte:head>

<div class="min-h-screen">
  <!-- top bar -->
  <header class="border-b border-border">
    <div class="mx-auto flex max-w-6xl items-center justify-between gap-4 px-6 py-3">
      <a
        href="/"
        class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary hover:text-fg"
      >
        ← overview
      </a>
      <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
        {server?.display_name ?? '…'}
      </span>
      {#if authStore.state.user?.role === 'admin' && server?.online}
        <a
          href={`/servers/${serverId}/terminal`}
          class="inline-flex h-7 items-center rounded border border-border px-3 font-mono text-2xs uppercase tracking-wider text-fg-secondary transition-colors hover:border-border-strong hover:text-fg"
        >
          open terminal
        </a>
      {:else}
        <button
          class="inline-flex h-7 items-center rounded border border-border px-3 font-mono text-2xs uppercase tracking-wider text-fg-quaternary"
          disabled
          title={authStore.state.user?.role === 'admin' ? 'agent offline' : 'admin only'}
        >
          open terminal
        </button>
      {/if}
    </div>
  </header>

  <main class="mx-auto max-w-6xl px-6 py-6">
    {#if error}
      <div
        class="rounded border px-4 py-3 text-sm text-error"
        style:background="color-mix(in oklch, var(--status-error) 6%, transparent)"
        style:border-color="color-mix(in oklch, var(--status-error) 28%, transparent)"
      >
        {error}
      </div>
    {:else if loading || !server}
      <div class="py-20 text-center font-mono text-xs text-fg-tertiary">loading…</div>
    {:else}
      <!-- ── Header block ── -->
      <section class="mb-6 flex flex-col gap-6 md:flex-row md:items-start md:justify-between">
        <div class="min-w-0">
          <div class="flex items-center gap-3">
            <StatusDot kind={server.online ? 'online' : 'error'} size={10} />
            <h1 class="text-xl font-medium tracking-tight">{server.display_name}</h1>
            <Badge tone={server.online ? 'online' : 'error'}>
              {server.online ? 'online' : 'offline'}
            </Badge>
            {#each (server.tags ?? []).slice(0, 3) as tag}
              <Badge tone="neutral">{tag}</Badge>
            {/each}
          </div>
          <div class="mt-1 flex items-center gap-1.5 font-mono text-xs text-fg-tertiary">
            <span>{server.hardware?.os ?? '—'}{server.hardware?.arch ? ` · ${server.hardware.arch}` : ''}</span>
            <span class="text-fg-quaternary">·</span>
            <span>{server.hardware?.cpu_model ?? '—'}</span>
            {#if server.hardware?.cpu_cores}
              <span class="text-fg-quaternary">·</span>
              <span>{server.hardware.cpu_cores}c</span>
            {/if}
            {#if server.hardware?.mem_bytes}
              <span class="text-fg-quaternary">·</span>
              <span>{bytes(server.hardware.mem_bytes)} RAM</span>
            {/if}
            {#if server.hardware?.disk_bytes}
              <span class="text-fg-quaternary">·</span>
              <span>{bytes(server.hardware.disk_bytes)} disk</span>
            {/if}
          </div>
          <div class="mt-1 font-mono text-2xs text-fg-quaternary">
            agent {server.agent_version ?? '—'} · last seen {ageFromIso(server.last_seen_at, nowTick)}
          </div>
        </div>

        <!-- range switcher -->
        <div class="flex gap-1 self-start rounded border border-border bg-elev-1 p-0.5">
          {#each RANGES as r}
            <button
              onclick={() => selectRange(r.key)}
              class="rounded-xs px-2.5 py-1 font-mono text-2xs uppercase tracking-wider transition-colors duration-100
                     {rangeKey === r.key
                ? 'bg-elev-2 text-fg'
                : 'text-fg-tertiary hover:text-fg-secondary'}"
            >
              {r.key}
            </button>
          {/each}
        </div>
      </section>

      <!-- ── Charts ── -->
      <section class="space-y-4">
        <Panel padded={false}>
          <div class="flex items-center justify-between px-4 py-2.5">
            <SectionHeader label="cpu" />
            <span class="font-mono text-2xs text-fg-tertiary">
              {server.latest ? percent(server.latest.cpu_pct) + '%' : '—'} now
            </span>
          </div>
          <div class="px-2 pb-3">
            <TimeSeriesChart
              timestamps={timestamps}
              series={[{ name: 'cpu', color: color('--data-1'), unit: '%', values: cpuValues }]}
              min={0}
              max={100}
              formatY={(v) => `${v.toFixed(0)}%`}
            />
          </div>
        </Panel>

        <Panel padded={false}>
          <div class="flex items-center justify-between px-4 py-2.5">
            <SectionHeader label="memory" />
            <span class="font-mono text-2xs text-fg-tertiary">
              {server.latest
                ? `${bytes(server.latest.mem_used)} / ${bytes(server.latest.mem_total)}`
                : '—'}
            </span>
          </div>
          <div class="px-2 pb-3">
            <TimeSeriesChart
              timestamps={timestamps}
              series={[
                { name: 'mem %', color: color('--data-2'), unit: '%', values: memPctValues }
              ]}
              min={0}
              max={100}
              formatY={(v) => `${v.toFixed(0)}%`}
            />
          </div>
        </Panel>

        <Panel padded={false}>
          <div class="flex items-center justify-between px-4 py-2.5">
            <SectionHeader label="network" />
            <span class="font-mono text-2xs text-fg-tertiary">
              {server.latest ? bitsPerSec(server.latest.net_in_bps) : '—'} in ·
              {server.latest ? bitsPerSec(server.latest.net_out_bps) : '—'} out
            </span>
          </div>
          <div class="px-2 pb-3">
            <TimeSeriesChart
              timestamps={timestamps}
              series={[
                { name: 'in', color: color('--data-3'), values: netInValues },
                { name: 'out', color: color('--data-4'), values: netOutValues }
              ]}
              formatY={bytes}
            />
          </div>
        </Panel>

        <Panel padded={false}>
          <div class="flex items-center justify-between px-4 py-2.5">
            <SectionHeader label="load" />
            <span class="font-mono text-2xs text-fg-tertiary">
              {server.latest ? server.latest.load_1.toFixed(2) : '—'}
            </span>
          </div>
          <div class="px-2 pb-3">
            <TimeSeriesChart
              timestamps={timestamps}
              series={[{ name: 'load_1', color: color('--data-5'), values: loadValues }]}
              min={0}
              formatY={(v) => v.toFixed(1)}
            />
          </div>
        </Panel>
      </section>
    {/if}
  </main>
</div>
