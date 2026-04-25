<!--
  Persistent top bar. Mirrors the dashboard's information density:
  hairline border under the row, all-mono nav, tight padding. Active route
  is the one design honor — a 1px accent rule under the matching link.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { authStore } from './auth.svelte';

  type NavItem = { href: string; label: string; visibility: 'public' | 'auth' | 'admin' };
  const items: NavItem[] = [
    { href: '/', label: 'Servers', visibility: 'public' },
    { href: '/probes', label: 'Probes', visibility: 'auth' },
    { href: '/settings/general', label: 'General', visibility: 'admin' },
    { href: '/settings/servers', label: 'Manage', visibility: 'admin' },
    { href: '/settings/groups', label: 'Groups', visibility: 'admin' },
    { href: '/settings/users', label: 'Users', visibility: 'admin' },
    { href: '/settings/security', label: 'Security', visibility: 'auth' },
    { href: '/settings/updates', label: 'Updates', visibility: 'admin' },
    { href: '/settings/audit', label: 'Audit', visibility: 'admin' }
  ];

  const visibleItems = $derived(
    items.filter((i) => {
      if (i.visibility === 'public') return true;
      if (!authStore.state.user) return false;
      if (i.visibility === 'admin') return authStore.state.user.role === 'admin';
      return true;
    })
  );

  function isActive(href: string): boolean {
    const path = $page.url.pathname;
    if (href === '/') return path === '/';
    return path.startsWith(href);
  }

  async function handleLogout() {
    await authStore.logout();
    await goto('/login', { replaceState: true });
  }
</script>

<header class="border-b border-border bg-elev-1">
  <div class="mx-auto flex max-w-screen-2xl items-center gap-6 px-6 py-3">
    <a href="/" class="flex items-center gap-2">
      <span class="inline-block h-2 w-2 rounded-full" style="background: var(--status-online)"
      ></span>
      <span class="font-mono text-2xs uppercase tracking-[0.18em] text-fg-secondary"
        >server-monitor</span
      >
    </a>

    <nav class="flex flex-1 items-center gap-1">
      {#each visibleItems as item}
        <a
          href={item.href}
          class="relative rounded px-2.5 py-1.5 font-mono text-2xs uppercase tracking-[0.12em] transition-colors duration-150 ease-out-quart hover:bg-elev-2"
          class:text-fg={isActive(item.href)}
          class:text-fg-tertiary={!isActive(item.href)}
        >
          {item.label}
          {#if isActive(item.href)}
            <span
              class="absolute -bottom-[13px] left-2.5 right-2.5 h-px"
              style="background: var(--border-accent)"
            ></span>
          {/if}
        </a>
      {/each}
    </nav>

    {#if authStore.state.user}
      <div class="flex items-center gap-2">
        <span
          class="rounded border border-border px-2 py-1 font-mono text-2xs uppercase tracking-[0.12em] text-fg-secondary"
        >
          {authStore.state.user.username}
          {#if authStore.state.user.totp_enabled}
            <span class="ml-1.5 text-fg-quaternary">· 2fa</span>
          {/if}
        </span>
        <button
          type="button"
          onclick={handleLogout}
          class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
        >
          sign out
        </button>
      </div>
    {:else}
      <a
        href="/login"
        class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] transition-colors hover:bg-elev-2"
        style:border-color="var(--border-accent)"
        style:color="var(--border-accent)"
      >
        sign in
      </a>
    {/if}
  </div>
</header>
