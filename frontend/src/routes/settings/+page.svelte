<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { authStore } from '$lib/auth.svelte';

  // Anyone authenticated lands on Security (their own settings); admins
  // generally want General first since it gates server creation.
  onMount(() => {
    const path =
      authStore.state.user?.role === 'admin' ? '/settings/general' : '/settings/security';
    goto(path, { replaceState: true });
  });
</script>
