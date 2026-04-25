<!--
  Root layout. Owns the one-shot auth bootstrap and the persistent site
  chrome. Public auth pages (`/setup`, `/login`) suppress the chrome so they
  can present the focused split-screen forms. The chrome appears everywhere
  else with a per-user pill + admin nav.
-->
<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import SiteHeader from '$lib/SiteHeader.svelte';
  import { authStore } from '$lib/auth.svelte';

  let { children } = $props();

  // The two pages that exist precisely to handle the *not yet* states.
  // Listing them avoids the redirect loop bug where /login redirects to /
  // and the layout sees no user there and bounces back.
  const publicRoutes = ['/setup', '/login'];

  let bootError = $state<string | null>(null);

  onMount(async () => {
    try {
      await authStore.refresh();
    } catch (err) {
      bootError = err instanceof Error ? err.message : String(err);
    }
  });

  // Redirect logic runs whenever auth state changes:
  //   - panel uninitialized → /setup (only path that's allowed in that
  //     state besides the dashboard's own redirect)
  //   - admin-only path with no session → /login
  //   - everything else stays as-is so anonymous viewing of `/` works.
  $effect(() => {
    if (!authStore.state.loaded) return;
    const path = $page.url.pathname;

    if (!authStore.state.initialized && path !== '/setup') {
      goto('/setup', { replaceState: true });
      return;
    }
    if (publicRoutes.includes(path)) return;
    if (!authStore.state.user && path.startsWith('/settings')) {
      goto('/login', { replaceState: true });
    }
  });

  const showChrome = $derived(
    authStore.state.loaded &&
      !publicRoutes.includes($page.url.pathname) &&
      !$page.url.pathname.startsWith('/setup')
  );
</script>

{#if bootError}
  <div class="flex min-h-screen items-center justify-center px-6">
    <div class="max-w-md text-center">
      <h1 class="text-xl font-medium text-fg">Panel unreachable</h1>
      <p class="mt-2 font-mono text-xs text-fg-tertiary">{bootError}</p>
    </div>
  </div>
{:else if !authStore.state.loaded}
  <div class="flex min-h-screen items-center justify-center">
    <span class="font-mono text-2xs uppercase tracking-[0.2em] text-fg-quaternary">loading</span>
  </div>
{:else}
  {#if showChrome}
    <SiteHeader />
  {/if}
  {@render children()}
{/if}
