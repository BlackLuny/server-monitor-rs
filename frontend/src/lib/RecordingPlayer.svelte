<!--
  In-browser asciinema replay. Mounts asciinema-player against the panel's
  /api/recordings/:id/download stream. The player handles the v2 .cast
  parsing, scrubbing, speed control, and keyboard shortcuts itself; we
  just wrap it in a modal-friendly dispose pattern.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  // Type-only import keeps the bundle tree-shakeable; the runtime side is
  // dynamically imported in onMount so SSR builds don't try to resolve the
  // browser-only player module.
  import 'asciinema-player/dist/bundle/asciinema-player.css';
  import { recordingDownloadUrl } from './api';

  interface Props {
    sessionId: string;
    /** Override the source URL — useful for tests. */
    src?: string;
    /** uPlot style cols/rows hint; the player auto-fits otherwise. */
    cols?: number;
    rows?: number;
  }

  let { sessionId, src, cols, rows }: Props = $props();

  let container: HTMLDivElement;
  let player: { dispose?: () => void } | null = null;
  let error = $state<string | null>(null);
  let loading = $state(true);

  onMount(async () => {
    try {
      const mod = await import('asciinema-player');
      const target = src ?? recordingDownloadUrl(sessionId);
      // The 'fetch' source uses the browser's Fetch API and forwards the
      // session cookie automatically (same-origin). It also gracefully
      // handles a non-200 by surfacing an `error` event we can listen to.
      player = mod.create(
        { url: target, fetchOpts: { credentials: 'same-origin' } },
        container,
        {
          theme: 'monokai',
          fit: 'width',
          terminalFontFamily: '"JetBrains Mono Variable", monospace',
          cols,
          rows,
          autoPlay: false,
          idleTimeLimit: 2 // jump dead air longer than 2s — no point waiting in playback
        }
      );
      loading = false;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
      loading = false;
    }
  });

  onDestroy(() => {
    try {
      player?.dispose?.();
    } catch {
      // best-effort cleanup; the modal is being torn down anyway
    }
  });
</script>

<div class="rounded border border-border bg-recess">
  {#if loading}
    <div class="px-4 py-8 text-center font-mono text-xs text-fg-tertiary">loading recording…</div>
  {/if}
  {#if error}
    <div
      class="px-4 py-3 font-mono text-xs"
      style="color: var(--status-error)"
    >
      failed to load recording: {error}
    </div>
  {/if}
  <div bind:this={container} class="ap-host"></div>
</div>

<style>
  /* asciinema-player ships its own black background; let it bleed to the
     edges of our panel so the chrome doesn't look stacked on chrome. */
  :global(.ap-host .ap-player) {
    background: transparent;
  }
  :global(.ap-host .asciinema-player-wrapper) {
    border-radius: 4px;
    overflow: hidden;
  }
</style>
