<!--
  Uppercase, letter-spaced, monospace mini-label used for status chips
  (ONLINE / DEGRADED / OFFLINE), tags, and inline labels. No pill shape
  — flat 2px radius makes it read as a data cell, not a UI element.
-->
<script lang="ts">
  type Tone = 'neutral' | 'online' | 'warn' | 'error' | 'accent';
  interface Props {
    tone?: Tone;
    children?: import('svelte').Snippet;
  }
  let { tone = 'neutral', children }: Props = $props();

  const styles = $derived({
    neutral: {
      fg: 'var(--fg-secondary)',
      bg: 'color-mix(in oklch, var(--fg-secondary) 10%, transparent)',
      border: 'color-mix(in oklch, var(--fg-secondary) 25%, transparent)'
    },
    online: {
      fg: 'var(--status-online)',
      bg: 'color-mix(in oklch, var(--status-online) 12%, transparent)',
      border: 'color-mix(in oklch, var(--status-online) 28%, transparent)'
    },
    warn: {
      fg: 'var(--status-warn)',
      bg: 'color-mix(in oklch, var(--status-warn) 12%, transparent)',
      border: 'color-mix(in oklch, var(--status-warn) 28%, transparent)'
    },
    error: {
      fg: 'var(--status-error)',
      bg: 'color-mix(in oklch, var(--status-error) 14%, transparent)',
      border: 'color-mix(in oklch, var(--status-error) 32%, transparent)'
    },
    accent: {
      fg: 'var(--border-accent)',
      bg: 'color-mix(in oklch, var(--border-accent) 10%, transparent)',
      border: 'color-mix(in oklch, var(--border-accent) 28%, transparent)'
    }
  }[tone]);
</script>

<span
  class="inline-flex h-[18px] items-center rounded-xs border px-1.5 font-mono text-2xs uppercase tracking-wider"
  style:color={styles.fg}
  style:background={styles.bg}
  style:border-color={styles.border}
>
  {@render children?.()}
</span>
