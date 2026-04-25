<!--
  Network probe overview. Lists every probe row with the most-recent success
  rate sparkline and an "N/M agents" effective-coverage badge. Admins get an
  "+ add probe" button + per-row delete; everyone authenticated can read.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { authStore } from '$lib/auth.svelte';
  import {
    ApiError,
    createProbe,
    deleteProbe,
    listProbes,
    type ProbeKind,
    type ProbeRow
  } from '$lib/api';

  let rows = $state<ProbeRow[]>([]);
  let loading = $state(true);
  let listError = $state<string | null>(null);

  // create modal
  let showCreate = $state(false);
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let formKind = $state<ProbeKind>('icmp');
  let formName = $state('');
  let formTarget = $state('');
  let formPort = $state<number | undefined>(undefined);
  let formInterval = $state(60);
  let formTimeout = $state(3000);
  let formHttpMethod = $state('GET');
  let formHttpExpectCode = $state<number | undefined>(undefined);
  let formHttpExpectBody = $state('');
  let formDefaultEnabled = $state(true);

  const isAdmin = $derived(authStore.state.user?.role === 'admin');

  onMount(reload);

  async function reload() {
    loading = true;
    try {
      rows = await listProbes();
      listError = null;
    } catch (err) {
      listError = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  function resetForm() {
    formKind = 'icmp';
    formName = '';
    formTarget = '';
    formPort = undefined;
    formInterval = 60;
    formTimeout = 3000;
    formHttpMethod = 'GET';
    formHttpExpectCode = undefined;
    formHttpExpectBody = '';
    formDefaultEnabled = true;
    createError = null;
  }

  async function handleCreate() {
    if (creating) return;
    createError = null;
    if (!formName.trim() || !formTarget.trim()) {
      createError = 'name + target required';
      return;
    }
    if (formKind === 'tcp' && (!formPort || formPort <= 0)) {
      createError = 'tcp probe needs a port';
      return;
    }
    creating = true;
    try {
      await createProbe({
        name: formName.trim(),
        kind: formKind,
        target: formTarget.trim(),
        port: formKind === 'tcp' ? formPort ?? null : null,
        interval_s: formInterval,
        timeout_ms: formTimeout,
        http_method: formKind === 'http' ? formHttpMethod : null,
        http_expect_code:
          formKind === 'http' && formHttpExpectCode ? formHttpExpectCode : null,
        http_expect_body:
          formKind === 'http' && formHttpExpectBody ? formHttpExpectBody : null,
        default_enabled: formDefaultEnabled
      });
      showCreate = false;
      resetForm();
      await reload();
    } catch (err) {
      createError =
        err instanceof ApiError
          ? err.message || err.code
          : err instanceof Error
            ? err.message
            : String(err);
    } finally {
      creating = false;
    }
  }

  async function handleDelete(p: ProbeRow) {
    if (!confirm(`Delete probe "${p.name}"? Historical results stay in the DB.`)) return;
    try {
      await deleteProbe(p.id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  function fmtTarget(p: ProbeRow): string {
    if (p.kind === 'tcp' && p.port) return `${p.target}:${p.port}`;
    return p.target;
  }
</script>

<svelte:head>
  <title>Probes</title>
</svelte:head>

<div>
  <div class="border-b border-border">
    <div
      class="mx-auto flex max-w-screen-2xl items-center justify-between gap-4 px-6 py-3"
    >
      <h1 class="text-md font-medium tracking-tight">probes</h1>
      <div class="flex items-center gap-3">
        <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
          {rows.length} configured
        </span>
        {#if isAdmin}
          <button
            type="button"
            onclick={() => {
              resetForm();
              showCreate = true;
            }}
            class="rounded border px-3 py-1 font-mono text-2xs uppercase tracking-[0.14em] transition-colors hover:bg-elev-2"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            + add probe
          </button>
        {/if}
      </div>
    </div>
  </div>

  <main class="mx-auto max-w-screen-2xl px-6 py-6">
    {#if listError}
      <div
        class="rounded border border-border bg-recess px-4 py-3 font-mono text-xs"
        style="color: var(--status-error)"
      >
        {listError}
      </div>
    {:else if loading}
      <div class="font-mono text-xs text-fg-tertiary">loading…</div>
    {:else if !rows.length}
      <div
        class="rounded border border-dashed border-border px-5 py-12 text-center text-sm text-fg-tertiary"
      >
        No probes yet — add one to start measuring reachability + latency.
      </div>
    {:else}
      <div class="overflow-hidden rounded border border-border">
        <table class="w-full text-sm">
          <thead class="bg-elev-2">
            <tr>
              <th
                class="w-20 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >kind</th
              >
              <th
                class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >name</th
              >
              <th
                class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >target</th
              >
              <th
                class="w-24 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >interval</th
              >
              <th
                class="w-20 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >default</th
              >
              <th
                class="w-24 px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary"
                >state</th
              >
              <th class="px-4 py-2"></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as p (p.id)}
              <tr
                class="cursor-pointer border-t border-border hover:bg-elev-2"
                onclick={() => goto(`/probes/${p.id}`)}
              >
                <td class="px-4 py-2 font-mono text-2xs uppercase tracking-[0.14em] text-fg-secondary"
                  >{p.kind}</td
                >
                <td class="px-4 py-2 font-mono">{p.name}</td>
                <td class="px-4 py-2 font-mono text-fg-secondary">{fmtTarget(p)}</td>
                <td class="px-4 py-2 font-mono text-2xs text-fg-tertiary">{p.interval_s}s</td>
                <td class="px-4 py-2 font-mono text-2xs">
                  {#if p.default_enabled}
                    <span class="text-online">all agents</span>
                  {:else}
                    <span class="text-fg-tertiary">opt-in only</span>
                  {/if}
                </td>
                <td class="px-4 py-2 font-mono text-2xs">
                  {#if p.enabled}
                    <span class="text-online">enabled</span>
                  {:else}
                    <span class="text-fg-quaternary">disabled</span>
                  {/if}
                </td>
                <td class="px-4 py-2 text-right">
                  {#if isAdmin}
                    <button
                      type="button"
                      onclick={(ev) => {
                        ev.stopPropagation();
                        handleDelete(p);
                      }}
                      class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-quaternary hover:text-error"
                    >
                      delete
                    </button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </main>

  {#if showCreate}
    <button
      type="button"
      onclick={() => (showCreate = false)}
      class="fixed inset-0 z-40 cursor-default bg-black/60"
      aria-label="close"
    ></button>
    <div
      role="dialog"
      aria-modal="true"
      aria-label="add probe"
      class="fixed left-1/2 top-1/2 z-50 w-[480px] -translate-x-1/2 -translate-y-1/2 rounded border border-border bg-elev-1 p-6"
    >
      <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
        new probe
      </div>
      <h2 class="mb-4 text-md font-medium text-fg">Add probe</h2>

      <div class="mb-3">
        <span class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
          >kind</span
        >
        <div class="flex gap-1">
          {#each ['icmp', 'tcp', 'http'] as k}
            <button
              type="button"
              onclick={() => (formKind = k as ProbeKind)}
              class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em]"
              style:border-color={formKind === k ? 'var(--border-accent)' : 'var(--border)'}
              style:color={formKind === k ? 'var(--border-accent)' : 'var(--fg-tertiary)'}
            >
              {k}
            </button>
          {/each}
        </div>
      </div>

      <label class="mb-3 block">
        <span class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
          >name</span
        >
        <input
          type="text"
          bind:value={formName}
          placeholder="Cloudflare DNS"
          class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
      </label>

      <div class="mb-3 grid grid-cols-1 gap-3 md:grid-cols-2">
        <label class="block">
          <span
            class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
          >
            {formKind === 'http' ? 'url' : 'target host/IP'}
          </span>
          <input
            type="text"
            bind:value={formTarget}
            placeholder={formKind === 'http' ? 'https://example.com/health' : '1.1.1.1'}
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>

        {#if formKind === 'tcp'}
          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
              >port</span
            >
            <input
              type="number"
              bind:value={formPort}
              placeholder="443"
              class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
            />
          </label>
        {/if}
      </div>

      <div class="mb-3 grid grid-cols-2 gap-3">
        <label class="block">
          <span
            class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >interval (s)</span
          >
          <input
            type="number"
            bind:value={formInterval}
            min="5"
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>
        <label class="block">
          <span
            class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >timeout (ms)</span
          >
          <input
            type="number"
            bind:value={formTimeout}
            min="100"
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>
      </div>

      {#if formKind === 'http'}
        <div class="mb-3 grid grid-cols-2 gap-3">
          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
              >method</span
            >
            <select
              bind:value={formHttpMethod}
              class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg"
            >
              {#each ['GET', 'POST', 'HEAD'] as m}
                <option value={m}>{m}</option>
              {/each}
            </select>
          </label>
          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
              >expect status (0 = any 2xx)</span
            >
            <input
              type="number"
              bind:value={formHttpExpectCode}
              placeholder="0"
              class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
            />
          </label>
        </div>
        <label class="mb-3 block">
          <span
            class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >expect body contains (optional)</span
          >
          <input
            type="text"
            bind:value={formHttpExpectBody}
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>
      {/if}

      <label class="mb-4 inline-flex items-center gap-2">
        <input
          type="checkbox"
          bind:checked={formDefaultEnabled}
          class="h-4 w-4 accent-current"
          style:accent-color="var(--border-accent)"
        />
        <span class="font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary">
          run on every agent by default
        </span>
      </label>

      {#if createError}
        <div
          class="mb-3 rounded border border-border bg-recess px-3 py-2 font-mono text-xs"
          style="color: var(--status-error)"
        >
          {createError}
        </div>
      {/if}

      <div class="flex justify-end gap-2">
        <button
          type="button"
          onclick={() => (showCreate = false)}
          class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
        >
          cancel
        </button>
        <button
          type="button"
          onclick={handleCreate}
          disabled={creating}
          class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
          style:border-color="var(--border-accent)"
          style:color="var(--border-accent)"
        >
          {creating ? 'creating…' : 'create'}
        </button>
      </div>
    </div>
  {/if}
</div>
