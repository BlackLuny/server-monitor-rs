<!--
  Server admin page. The overview is read-only by design — anything that
  mutates a server (add / rename / delete) lives here behind the admin
  guard so a stray click on a card on the dashboard can't cause damage.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { authStore } from '$lib/auth.svelte';
  import {
    adminCreateServer,
    deleteServer,
    listServers,
    updateServer,
    type CreatedServer,
    type ServerRow
  } from '$lib/api';
  import { ageFromIso } from '$lib/format';

  let servers = $state<ServerRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let nowTick = $state(Date.now());

  let showAdd = $state(false);
  let newName = $state('');
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let lastCreated = $state<CreatedServer | null>(null);

  let editingId = $state<number | null>(null);
  let editName = $state('');
  let savingEdit = $state(false);

  async function reload() {
    try {
      const res = await listServers();
      servers = res.servers;
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
      await goto('/login?next=/settings/servers', { replaceState: true });
      return;
    }
    await reload();
    setInterval(() => (nowTick = Date.now()), 1000);
  });

  async function handleCreate() {
    createError = null;
    if (!newName.trim()) {
      createError = 'name required';
      return;
    }
    creating = true;
    try {
      lastCreated = await adminCreateServer({ display_name: newName.trim() });
      newName = '';
      await reload();
    } catch (err) {
      createError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  async function handleDelete(id: number, name: string) {
    if (!confirm(`Remove "${name}" from the panel? The agent process keeps running until you stop it on the host.`)) return;
    try {
      await deleteServer(id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  function startEdit(s: ServerRow) {
    editingId = s.id;
    editName = s.display_name;
  }

  async function saveEdit() {
    if (editingId == null) return;
    const name = editName.trim();
    if (!name) {
      editingId = null;
      return;
    }
    savingEdit = true;
    try {
      await updateServer(editingId, { display_name: name });
      editingId = null;
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    } finally {
      savingEdit = false;
    }
  }

  function cancelEdit() {
    editingId = null;
  }
</script>

<svelte:head>
  <title>Servers · settings</title>
</svelte:head>

<header class="mb-6 flex items-baseline justify-between gap-4">
  <div>
    <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
    <h1 class="mt-1 text-xl font-medium tracking-tight">Servers</h1>
    <p class="mt-1 text-sm text-fg-secondary">
      Register a new host or remove one from the panel. Removing a server
      does not stop its agent — uninstall the binary on the host if you
      want to fully retire it.
    </p>
  </div>
  <button
    type="button"
    onclick={() => {
      showAdd = true;
      lastCreated = null;
    }}
    class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] transition-colors hover:bg-elev-2"
    style:border-color="var(--border-accent)"
    style:color="var(--border-accent)"
  >
    + add server
  </button>
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
{:else if servers.length === 0}
  <div class="rounded border border-border bg-elev-1 px-6 py-10 text-center">
    <div class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">empty</div>
    <h2 class="mt-2 text-md font-medium">No servers registered</h2>
    <p class="mt-2 text-sm text-fg-secondary">Click "+ add server" to mint your first install command.</p>
  </div>
{:else}
  <div class="overflow-hidden rounded border border-border">
    <table class="w-full font-mono text-xs">
      <thead class="bg-elev-1 text-2xs uppercase tracking-[0.14em] text-fg-quaternary">
        <tr>
          <th class="px-3 py-2 text-left">name</th>
          <th class="px-3 py-2 text-left">group</th>
          <th class="px-3 py-2 text-left">os</th>
          <th class="px-3 py-2 text-left">agent</th>
          <th class="px-3 py-2 text-left">last seen</th>
          <th class="px-3 py-2 text-right">actions</th>
        </tr>
      </thead>
      <tbody>
        {#each servers as s (s.id)}
          <tr class="border-t border-border">
            <td class="px-3 py-2">
              {#if editingId === s.id}
                <form onsubmit={(e) => { e.preventDefault(); void saveEdit(); }} class="flex gap-1.5">
                  <input
                    type="text"
                    bind:value={editName}
                    disabled={savingEdit}
                    class="flex-1 rounded border border-border bg-recess px-2 py-1 font-mono text-xs"
                  />
                  <button
                    type="submit"
                    class="text-accent hover:underline"
                  >save</button>
                  <button
                    type="button"
                    onclick={cancelEdit}
                    class="text-fg-tertiary hover:text-fg"
                  >cancel</button>
                </form>
              {:else}
                <div class="flex items-center gap-2">
                  <span
                    class="inline-block h-1.5 w-1.5 rounded-full"
                    style:background={s.online ? 'var(--status-online)' : 'var(--status-error)'}
                  ></span>
                  <a href={`/servers/${s.id}`} class="text-fg hover:underline">{s.display_name}</a>
                </div>
              {/if}
            </td>
            <td class="px-3 py-2 text-fg-tertiary">{s.group_name ?? '—'}</td>
            <td class="px-3 py-2 text-fg-tertiary">
              {s.hardware?.os ?? '—'}{s.hardware?.arch ? ' · ' + s.hardware.arch : ''}
            </td>
            <td class="px-3 py-2 text-fg-tertiary">{s.agent_version ?? '—'}</td>
            <td class="px-3 py-2 text-fg-tertiary">{ageFromIso(s.last_seen_at, nowTick)}</td>
            <td class="px-3 py-2 text-right">
              {#if editingId !== s.id}
                <button
                  type="button"
                  onclick={() => startEdit(s)}
                  class="mr-3 text-fg-tertiary hover:text-fg"
                >rename</button>
                <button
                  type="button"
                  onclick={() => handleDelete(s.id, s.display_name)}
                  class="text-error hover:underline"
                >delete</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

{#if showAdd}
  <button
    type="button"
    onclick={() => (showAdd = false)}
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
        >copy</button>
        <button
          type="button"
          onclick={() => {
            showAdd = false;
            lastCreated = null;
          }}
          class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em]"
          style:border-color="var(--border-accent)"
          style:color="var(--border-accent)"
        >done</button>
      </div>
    {:else}
      <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">new server</div>
      <h2 class="mb-4 text-md font-medium text-fg">Register a host</h2>
      <label class="block">
        <span class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
          display name
        </span>
        <input
          type="text"
          bind:value={newName}
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
          onclick={() => (showAdd = false)}
          class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
        >cancel</button>
        <button
          type="button"
          disabled={creating || !newName.trim()}
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
