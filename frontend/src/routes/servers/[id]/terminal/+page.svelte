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
  import {
    listServers,
    listTerminalSessions,
    recordingDownloadUrl,
    type ServerRow,
    type TerminalSessionRow
  } from '$lib/api';
  import Terminal from '$lib/Terminal.svelte';

  let server = $state<ServerRow | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let sessions = $state<TerminalSessionRow[]>([]);
  let sessionsLoading = $state(false);

  const serverId = $derived(Number(page.params.id));

  async function refreshSessions() {
    sessionsLoading = true;
    try {
      sessions = await listTerminalSessions(serverId);
    } catch {
      // non-fatal — the live shell still works without history
    } finally {
      sessionsLoading = false;
    }
  }

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
      } else {
        void refreshSessions();
      }
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  });

  function fmtBytes(n: number | null): string {
    if (!n || n <= 0) return '—';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / 1024 / 1024).toFixed(2)} MiB`;
  }

  function fmtTs(s: string): string {
    return new Date(s).toLocaleString();
  }
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

      <section class="mt-8">
        <header class="mb-3 flex items-baseline justify-between">
          <h2 class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
            recordings
          </h2>
          <button
            type="button"
            onclick={() => void refreshSessions()}
            class="font-mono text-2xs uppercase tracking-wider text-fg-quaternary hover:text-fg-tertiary"
            disabled={sessionsLoading}
          >
            {sessionsLoading ? 'refreshing…' : 'refresh'}
          </button>
        </header>

        {#if sessions.length === 0 && !sessionsLoading}
          <p class="font-mono text-2xs uppercase tracking-wider text-fg-quaternary">
            no recordings yet
          </p>
        {:else}
          <div class="overflow-hidden rounded border border-border">
            <table class="w-full font-mono text-xs">
              <thead class="bg-page-bg/50 text-2xs uppercase tracking-wider text-fg-quaternary">
                <tr>
                  <th class="px-3 py-2 text-left">opened</th>
                  <th class="px-3 py-2 text-left">user</th>
                  <th class="px-3 py-2 text-left">exit</th>
                  <th class="px-3 py-2 text-left">size</th>
                  <th class="px-3 py-2 text-right"></th>
                </tr>
              </thead>
              <tbody>
                {#each sessions as s (s.id)}
                  <tr class="border-t border-border">
                    <td class="px-3 py-2 text-fg-secondary">{fmtTs(s.opened_at)}</td>
                    <td class="px-3 py-2 text-fg-tertiary">{s.username ?? '—'}</td>
                    <td class="px-3 py-2 text-fg-tertiary">
                      {s.closed_at ? (s.exit_code ?? '?') : 'live'}
                    </td>
                    <td class="px-3 py-2 text-fg-tertiary">{fmtBytes(s.recording_size)}</td>
                    <td class="px-3 py-2 text-right">
                      {#if s.recording_path}
                        <a
                          href={recordingDownloadUrl(s.id)}
                          download={`${s.id}.cast`}
                          class="text-accent hover:underline"
                        >
                          download .cast
                        </a>
                      {:else}
                        <span class="text-fg-quaternary">—</span>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}

        <p class="mt-3 font-mono text-2xs uppercase tracking-wider text-fg-quaternary">
          playback locally with <code class="text-fg-tertiary">asciinema play file.cast</code>
        </p>
      </section>
    {/if}
  </main>
</div>
