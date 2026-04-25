// Reactive auth state.
//
// Loaded once at boot (from +layout.svelte) and refreshed after login /
// logout / password change. Components read it via `authStore.state` and the
// derived helpers; mutators handle their own optimistic updates.

import { getSetupStatus, listSettings, logout as apiLogout, whoami, type User } from './api';

export interface AuthState {
  loaded: boolean;
  /** Panel has at least one admin row. False before `/setup` runs. */
  initialized: boolean;
  user: User | null;
  guestEnabled: boolean;
}

const state: AuthState = $state({
  loaded: false,
  initialized: true,
  user: null,
  guestEnabled: true
});

async function refresh(): Promise<void> {
  try {
    const [setup, me, settings] = await Promise.all([
      getSetupStatus().catch(() => ({ initialized: true })),
      whoami(),
      // Settings is admin-gated; the failure path quietly defaults to
      // "guest enabled" so anonymous visitors still see the dashboard.
      listSettings().catch(() => null)
    ]);
    state.initialized = setup.initialized;
    state.user = me;
    if (settings) {
      const g = settings.find((s) => s.key === 'guest_enabled');
      state.guestEnabled = typeof g?.value === 'boolean' ? g.value : true;
    }
  } catch {
    state.user = null;
  } finally {
    state.loaded = true;
  }
}

async function logoutAndReset(): Promise<void> {
  try {
    await apiLogout();
  } catch {
    /* swallow — we're tearing down anyway */
  }
  state.user = null;
}

function setUser(u: User | null): void {
  state.user = u;
}

export const authStore = {
  state,
  refresh,
  setUser,
  logout: logoutAndReset
};
