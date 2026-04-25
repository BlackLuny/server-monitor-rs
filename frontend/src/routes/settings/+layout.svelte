<!--
  Common chrome around every /settings/* page: max-width container, page
  heading slot, and a left-rail rendered by the SiteHeader. We deliberately
  don't add a sub-nav here — the global header already exposes one — keeping
  this layout to a thin alignment wrapper.
-->
<script lang="ts">
  let { children } = $props();
</script>

<main class="mx-auto max-w-screen-2xl px-6 py-8">
  {@render children()}
</main>
