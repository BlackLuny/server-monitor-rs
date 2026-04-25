<!--
  Web SSH terminal — xterm.js bridged to /ws/terminal/:server_id.

  Visuals are loosely inspired by the reference Vue dialog in /zfc: a dark
  terminal panel with the macOS-style traffic-light row, a centered server
  name, and a connection LED in the bottom bar. Implementation is
  hand-rolled for SvelteKit so we don't pull in a wrapper library — xterm
  imperatively writes into a div we own, and we forward data both ways.

  Lifecycle:
    1. onMount → open WebSocket with the current viewport size as query.
    2. WS open → terminal switches from "connecting" to "connected".
    3. ResizeObserver on the host → fit addon recomputes size and we ship
       a `{type:"resize",cols,rows}` text frame so the agent's pty matches.
    4. Terminal data event → binary frame upstream.
    5. Binary frame downstream → term.write.
    6. Final text frame `{type:"closed"...}` → "disconnected" + reason.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { Terminal, type IDisposable } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { WebLinksAddon } from '@xterm/addon-web-links';
  import '@xterm/xterm/css/xterm.css';

  type Props = {
    serverId: number;
    serverName: string;
  };

  let { serverId, serverName }: Props = $props();

  type ConnState = 'connecting' | 'connected' | 'closed' | 'error';

  let host: HTMLDivElement | undefined = $state();
  let viewport: HTMLDivElement | undefined = $state();
  let banner: string | null = $state(null);
  let connState = $state<ConnState>('connecting');
  let exitCode: number | null = $state(null);
  let recordingPath: string | null = $state(null);

  let term: Terminal | null = null;
  let fit: FitAddon | null = null;
  let ws: WebSocket | null = null;
  let dataDisposer: IDisposable | null = null;
  let resizeObs: ResizeObserver | null = null;
  let resizeTimer: ReturnType<typeof setTimeout> | null = null;
  let lastSize = { cols: 0, rows: 0 };

  function buildSocketUrl(cols: number, rows: number): string {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const params = new URLSearchParams({
      cols: String(Math.max(1, cols)),
      rows: String(Math.max(1, rows))
    });
    return `${proto}//${location.host}/ws/terminal/${serverId}?${params.toString()}`;
  }

  function setupTerminal(target: HTMLDivElement) {
    term = new Terminal({
      cursorBlink: true,
      allowProposedApi: true,
      fontFamily: '"JetBrains Mono Variable", Menlo, Monaco, "Courier New", monospace',
      fontSize: 13,
      letterSpacing: 0,
      lineHeight: 1.25,
      theme: {
        background: '#0f1115',
        foreground: '#d4d4d4',
        cursor: '#7ee787',
        cursorAccent: '#0f1115',
        black: '#1e1e1e',
        red: '#f85149',
        green: '#7ee787',
        yellow: '#e3b341',
        blue: '#58a6ff',
        magenta: '#bc8cff',
        cyan: '#56d4dd',
        white: '#d4d4d4',
        brightBlack: '#6e7681',
        brightRed: '#ff7b72',
        brightGreen: '#7ee787',
        brightYellow: '#e3b341',
        brightBlue: '#79c0ff',
        brightMagenta: '#d2a8ff',
        brightCyan: '#56d4dd',
        brightWhite: '#f0f6fc'
      }
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(target);
    fit.fit();
    return { cols: term.cols, rows: term.rows };
  }

  function connect() {
    if (!term || !fit) return;
    const size = fit.proposeDimensions() ?? { cols: 80, rows: 24 };
    lastSize = { cols: size.cols, rows: size.rows };
    connState = 'connecting';
    banner = null;
    exitCode = null;
    recordingPath = null;

    const socket = new WebSocket(buildSocketUrl(size.cols, size.rows));
    socket.binaryType = 'arraybuffer';
    ws = socket;

    socket.onopen = () => {
      connState = 'connected';
      term?.focus();
    };
    socket.onmessage = (ev) => {
      if (typeof ev.data === 'string') {
        try {
          const msg = JSON.parse(ev.data);
          if (msg?.type === 'closed') {
            connState = 'closed';
            exitCode = typeof msg.exit_code === 'number' ? msg.exit_code : null;
            recordingPath = msg.recording?.path || null;
            if (msg.error) {
              banner = msg.error;
            }
          }
        } catch {
          /* unknown text frame — ignore */
        }
        return;
      }
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      term?.write(buf);
    };
    socket.onerror = () => {
      connState = 'error';
      banner = 'connection error — try reconnecting';
    };
    socket.onclose = () => {
      // Only flip to "closed" if we didn't already get a structured close
      // frame; otherwise keep the captured exit_code.
      if (connState === 'connecting' || connState === 'connected') {
        connState = 'closed';
      }
    };

    dataDisposer?.dispose();
    dataDisposer = term.onData((data) => {
      if (socket.readyState === WebSocket.OPEN) {
        const enc = new TextEncoder();
        socket.send(enc.encode(data));
      }
    });
  }

  function shipResize() {
    if (!term || !fit || !ws) return;
    fit.fit();
    const cols = term.cols;
    const rows = term.rows;
    if (cols === lastSize.cols && rows === lastSize.rows) return;
    lastSize = { cols, rows };
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'resize', cols, rows }));
    }
  }

  function reconnect() {
    teardownSocket();
    term?.reset();
    connect();
  }

  function teardownSocket() {
    dataDisposer?.dispose();
    dataDisposer = null;
    if (ws) {
      try {
        ws.close();
      } catch {
        /* noop */
      }
      ws = null;
    }
  }

  onMount(() => {
    if (!viewport) return;
    setupTerminal(viewport);
    connect();
    if (host) {
      resizeObs = new ResizeObserver(() => {
        if (resizeTimer) clearTimeout(resizeTimer);
        resizeTimer = setTimeout(shipResize, 80);
      });
      resizeObs.observe(host);
    }
  });

  onDestroy(() => {
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeObs?.disconnect();
    teardownSocket();
    term?.dispose();
    term = null;
  });

  const ledClass = $derived(
    connState === 'connected'
      ? 'led-on'
      : connState === 'connecting'
        ? 'led-pulse'
        : connState === 'error'
          ? 'led-error'
          : 'led-off'
  );
  const statusLabel = $derived(
    connState === 'connected'
      ? 'active'
      : connState === 'connecting'
        ? 'connecting'
        : connState === 'error'
          ? 'error'
          : exitCode === null
            ? 'closed'
            : `closed · exit ${exitCode}`
  );
