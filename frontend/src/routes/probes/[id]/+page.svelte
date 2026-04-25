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

  const isAllAgents = $derived(agentFilter === undefined);

  /** Per-timestamp bucket: collects every agent's sample at that ts so we
   *  can render either a per-agent line (specific agent selected) or a
   *  fleet-wide aggregate (all-agents view). */
  interface Bucket {
    /** Mean of latency_us for the OK samples at this ts, in ms. */
    mean_lat_ms: number | null;
    /** Number of failed samples at this ts (across all agents). */
    failures: number;
    /** Total samples (ok + fail) at this ts. */
    total: number;
    /** Pre-aggregated success_rate when the API supplies it (m1/m5/h1
     *  granularities); we use it directly rather than re-deriving. */
    success_rates: number[];
  }

  // Build chart data. The shape depends on `agentFilter`:
  //   - all agents: collapse to ONE aggregate line per metric. Latency is
  //     the mean across agents reporting at that timestamp; loss is total
  //     failures / total samples (or the mean of pre-aggregated
  //     success_rate when the API gives us one).
  //   - one agent: one line per metric, just for that agent.
  // Using a unified time axis lets uPlot drop nulls for gaps cleanly.
  const chartData = $derived.by(() => {
    if (!series || !series.points.length) return null;

    if (isAllAgents) {
      const buckets = new Map<number, { ok_lat_us: number[]; failures: number; total: number; rates: number[] }>();
      for (const p of series.points) {
        const epoch = Math.floor(new Date(p.ts).getTime() / 1000);
        let b = buckets.get(epoch);
        if (!b) {
          b = { ok_lat_us: [], failures: 0, total: 0, rates: [] };
          buckets.set(epoch, b);
        }
        b.total += 1;
        if (p.ok) b.ok_lat_us.push(p.latency_us);
        else b.failures += 1;
        if (p.success_rate !== null && p.success_rate !== undefined) {
          b.rates.push(p.success_rate);
        }
      }
      const timestamps = Array.from(buckets.keys()).sort((a, b) => a - b);
      if (!timestamps.length) return null;
      const latencyVals = timestamps.map((t) => {
        const b = buckets.get(t)!;
        if (!b.ok_lat_us.length) return null;
        const mean = b.ok_lat_us.reduce((a, c) => a + c, 0) / b.ok_lat_us.length;
        return mean / 1000;
      });
      const lossVals = timestamps.map((t) => {
        const b = buckets.get(t)!;
        if (b.rates.length) {
          // Prefer the rolled-up fraction when present (more accurate at
          // m1/m5/h1 granularity than recomputing from buckets).
          const mean = b.rates.reduce((a, c) => a + c, 0) / b.rates.length;
          return Math.max(0, Math.min(100, (1 - mean) * 100));
        }
        return b.total ? (b.failures / b.total) * 100 : 0;
      });
      return {
        timestamps,
        latencySeries: [
          { name: 'fleet mean', color: 'var(--data-1)', values: latencyVals, unit: 'ms' }
        ],
        lossSeries: [
          { name: 'fleet loss', color: 'var(--data-1)', values: lossVals, unit: '%' }
        ]
      };
    }

    // Specific-agent mode: one line per agent (always one when filter is set,
    // but we keep the multi-agent code path for extensibility).
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
    // In all-agents mode, sort by loss% desc so the noisiest hosts surface
    // at the top of the leaderboard. When a specific agent is filtered, the
    // alphabetic order doesn't matter (single row).
    if (isAllAgents) {
      out.sort((a, b) => {
        if (b.loss_pct !== a.loss_pct) return b.loss_pct - a.loss_pct;
        return (b.p95_ms ?? 0) - (a.p95_ms ?? 0);
      });
    } else {
      out.sort((a, b) => agentName(a.agent_id).localeCompare(agentName(b.agent_id)));
    }
    return out;
  });

  /** Fleet-wide summary across every reporting agent. Latency is the mean
   *  of OK samples across the whole window; loss is total failures over
   *  total attempts so a noisy minority can't drown out a healthy majority
   *  (the per-agent leaderboard below still surfaces the offenders). */
  const fleetSummary = $derived.by(() => {
    if (!series || !series.points.length) return null;
    const lats: number[] = [];
    let failures = 0;
    let total = 0;
    for (const p of series.points) {
      total += 1;
      if (p.ok) lats.push(p.latency_us);
      else failures += 1;
    }
    const sorted = lats.slice().sort((a, b) => a - b);
    return {
      agents: summary.length,
      samples: total,
      failures,
      loss_pct: total > 0 ? (failures / total) * 100 : 0,
      mean_ms: sorted.length ? sorted.reduce((a, b) => a + b, 0) / sorted.length / 1000 : null,
      p50_ms: sorted.length ? sorted[Math.floor(sorted.length * 0.5)] / 1000 : null,
      p95_ms: sorted.length
        ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))] / 1000
        : null
    };
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

    <!-- Fleet aggregate (all-agents mode) -->
    {#if isAllAgents && fleetSummary}
      <section class="rounded border border-border bg-elev-1 p-4">
        <div class="mb-2 flex items-baseline justify-between">
          <h2 class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
            fleet aggregate · last {range}
          </h2>
          <span class="font-mono text-2xs text-fg-quaternary">
            {fleetSummary.agents} agent{fleetSummary.agents === 1 ? '' : 's'}
          </span>
        </div>
        <div class="grid grid-cols-2 gap-3 sm:grid-cols-5">
          <div>
            <div class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">samples</div>
            <div class="mt-0.5 font-mono text-md text-fg">{fleetSummary.samples}</div>
          </div>
          <div>
            <div class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">loss</div>
            <div
              class="mt-0.5 font-mono text-md"
              class:text-fg={fleetSummary.loss_pct === 0}
              class:text-warning={fleetSummary.loss_pct > 0 && fleetSummary.loss_pct < 5}
              class:text-error={fleetSummary.loss_pct >= 5}
            >
              {fleetSummary.loss_pct.toFixed(fleetSummary.loss_pct < 1 ? 2 : 1)}%
              {#if fleetSummary.failures > 0}
                <span class="font-mono text-2xs text-fg-quaternary">· {fleetSummary.failures} fail</span>
              {/if}
            </div>
          </div>
          <div>
            <div class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">mean</div>
            <div class="mt-0.5 font-mono text-md text-fg">{fmtMs(fleetSummary.mean_ms)}</div>
          </div>
          <div>
            <div class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">p50</div>
            <div class="mt-0.5 font-mono text-md text-fg-secondary">{fmtMs(fleetSummary.p50_ms)}</div>
          </div>
          <div>
            <div class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">p95</div>
            <div class="mt-0.5 font-mono text-md text-fg-secondary">{fmtMs(fleetSummary.p95_ms)}</div>
          </div>
        </div>
      </section>
    {/if}

    <!-- Per-agent breakdown. In all-agents mode this is a leaderboard
         sorted by loss%, so the worst offenders rise to the top. When a
         specific agent is filtered we just show that one row. -->
    {#if summary.length}
      <section class="rounded border border-border bg-elev-1 p-4">
        <h2 class="mb-3 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
          {isAllAgents ? 'agents · sorted by loss' : 'agent'}
        </h2>
        <div class="overflow-hidden rounded border border-border">
          <table class="w-full font-mono text-xs">
            <thead class="bg-elev-2 text-2xs uppercase tracking-[0.14em] text-fg-quaternary">
              <tr>
                {#if isAllAgents}<th class="w-10 px-3 py-2 text-right">#</th>{/if}
                <th class="px-3 py-2 text-left">agent</th>
                <th class="px-3 py-2 text-right">samples</th>
                <th class="px-3 py-2 text-right">loss</th>
                <th class="px-3 py-2 text-right">mean</th>
                <th class="px-3 py-2 text-right">p50</th>
                <th class="px-3 py-2 text-right">p95</th>
              </tr>
            </thead>
            <tbody>
              {#each summary as s, idx (s.agent_id)}
                <tr
                  class="border-t border-border cursor-pointer hover:bg-elev-2"
                  onclick={() => (agentFilter = s.agent_id)}
                  title="filter to this agent"
                >
                  {#if isAllAgents}
                    <td class="px-3 py-2 text-right text-fg-quaternary">{idx + 1}</td>
                  {/if}
                  <td class="px-3 py-2 text-fg">{agentName(s.agent_id)}</td>
                  <td class="px-3 py-2 text-right text-fg-secondary">{s.samples}</td>
                  <td
                    class="px-3 py-2 text-right"
                    class:text-fg={s.loss_pct === 0}
                    class:text-warning={s.loss_pct > 0 && s.loss_pct < 5}
                    class:text-error={s.loss_pct >= 5}
                  >
                    {s.loss_pct.toFixed(s.loss_pct < 1 ? 2 : 1)}%
                    {#if s.failures > 0}
                      <span class="text-fg-quaternary">· {s.failures}</span>
                    {/if}
                  </td>
                  <td class="px-3 py-2 text-right text-fg">{fmtMs(s.mean_ms)}</td>
                  <td class="px-3 py-2 text-right text-fg-secondary">{fmtMs(s.p50_ms)}</td>
                  <td class="px-3 py-2 text-right text-fg-secondary">{fmtMs(s.p95_ms)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        {#if isAllAgents && summary.length > 1}
          <p class="mt-2 font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary">
            click a row to drill into that agent
          </p>
        {/if}
      </section>
    {/if}

    <!-- Latency chart -->
    <section class="rounded border border-border bg-elev-1 p-4">
      <div class="mb-3 flex items-center justify-between font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
        <span>latency · last {range}</span>
        <span class="text-fg-quaternary">
          {isAllAgents ? 'mean across agents' : agentName(agentFilter ?? '')}
        </span>
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
            {#if isAllAgents}fleet-wide{:else if range === '1h'}raw — failures spike to 100%{:else}aggregated{/if}
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
