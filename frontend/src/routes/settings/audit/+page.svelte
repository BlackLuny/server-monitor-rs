<!--
  Audit log viewer. Reverse-chronological, fixed table layout. The log is
  intentionally text-only — no charts, no filters — because in practice an
  admin scans for a specific username + action pair, and a flat table with
  copy-friendly text beats a fancier UI for that.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { listAudit, type AuditRow } from '$lib/api';

  let rows = $state<AuditRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let limit = $state(100);

  async function reload() {
    loading = true;
    try {
      rows = await listAudit(limit);
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  onMount(reload);

  function fmt(ts: string): string {
    return ts.replace('T', ' ').replace(/\..*/, '');
  }
</script>

<svelte:head>
  <title>Audit · settings</title>
</svelte:head>

<header class="mb-6 flex items-end justify-between gap-3">
  <div>
    <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
    <h1 class="mt-1 text-xl font-medium tracking-tight">Audit log</h1>
    <p class="mt-1 text-sm text-fg-secondary">
      Every admin action: logins, server / user / settings changes, 2FA events.
    </p>
  </div>
  <label class="flex items-center gap-2 font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
    show
    <select
      bind:value={limit}
      onchange={reload}
      class="rounded border border-border bg-recess px-2 py-1 font-mono text-2xs text-fg"
    >
      <option value={50}>50</option>
      <option value={100}>100</option>
      <option value={250}>250</option>
      <option value={500}>500</option>
    </select>
  </label>
</header>

{#if error}
  <div
    class="rounded border border-border bg-recess px-4 py-3 font-mono text-xs"
    style="color: var(--status-error)"
  >
    {error}
  </div>
{:else if loading}
  <div class="font-mono text-xs text-fg-tertiary">loading…</div>
{:else if !rows.length}
  <div class="rounded border border-dashed border-border px-5 py-10 text-center text-sm text-fg-tertiary">
    no events yet
  </div>
{:else}
  <div class="overflow-hidden rounded border border-border">
    <table class="w-full text-sm">
      <thead class="bg-elev-2">
        <tr>
          <th class="w-44 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">when</th>
          <th class="w-36 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">who</th>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">action</th>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">target</th>
          <th class="w-32 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">ip</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.id)}
          <tr class="border-t border-border align-top">
            <td class="px-4 py-2 font-mono text-2xs text-fg-tertiary">{fmt(r.ts)}</td>
            <td class="px-4 py-2 font-mono text-fg-secondary">
              {r.username ?? '—'}
            </td>
            <td class="px-4 py-2 font-mono">{r.action}</td>
            <td class="px-4 py-2 font-mono text-fg-secondary">{r.target ?? '—'}</td>
            <td class="px-4 py-2 font-mono text-2xs text-fg-tertiary">{r.ip ?? '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