</script>

<div class="terminal-window" bind:this={host}>
  <header class="title-bar">
    <div class="traffic-lights" aria-hidden="true">
      <span class="light light-close"></span>
      <span class="light light-min"></span>
      <span class="light light-max"></span>
    </div>
    <div class="title">{serverName}</div>
    <div class="status-pill" data-state={connState}>
      <span class="led {ledClass}"></span>
      <span>{statusLabel}</span>
    </div>
  </header>

  <div class="viewport" bind:this={viewport}></div>

  <footer class="status-bar">
    <span class="led-sm {ledClass}"></span>
    <span class="hint">
      {#if connState === 'connected'}
        session live · paste with ⌘V / ctrl-shift-V
      {:else if connState === 'connecting'}
        opening shell on agent…
      {:else if recordingPath}
        recorded → {recordingPath}
      {:else}
        no session
      {/if}
    </span>
    {#if connState === 'closed' || connState === 'error'}
      <button class="reconnect" type="button" onclick={reconnect}>reconnect</button>
    {/if}
  </footer>

  {#if banner}
    <div class="error-banner" role="alert">
      <span>{banner}</span>
      <button type="button" onclick={() => (banner = null)} aria-label="dismiss">×</button>
    </div>
  {/if}
</div>

<style>
  .terminal-window {
    --bg: #0f1115;
    --chrome: #14171d;
    --chrome-strong: #1a1d24;
    --border: rgba(255, 255, 255, 0.06);
    --fg: #d4d4d4;
    --muted: rgba(212, 212, 212, 0.55);
    position: relative;
    display: flex;
    flex-direction: column;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--bg);
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.4),
      0 30px 60px -20px rgba(0, 0, 0, 0.5),
      inset 0 1px 0 rgba(255, 255, 255, 0.04);
    overflow: hidden;
    color: var(--fg);
  }

  .title-bar {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    height: 38px;
    padding: 0 12px;
    background: linear-gradient(180deg, var(--chrome-strong) 0%, var(--chrome) 100%);
    border-bottom: 1px solid var(--border);
  }

  .traffic-lights {
    display: flex;
    gap: 8px;
  }
  .light {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #3f4350;
    transition: background-color 120ms ease;
  }
  .terminal-window:hover .light-close {
    background: #ff5f56;
  }
  .terminal-window:hover .light-min {
    background: #ffbd2e;
  }
  .terminal-window:hover .light-max {
    background: #27c93f;
  }

  .title {
    font-family: 'JetBrains Mono Variable', Menlo, Monaco, monospace;
    font-size: 12px;
    text-align: center;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    letter-spacing: 0.04em;
  }

  .status-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: rgba(0, 0, 0, 0.3);
    font-family: 'JetBrains Mono Variable', Menlo, Monaco, monospace;
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--muted);
  }
  .status-pill[data-state='connected'] {
    color: #7ee787;
    border-color: rgba(126, 231, 135, 0.35);
    background: rgba(126, 231, 135, 0.08);
  }
  .status-pill[data-state='error'] {
    color: #ff7b72;
    border-color: rgba(255, 123, 114, 0.35);
    background: rgba(255, 123, 114, 0.08);
  }

  .viewport {
    flex: 1;
    height: clamp(320px, 60vh, 700px);
    padding: 10px 14px 6px;
    background: #1e1e1e;
    box-shadow: inset 0 1px 0 rgba(0, 0, 0, 0.55);
  }

  .status-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 26px;
    padding: 0 12px;
    background: var(--chrome);
    border-top: 1px solid var(--border);
    font-family: 'JetBrains Mono Variable', Menlo, Monaco, monospace;
    font-size: 10.5px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .status-bar .hint {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reconnect {
    background: transparent;
    border: 1px solid rgba(126, 231, 135, 0.4);
    color: #7ee787;
    padding: 2px 10px;
    font: inherit;
    border-radius: 6px;
    cursor: pointer;
    transition:
      background 120ms ease,
      transform 80ms ease;
  }
  .reconnect:hover {
    background: rgba(126, 231, 135, 0.12);
  }
  .reconnect:active {
    transform: translateY(1px);
  }

  .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #555;
  }
  .led-sm {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #555;
  }
  .led-on,
  .led-on.led-sm {
    background: #7ee787;
    box-shadow: 0 0 6px rgba(126, 231, 135, 0.6);
  }
  .led-pulse,
  .led-pulse.led-sm {
    background: #e3b341;
    animation: pulse 1.4s ease-in-out infinite;
  }
  .led-error,
  .led-error.led-sm {
    background: #ff7b72;
    box-shadow: 0 0 6px rgba(255, 123, 114, 0.6);
  }
  .led-off,
  .led-off.led-sm {
    background: #3f4350;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  .error-banner {
    position: absolute;
    left: 16px;
    right: 16px;
    bottom: 36px;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    border-radius: 8px;
    background: rgba(255, 123, 114, 0.18);
    border: 1px solid rgba(255, 123, 114, 0.4);
    backdrop-filter: blur(6px);
    color: #ffd5d2;
    font-family: 'JetBrains Mono Variable', Menlo, Monaco, monospace;
    font-size: 12px;
  }
  .error-banner button {
    margin-left: auto;
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
  }

  :global(.viewport .xterm) {
    height: 100%;
  }
  :global(.viewport .xterm-viewport) {
    background: transparent !important;
  }
</style>
