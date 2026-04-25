<!--
  First-run wizard. Reachable at /setup. The panel exposes the matching
  backend only when the `users` table is empty; once anyone submits, the
  endpoint starts returning 403 and we redirect subsequent visitors to /.

  Layout: split screen — left a manifesto-style headline pinning the brand,
  right a single focused form. Deliberately monochrome: the one accent is
  the submit button, so users' eyes land where they can act.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { ApiError, getSetupStatus, runSetup } from '$lib/api';
  import { authStore } from '$lib/auth.svelte';

  const MIN_PASSWORD = 8;

  let username = $state('');
  let password = $state('');
  let confirm = $state('');
  let submitting = $state(false);
  let formError = $state<string | null>(null);
  let loaded = $state(false);

  // We only show the form once we've confirmed setup is actually needed.
  // Hitting /setup after first-run completion would otherwise silently 403
  // on submit; this redirect keeps the URL honest.
  onMount(async () => {
    try {
      const s = await getSetupStatus();
      if (s.initialized) {
        await goto('/', { replaceState: true });
        return;
      }
    } catch {
      /* Keep loaded=false → status line shows the error state. */
      formError = 'Panel is unreachable. Retry in a moment.';
    }
    loaded = true;
  });

  const passwordTooShort = $derived(password.length > 0 && password.length < MIN_PASSWORD);
  const mismatch = $derived(confirm.length > 0 && confirm !== password);
  const canSubmit = $derived(
    username.trim().length > 0 &&
      password.length >= MIN_PASSWORD &&
      confirm === password &&
      !submitting
  );

  async function handleSubmit(ev: SubmitEvent) {
    ev.preventDefault();
    if (!canSubmit) return;
    formError = null;
    submitting = true;
    try {
      await runSetup(username.trim(), password);
      // Re-sync the global store so the layout's redirect guard sees both
      // `initialized=true` and the freshly issued session before we leave
      // this route. Without this the layout snapshots the stale state and
      // bounces back here in a redirect loop.
      await authStore.refresh();
      await goto('/', { replaceState: true });
    } catch (err) {
      if (err instanceof ApiError) {
        formError = err.message || err.code;
      } else {
        formError = 'Unexpected error — check the panel logs.';
      }
      submitting = false;
    }
  }
</script>

<svelte:head>
  <title>Set up · server-monitor</title>
</svelte:head>

<div class="flex min-h-screen">
  <!-- Left: static brand / manifesto. Hidden on narrow viewports. -->
  <aside
    class="hidden flex-1 flex-col justify-between border-r border-border p-10 md:flex"
    style="background: linear-gradient(180deg, var(--bg-elev-1) 0%, var(--bg-base) 100%);"
  >
    <div>
      <div class="mb-1 font-mono text-2xs uppercase tracking-[0.18em] text-fg-quaternary">
        monitor-panel · v0
      </div>
      <div class="flex items-center gap-2">
        <span class="inline-block h-2 w-2 rounded-full" style="background: var(--status-online)"
        ></span>
        <span class="font-mono text-xs text-fg-secondary">ready for first admin</span>
      </div>
    </div>

    <div>
      <h1 class="text-3xl font-medium leading-tight text-fg">
        A lightweight control plane<br />for the servers you own.
      </h1>
      <p class="mt-4 max-w-md text-sm text-fg-secondary">
        Set up the first administrator to unlock the dashboard. Guests can still see any server you
        mark public; everything else stays behind this login.
      </p>
    </div>

    <ul class="space-y-2 font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary">
      <li>▸ real-time metrics, 5-second resolution</li>
      <li>▸ agent-forwarded ssh, record toggle per-server</li>
      <li>▸ postgres-backed, runs in one container</li>
    </ul>
  </aside>

  <!-- Right: form column -->
  <main class="flex flex-1 items-center justify-center px-6 py-14">
    <div class="w-full max-w-[360px]">
      <div class="mb-8">
        <div class="font-mono text-2xs uppercase tracking-[0.2em] text-fg-quaternary">
          step 01 / 01
        </div>
        <h2 class="mt-2 text-xl font-medium text-fg">Create administrator</h2>
        <p class="mt-1 text-sm text-fg-secondary">
          This account manages every server, user, and setting on the panel.
        </p>
      </div>

      {#if !loaded && !formError}
        <div class="font-mono text-xs text-fg-tertiary">checking setup status…</div>
      {:else}
        <form class="space-y-5" onsubmit={handleSubmit}>
          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >
              username
            </span>
            <input
              type="text"
              autocomplete="username"
              required
              bind:value={username}
              disabled={submitting}
              class="block w-full rounded border border-border bg-recess px-3 py-2 text-sm font-mono text-fg placeholder:text-fg-quaternary focus:border-border-accent"
              placeholder="admin"
            />
          </label>

          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >
              password
            </span>
            <input
              type="password"
              autocomplete="new-password"
              required
              bind:value={password}
              disabled={submitting}
              class="block w-full rounded border border-border bg-recess px-3 py-2 text-sm font-mono text-fg focus:border-border-accent"
            />
            <span class="mt-1 block font-mono text-2xs text-fg-quaternary">
              minimum {MIN_PASSWORD} characters
              {#if passwordTooShort}
                <span class="ml-1" style="color: var(--status-warn)">
                  · {password.length}/{MIN_PASSWORD}
                </span>
              {/if}
            </span>
          </label>

          <label class="block">
            <span
              class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >
              confirm
            </span>
            <input
              type="password"
              autocomplete="new-password"
              required
              bind:value={confirm}
              disabled={submitting}
              class="block w-full rounded border border-border bg-recess px-3 py-2 text-sm font-mono text-fg focus:border-border-accent"
            />
            {#if mismatch}
              <span
                class="mt-1 block font-mono text-2xs"
                style="color: var(--status-warn)"
              >
                passwords do not match
              </span>
            {/if}
          </label>

          {#if formError}
            <div
              class="rounded border border-border bg-recess px-3 py-2 font-mono text-xs"
              style="color: var(--status-error)"
            >
              {formError}
            </div>
          {/if}

          <button
            type="submit"
            disabled={!canSubmit}
            class="mt-2 block w-full rounded border px-3 py-2 text-sm font-medium transition-colors ease-out-quart focus-visible:outline-1 disabled:cursor-not-allowed disabled:opacity-40"
            style:border-color="var(--border-accent)"
            style:color={canSubmit ? 'var(--bg-base)' : 'var(--fg-tertiary)'}
            style:background={canSubmit ? 'var(--border-accent)' : 'transparent'}
          >
            {submitting ? 'Creating admin…' : 'Create admin & sign in'}
          </button>
        </form>
      {/if}
    </div>
  </main>
</div>
