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
    const byAgent = new Map<string, Map<number, number | null>>();
    const allTs = new Set<number>();
    for (const p of series.points) {
      const epoch = Math.floor(new Date(p.ts).getTime() / 1000);
      allTs.add(epoch);
      let m = byAgent.get(p.agent_id);
      if (!m) {
        m = new Map();
        byAgent.set(p.agent_id, m);
      }
      m.set(epoch, p.ok ? p.latency_us / 1000 : null);
    }
    const timestamps = Array.from(allTs).sort((a, b) => a - b);
    const ids = Array.from(byAgent.keys()).sort();
    if (!ids.length) return null;
    const seriesArr = ids.map((aid, idx) => ({
      name: agentName(aid),
      color: PALETTE[idx % PALETTE.length],
      values: timestamps.map((t) => byAgent.get(aid)!.get(t) ?? null) as (number | null)[],
      unit: 'ms'
    }));
    return { timestamps, series: seriesArr };
  });

  function agentName(id: string): string {
    const a = agents.find((r) => r.agent_id === id);
    return a ? a.display_name : id.slice(0, 8);
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

    <!-- Chart -->
    <section class="rounded border border-border bg-elev-1 p-4">
      <div class="mb-3 flex items-center justify-between gap-3">
        <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
          latency · last {range}
        </div>
        <div class="flex items-center gap-2">
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
        </div>
      </div>

      {#if chartData}
        <TimeSeriesChart
          timestamps={chartData.timestamps}
          series={chartData.series}
          height={260}
          formatY={(v) => `${v.toFixed(0)}ms`}
        />
      {:else if loading}
        <div class="py-12 text-center font-mono text-xs text-fg-tertiary">loading…</div>
      {:else}
        <div class="py-12 text-center font-mono text-xs text-fg-tertiary">
          no data in this window
        </div>
      {/if}
    </section>

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
