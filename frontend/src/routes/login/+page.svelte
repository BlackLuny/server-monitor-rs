<!--
  /login — admin sign-in. Pairs visually with /setup (same split-screen
  layout, same field chrome) so the two pages read as a continuous flow.

  The form is single-stage today; when the backend replies with
  `totp_required` we surface a second input for the six-digit code without
  a route change — a full page swap for a six-digit field would feel
  disproportionate.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { ApiError, getSetupStatus, login, whoami } from '$lib/api';
  import { authStore } from '$lib/auth.svelte';

  let username = $state('');
  let password = $state('');
  let totpCode = $state('');
  let needsTotp = $state(false);

  let checking = $state(true);
  let submitting = $state(false);
  let formError = $state<string | null>(null);

  // Cold-load discovery: the panel might not be initialized yet, or the
  // caller might already be signed in. Resolve both before showing a form
  // so users never stare at a login they don't need.
  onMount(async () => {
    try {
      const s = await getSetupStatus();
      if (!s.initialized) {
        await goto('/setup', { replaceState: true });
        return;
      }
      const me = await whoami();
      if (me) {
        await goto('/', { replaceState: true });
        return;
      }
    } catch {
      formError = 'Panel is unreachable. Retry in a moment.';
    }
    checking = false;
  });

  const canSubmit = $derived(
    username.trim().length > 0 &&
      password.length > 0 &&
      (!needsTotp || totpCode.trim().length > 0) &&
      !submitting
  );

  async function handleSubmit(ev: SubmitEvent) {
    ev.preventDefault();
    if (!canSubmit) return;
    formError = null;
    submitting = true;
    try {
      await login(username.trim(), password, needsTotp ? totpCode.trim() : undefined);
      // Sync the global store so the dashboard's auth-derived state (admin
      // CTAs, guest mode, WS scope) is correct on the very first paint
      // after we navigate away.
      await authStore.refresh();
      await goto('/', { replaceState: true });
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.code === 'totp_required') {
          needsTotp = true;
          formError = null;
          totpCode = '';
        } else {
          formError = err.message || err.code;
        }
      } else {
        formError = 'Unexpected error — check the panel logs.';
      }
      submitting = false;
    }
  }
</script>

<svelte:head>
  <title>Sign in · server-monitor</title>
</svelte:head>

<div class="flex min-h-screen">
  <aside
    class="hidden flex-1 flex-col justify-between border-r border-border p-10 md:flex"
    style="background: linear-gradient(180deg, var(--bg-elev-1) 0%, var(--bg-base) 100%);"
  >
    <div>
      <div class="mb-1 font-mono text-2xs uppercase tracking-[0.18em] text-fg-quaternary">
        monitor-panel
      </div>
      <div class="flex items-center gap-2">
        <span class="inline-block h-2 w-2 rounded-full" style="background: var(--status-online)"
        ></span>
        <span class="font-mono text-xs text-fg-secondary">panel online</span>
      </div>
    </div>

    <div>
      <h1 class="text-3xl font-medium leading-tight text-fg">Sign in to continue.</h1>
      <p class="mt-4 max-w-md text-sm text-fg-secondary">
        Guests can browse public servers without signing in. Admins manage the fleet, users, and
        settings.
      </p>
    </div>

    <a
      href="/"
      class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary underline decoration-dotted underline-offset-4 hover:text-fg-secondary"
    >
      ← continue as guest
    </a>
  </aside>

  <main class="flex flex-1 items-center justify-center px-6 py-14">
    <div class="w-full max-w-[360px]">
      <div class="mb-8">
        <div class="font-mono text-2xs uppercase tracking-[0.2em] text-fg-quaternary">
          {needsTotp ? 'step 02 / 02' : 'admin sign-in'}
        </div>
        <h2 class="mt-2 text-xl font-medium text-fg">
          {needsTotp ? 'Two-factor code' : 'Welcome back'}
        </h2>
        <p class="mt-1 text-sm text-fg-secondary">
          {needsTotp
            ? 'Enter the six-digit code from your authenticator app.'
            : 'Use your administrator credentials.'}
        </p>
      </div>

      {#if checking && !formError}
        <div class="font-mono text-xs text-fg-tertiary">checking session…</div>
      {:else}
        <form class="space-y-5" onsubmit={handleSubmit}>
          {#if !needsTotp}
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
                class="block w-full rounded border border-border bg-recess px-3 py-2 text-sm font-mono text-fg focus:border-border-accent"
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
                autocomplete="current-password"
                required
                bind:value={password}
                disabled={submitting}
                class="block w-full rounded border border-border bg-recess px-3 py-2 text-sm font-mono text-fg focus:border-border-accent"
              />
            </label>
          {:else}
            <label class="block">
              <span
                class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
              >
                six-digit code
              </span>
              <input
                type="text"
                inputmode="numeric"
                pattern="[0-9]*"
                maxlength="8"
                autocomplete="one-time-code"
                required
                bind:value={totpCode}
                disabled={submitting}
                class="block w-full rounded border border-border bg-recess px-3 py-2 text-lg font-mono tracking-[0.3em] text-fg focus:border-border-accent"
                placeholder="••••••"
              />
            </label>
          {/if}

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
            {#if submitting}
              Signing in…
            {:else if needsTotp}
              Verify &amp; sign in
            {:else}
              Sign in
            {/if}
          </button>

          {#if needsTotp}
            <button
              type="button"
              onclick={() => {
                needsTotp = false;
                totpCode = '';
                formError = null;
              }}
              class="block w-full text-center font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg-secondary"
            >
              ← start over
            </button>
          {/if}
        </form>
      {/if}
    </div>
  </main>
</div>
