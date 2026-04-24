<!--
  Status indicator — a tiny filled circle in one of four states. Online
  state gets a subtle ripple; others stay static so the eye flows to
  live hosts, not to things that aren't changing.
-->
<script lang="ts">
  type Kind = 'online' | 'warn' | 'error' | 'idle';
  interface Props {
    kind?: Kind;
    size?: number;
  }
  let { kind = 'idle', size = 8 }: Props = $props();

  const fill = $derived({
    online: 'var(--status-online)',
    warn: 'var(--status-warn)',
    error: 'var(--status-error)',
    idle: 'var(--fg-quaternary)'
  }[kind]);
</script>

<span
  class="relative inline-block rounded-full"
  class:dot-pulse={kind === 'online'}
  style:width="{size}px"
  style:height="{size}px"
  style:background={fill}
  aria-label={kind}
></span>
