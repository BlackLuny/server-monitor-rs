<!--
  Server admin page. The overview is read-only by design — anything that
  mutates a server (add / edit / delete) lives here behind the admin
  guard so a stray click on a card on the dashboard can't cause damage.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { authStore } from '$lib/auth.svelte';
  import {
    adminCreateServer,
    deleteServer,
    listGroups,
    listServers,
    updateServer,
    type CreatedServer,
    type GroupRow,
    type ServerRow
  } from '$lib/api';
  import { ageFromIso } from '$lib/format';

  let servers = $state<ServerRow[]>([]);
  let groups = $state<GroupRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let nowTick = $state(Date.now());

  let showAdd = $state(false);
  let newName = $state('');
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let lastCreated = $state<CreatedServer | null>(null);

  // Edit modal state — one server at a time.
  let editing = $state<ServerRow | null>(null);
  let editForm = $state({
    display_name: '',
    group_id: null as number | null,
    tags_text: '',
    location: '',
    flag_emoji: '',
    hidden_from_guest: false,
    terminal_enabled: true,
    ssh_recording: 'default' as 'default' | 'on' | 'off'
  });
  let savingEdit = $state(false);
  let editError = $state<string | null>(null);

  async function reload() {
    try {
      const [res, grps] = await Promise.all([listServers(), listGroups().catch(() => [])]);
      servers = res.servers;
      groups = grps;
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
    if (
      !confirm(
        `Remove "${name}" from the panel? The agent process keeps running until you stop it on the host.`
      )
    )
      return;
    try {
      await deleteServer(id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  function openEdit(s: ServerRow) {
    editing = s;
    editForm = {
      display_name: s.display_name,
      group_id: s.group_id,
      tags_text: (s.tags ?? []).join(', '),
      location: s.location ?? '',
      flag_emoji: s.flag_emoji ?? '',
      hidden_from_guest: s.hidden_from_guest ?? false,
      terminal_enabled: s.terminal_enabled ?? true,
      ssh_recording: (s.ssh_recording ?? 'default') as 'default' | 'on' | 'off'
    };
    editError = null;
  }

  function closeEdit() {
    editing = null;
    editError = null;
  }

  /** Parse a comma-separated tag list into a clean string[] — trim each
   *  segment, drop empties, dedupe. Empty input collapses to an empty
   *  array, which the API accepts as "no tags." */
  function parseTags(raw: string): string[] {
    const out = new Set<string>();
    for (const seg of raw.split(',')) {
      const t = seg.trim();
      if (t) out.add(t);
    }
    return Array.from(out);
  }

  async function saveEdit() {
    if (!editing) return;
    const name = editForm.display_name.trim();
    if (!name) {
      editError = 'display name is required';
      return;
    }
    savingEdit = true;
    editError = null;
    try {
      const body: Record<string, unknown> = {
        display_name: name,
        // Option<Option<T>> on the backend — use null to clear, a value to
        // set, omit to leave alone. The frontend always sends both halves
        // because the modal shows them as edit fields.
        group_id: editForm.group_id, // null clears
        tags: parseTags(editForm.tags_text),
        location: editForm.location.trim() || null,
        flag_emoji: editForm.flag_emoji.trim() || null,
        hidden_from_guest: editForm.hidden_from_guest,
        terminal_enabled: editForm.terminal_enabled,
        ssh_recording: editForm.ssh_recording
      };
      await updateServer(editing.id, body);
      closeEdit();
      await reload();
    } catch (err) {
      editError = err instanceof Error ? err.message : String(err);
    } finally {
      savingEdit = false;
    }
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
      Register a new host or edit an existing one. Removing a server here does
      not stop its agent — uninstall the binary on the host if you want to
      fully retire it.
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
    <p class="mt-2 text-sm text-fg-secondary">
      Click "+ add server" to mint your first install command.
    </p>
  </div>
{:else}
  <div class="overflow-hidden rounded border border-border">
    <table class="w-full font-mono text-xs">
      <thead class="bg-elev-1 text-2xs uppercase tracking-[0.14em] text-fg-quaternary">
        <tr>
          <th class="px-3 py-2 text-left">name</th>
          <th class="px-3 py-2 text-left">group</th>
          <th class="px-3 py-2 text-left">tags</th>
          <th class="px-3 py-2 text-left">visibility</th>
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
              <div class="flex items-center gap-2">
                <span
                  class="inline-block h-1.5 w-1.5 rounded-full"
                  style:background={s.online ? 'var(--status-online)' : 'var(--status-error)'}
                ></span>
                <a href={`/servers/${s.id}`} class="text-fg hover:underline">{s.display_name}</a>
                {#if s.flag_emoji}
                  <span title={s.location ?? ''}>{s.flag_emoji}</span>
                {/if}
              </div>
            </td>
            <td class="px-3 py-2 text-fg-tertiary">{s.group_name ?? '—'}</td>
            <td class="px-3 py-2 text-fg-tertiary">
              {#if s.tags && s.tags.length}
                <div class="flex flex-wrap gap-1">
                  {#each s.tags as t}
                    <span class="rounded border border-border bg-recess px-1.5 py-0.5 text-2xs">
                      {t}
                    </span>
                  {/each}
                </div>
              {:else}
                —
              {/if}
            </td>
            <td class="px-3 py-2 text-fg-tertiary">
              {s.hidden_from_guest ? 'admin only' : 'public'}
            </td>
            <td class="px-3 py-2 text-fg-tertiary">
              {s.hardware?.os ?? '—'}{s.hardware?.arch ? ' · ' + s.hardware.arch : ''}
            </td>
            <td class="px-3 py-2 text-fg-tertiary">{s.agent_version ?? '—'}</td>
            <td class="px-3 py-2 text-fg-tertiary">{ageFromIso(s.last_seen_at, nowTick)}</td>
            <td class="px-3 py-2 text-right">
              <button
                type="button"
                onclick={() => openEdit(s)}
                class="mr-3 text-fg-tertiary hover:text-fg"
              >edit</button>
              <button
                type="button"
                onclick={() => handleDelete(s.id, s.display_name)}
                class="text-error hover:underline"
              >delete</button>
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
      <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
        new server
      </div>
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

{#if editing}
  <button
    type="button"
    onclick={closeEdit}
    class="fixed inset-0 z-40 cursor-default bg-black/60"
    aria-label="close"
  ></button>
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Edit server"
    class="fixed left-1/2 top-1/2 z-50 max-h-[90vh] w-[520px] -translate-x-1/2 -translate-y-1/2 overflow-auto rounded border border-border bg-elev-1 p-6"
  >
    <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">
      edit server
    </div>
    <h2 class="mb-4 text-md font-medium text-fg">{editing.display_name}</h2>

    <form
      onsubmit={(e) => {
        e.preventDefault();
        void saveEdit();
      }}
      class="grid gap-3"
    >
      <label class="block">
        <span class="mb-1 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
          display name
        </span>
        <input
          type="text"
          bind:value={editForm.display_name}
          disabled={savingEdit}
          class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
      </label>

      <label class="block">
        <span class="mb-1 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
          group
        </span>
        <select
          bind:value={editForm.group_id}
          disabled={savingEdit}
          class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg"
        >
          <option value={null}>— unassigned —</option>
          {#each groups as g (g.id)}
            <option value={g.id}>{g.name}</option>
          {/each}
        </select>
        {#if groups.length === 0}
          <p class="mt-1 font-mono text-2xs text-fg-quaternary">
            no groups yet — create them in <a href="/settings/groups" class="underline">settings → groups</a>
          </p>
        {/if}
      </label>

      <label class="block">
        <span class="mb-1 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
          tags · comma separated
        </span>
        <input
          type="text"
          bind:value={editForm.tags_text}
          disabled={savingEdit}
          placeholder="prod, web, eu-west"
          class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
      </label>

      <div class="grid grid-cols-2 gap-3">
        <label class="block">
          <span class="mb-1 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
            location
          </span>
          <input
            type="text"
            bind:value={editForm.location}
            disabled={savingEdit}
            placeholder="Frankfurt"
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>
        <label class="block">
          <span class="mb-1 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
            flag · emoji
          </span>
          <input
            type="text"
            bind:value={editForm.flag_emoji}
            disabled={savingEdit}
            maxlength="4"
            placeholder="🇩🇪"
            class="block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          />
        </label>
      </div>

      <fieldset class="grid gap-2 rounded border border-border bg-recess px-3 py-3">
        <legend class="px-1 font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
          access
        </legend>
        <label class="flex items-center gap-2 font-mono text-xs text-fg-secondary">
          <input
            type="checkbox"
            bind:checked={editForm.hidden_from_guest}
            disabled={savingEdit}
          />
          <span>hide from guest dashboard</span>
        </label>
        <label class="flex items-center gap-2 font-mono text-xs text-fg-secondary">
          <input
            type="checkbox"
            bind:checked={editForm.terminal_enabled}
            disabled={savingEdit}
          />
          <span>web SSH terminal enabled</span>
        </label>
        <div class="flex items-center gap-3 font-mono text-xs">
          <span class="text-fg-tertiary">recording</span>
          {#each ['default', 'on', 'off'] as opt}
            <label class="flex items-center gap-1.5">
              <input
                type="radio"
                value={opt}
                bind:group={editForm.ssh_recording}
                disabled={savingEdit}
              />
              <span class="text-fg-secondary">{opt}</span>
            </label>
          {/each}
        </div>
      </fieldset>

      {#if editError}
        <div
          class="rounded border border-border bg-recess px-3 py-2 font-mono text-xs"
          style="color: var(--status-error)"
        >
          {editError}
        </div>
      {/if}

      <div class="mt-2 flex justify-end gap-2">
        <button
          type="button"
          onclick={closeEdit}
          class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
        >cancel</button>
        <button
          type="submit"
          disabled={savingEdit || !editForm.display_name.trim()}
          class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
          style:border-color="var(--border-accent)"
          style:color="var(--border-accent)"
        >
          {savingEdit ? 'saving…' : 'save'}
        </button>
      </div>
    </form>
  </div>
{/if}
