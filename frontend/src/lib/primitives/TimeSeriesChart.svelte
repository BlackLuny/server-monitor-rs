<!--
  Thin wrapper around uPlot. Expects:
  - series[] with {name, color, values} — same length as timestamps[]
  - timestamps[] of unix seconds
  Renders a single chart that fills the container width.

  Style is tuned to the rest of the dashboard: hairline axes, mono tick
  labels, no grid on the Y axis (grid noise distracts from the line).
-->
<script lang="ts">
  import { onMount, onDestroy, untrack } from 'svelte';
  import uPlot from 'uplot';
  import 'uplot/dist/uPlot.min.css';

  export interface Series {
    name: string;
    color: string;
    /** Unit suffix rendered in the tooltip legend (e.g. `%`, `MB/s`). */
    unit?: string;
    values: (number | null)[];
  }

  interface Props {
    timestamps: number[];
    series: Series[];
    height?: number;
    min?: number | null;
    max?: number | null;
    /** Format a Y value for axis ticks + tooltip. */
    formatY?: (v: number) => string;
  }

  let {
    timestamps = [],
    series = [],
    height = 180,
    min = null,
    max = null,
    formatY = (v: number) => v.toFixed(0)
  }: Props = $props();

  let container: HTMLDivElement;
  let plot: uPlot | null = null;
  let ro: ResizeObserver | null = null;

  function axisColors() {
    const style = getComputedStyle(document.documentElement);
    return {
      grid: style.getPropertyValue('--border').trim() || '#2a2f38',
      fg: style.getPropertyValue('--fg-tertiary').trim() || '#5d6876'
    };
  }

  function buildOpts(width: number): uPlot.Options {
    const { grid, fg } = axisColors();
    return {
      width,
      height,
      pxAlign: 0,
      cursor: { y: false, points: { show: true, size: 6 } },
      legend: { show: false },
      axes: [
        {
          stroke: fg,
          grid: { show: true, stroke: grid, width: 1 },
          ticks: { show: true, stroke: grid, width: 1, size: 4 },
          font: '10px "JetBrains Mono Variable", monospace'
        },
        {
          stroke: fg,
          grid: { show: false },
          ticks: { show: false },
          size: 44,
          font: '10px "JetBrains Mono Variable", monospace',
          values: (_u, vals) => vals.map(formatY)
        }
      ],
      scales: {
        x: { time: true },
        y: { range: [min ?? null, max ?? null] as [number | null, number | null] }
      },
      series: [
        { label: 'time' },
        ...series.map((s) => ({
          label: s.name,
          stroke: s.color,
          width: 1.25,
          fill: `color-mix(in oklch, ${s.color} 12%, transparent)`,
          points: { show: false }
        }))
      ]
    };
  }

  function buildData(): uPlot.AlignedData {
    return [timestamps, ...series.map((s) => s.values as number[])] as uPlot.AlignedData;
  }

  onMount(() => {
    const opts = buildOpts(container.clientWidth);
    plot = new uPlot(opts, buildData(), container);

    ro = new ResizeObserver(() => {
      if (!plot) return;
      plot.setSize({ width: container.clientWidth, height });
    });
    ro.observe(container);
  });

  onDestroy(() => {
    ro?.disconnect();
    plot?.destroy();
    plot = null;
  });

  $effect(() => {
    // React to prop changes by pushing new data through uPlot. We read the
    // reactive inputs outside `untrack` so Svelte tracks them, then mutate.
    const ts = timestamps;
    const s = series;
    untrack(() => {
      if (!plot) return;
      // If series count changed we need a full rebuild; simpler to destroy
      // and recreate since detail views switch ranges at most a few Hz.
      if (plot.series.length - 1 !== s.length) {
        plot.destroy();
        plot = new uPlot(buildOpts(container.clientWidth), buildData(), container);
      } else {
        plot.setData(buildData());
      }
    });
  });
</script>

<div bind:this={container} class="w-full" style:height="{height}px"></div>
