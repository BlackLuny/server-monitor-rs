<!--
  Terminal page — admin-only. Wraps the Terminal component, handles auth
  gating + a thin top bar with a "back to detail" link. The component itself
  owns the WebSocket bridge.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { authStore } from '$lib/auth.svelte';
  import { listServers, type ServerRow } from '$lib/api';
  import Terminal from '$lib/Terminal.svelte';

  let server = $state<ServerRow | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const serverId = $derived(Number(page.params.id));

  onMount(async () => {
    if (!authStore.state.loaded) await authStore.refresh();
    if (!authStore.state.user || authStore.state.user.role !== 'admin') {
      await goto(`/login?next=/servers/${serverId}/terminal`, { replaceState: true });
      return;
    }
    try {
      const all = await listServers();
      server = all.servers.find((s) => s.id === serverId) ?? null;
      if (!server) {
        error = 'server not found';
      } else if (!server.online) {
        error = 'agent is offline — terminal unavailable';
      }
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  });
</script>

<svelte:head>
  <title>{server?.display_name ?? 'terminal'} · ssh</title>
</svelte:head>

<div class="min-h-screen bg-page-bg">
  <header class="border-b border-border">
    <div class="mx-auto flex max-w-screen-xl items-center justify-between gap-4 px-6 py-3">
      <a
        href={`/servers/${serverId}`}
        class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary hover:text-fg"
      >
        ← back to detail
      </a>
      <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
        ssh · {server?.display_name ?? '…'}
      </span>
      <span></span>
    </div>
  </header>

  <main class="mx-auto max-w-screen-xl px-6 py-6">
    {#if loading}
      <div class="py-20 text-center font-mono text-xs text-fg-tertiary">loading…</div>
    {:else if error}
      <div
        class="rounded border px-4 py-3 text-sm text-error"
        style:background="color-mix(in oklch, var(--status-error) 6%, transparent)"
        style:border-color="color-mix(in oklch, var(--status-error) 28%, transparent)"
      >
        {error}
      </div>
    {:else if server}
      <Terminal {serverId} serverName={server.display_name} />
      <p class="mt-4 font-mono text-2xs uppercase tracking-wider text-fg-quaternary">
        recordings (when enabled) live on the agent at
        <code class="text-fg-tertiary">/var/lib/monitor-agent/recordings</code> ·
        playback with <code class="text-fg-tertiary">asciinema play</code>
      </p>
    {/if}
  </main>
</div>
