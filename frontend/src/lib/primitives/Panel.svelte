<!--
  Base surface primitive — think `<Card>` but with this project's specific
  chrome: 1px hairline border, elev-1 fill, 6px radius, no shadow.
  Optional `accent` prop adds a 2px coloured rail on the left to mark
  the card as active / flagged / selected.
-->
<script lang="ts">
  type Accent = 'none' | 'online' | 'warn' | 'error' | 'brand';
  interface Props {
    accent?: Accent;
    interactive?: boolean;
    padded?: boolean;
    class?: string;
    children?: import('svelte').Snippet;
  }
  let {
    accent = 'none',
    interactive = false,
    padded = true,
    class: klass = '',
    children
  }: Props = $props();

  const railColour = $derived({
    none: null,
    online: 'var(--status-online)',
    warn: 'var(--status-warn)',
    error: 'var(--status-error)',
    brand: 'var(--border-accent)'
  }[accent]);

  const classes = $derived(
    [
      'relative rounded border bg-elev-1 transition-colors duration-150 ease-out-quart',
      padded ? 'px-4 py-4' : '',
      interactive ? 'cursor-pointer hover:bg-elev-2 hover:border-border-strong' : '',
      klass
    ]
      .filter(Boolean)
      .join(' ')
  );
</script>

<div class={classes}>
  {#if railColour}
    <span
      class="absolute left-0 top-0 h-full w-[2px] rounded-l"
      style:background={railColour}
    ></span>
  {/if}
  {@render children?.()}
</div>
