<!--
  Server-group management. Each row is editable in place; Save commits a
  PATCH, Delete drops it (servers in that group fall back to "unassigned").
  Adding a group sits at the top so the create flow is always reachable.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import {
    createGroup,
    deleteGroup,
    listGroups,
    updateGroup,
    type GroupRow
  } from '$lib/api';

  let rows = $state<GroupRow[]>([]);
  let loading = $state(true);
  let listError = $state<string | null>(null);

  let newName = $state('');
  let newColor = $state('');
  let creating = $state(false);
  let createError = $state<string | null>(null);

  onMount(reload);

  async function reload() {
    try {
      rows = await listGroups();
      listError = null;
    } catch (err) {
      listError = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  async function handleCreate() {
    if (!newName.trim()) return;
    creating = true;
    createError = null;
    try {
      await createGroup({
        name: newName.trim(),
        color: newColor.trim() ? newColor.trim() : null
      });
      newName = '';
      newColor = '';
      await reload();
    } catch (err) {
      createError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  async function handleSave(g: GroupRow) {
    try {
      await updateGroup(g.id, {
        name: g.name,
        order_idx: g.order_idx,
        description: g.description ?? null,
        color: g.color ?? null
      });
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleDelete(g: GroupRow) {
    if (!confirm(`Delete group "${g.name}"?`)) return;
    try {
      await deleteGroup(g.id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }
</script>

<svelte:head>
  <title>Groups · settings</title>
</svelte:head>

<header class="mb-6">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
  <h1 class="mt-1 text-xl font-medium tracking-tight">Groups</h1>
  <p class="mt-1 text-sm text-fg-secondary">
    Buckets for organizing servers on the dashboard.
  </p>
</header>

<section class="mb-6 rounded border border-border bg-elev-1 px-5 py-4">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">add group</div>
  <div class="mt-3 grid grid-cols-1 gap-3 md:grid-cols-[1fr_180px_auto]">
    <input
      type="text"
      bind:value={newName}
      placeholder="group name"
      disabled={creating}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <input
      type="text"
      bind:value={newColor}
      placeholder="optional color (#hex)"
      disabled={creating}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <button
      type="button"
      onclick={handleCreate}
      disabled={creating || !newName.trim()}
      class="rounded border px-3 py-2 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
      style:border-color="var(--border-accent)"
      style:color="var(--border-accent)"
    >
      {creating ? 'creating…' : 'create'}
    </button>
  </div>
  {#if createError}
    <div class="mt-2 font-mono text-2xs" style="color: var(--status-error)">{createError}</div>
  {/if}
</section>

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
  <div class="rounded border border-dashed border-border px-5 py-10 text-center text-sm text-fg-tertiary">
    no groups yet
  </div>
{:else}
  <div class="space-y-3">
    {#each rows as g (g.id)}
      <div
        class="grid grid-cols-1 gap-3 rounded border border-border bg-elev-1 px-5 py-4 md:grid-cols-[1fr_120px_140px_auto] md:items-center"
      >
        <input
          type="text"
          bind:value={g.name}
          class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
        <input
          type="number"
          bind:value={g.order_idx}
          class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
          title="display order"
        />
        <input
          type="text"
          bind:value={g.color}
          placeholder="#hex"
          class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
        <div class="flex justify-end gap-2">
          <button
            type="button"
            onclick={() => handleSave(g)}
            class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em]"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            save
          </button>
          <button
            type="button"
            onclick={() => handleDelete(g)}
            class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-error"
          >
            delete
          </button>
        </div>
      </div>
    {/each}
  </div>
{/if}
