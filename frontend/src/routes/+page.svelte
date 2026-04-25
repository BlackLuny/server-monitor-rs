<!--
  Server list dashboard. Read-only by design: all admin actions
  (add / rename / delete) live on /settings/servers so a stray click
  here can't damage the fleet inventory.

  - Loads all servers via `GET /api/servers` on mount.
  - Subscribes to `/ws/live` and folds updates into per-server ring buffers
    so each card's sparkline reflects the last 60s of history.
  - Search filters by display name, group, or tag (case-insensitive).
  - View mode (cards | list) is persisted in localStorage so the same
    operator gets their preferred density on every visit.
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
  import { goto } from '$app/navigation';
  import {
    fetchSparklines,
    listServers,
    subscribeLive,
    type LiveUpdate,
    type ServerRow
  } from '$lib/api';
  import { authStore } from '$lib/auth.svelte';
  import { ageFromIso, bitsPerSec, percent, usagePct } from '$lib/format';

  type ViewMode = 'cards' | 'list';
  const VIEW_KEY = 'server-monitor.overview.view';

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

  let viewMode = $state<ViewMode>('cards');

  // Auth-derived view mode: when there is no logged-in user we ask the panel
  // to filter for guests. The panel responds with hardware nulled out and
  // hidden_from_guest rows dropped.
  const guestMode = $derived(!authStore.state.user);

  // ----------------------------------------------------------------
  // data load + live merge
  // ----------------------------------------------------------------
  async function reload() {
    try {
      const res = await listServers({ guest: guestMode });
      servers = res.servers;
      lastUpdate = res.updated_at;

      // Seed rings from real history so cards show the actual last 60s
      // instead of a flat seed-and-grow line. Falls back to the latest
      // sample if the sparklines call fails or hasn't returned yet.
      let history: Awaited<ReturnType<typeof fetchSparklines>> = [];
      try {
        history = await fetchSparklines(RING_SIZE);
      } catch {
        history = [];
      }
      const byServer = new Map(history.map((row) => [row.server_id, row]));

      const seeded: Record<number, Ring> = { ...rings };
      for (const s of res.servers) {
        if (seeded[s.id]) continue;
        const past = byServer.get(s.id);
        const cpu = padLeft(past?.cpu_pct, RING_SIZE, s.latest?.cpu_pct ?? 0);
        const memSeed = s.latest ? usagePct(s.latest.mem_used, s.latest.mem_total) : 0;
        const mem = padLeft(past?.mem_pct, RING_SIZE, memSeed);
        const netIn = padLeft(past?.net_in_bps, RING_SIZE, s.latest?.net_in_bps ?? 0);
        const netOut = padLeft(past?.net_out_bps, RING_SIZE, s.latest?.net_out_bps ?? 0);
        seeded[s.id] = { cpu, mem, netIn, netOut };
      }
      rings = seeded;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  /** Right-align `tail` into a fixed-size ring; pad missing prefix with
   *  the same value so the curve "starts" at the seed instead of zero. */
  function padLeft(tail: number[] | undefined, size: number, seed: number): number[] {
    if (!tail || tail.length === 0) return Array(size).fill(seed);
    const trimmed = tail.length > size ? tail.slice(tail.length - size) : tail.slice();
    if (trimmed.length === size) return trimmed;
    const padCount = size - trimmed.length;
    const padValue = trimmed[0] ?? seed;
    const out = Array(padCount).fill(padValue);
    return out.concat(trimmed);
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
    if (typeof localStorage !== 'undefined') {
      const saved = localStorage.getItem(VIEW_KEY);
      if (saved === 'cards' || saved === 'list') viewMode = saved;
    }
    reload();
    liveUnsub = subscribeLive(handleLive, { guest: guestMode });
    agePoll = setInterval(() => (nowTick = Date.now()), 1000);
  });

  function setView(v: ViewMode) {
    viewMode = v;
    if (typeof localStorage !== 'undefined') localStorage.setItem(VIEW_KEY, v);
  }

  onDestroy(() => {
    liveUnsub?.();
    if (agePoll) clearInterval(agePoll);
  });

  // Re-subscribe when the auth state flips (login from /login lands back
  // here without a remount, so the WS would otherwise keep the guest scope).
  $effect(() => {
    void authStore.state.user;
    liveUnsub?.();
    liveUnsub = subscribeLive(handleLive, { guest: guestMode });
    reload();
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
  <Panel
    accent={s.online ? 'online' : 'error'}
    href={`/servers/${s.id}`}
    ariaLabel={`Open ${s.display_name} detail`}
  >
    <div class="flex items-start justify-between gap-3">
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
      <div class="flex items-center gap-1.5">
        {#each (s.tags ?? []).slice(0, 2) as tag}
          <Badge tone="neutral">{tag}</Badge>
        {/each}
        {#if s.agent_version}
          <span
            class="rounded border border-border px-1.5 py-0.5 font-mono text-2xs text-fg-tertiary"
            title="agent version"
          >
            {s.agent_version}
          </span>
        {/if}
      </div>
    </div>

    <div class="mt-4 grid grid-cols-3 gap-3">
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

    <div class="mt-4 space-y-2">
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

<div>
  <!-- ── page-level toolbar (sits below the global SiteHeader) ── -->
  <div class="border-b border-border">
    <div
      class="mx-auto flex max-w-screen-2xl items-center justify-between gap-4 px-6 py-3"
    >
      <h1 class="text-md font-medium tracking-tight">overview</h1>
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
        <div class="flex gap-0.5 self-start rounded border border-border bg-elev-1 p-0.5">
          <button
            type="button"
            onclick={() => setView('cards')}
            class="rounded-xs px-2 py-1 font-mono text-2xs uppercase tracking-[0.12em] transition-colors duration-100
                   {viewMode === 'cards' ? 'bg-elev-2 text-fg' : 'text-fg-tertiary hover:text-fg-secondary'}"
            title="card view"
          >cards</button>
          <button
            type="button"
            onclick={() => setView('list')}
            class="rounded-xs px-2 py-1 font-mono text-2xs uppercase tracking-[0.12em] transition-colors duration-100
                   {viewMode === 'list' ? 'bg-elev-2 text-fg' : 'text-fg-tertiary hover:text-fg-secondary'}"
            title="list view"
          >list</button>
        </div>
      </div>
    </div>
  </div>

  {#if guestMode && !authStore.state.guestEnabled}
    <div class="mx-auto max-w-md px-6 py-20 text-center">
      <h2 class="text-lg font-medium text-fg">Guest access is disabled</h2>
      <p class="mt-2 text-sm text-fg-secondary">
        Sign in to view the dashboard.
      </p>
      <button
        type="button"
        onclick={() => goto('/login')}
        class="mt-4 rounded border px-4 py-2 font-mono text-2xs uppercase tracking-[0.14em] transition-colors hover:bg-elev-2"
        style:border-color="var(--border-accent)"
        style:color="var(--border-accent)"
      >
        sign in
      </button>
    </div>
  {:else}
  <main class="mx-auto max-w-screen-2xl px-6 py-6">
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
    {:else if viewMode === 'cards'}
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
    {:else}
      <!-- list view: one row per server, dense and sortable-by-eye -->
      {#each groups as [groupName, rows] (groupName)}
        <section class="mb-6 last:mb-0">
          <SectionHeader label={groupName || 'unassigned'} count={rows.length} />
          <div class="mt-2 overflow-hidden rounded border border-border">
            <table class="w-full font-mono text-xs">
              <thead class="bg-elev-1 text-2xs uppercase tracking-[0.14em] text-fg-quaternary">
                <tr>
                  <th class="px-3 py-2 text-left">name</th>
                  <th class="px-3 py-2 text-left">os</th>
                  <th class="px-3 py-2 text-left">agent</th>
                  <th class="px-3 py-2 text-right">cpu</th>
                  <th class="px-3 py-2 text-right">mem</th>
                  <th class="px-3 py-2 text-right">load</th>
                  <th class="px-3 py-2 text-right">net in / out</th>
                  <th class="px-3 py-2 text-left">last seen</th>
                </tr>
              </thead>
              <tbody>
                {#each rows as s (s.id)}
                  {@const memPct = s.latest ? usagePct(s.latest.mem_used, s.latest.mem_total) : 0}
                  <tr class="border-t border-border hover:bg-elev-1/40">
                    <td class="px-3 py-2">
                      <a href={`/servers/${s.id}`} class="flex items-center gap-2 text-fg hover:underline">
                        <StatusDot kind={s.online ? 'online' : 'error'} size={6} />
                        <span class="truncate">{s.display_name}</span>
                      </a>
                    </td>
                    <td class="px-3 py-2 text-fg-tertiary">
                      {s.hardware?.os ?? '—'}{s.hardware?.arch ? ' · ' + s.hardware.arch : ''}
                    </td>
                    <td class="px-3 py-2 text-fg-tertiary">{s.agent_version ?? '—'}</td>
                    <td class="px-3 py-2 text-right text-fg">
                      {s.latest ? percent(s.latest.cpu_pct) + '%' : '—'}
                    </td>
                    <td class="px-3 py-2 text-right text-fg">
                      {s.latest ? percent(memPct, 0) + '%' : '—'}
                    </td>
                    <td class="px-3 py-2 text-right text-fg-secondary">
                      {s.latest ? s.latest.load_1.toFixed(2) : '—'}
                    </td>
                    <td class="px-3 py-2 text-right text-fg-tertiary">
                      {s.latest ? bitsPerSec(s.latest.net_in_bps) : '—'}
                      <span class="text-fg-quaternary"> · </span>
                      {s.latest ? bitsPerSec(s.latest.net_out_bps) : '—'}
                    </td>
                    <td class="px-3 py-2 text-fg-tertiary">{ageFromIso(s.last_seen_at, nowTick)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </section>
      {/each}
    {/if}
  </main>
  {/if}

</div>
