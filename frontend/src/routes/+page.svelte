<!--
  Server list dashboard.

  - Loads all servers via `GET /api/servers` on mount.
  - Subscribes to `/ws/live` and folds updates into per-server ring buffers
    so each card's sparkline reflects the last 60s of history.
  - Search filters by display name, group, or tag (case-insensitive).
  - Cards group under their SectionHeader; ungrouped lands in "unassigned".
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  import {
    Badge,
    Panel,
    SectionHeader,
    Sparkline,
    Stat,
    StatusDot
  } from '$lib/primitives';
  import {
    listServers,
    subscribeLive,
    type LiveUpdate,
    type ServerRow
  } from '$lib/api';
  import { ageFromIso, bitsPerSec, percent, usagePct } from '$lib/format';

  // ----------------------------------------------------------------
  // state
  // ----------------------------------------------------------------
  const RING_SIZE = 60;
  interface Ring {
    cpu: number[];
    mem: number[];
    netIn: number[];
    netOut: number[];
  }

  let servers = $state<ServerRow[]>([]);
  let rings = $state<Record<number, Ring>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);
  let search = $state('');
  let nowTick = $state(Date.now());
  let lastUpdate = $state<string | null>(null);

  let liveUnsub: (() => void) | null = null;
  let agePoll: ReturnType<typeof setInterval> | null = null;

  // ----------------------------------------------------------------
  // data load + live merge
  // ----------------------------------------------------------------
  async function reload() {
    try {
      const res = await listServers();
      servers = res.servers;
      lastUpdate = res.updated_at;
      // Seed rings from the latest sample so every card shows something
      // meaningful during the first ~60s instead of a blank line.
      const seeded: Record<number, Ring> = { ...rings };
      for (const s of res.servers) {
        if (seeded[s.id]) continue;
        const cpu = s.latest?.cpu_pct ?? 0;
        const memPct = s.latest ? usagePct(s.latest.mem_used, s.latest.mem_total) : 0;
        seeded[s.id] = {
          cpu: Array(RING_SIZE).fill(cpu),
          mem: Array(RING_SIZE).fill(memPct),
          netIn: Array(RING_SIZE).fill(s.latest?.net_in_bps ?? 0),
          netOut: Array(RING_SIZE).fill(s.latest?.net_out_bps ?? 0)
        };
      }
      rings = seeded;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  function handleLive(u: LiveUpdate) {
    lastUpdate = u.ts;
    const r = rings[u.server_id];
    if (!r) return;
    const memPct = usagePct(u.mem_used, u.mem_total);
    rings = {
      ...rings,
      [u.server_id]: {
        cpu: shift(r.cpu, u.cpu_pct),
        mem: shift(r.mem, memPct),
        netIn: shift(r.netIn, u.net_in_bps),
        netOut: shift(r.netOut, u.net_out_bps)
      }
    };
    const idx = servers.findIndex((s) => s.id === u.server_id);
    if (idx >= 0) {
      const s = servers[idx];
      const next: ServerRow = {
        ...s,
        online: true,
        last_seen_at: u.ts,
        latest: {
          ts: u.ts,
          cpu_pct: u.cpu_pct,
          mem_used: u.mem_used,
          mem_total: u.mem_total,
          swap_used: s.latest?.swap_used ?? 0,
          swap_total: s.latest?.swap_total ?? 0,
          load_1: u.load_1,
          disk_used: s.latest?.disk_used ?? 0,
          disk_total: s.latest?.disk_total ?? 0,
          net_in_bps: u.net_in_bps,
          net_out_bps: u.net_out_bps,
          process_count: s.latest?.process_count ?? 0
        }
      };
      const copy = servers.slice();
      copy[idx] = next;
      servers = copy;
    }
  }

  function shift(ring: number[], v: number): number[] {
    const n = ring.slice(1);
    n.push(Number.isFinite(v) ? v : 0);
    return n;
  }

  onMount(() => {
    reload();
    liveUnsub = subscribeLive(handleLive);
    agePoll = setInterval(() => (nowTick = Date.now()), 1000);
  });

  onDestroy(() => {
    liveUnsub?.();
    if (agePoll) clearInterval(agePoll);
  });

  // ----------------------------------------------------------------
  // grouping + filtering
  // ----------------------------------------------------------------
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    if (!q) return servers;
    return servers.filter((s) => {
      const hay = [
        s.display_name,
        s.group_name ?? '',
        (s.tags ?? []).join(' '),
        s.location ?? ''
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  });

  const groups = $derived.by(() => {
    const map = new Map<string, ServerRow[]>();
    for (const s of filtered) {
      const key = s.group_name ?? '';
      if (!map.has(key)) map.set(key, []);
      map.get(key)!.push(s);
    }
    return Array.from(map.entries()).sort((a, b) => a[0].localeCompare(b[0]));
  });

  const totals = $derived.by(() => {
    const online = servers.filter((s) => s.online).length;
    return { online, total: servers.length };
  });
</script>

<svelte:head>
  <title>server-monitor</title>
</svelte:head>

{#snippet emptyState()}
  <div class="mx-auto mt-16 max-w-xl rounded border border-border bg-elev-1 px-6 py-10 text-center">
    <div class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">empty</div>
    <h2 class="mt-2 text-md font-medium">No servers yet</h2>
    <p class="mt-2 text-sm text-fg-secondary">
      Add a server from the settings page, then copy the generated
      <code class="rounded bg-recess px-1.5 py-0.5 font-mono text-xs">install-agent</code>
      command onto the host you want to watch.
    </p>
  </div>
{/snippet}

{#snippet serverCard(s: ServerRow, ring: Ring | undefined, tick: number)}
  {@const ringData = ring ?? { cpu: [], mem: [], netIn: [], netOut: [] }}
  {@const memPct = s.latest ? usagePct(s.latest.mem_used, s.latest.mem_total) : 0}
  <Panel accent={s.online ? 'online' : 'error'} interactive>
    <a href={`/servers/${s.id}`} class="absolute inset-0" aria-label="open detail"></a>
    <div class="relative flex items-start justify-between gap-3">
      <div class="min-w-0">
        <div class="flex items-center gap-2">
          <StatusDot kind={s.online ? 'online' : 'error'} />
          <span class="truncate text-md font-medium">{s.display_name}</span>
        </div>
        <div class="mt-0.5 flex items-center gap-1.5 font-mono text-2xs text-fg-tertiary">
          {#if s.hardware?.os}
            <span>{s.hardware.os} · {s.hardware.arch ?? ''}</span>
            <span class="text-fg-quaternary">·</span>
          {/if}
          <span>
            {#if s.online}
              last seen {ageFromIso(s.last_seen_at, tick)}
            {:else}
              offline · last seen {ageFromIso(s.last_seen_at, tick)}
            {/if}
          </span>
        </div>
      </div>
      <div class="relative flex items-center gap-1.5">
        {#each (s.tags ?? []).slice(0, 2) as tag}
          <Badge tone="neutral">{tag}</Badge>
        {/each}
      </div>
    </div>

    <div class="relative mt-4 grid grid-cols-3 gap-3">
      <Stat
        label="cpu"
        value={s.latest ? percent(s.latest.cpu_pct) : '—'}
        unit="%"
        tone="data1"
        flashKey={s.latest?.ts}
      />
      <Stat
        label="mem"
        value={s.latest ? percent(memPct, 0) : '—'}
        unit="%"
        tone="data2"
        flashKey={s.latest?.ts}
      />
      <Stat label="load" value={s.latest ? s.latest.load_1.toFixed(2) : '—'} tone="data5" />
    </div>

    <div class="relative mt-4 space-y-2">
      <div
        class="flex items-center justify-between font-mono text-2xs uppercase tracking-wider text-fg-tertiary"
      >
        <span>cpu · last 60s</span>
        <span class="text-fg-quaternary">
          {s.latest ? percent(s.latest.cpu_pct) : '—'}%
        </span>
      </div>
      <Sparkline values={ringData.cpu} max={100} tone="data1" />

      <div
        class="flex items-center justify-between font-mono text-2xs uppercase tracking-wider text-fg-tertiary"
      >
        <span>net · in / out</span>
        <span class="text-fg-quaternary">
          {s.latest ? bitsPerSec(s.latest.net_in_bps) : '—'}
          {' '}·{' '}
          {s.latest ? bitsPerSec(s.latest.net_out_bps) : '—'}
        </span>
      </div>
      <div class="grid grid-cols-2 gap-2">
        <Sparkline values={ringData.netIn} tone="data3" />
        <Sparkline values={ringData.netOut} tone="data4" />
      </div>
    </div>
  </Panel>
{/snippet}

<div class="min-h-screen">
  <!-- ── top bar ── -->
  <header class="border-b border-border">
    <div class="mx-auto flex max-w-7xl items-center justify-between gap-4 px-6 py-3">
      <div class="flex items-baseline gap-3">
        <span class="font-mono text-xs uppercase tracking-wider text-fg-tertiary">svrmon</span>
        <h1 class="text-md font-medium tracking-tight">overview</h1>
      </div>
      <div class="flex items-center gap-3">
        <input
          type="text"
          placeholder="search…"
          bind:value={search}
          class="h-7 w-56 rounded border border-border bg-recess px-2.5 font-mono text-xs text-fg placeholder:text-fg-quaternary focus:border-border-strong"
        />
        <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
          <span class="text-online">{totals.online}</span>
          <span class="text-fg-quaternary">/</span>
          {totals.total} online
        </span>
      </div>
    </div>
  </header>

  <main class="mx-auto max-w-7xl px-6 py-6">
    {#if error}
      <div
        class="rounded border px-4 py-3 text-sm text-error"
        style:background="color-mix(in oklch, var(--status-error) 6%, transparent)"
        style:border-color="color-mix(in oklch, var(--status-error) 28%, transparent)"
      >
        failed to load servers: {error}
      </div>
    {/if}

    {#if loading && !servers.length}
      <div class="py-20 text-center font-mono text-xs text-fg-tertiary">loading…</div>
    {:else if !servers.length}
      {@render emptyState()}
    {:else}
      {#each groups as [groupName, rows] (groupName)}
        <section class="mb-8 last:mb-0">
          <SectionHeader label={groupName || 'unassigned'} count={rows.length} />
          <div class="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
            {#each rows as s (s.id)}
              {@render serverCard(s, rings[s.id], nowTick)}
            {/each}
          </div>
        </section>
      {/each}
    {/if}
  </main>
</div>
