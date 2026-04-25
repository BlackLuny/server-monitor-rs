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
  import { goto } from '$app/navigation';
  import {
    adminCreateServer,
    deleteServer,
    listServers,
    subscribeLive,
    type CreatedServer,
    type LiveUpdate,
    type ServerRow
  } from '$lib/api';
  import { authStore } from '$lib/auth.svelte';
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

  // Drives the "Add server" modal + the install-command reveal.
  let showAddServer = $state(false);
  let newServerName = $state('');
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let lastCreated = $state<CreatedServer | null>(null);

  // Auth-derived view mode: when there is no logged-in user we ask the panel
  // to filter for guests. The panel responds with hardware nulled out and
  // hidden_from_guest rows dropped.
  const guestMode = $derived(!authStore.state.user);
  const isAdmin = $derived(authStore.state.user?.role === 'admin');

  // ----------------------------------------------------------------
  // data load + live merge
  // ----------------------------------------------------------------
  async function reload() {
    try {
      const res = await listServers({ guest: guestMode });
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
    liveUnsub = subscribeLive(handleLive, { guest: guestMode });
    agePoll = setInterval(() => (nowTick = Date.now()), 1000);
  });

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

  async function handleCreate() {
    createError = null;
    if (!newServerName.trim()) {
      createError = 'name required';
      return;
    }
    creating = true;
    try {
      const created = await adminCreateServer({ display_name: newServerName.trim() });
      lastCreated = created;
      newServerName = '';
      await reload();
    } catch (err) {
      createError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  async function handleDelete(id: number, name: string) {
    if (!confirm(`Remove "${name}" from the panel? This does not stop the agent.`)) return;
    try {
      await deleteServer(id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

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
        {#if isAdmin}
          <button
            type="button"
            onclick={(ev) => {
              ev.preventDefault();
              ev.stopPropagation();
              handleDelete(s.id, s.display_name);
            }}
            class="ml-1 font-mono text-2xs uppercase tracking-[0.12em] text-fg-quaternary hover:text-error"
            title="remove from panel"
          >
            ✕
          </button>
        {/if}
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
        {#if isAdmin}
          <button
            type="button"
            onclick={() => {
              showAddServer = true;
              lastCreated = null;
            }}
            class="rounded border px-3 py-1 font-mono text-2xs uppercase tracking-[0.14em] transition-colors hover:bg-elev-2"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            + add server
          </button>
        {/if}
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
  {/if}

  {#if showAddServer}
    <!-- Admin "add server" modal — surfaces the install command on success. -->
    <button
      type="button"
      onclick={() => (showAddServer = false)}
      class="fixed inset-0 z-40 cursor-default bg-black/60"
      aria-label="close"
    ></button>
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Add server"
      class="fixed left-1/2 top-1/2 z-50 w-[460px] -translate-x-1/2 -translate-y-1/2 rounded border border-border bg-elev-1 p-6"
    >
      {#if lastCreated}
        <div class="mb-3 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
          install command — copy it onto the host
        </div>
        <pre
          class="mb-4 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded border border-border bg-recess p-3 font-mono text-xs text-fg"
        >{lastCreated.install_command}</pre>
        <div class="flex justify-end gap-2">
          <button
            type="button"
            onclick={() => navigator.clipboard.writeText(lastCreated!.install_command)}
            class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
          >
            copy
          </button>
          <button
            type="button"
            onclick={() => {
              showAddServer = false;
              lastCreated = null;
            }}
            class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em]"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            done
          </button>
        </div>
      {:else}
        <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
          new server
        </div>
        <h2 class="mb-4 text-md font-medium text-fg">Register a host</h2>
        <label class="block">
          <span
            class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >display name</span
          >
          <input
            type="text"
            bind:value={newServerName}
            disabled={creating}
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
            placeholder="prod-web-01"
          />
        </label>
        {#if createError}
          <div
            class="mt-3 rounded border border-border bg-recess px-3 py-2 font-mono text-xs"
            style="color: var(--status-error)"
          >
            {createError}
          </div>
        {/if}
        <div class="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onclick={() => (showAddServer = false)}
            class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
          >
            cancel
          </button>
          <button
            type="button"
            disabled={creating || !newServerName.trim()}
            onclick={handleCreate}
            class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            {creating ? 'creating…' : 'create'}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</div>
