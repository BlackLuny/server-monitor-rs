<!--
  Inline 60-point sparkline. Deliberately sparse: no grid, no axis, no
  dots — just the line and a soft area underneath. Area opacity is driven
  by the current tone so the chart reads from across the room without
  studying it.

  These are functional (real CPU/mem/net over the last N samples), not
  decorative, so they pass the "don't-use-sparklines-as-decoration" test.
-->
<script lang="ts">
  interface Props {
    values: number[];
    /** Upper bound of the Y axis. When omitted, auto-fit with a 10% ceiling. */
    max?: number | null;
    tone?: 'data1' | 'data2' | 'data3' | 'data4' | 'data5';
    height?: number;
    class?: string;
  }
  let {
    values = [],
    max = null,
    tone = 'data1',
    height = 32,
    class: klass = ''
  }: Props = $props();

  const stroke = $derived(
    {
      data1: 'var(--data-1)',
      data2: 'var(--data-2)',
      data3: 'var(--data-3)',
      data4: 'var(--data-4)',
      data5: 'var(--data-5)'
    }[tone]
  );

  // viewBox width is always 100 — we render at any pixel size and the
  // browser scales the path. Keeps math simple.
  const WIDTH = 100;

  const points = $derived.by(() => {
    if (values.length === 0) return { line: '', area: '' };
    const ceiling =
      max ?? Math.max(1, ...values.map((v) => (Number.isFinite(v) ? v : 0))) * 1.1;
    const n = values.length;
    const step = WIDTH / Math.max(1, n - 1);
    const coords = values.map((raw, i) => {
      const v = Number.isFinite(raw) ? raw : 0;
      const x = i * step;
      const y = height - (Math.max(0, Math.min(ceiling, v)) / ceiling) * height;
      return [x, y] as const;
    });
    const line = coords.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`).join(' ');
    const area = `${line} L${WIDTH},${height} L0,${height} Z`;
    return { line, area };
  });
</script>

<svg
  viewBox={`0 0 ${WIDTH} ${height}`}
  preserveAspectRatio="none"
  class="block w-full {klass}"
  style:height="{height}px"
  aria-hidden="true"
>
  <path d={points.area} fill={stroke} fill-opacity="0.12" />
  <path d={points.line} stroke={stroke} stroke-width="1.25" fill="none" vector-effect="non-scaling-stroke" />
</svg>
