<!--
  General settings page. Reads/writes the four KV entries the admin actually
  cares about. Each row is independently saveable so a typo on one field
  doesn't roll back unrelated edits — and the panel only audits per-key
  updates, which lines up with what shows in /settings/audit.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { listSettings, putSetting, type SettingRow } from '$lib/api';

  interface RowState {
    value: string | boolean;
    saving: boolean;
    saved: boolean;
    error: string | null;
  }

  let rows = $state<Record<string, RowState>>({});
  let loading = $state(true);
  let loadError = $state<string | null>(null);

  const KEYS: { key: string; label: string; help: string; kind: 'string' | 'bool' }[] = [
    {
      key: 'site_name',
      label: 'Panel name',
      help: 'Shown in the browser tab title and the page header.',
      kind: 'string'
    },
    {
      key: 'agent_endpoint',
      label: 'Agent endpoint URL',
      help: 'Public address agents dial into. Required before any server can be added.',
      kind: 'string'
    },
    {
      key: 'guest_enabled',
      label: 'Allow guest access',
      help: 'When off, anonymous visitors are redirected to /login.',
      kind: 'bool'
    },
    {
      key: 'ssh_recording_enabled',
      label: 'Record SSH sessions by default',
      help: 'Per-server override is available on each server detail page.',
      kind: 'bool'
    }
  ];

  onMount(async () => {
    try {
      const data = await listSettings();
      rows = Object.fromEntries(
        KEYS.map((k) => {
          const row = data.find((r) => r.key === k.key);
          const v = row?.value;
          return [
            k.key,
            {
              value: k.kind === 'bool' ? Boolean(v) : typeof v === 'string' ? v : '',
              saving: false,
              saved: false,
              error: null
            }
          ];
        })
      );
    } catch (err) {
      loadError = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  });

  async function save(key: string, kind: 'string' | 'bool') {
    const r = rows[key];
    if (!r) return;
    r.saving = true;
    r.error = null;
    r.saved = false;
    try {
      const value: SettingRow['value'] = kind === 'bool' ? Boolean(r.value) : String(r.value);
      await putSetting(key, value);
      r.saved = true;
      setTimeout(() => {
        if (rows[key]) rows[key].saved = false;
      }, 1500);
    } catch (err) {
      r.error = err instanceof Error ? err.message : String(err);
    } finally {
      r.saving = false;
    }
  }
</script>

<svelte:head>
  <title>General · settings</title>
</svelte:head>

<header class="mb-6">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
  <h1 class="mt-1 text-xl font-medium tracking-tight">General</h1>
</header>

{#if loadError}
  <div
    class="rounded border border-border bg-recess px-4 py-3 font-mono text-xs"
    style="color: var(--status-error)"
  >
    {loadError}
  </div>
{:else if loading}
  <div class="font-mono text-xs text-fg-tertiary">loading…</div>
{:else}
  <div class="space-y-4">
    {#each KEYS as k}
      {@const r = rows[k.key]}
      <section
        class="grid grid-cols-1 gap-3 rounded border border-border bg-elev-1 px-5 py-4 md:grid-cols-[260px_1fr_auto] md:items-center"
      >
        <div>
          <div class="font-medium text-fg">{k.label}</div>
          <div class="mt-0.5 text-2xs text-fg-tertiary">{k.help}</div>
        </div>
        <div>
          {#if k.kind === 'bool'}
            <label class="inline-flex items-center gap-2">
              <input
                type="checkbox"
                bind:checked={r.value as boolean}
                disabled={r.saving}
                class="h-4 w-4 accent-current"
                style:accent-color="var(--border-accent)"
              />
              <span class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary">
                {r.value ? 'enabled' : 'disabled'}
              </span>
            </label>
          {:else}
            <input
              type="text"
              bind:value={r.value as string}
              disabled={r.saving}
              class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
            />
          {/if}
          {#if r.error}
            <div class="mt-1 font-mono text-2xs" style="color: var(--status-error)">
              {r.error}
            </div>
          {/if}
        </div>
        <div class="flex items-center gap-2">
          {#if r.saved}
            <span class="font-mono text-2xs uppercase tracking-[0.12em] text-online">saved</span>
          {/if}
          <button
            type="button"
            onclick={() => save(k.key, k.kind)}
            disabled={r.saving}
            class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            {r.saving ? 'saving…' : 'save'}
          </button>
        </div>
      </section>
    {/each}
  </div>
{/if}
