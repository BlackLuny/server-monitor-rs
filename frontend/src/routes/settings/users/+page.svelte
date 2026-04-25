<!--
  Admin user management. Lists every account, lets the current admin add a
  new admin, reset anyone's password, and delete an account that isn't
  themselves and isn't the last remaining admin.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { authStore } from '$lib/auth.svelte';
  import {
    createUser,
    deleteUser,
    listUsers,
    resetUserPassword,
    type AdminUser
  } from '$lib/api';

  let rows = $state<AdminUser[]>([]);
  let loading = $state(true);

  // create form
  let newName = $state('');
  let newPassword = $state('');
  let creating = $state(false);
  let createError = $state<string | null>(null);

  // password reset modal
  let resetTarget = $state<AdminUser | null>(null);
  let resetPw = $state('');
  let resetting = $state(false);
  let resetError = $state<string | null>(null);

  onMount(reload);

  async function reload() {
    try {
      rows = await listUsers();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    } finally {
      loading = false;
    }
  }

  async function handleCreate() {
    if (!newName.trim() || newPassword.length < 8) {
      createError = 'username + 8-char password required';
      return;
    }
    creating = true;
    createError = null;
    try {
      await createUser(newName.trim(), newPassword);
      newName = '';
      newPassword = '';
      await reload();
    } catch (err) {
      createError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  async function handleDelete(u: AdminUser) {
    if (u.id === authStore.state.user?.user_id) {
      alert("you can't delete yourself");
      return;
    }
    if (!confirm(`Delete admin "${u.username}"?`)) return;
    try {
      await deleteUser(u.id);
      await reload();
    } catch (err) {
      alert(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleReset() {
    if (!resetTarget) return;
    if (resetPw.length < 8) {
      resetError = 'password must be at least 8 characters';
      return;
    }
    resetting = true;
    resetError = null;
    try {
      await resetUserPassword(resetTarget.id, resetPw);
      resetTarget = null;
      resetPw = '';
    } catch (err) {
      resetError = err instanceof Error ? err.message : String(err);
    } finally {
      resetting = false;
    }
  }

  function fmt(ts: string): string {
    return ts.replace('T', ' ').replace(/\..*/, '');
  }
</script>

<svelte:head>
  <title>Users · settings</title>
</svelte:head>

<header class="mb-6">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
  <h1 class="mt-1 text-xl font-medium tracking-tight">Users</h1>
</header>

<section class="mb-6 rounded border border-border bg-elev-1 px-5 py-4">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">add admin</div>
  <div class="mt-3 grid grid-cols-1 gap-3 md:grid-cols-[1fr_1fr_auto]">
    <input
      type="text"
      bind:value={newName}
      placeholder="username"
      disabled={creating}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <input
      type="password"
      bind:value={newPassword}
      placeholder="password (≥ 8 chars)"
      disabled={creating}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <button
      type="button"
      onclick={handleCreate}
      disabled={creating || !newName.trim() || newPassword.length < 8}
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

{#if loading}
  <div class="font-mono text-xs text-fg-tertiary">loading…</div>
{:else}
  <div class="overflow-hidden rounded border border-border">
    <table class="w-full text-sm">
      <thead class="bg-elev-2">
        <tr>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">user</th>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">role</th>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">2fa</th>
          <th class="px-4 py-2 text-left font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">created</th>
          <th class="px-4 py-2 text-right font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">actions</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as u (u.id)}
          <tr class="border-t border-border">
            <td class="px-4 py-2 font-mono">
              {u.username}
              {#if u.id === authStore.state.user?.user_id}
                <span class="ml-1 text-2xs text-fg-quaternary">(you)</span>
              {/if}
            </td>
            <td class="px-4 py-2 font-mono text-fg-secondary">{u.role}</td>
            <td class="px-4 py-2 font-mono text-fg-secondary">
              {u.totp_enabled ? 'on' : '—'}
            </td>
            <td class="px-4 py-2 font-mono text-2xs text-fg-tertiary">{fmt(u.created_at)}</td>
            <td class="px-4 py-2">
              <div class="flex justify-end gap-2">
                <button
                  type="button"
                  onclick={() => {
                    resetTarget = u;
                    resetPw = '';
                    resetError = null;
                  }}
                  class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
                >
                  reset password
                </button>
                {#if u.id !== authStore.state.user?.user_id}
                  <button
                    type="button"
                    onclick={() => handleDelete(u)}
                    class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-quaternary hover:text-error"
                  >
                    delete
                  </button>
                {/if}
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

{#if resetTarget}
  <button
    type="button"
    onclick={() => (resetTarget = null)}
    class="fixed inset-0 z-40 bg-black/60"
    aria-label="close"
  ></button>
  <div
    role="dialog"
    aria-modal="true"
    class="fixed left-1/2 top-1/2 z-50 w-[400px] -translate-x-1/2 -translate-y-1/2 rounded border border-border bg-elev-1 p-6"
  >
    <div class="mb-1 font-mono text-2xs uppercase tracking-[0.16em] text-fg-tertiary">reset password</div>
    <h2 class="mb-4 text-md font-medium text-fg">{resetTarget.username}</h2>
    <input
      type="password"
      bind:value={resetPw}
      placeholder="new password"
      class="mb-3 block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    {#if resetError}
      <div class="mb-3 font-mono text-2xs" style="color: var(--status-error)">{resetError}</div>
    {/if}
    <div class="flex justify-end gap-2">
      <button
        type="button"
        onclick={() => (resetTarget = null)}
        class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary hover:bg-elev-2"
      >
        cancel
      </button>
      <button
        type="button"
        onclick={handleReset}
        disabled={resetting || resetPw.length < 8}
        class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
        style:border-color="var(--border-accent)"
        style:color="var(--border-accent)"
      >
        {resetting ? 'saving…' : 'apply'}
      </button>
    </div>
  </div>
{/if}
