<!--
  Section divider. A single horizontal hairline with the label sitting
  on top of it, left-aligned, all caps monospace. Meta row on the right
  is optional (count, filter chip, etc). No big heading = more density.
-->
<script lang="ts">
  interface Props {
    label: string;
    count?: number | null;
    children?: import('svelte').Snippet;
  }
  let { label, count = null, children }: Props = $props();
</script>

<div class="relative flex items-center gap-3 py-2">
  <span class="font-mono text-2xs uppercase tracking-wider text-fg-tertiary">
    {label}
  </span>
  {#if count !== null}
    <span class="font-mono text-2xs text-fg-quaternary">{count}</span>
  {/if}
  <div class="h-px flex-1 bg-border"></div>
  {#if children}
    <div class="text-xs text-fg-secondary">
      {@render children()}
    </div>
  {/if}
</div>
