<!--
  Updates / rollouts admin page.

  Two stacked sections:
    1. "Latest available" — what the panel sees as the newest cached
       release. Links to the GitHub release HTML and lists its assets +
       sha256 hashes.
    2. "Rollouts" — list of historical + active rollouts, with progress
       bars + pause / resume / abort buttons inline. Creating one is a
       small form below the latest-release card: pick percent, optionally
       restrict to a subset of agents.

  Polling: 10s reload of the list to pick up assignment state changes the
  agents push back via PanelToAgent UpdateStatus → DB.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { authStore } from '$lib/auth.svelte';
  import {
    abortRollout,
    createRollout,
    getLatestRelease,
    listAgents,
    listRollouts,
    pauseRollout,
    resumeRollout,
    type AgentRow,
    type LatestRelease,
    type RolloutSummary
  } from '$lib/api';

  let latest = $state<LatestRelease | null>(null);
  let rollouts = $state<RolloutSummary[]>([]);
  let agents = $state<AgentRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let percent = $state(100);
  let note = $state('');
  let selectedAgents = $state<Set<string>>(new Set());

  let timer: ReturnType<typeof setInterval> | null = null;

  async function reload() {
    try {
      const [rel, list] = await Promise.all([getLatestRelease(), listRollouts()]);
      latest = rel ?? null;
      rollouts = list;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  onMount(async () => {
    if (!authStore.state.loaded) await authStore.refresh();
    if (authStore.state.user?.role !== 'admin') {
      await goto('/login?next=/settings/updates', { replaceState: true });
      return;
    }
    try {
      agents = await listAgents();
    } catch {
      /* tolerated — selection just stays empty */
    }
    await reload();
    timer = setInterval(reload, 10_000);
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  async function submitRollout(e: Event) {
    e.preventDefault();
    if (!latest) return;
    creating = true;
    createError = null;
    try {
      const ids = [...selectedAgents];
      await createRollout({
        version: latest.tag,
        percent,
        agent_ids: ids,
        note: note.trim() || undefined
      });
      percent = 100;
      note = '';
      selectedAgents = new Set();
      await reload();
    } catch (err) {
      createError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  async function transition(id: number, op: 'pause' | 'resume' | 'abort') {
    const fn = op === 'pause' ? pauseRollout : op === 'resume' ? resumeRollout : abortRollout;
    try {
      await fn(id);
      await reload();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  function toggleAgent(id: string) {
    const next = new Set(selectedAgents);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedAgents = next;
  }

  function fmtSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    const kb = bytes / 1024;
    if (kb < 1024) return `${kb.toFixed(0)} KiB`;
    return `${(kb / 1024).toFixed(1)} MiB`;
  }

  function fmtTs(ts: string): string {
    return ts.replace('T', ' ').replace(/\..*/, '');
  }

  function progress(r: RolloutSummary): { pct: number; label: string } {
    if (r.assignments_total === 0) return { pct: 0, label: 'no agents' };
    const pct = Math.round(((r.assignments_succeeded + r.assignments_failed) / r.assignments_total) * 100);
    return {
      pct,
      label: `${r.assignments_succeeded}/${r.assignments_total} succeeded · ${r.assignments_failed} failed`
    };
  }

  function stateTone(s: string): string {
    switch (s) {
      case 'active':
        return 'border-accent text-accent';
      case 'paused':
        return 'border-warning text-warning';
      case 'completed':
        return 'border-fg-tertiary text-fg-secondary';
      case 'aborted':
        return 'border-error text-error';
      default:
        return 'border-border text-fg-tertiary';
    }
  }
</script>

<svelte:head>
  <title>Updates · settings</title>
</svelte:head>

<header class="mb-6">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
  <h1 class="mt-1 text-xl font-medium tracking-tight">Agent updates</h1>
  <p class="mt-1 text-sm text-fg-secondary">
    Roll out a new agent build to a percentage of the fleet, or to specific
    hosts. Assignments stay <code class="text-fg-tertiary">pending</code>
    until the agent reconnects, so a rollout works during maintenance windows.
  </p>
</header>

{#if error}
  <div
    class="mb-4 rounded border px-4 py-3 text-sm text-error"
    style:background="color-mix(in oklch, var(--status-error) 6%, transparent)"
    style:border-color="color-mix(in oklch, var(--status-error) 28%, transparent)"
  >
    {error}
  </div>
{/if}

{#if loading}
  <div class="py-20 text-center font-mono text-xs text-fg-tertiary">loading…</div>
{:else}
  <!-- Latest release card -->
  <section class="mb-8 rounded-lg border border-border bg-elev-1 p-5">
    <div class="mb-3 flex items-baseline justify-between gap-3">
      <div class="flex items-baseline gap-3">
        <h2 class="text-lg font-medium tracking-tight">Latest available</h2>
        {#if latest}
          <span
            class="inline-flex items-center rounded border border-accent px-2 py-0.5 font-mono text-2xs uppercase tracking-[0.14em] text-accent"
          >
            {latest.tag}
          </span>
          {#if latest.prerelease}
            <span class="font-mono text-2xs uppercase tracking-[0.12em] text-warning">prerelease</span>
          {/if}
        {/if}
      </div>
      {#if latest?.html_url}
        <a
          href={latest.html_url}
          target="_blank"
          rel="noopener"
          class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
        >
          view on github ↗
        </a>
      {/if}
    </div>

    {#if !latest}
      <p class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
        poller hasn't fetched a release yet — check back in ~5 minutes
      </p>
    {:else}
      <p class="mb-4 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
        published {fmtTs(latest.published_at)} · fetched {fmtTs(latest.fetched_at)}
      </p>

      <details class="mb-4">
        <summary class="cursor-pointer font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg">
          {latest.assets.length} assets · click to inspect
        </summary>
        <ul class="mt-2 max-h-48 overflow-auto rounded border border-border bg-recess p-2 font-mono text-2xs text-fg-tertiary">
          {#each latest.assets as a (a.name)}
            <li class="flex justify-between gap-3 py-0.5">
              <span class="truncate">{a.name}</span>
              <span>{fmtSize(a.size)}</span>
              <span class="truncate text-fg-quaternary">{a.sha256.slice(0, 12) || '—'}…</span>
            </li>
          {/each}
        </ul>
      </details>

      <form class="grid gap-4 md:grid-cols-[1fr_auto]" onsubmit={submitRollout}>
        <div class="grid gap-3">
          <label class="flex items-center gap-3">
            <span class="w-24 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
              percent
            </span>
            <input
              type="range"
              min="1"
              max="100"
              bind:value={percent}
              class="flex-1 accent-[var(--border-accent)]"
            />
            <span class="w-12 text-right font-mono text-2xs text-fg">{percent}%</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="w-24 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">note</span>
            <input
              type="text"
              bind:value={note}
              placeholder="optional comment shown in the rollout list"
              class="flex-1 rounded border border-border bg-recess px-2 py-1 text-sm"
            />
          </label>
          <details class="mt-1">
            <summary class="cursor-pointer font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg">
              target specific agents ({selectedAgents.size}/{agents.length})
            </summary>
            <div class="mt-2 grid max-h-48 gap-1 overflow-auto rounded border border-border bg-recess p-2">
              {#each agents as a (a.agent_id)}
                <label class="flex items-center gap-2 font-mono text-2xs">
                  <input
                    type="checkbox"
                    checked={selectedAgents.has(a.agent_id)}
                    onchange={() => toggleAgent(a.agent_id)}
                  />
                  <span class={a.online ? 'text-fg' : 'text-fg-quaternary'}>
                    {a.display_name || a.agent_id}
                  </span>
                  <span class="ml-auto text-fg-quaternary">{a.online ? 'online' : 'offline'}</span>
                </label>
              {/each}
            </div>
          </details>
        </div>
        <button
          type="submit"
          disabled={creating}
          class="self-start rounded border border-accent px-4 py-2 font-mono text-2xs uppercase tracking-[0.16em] text-accent hover:bg-elev-2 disabled:opacity-60"
        >
          {creating ? 'starting…' : 'start rollout'}
        </button>
      </form>
      {#if createError}
        <p class="mt-2 text-sm text-error">{createError}</p>
      {/if}
    {/if}
  </section>

  <!-- Rollout history -->
  <section>
    <h2 class="mb-3 text-lg font-medium tracking-tight">Rollouts</h2>
    {#if rollouts.length === 0}
      <p class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
        no rollouts yet
      </p>
    {:else}
      <ul class="grid gap-3">
        {#each rollouts as r (r.id)}
          {@const p = progress(r)}
          <li class="rounded-lg border border-border bg-elev-1 p-4">
            <div class="flex flex-wrap items-baseline justify-between gap-3">
              <div class="flex items-baseline gap-3">
                <span class="font-mono text-sm">#{r.id}</span>
                <span class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
                  {r.version}
                </span>
                <span
                  class="inline-flex items-center rounded border px-2 py-0.5 font-mono text-2xs uppercase tracking-[0.14em] {stateTone(r.state)}"
                >
                  {r.state}
                </span>
                <span class="font-mono text-2xs text-fg-quaternary">
                  {r.percent}% · {fmtTs(r.created_at)}
                </span>
              </div>
              <div class="flex gap-2">
                {#if r.state === 'active'}
                  <button
                    type="button"
                    class="rounded border border-border px-2 py-1 font-mono text-2xs uppercase tracking-[0.14em] text-fg-secondary hover:text-fg"
                    onclick={() => transition(r.id, 'pause')}>pause</button
                  >
                  <button
                    type="button"
                    class="rounded border border-error px-2 py-1 font-mono text-2xs uppercase tracking-[0.14em] text-error hover:bg-elev-2"
                    onclick={() => transition(r.id, 'abort')}>abort</button
                  >
                {:else if r.state === 'paused'}
                  <button
                    type="button"
                    class="rounded border border-accent px-2 py-1 font-mono text-2xs uppercase tracking-[0.14em] text-accent hover:bg-elev-2"
                    onclick={() => transition(r.id, 'resume')}>resume</button
                  >
                  <button
                    type="button"
                    class="rounded border border-error px-2 py-1 font-mono text-2xs uppercase tracking-[0.14em] text-error hover:bg-elev-2"
                    onclick={() => transition(r.id, 'abort')}>abort</button
                  >
                {/if}
              </div>
            </div>

            {#if r.note}
              <p class="mt-2 text-sm text-fg-secondary">{r.note}</p>
            {/if}

            <div class="mt-3">
              <div class="h-1.5 overflow-hidden rounded bg-recess">
                <div
                  class="h-full bg-accent transition-all duration-300"
                  style:width="{p.pct}%"
                  style:background="var(--border-accent)"
                ></div>
              </div>
              <p class="mt-1 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
                {p.label}
              </p>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}
