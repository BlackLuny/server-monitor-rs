<!--
  Numeric metric display. The value is always mono; the label is uppercase
  10px Geist with tracked letter-spacing. Unit sits next to value in a
  dimmer weight so the eye snaps to the number first.
-->
<script lang="ts">
  interface Props {
    label: string;
    value: string | number;
    unit?: string;
    tone?: 'default' | 'data1' | 'data2' | 'data3' | 'data4' | 'data5';
    align?: 'left' | 'right';
    flashKey?: unknown;
  }
  let {
    label,
    value,
    unit = '',
    tone = 'default',
    align = 'left',
    flashKey = undefined
  }: Props = $props();

  const valueColour = $derived(
    {
      default: 'var(--fg-primary)',
      data1: 'var(--data-1)',
      data2: 'var(--data-2)',
      data3: 'var(--data-3)',
      data4: 'var(--data-4)',
      data5: 'var(--data-5)'
    }[tone]
  );

  // Flash on value change. We track the latest "key" through a plain
  // module-local variable (not $state) so reading/writing it inside the
  // effect doesn't cause a re-run. First run seeds `lastKey` without
  // triggering the animation — only subsequent changes flash.
  let flashing = $state(false);
  let lastKey: unknown = undefined;
  let initialized = false;

  $effect(() => {
    const key = flashKey === undefined ? value : flashKey;
    if (!initialized) {
      lastKey = key;
      initialized = true;
      return;
    }
    if (key === lastKey) return;
    lastKey = key;
    flashing = false;
    requestAnimationFrame(() => {
      flashing = true;
      setTimeout(() => (flashing = false), 360);
    });
  });
</script>

<div class="flex flex-col" class:items-end={align === 'right'}>
  <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
    {label}
  </span>
  <span
    class="font-mono text-xl font-medium"
    class:flash={flashing}
    style:color={valueColour}
  >
    {value}{#if unit}<span class="ml-0.5 text-sm font-normal text-fg-tertiary">{unit}</span>{/if}
  </span>
</div>
