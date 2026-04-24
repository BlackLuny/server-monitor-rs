<!--
  Design system showcase. Exists so future contributors can see the
  primitives in place before wiring them into the real dashboard.
  Route: /design.
-->
<script lang="ts">
  import {
    Badge,
    Panel,
    SectionHeader,
    Sparkline,
    Stat,
    StatusDot
  } from '$lib/primitives';

  // Fabricated CPU / mem / net data for the sparkline demo.
  const cpuSeries = Array.from({ length: 60 }, (_, i) => 20 + 30 * Math.sin(i / 4) + Math.random() * 8);
  const memSeries = Array.from({ length: 60 }, (_, i) => 60 + 6 * Math.sin(i / 8) + Math.random() * 2);
  const netSeries = Array.from({ length: 60 }, () => Math.max(0, Math.random() ** 3 * 1_500_000));

  // Fake live-update mechanism to demo the `flashKey` on Stat.
  let tick = $state(0);
  setInterval(() => (tick = (tick + 1) % 1000), 2000);
  const cpuNow = $derived((30 + Math.sin(tick / 4) * 15 + Math.random() * 2).toFixed(1));
</script>

<main class="mx-auto max-w-6xl px-6 py-12">
  <header class="mb-10">
    <div class="flex items-baseline gap-4">
      <h1 class="text-lg font-medium tracking-tight">design system</h1>
      <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
        v0.1 · M2
      </span>
    </div>
    <p class="mt-2 max-w-2xl text-sm text-fg-secondary">
      Primitives for the server-monitor dashboard. Dark-first, hairline borders,
      muted semantic colour, mono numerics. Nothing here is decorative.
    </p>
  </header>

  <!-- ── Palette ── -->
  <SectionHeader label="palette" />
  <div class="mt-3 grid grid-cols-4 gap-3 md:grid-cols-8">
    {#each [
      { key: 'bg-base', label: 'base' },
      { key: 'bg-elev-1', label: 'elev-1' },
      { key: 'bg-elev-2', label: 'elev-2' },
      { key: 'bg-recess', label: 'recess' },
      { key: 'border', label: 'border' },
      { key: 'border-strong', label: 'border/s' },
      { key: 'fg-primary', label: 'fg' },
      { key: 'fg-tertiary', label: 'fg/t' },
      { key: 'status-online', label: 'online' },
      { key: 'status-warn', label: 'warn' },
      { key: 'status-error', label: 'error' },
      { key: 'status-idle', label: 'idle' },
      { key: 'data-1', label: 'data-1' },
      { key: 'data-2', label: 'data-2' },
      { key: 'data-3', label: 'data-3' },
      { key: 'data-4', label: 'data-4' }
    ] as swatch}
      <div class="flex flex-col gap-1">
        <div
          class="h-12 rounded-xs border"
          style:background="var(--{swatch.key})"
        ></div>
        <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
          {swatch.label}
        </span>
      </div>
    {/each}
  </div>

  <!-- ── Status & badges ── -->
  <div class="mt-10">
    <SectionHeader label="status" />
    <div class="mt-3 flex flex-wrap items-center gap-6">
      <span class="inline-flex items-center gap-2 text-sm">
        <StatusDot kind="online" /> online
      </span>
      <span class="inline-flex items-center gap-2 text-sm">
        <StatusDot kind="warn" /> degraded
      </span>
      <span class="inline-flex items-center gap-2 text-sm">
        <StatusDot kind="error" /> offline
      </span>
      <span class="inline-flex items-center gap-2 text-sm">
        <StatusDot kind="idle" /> idle
      </span>

      <span class="h-4 w-px bg-border"></span>

      <Badge tone="online">online</Badge>
      <Badge tone="warn">degraded</Badge>
      <Badge tone="error">offline</Badge>
      <Badge tone="neutral">pending</Badge>
      <Badge tone="accent">prod</Badge>
    </div>
  </div>

  <!-- ── Metric cards ── -->
  <div class="mt-10">
    <SectionHeader label="server cards" count={3}>live · 5s</SectionHeader>
    <div class="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
      <Panel accent="online" interactive>
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2">
              <StatusDot kind="online" />
              <span class="truncate text-md font-medium">edge-us-east-1</span>
            </div>
            <div class="mt-0.5 flex items-center gap-1.5 font-mono text-2xs text-fg-tertiary">
              <span>aws · t3.medium</span>
              <span class="text-fg-quaternary">·</span>
              <span>up 14d 06:12</span>
            </div>
          </div>
          <Badge tone="accent">prod</Badge>
        </div>

        <div class="mt-4 grid grid-cols-3 gap-3">
          <Stat label="cpu" value={cpuNow} unit="%" tone="data1" flashKey={tick} />
          <Stat label="mem" value="62" unit="%" tone="data2" />
          <Stat label="load" value="1.24" tone="data5" />
        </div>

        <div class="mt-4 space-y-2">
          <div class="flex items-center justify-between font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
            <span>cpu · last 60s</span>
            <span class="text-fg-quaternary">peak 68%</span>
          </div>
          <Sparkline values={cpuSeries} max={100} tone="data1" />
          <div class="flex items-center justify-between font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
            <span>net in · last 60s</span>
            <span class="text-fg-quaternary">1.4 MB/s peak</span>
          </div>
          <Sparkline values={netSeries} tone="data3" />
        </div>
      </Panel>

      <Panel accent="warn">
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2">
              <StatusDot kind="warn" />
              <span class="truncate text-md font-medium">build-runner-02</span>
            </div>
            <div class="mt-0.5 font-mono text-2xs text-fg-tertiary">hetzner · cpx41 · up 3d 02:44</div>
          </div>
          <Badge tone="warn">hot</Badge>
        </div>
        <div class="mt-4 grid grid-cols-3 gap-3">
          <Stat label="cpu" value="87.4" unit="%" tone="data1" />
          <Stat label="mem" value="91" unit="%" tone="data2" />
          <Stat label="load" value="4.02" tone="data5" />
        </div>
        <div class="mt-4 space-y-2">
          <div class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">mem · last 60s</div>
          <Sparkline values={memSeries} max={100} tone="data2" />
        </div>
      </Panel>

      <Panel accent="error">
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2">
              <StatusDot kind="error" />
              <span class="truncate text-md font-medium text-fg-secondary">cache-sg-1</span>
            </div>
            <div class="mt-0.5 font-mono text-2xs text-fg-quaternary">last seen 9m 12s ago</div>
          </div>
          <Badge tone="error">offline</Badge>
        </div>
        <div class="mt-8 rounded bg-recess px-3 py-6 text-center font-mono text-xs text-fg-tertiary">
          no samples in window
        </div>
      </Panel>
    </div>
  </div>

  <!-- ── Typography ── -->
  <div class="mt-10">
    <SectionHeader label="type" />
    <dl class="mt-3 grid grid-cols-[min-content_1fr] gap-x-6 gap-y-2 text-sm">
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">2xs</dt>
      <dd class="text-2xs uppercase tracking-wider">The quick brown fox jumps over 01234567</dd>
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">xs</dt>
      <dd class="text-xs">The quick brown fox jumps over 01234567</dd>
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">sm</dt>
      <dd class="text-sm">The quick brown fox jumps over 01234567</dd>
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">md</dt>
      <dd class="text-md">The quick brown fox jumps over 01234567</dd>
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">xl mono</dt>
      <dd class="font-mono text-xl">0123456789 · 42.5% · 1.4MB/s</dd>
      <dt class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">2xl mono</dt>
      <dd class="font-mono text-2xl">0123456789</dd>
    </dl>
  </div>
</main>
