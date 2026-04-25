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
    /** When set, the panel renders as an `<a>` so the entire surface is a
     *  single, accessible click target. The previous absolute-link overlay
     *  trick was unreliable: any statically-positioned descendant rendered
     *  on top of the link and swallowed clicks. */
    href?: string;
    ariaLabel?: string;
    class?: string;
    children?: import('svelte').Snippet;
  }
  let {
    accent = 'none',
    interactive = false,
    padded = true,
    href,
    ariaLabel,
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
      'relative block rounded border bg-elev-1 transition-colors duration-150 ease-out-quart',
      padded ? 'px-4 py-4' : '',
      interactive || href
        ? 'cursor-pointer hover:bg-elev-2 hover:border-border-strong'
        : '',
      klass
    ]
      .filter(Boolean)
      .join(' ')
  );
</script>

{#if href}
  <a {href} aria-label={ariaLabel} class={classes}>
    {#if railColour}
      <span
        class="absolute left-0 top-0 h-full w-[2px] rounded-l"
        style:background={railColour}
      ></span>
    {/if}
    {@render children?.()}
  </a>
{:else}
  <div class={classes}>
    {#if railColour}
      <span
        class="absolute left-0 top-0 h-full w-[2px] rounded-l"
        style:background={railColour}
      ></span>
    {/if}
    {@render children?.()}
  </div>
{/if}
