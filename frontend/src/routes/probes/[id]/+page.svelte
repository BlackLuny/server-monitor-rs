<!--
  Probe detail. Top: latency time-series with an agent filter (so multi-agent
  views don't get crowded). Bottom: per-agent override matrix — admin can
  toggle each agent on/off relative to the probe default.
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { TimeSeriesChart } from '$lib/primitives';
  import { authStore } from '$lib/auth.svelte';
  import {
    deleteProbe,
    fetchProbeResults,
    listProbeAgents,
    listProbes,
    setProbeOverride,
    updateProbe,
    type ProbeAgentRow,
    type ProbeResultsSeries,
    type ProbeRow
  } from '$lib/api';

  const id = $derived(Number($page.params.id));
  const isAdmin = $derived(authStore.state.user?.role === 'admin');

  let probe = $state<ProbeRow | null>(null);
  let agents = $state<ProbeAgentRow[]>([]);
  let series = $state<ProbeResultsSeries | null>(null);
  let range = $state('1h');
  let agentFilter = $state<string | undefined>(undefined);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  const ranges = ['1h', '6h', '24h', '7d', '30d'];

  async function loadAll() {
    try {
      const [all, ag, rs] = await Promise.all([
        listProbes(),
        listProbeAgents(id),
        fetchProbeResults(id, range, agentFilter)
      ]);
      probe = all.find((p) => p.id === id) ?? null;
      agents = ag;
      series = rs;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    loadAll();
    // Light polling so the chart updates without manual refresh.
    pollHandle = setInterval(() => {
      fetchProbeResults(id, range, agentFilter)
        .then((rs) => {
          series = rs;
        })
        .catch(() => {});
    }, 30_000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  $effect(() => {
    void range;
    void agentFilter;
    if (!loading) {
      fetchProbeResults(id, range, agentFilter).then((rs) => (series = rs));
    }
  });

  async function toggleOverride(row: ProbeAgentRow) {
    if (!isAdmin) return;
    const current = row.effective_enabled;
    try {
      // If we're currently aligned with default, set explicit override to flip;
      // otherwise clear (snap back to default by sending the default value).
      const nextWanted = !current;
      await setProbeOverride(id, row.agent_id, nextWanted);
      agents = await listProbeAgents(id);
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function clearOverride(row: ProbeAgentRow) {
    if (!isAdmin) return;
    try {
      await setProbeOverride(id, row.agent_id, null);
      agents = await listProbeAgents(id);
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function toggleDefault() {
    if (!probe || !isAdmin) return;
    try {
      probe = await updateProbe(id, { default_enabled: !probe.default_enabled });
      agents = await listProbeAgents(id);
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function toggleEnabled() {
    if (!probe || !isAdmin) return;
    try {
      probe = await updateProbe(id, { enabled: !probe.enabled });
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleDelete() {
    if (!probe || !isAdmin) return;
    if (!confirm(`Delete probe "${probe.name}"?`)) return;
    try {
      await deleteProbe(id);
      await goto('/probes');
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  const PALETTE = [
    'var(--data-1)',
    'var(--data-2)',
    'var(--data-3)',
    'var(--data-4)',
    'var(--data-5)'
  ];

  // Build chart data: one series per agent_id, latency in milliseconds.
  // We use a unified time axis across agents — gaps where an agent has no
  // measurement become null so uPlot draws a break instead of stitching
  // misleadingly across.
  const chartData = $derived.by(() => {
    if (!series || !series.points.length) return null;
    const byAgentLatency = new Map<string, Map<number, number | null>>();
    const byAgentLoss = new Map<string, Map<number, number | null>>();
    const allTs = new Set<number>();
    for (const p of series.points) {
      const epoch = Math.floor(new Date(p.ts).getTime() / 1000);
      allTs.add(epoch);

      let lat = byAgentLatency.get(p.agent_id);
      if (!lat) {
        lat = new Map();
        byAgentLatency.set(p.agent_id, lat);
      }
      lat.set(epoch, p.ok ? p.latency_us / 1000 : null);

      // Loss rate: prefer the rolled-up success_rate (m1/m5/h1 buckets carry
      // a real fraction); fall back to the boolean for raw granularity so a
      // failure shows as a 100% spike.
      let loss = byAgentLoss.get(p.agent_id);
      if (!loss) {
        loss = new Map();
        byAgentLoss.set(p.agent_id, loss);
      }
      const lossPct =
        p.success_rate !== null && p.success_rate !== undefined
          ? Math.max(0, Math.min(100, (1 - p.success_rate) * 100))
          : p.ok
          ? 0
          : 100;
      loss.set(epoch, lossPct);
    }
    const timestamps = Array.from(allTs).sort((a, b) => a - b);
    const ids = Array.from(byAgentLatency.keys()).sort();
    if (!ids.length) return null;
    const latencySeries = ids.map((aid, idx) => ({
      name: agentName(aid),
      color: PALETTE[idx % PALETTE.length],
      values: timestamps.map((t) => byAgentLatency.get(aid)!.get(t) ?? null) as (
        | number
        | null
      )[],
      unit: 'ms'
    }));
    const lossSeries = ids.map((aid, idx) => ({
      name: agentName(aid),
      color: PALETTE[idx % PALETTE.length],
      values: timestamps.map((t) => byAgentLoss.get(aid)!.get(t) ?? null) as (
        | number
        | null
      )[],
      unit: '%'
    }));
    return { timestamps, latencySeries, lossSeries };
  });

  // Per-agent summary across the current window — total samples, loss
  // rate, mean / p50 / p95 latency. Computed client-side from the same
  // points the chart uses so the numbers can't drift from the curves.
  interface AgentSummary {
    agent_id: string;
    samples: number;
    failures: number;
    loss_pct: number;
    mean_ms: number | null;
    p50_ms: number | null;
    p95_ms: number | null;
  }

  const summary = $derived.by<AgentSummary[]>(() => {
    if (!series || !series.points.length) return [];
    const groups = new Map<string, { ok_lat_us: number[]; failures: number; total: number }>();
    for (const p of series.points) {
      let g = groups.get(p.agent_id);
      if (!g) {
        g = { ok_lat_us: [], failures: 0, total: 0 };
        groups.set(p.agent_id, g);
      }
      g.total += 1;
      if (p.ok) {
        g.ok_lat_us.push(p.latency_us);
      } else {
        g.failures += 1;
      }
    }
    const out: AgentSummary[] = [];
    for (const [agent_id, g] of groups) {
      const sorted = g.ok_lat_us.slice().sort((a, b) => a - b);
      const mean_ms = sorted.length
        ? sorted.reduce((a, b) => a + b, 0) / sorted.length / 1000
        : null;
      const p50_ms = sorted.length ? sorted[Math.floor(sorted.length * 0.5)] / 1000 : null;
      const p95_ms = sorted.length
        ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))] / 1000
        : null;
      out.push({
        agent_id,
        samples: g.total,
        failures: g.failures,
        loss_pct: g.total > 0 ? (g.failures / g.total) * 100 : 0,
        mean_ms,
        p50_ms,
        p95_ms
      });
    }
    out.sort((a, b) => agentName(a.agent_id).localeCompare(agentName(b.agent_id)));
    return out;
  });

  function agentName(id: string): string {
    const a = agents.find((r) => r.agent_id === id);
    return a ? a.display_name : id.slice(0, 8);
  }

  function fmtMs(v: number | null): string {
    if (v == null) return '—';
    if (v < 1) return `${(v * 1000).toFixed(0)}µs`;
    return `${v.toFixed(1)}ms`;
  }
</script>

<svelte:head>
  <title>{probe?.name ?? 'probe'} · server-monitor</title>
</svelte:head>

<div>
  <div class="border-b border-border">
    <div class="mx-auto max-w-screen-2xl px-6 py-3">
      <div class="flex items-center justify-between gap-4">
        <div>
          <a
            href="/probes"
            class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
          >
            ← all probes
          </a>
          {#if probe}
            <h1 class="mt-2 text-md font-medium tracking-tight">
              {probe.name}
              <span class="ml-2 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
                {probe.kind} · {probe.target}{probe.kind === 'tcp' && probe.port
                  ? `:${probe.port}`
                  : ''}
              </span>
            </h1>
          {/if}
        </div>
        {#if isAdmin && probe}
          <div class="flex gap-2">
            <button
              type="button"
              onclick={toggleEnabled}
              class="rounded border border-border px-3 py-1 font-mono text-2xs uppercase tracking-[0.14em] hover:bg-elev-2"
            >
              {probe.enabled ? 'disable globally' : 'enable globally'}
            </button>
            <button
              type="button"
              onclick={handleDelete}
              class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-quaternary hover:text-error"
            >
              delete
            </button>
          </div>
        {/if}
      </div>
    </div>
  </div>

  <main class="mx-auto max-w-screen-2xl px-6 py-6 space-y-6">
    {#if error}
      <div
        class="rounded border border-border bg-recess px-4 py-3 font-mono text-xs"
        style="color: var(--status-error)"
      >
        {error}
      </div>
    {/if}

    <!-- Range / agent filter (shared by all charts below) -->
    <section class="flex items-center justify-end gap-2">
      <select
        bind:value={agentFilter}
        class="rounded border border-border bg-recess px-2 py-1 font-mono text-2xs text-fg"
      >
        <option value={undefined}>all agents</option>
        {#each agents as a}
          <option value={a.agent_id}>{a.display_name}</option>
        {/each}
      </select>
      <div class="flex gap-1">
        {#each ranges as r}
          <button
            type="button"
            onclick={() => (range = r)}
            class="rounded px-2 py-1 font-mono text-2xs uppercase tracking-[0.12em]"
            class:text-fg={range === r}
            class:text-fg-tertiary={range !== r}
            style:background={range === r ? 'var(--bg-elev-2)' : 'transparent'}
          >
            {r}
          </button>
        {/each}
      </div>
    </section>

    <!-- Per-agent summary across the current window -->
    {#if summary.length}
      <section class="grid gap-3" style:grid-template-columns="repeat(auto-fit, minmax(220px, 1fr))">
        {#each summary as s, idx (s.agent_id)}
          {@const palette = ['var(--data-1)', 'var(--data-2)', 'var(--data-3)', 'var(--data-4)', 'var(--data-5)']}
          <div class="rounded border border-border bg-elev-1 p-3">
            <div class="flex items-center gap-2">
              <span class="inline-block h-2 w-2 rounded-full" style:background={palette[idx % palette.length]}></span>
              <span class="truncate font-mono text-xs text-fg">{agentName(s.agent_id)}</span>
            </div>
            <div class="mt-2 grid grid-cols-2 gap-y-1 font-mono text-2xs text-fg-tertiary">
              <span>samples</span>
              <span class="text-right text-fg">{s.samples}</span>
              <span>loss</span>
              <span
                class="text-right"
                class:text-fg={s.loss_pct === 0}
                class:text-warning={s.loss_pct > 0 && s.loss_pct < 5}
                class:text-error={s.loss_pct >= 5}
              >
                {s.loss_pct.toFixed(s.loss_pct < 1 ? 2 : 1)}%
                {#if s.failures > 0}
                  <span class="text-fg-quaternary">· {s.failures} fail</span>
                {/if}
              </span>
              <span>mean</span>
              <span class="text-right text-fg">{fmtMs(s.mean_ms)}</span>
              <span>p50</span>
              <span class="text-right text-fg-secondary">{fmtMs(s.p50_ms)}</span>
              <span>p95</span>
              <span class="text-right text-fg-secondary">{fmtMs(s.p95_ms)}</span>
            </div>
          </div>
        {/each}
      </section>
    {/if}

    <!-- Latency chart -->
    <section class="rounded border border-border bg-elev-1 p-4">
      <div class="mb-3 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
        latency · last {range}
      </div>
      {#if chartData}
        <TimeSeriesChart
          timestamps={chartData.timestamps}
          series={chartData.latencySeries}
          height={260}
          formatY={(v) => (v < 1 ? `${(v * 1000).toFixed(0)}µs` : `${v.toFixed(1)}ms`)}
        />
      {:else if loading}
        <div class="py-12 text-center font-mono text-xs text-fg-tertiary">loading…</div>
      {:else}
        <div class="py-12 text-center font-mono text-xs text-fg-tertiary">
          no data in this window
        </div>
      {/if}
    </section>

    <!-- Loss-rate chart -->
    {#if chartData}
      <section class="rounded border border-border bg-elev-1 p-4">
        <div class="mb-3 flex items-center justify-between font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
          <span>loss · last {range}</span>
          <span class="text-fg-quaternary">
            {#if range === '1h'}raw — failures spike to 100%{:else}aggregated{/if}
          </span>
        </div>
        <TimeSeriesChart
          timestamps={chartData.timestamps}
          series={chartData.lossSeries}
          height={180}
          min={0}
          max={100}
          formatY={(v) => `${v.toFixed(v < 1 ? 2 : 0)}%`}
        />
      </section>
    {/if}

    <!-- Per-agent matrix -->
    <section class="rounded border border-border bg-elev-1 p-4">
      <div class="mb-3 flex items-center justify-between">
        <div>
          <h2 class="text-md font-medium">Agent assignment</h2>
          <p class="text-2xs text-fg-tertiary mt-0.5">
            {#if probe}
              Default for new agents:
              {#if probe.default_enabled}
                <span class="text-online">enabled</span>
              {:else}
                <span class="text-fg-quaternary">disabled</span>
              {/if}
            {/if}
          </p>
        </div>
        {#if isAdmin && probe}
          <button
            type="button"
            onclick={toggleDefault}
            class="rounded border border-border px-3 py-1 font-mono text-2xs uppercase tracking-[0.14em] hover:bg-elev-2"
          >
            flip default
          </button>
        {/if}
      </div>

      <div class="overflow-hidden rounded border border-border">
        <table class="w-full text-sm">
          <thead class="bg-elev-2">
            <tr>
              <th
                class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >agent</th
              >
              <th
                class="w-32 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >state</th
              >
              <th
                class="w-32 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >source</th
              >
              <th class="px-4 py-2 text-right"></th>
            </tr>
          </thead>
          <tbody>
            {#each agents as a (a.agent_id)}
              <tr class="border-t border-border">
                <td class="px-4 py-2 font-mono">{a.display_name}</td>
                <td class="px-4 py-2 font-mono text-2xs">
                  {#if a.effective_enabled}
                    <span class="text-online">enabled</span>
                  {:else}
                    <span class="text-fg-quaternary">disabled</span>
                  {/if}
                </td>
                <td class="px-4 py-2 font-mono text-2xs text-fg-tertiary">
                  {a.override_enabled === null ? 'inherits default' : 'override'}
                </td>
                <td class="px-4 py-2 text-right">
                  {#if isAdmin}
                    <button
                      type="button"
                      onclick={() => toggleOverride(a)}
                      class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
                    >
                      {a.effective_enabled ? 'disable' : 'enable'}
                    </button>
                    {#if a.override_enabled !== null}
                      <button
                        type="button"
                        onclick={() => clearOverride(a)}
                        class="ml-3 font-mono text-2xs uppercase tracking-[0.14em] text-fg-quaternary hover:text-fg-tertiary"
                      >
                        clear override
                      </button>
                    {/if}
                  {/if}
                </td>
              </tr>
            {:else}
              <tr>
                <td
                  colspan="4"
                  class="px-4 py-8 text-center font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >
                  no agents registered yet
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  </main>
</div>
