<!--
  Per-user security settings: change password + enroll/disable TOTP.

  TOTP enroll uses a three-stage local flow:
    `idle` → admin clicks "set up", we POST /enroll, advance to
    `pending` → form shows QR + secret + code field, admin types a six-digit
    → POST /confirm flips totp_enabled and we land on
    `done` showing the ten plaintext backup codes once.
-->
<script lang="ts">
  import { changeOwnPassword, totpConfirm, totpDisable, totpEnroll, totpRegenerateBackup } from '$lib/api';
  import { authStore } from '$lib/auth.svelte';
  import { onMount } from 'svelte';

  // ----- password change -----
  let pwCurrent = $state('');
  let pwNew = $state('');
  let pwConfirm = $state('');
  let pwSubmitting = $state(false);
  let pwError = $state<string | null>(null);
  let pwSuccess = $state(false);

  // ----- TOTP -----
  type Stage = 'idle' | 'pending' | 'done';
  let stage = $state<Stage>('idle');
  let secret = $state('');
  let qr = $state('');
  let totpCode = $state('');
  let totpSubmitting = $state(false);
  let totpError = $state<string | null>(null);
  let backupCodes = $state<string[]>([]);

  // ----- TOTP disable / regen-backup -----
  let disablePassword = $state('');
  let disableSubmitting = $state(false);
  let disableError = $state<string | null>(null);
  let regenPassword = $state('');
  let regenSubmitting = $state(false);
  let regenError = $state<string | null>(null);

  onMount(() => {
    if (!authStore.state.user) {
      // Layout will redirect; silence type checker.
      return;
    }
  });

  async function handlePasswordChange() {
    pwError = null;
    pwSuccess = false;
    if (pwNew.length < 8) {
      pwError = 'new password must be ≥ 8 chars';
      return;
    }
    if (pwNew !== pwConfirm) {
      pwError = 'passwords do not match';
      return;
    }
    pwSubmitting = true;
    try {
      await changeOwnPassword(pwCurrent, pwNew);
      pwSuccess = true;
      pwCurrent = '';
      pwNew = '';
      pwConfirm = '';
      // The backend revoked all our sessions — refresh auth state so the
      // UI bounces us back to /login on the next protected nav.
      await authStore.refresh();
    } catch (err) {
      pwError = err instanceof Error ? err.message : String(err);
    } finally {
      pwSubmitting = false;
    }
  }

  async function startEnrollment() {
    totpError = null;
    try {
      const r = await totpEnroll();
      secret = r.secret;
      qr = r.qr_svg_data_url;
      stage = 'pending';
    } catch (err) {
      totpError = err instanceof Error ? err.message : String(err);
    }
  }

  async function confirmEnrollment() {
    if (totpCode.trim().length === 0) return;
    totpSubmitting = true;
    totpError = null;
    try {
      const r = await totpConfirm(totpCode.trim());
      backupCodes = r.backup_codes;
      stage = 'done';
      totpCode = '';
      await authStore.refresh();
    } catch (err) {
      totpError = err instanceof Error ? err.message : String(err);
    } finally {
      totpSubmitting = false;
    }
  }

  async function handleDisable() {
    disableSubmitting = true;
    disableError = null;
    try {
      await totpDisable(disablePassword);
      disablePassword = '';
      stage = 'idle';
      backupCodes = [];
      await authStore.refresh();
    } catch (err) {
      disableError = err instanceof Error ? err.message : String(err);
    } finally {
      disableSubmitting = false;
    }
  }

  async function handleRegenBackup() {
    regenSubmitting = true;
    regenError = null;
    try {
      const r = await totpRegenerateBackup(regenPassword);
      backupCodes = r.backup_codes;
      regenPassword = '';
    } catch (err) {
      regenError = err instanceof Error ? err.message : String(err);
    } finally {
      regenSubmitting = false;
    }
  }

  function downloadCodes() {
    const text = backupCodes.join('\n') + '\n';
    const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = 'monitor-panel-backup-codes.txt';
    a.click();
    URL.revokeObjectURL(url);
  }

  const totpEnabled = $derived(authStore.state.user?.totp_enabled === true);
</script>

<svelte:head>
  <title>Security · settings</title>
</svelte:head>

<header class="mb-6">
  <div class="font-mono text-2xs uppercase tracking-[0.16em] text-fg-quaternary">settings</div>
  <h1 class="mt-1 text-xl font-medium tracking-tight">Security</h1>
</header>

<!-- ===== change password ===== -->
<section class="mb-6 rounded border border-border bg-elev-1 px-5 py-5">
  <h2 class="text-md font-medium">Change password</h2>
  <p class="mt-1 mb-4 text-sm text-fg-secondary">
    All other sessions for your account are revoked when the password changes.
  </p>
  <div class="grid grid-cols-1 gap-3 md:grid-cols-3">
    <input
      type="password"
      bind:value={pwCurrent}
      placeholder="current password"
      disabled={pwSubmitting}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <input
      type="password"
      bind:value={pwNew}
      placeholder="new password"
      disabled={pwSubmitting}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
    <input
      type="password"
      bind:value={pwConfirm}
      placeholder="confirm new password"
      disabled={pwSubmitting}
      class="rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
    />
  </div>
  <div class="mt-3 flex items-center gap-3">
    <button
      type="button"
      onclick={handlePasswordChange}
      disabled={pwSubmitting || pwNew.length < 8 || pwNew !== pwConfirm}
      class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
      style:border-color="var(--border-accent)"
      style:color="var(--border-accent)"
    >
      {pwSubmitting ? 'saving…' : 'update password'}
    </button>
    {#if pwSuccess}
      <span class="font-mono text-2xs uppercase tracking-[0.14em] text-online">updated</span>
    {/if}
    {#if pwError}
      <span class="font-mono text-2xs" style="color: var(--status-error)">{pwError}</span>
    {/if}
  </div>
</section>

<!-- ===== TOTP ===== -->
<section class="rounded border border-border bg-elev-1 px-5 py-5">
  <h2 class="text-md font-medium">Two-factor authentication</h2>
  <p class="mt-1 mb-4 text-sm text-fg-secondary">
    Adds a six-digit code prompt after the password.
    {#if totpEnabled}
      <span class="text-online">currently enabled</span>
    {:else}
      currently disabled.
    {/if}
  </p>

  {#if !totpEnabled && stage !== 'pending' && stage !== 'done'}
    <button
      type="button"
      onclick={startEnrollment}
      class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em]"
      style:border-color="var(--border-accent)"
      style:color="var(--border-accent)"
    >
      set up
    </button>
    {#if totpError}
      <div class="mt-2 font-mono text-2xs" style="color: var(--status-error)">{totpError}</div>
    {/if}
  {/if}

  {#if stage === 'pending'}
    <div class="grid grid-cols-1 gap-5 md:grid-cols-[224px_1fr]">
      <div class="flex justify-center rounded border border-border bg-recess p-2">
        <img src={qr} alt="TOTP QR" class="h-56 w-56" />
      </div>
      <div>
        <p class="text-sm text-fg-secondary">
          Scan the QR with any authenticator app, or paste this secret manually:
        </p>
        <pre class="mt-2 mb-4 inline-block rounded border border-border bg-recess px-3 py-2 font-mono text-sm">{secret}</pre>
        <label class="block">
          <span class="mb-1.5 block font-mono text-2xs uppercase tracking-[0.12em] text-fg-tertiary"
            >six-digit code</span
          >
          <input
            type="text"
            inputmode="numeric"
            pattern="[0-9]*"
            maxlength="6"
            bind:value={totpCode}
            disabled={totpSubmitting}
            class="block w-40 rounded border border-border bg-recess px-3 py-2 text-lg font-mono tracking-[0.3em] text-fg focus:border-border-accent"
            placeholder="••••••"
          />
        </label>
        {#if totpError}
          <div class="mt-2 font-mono text-2xs" style="color: var(--status-error)">{totpError}</div>
        {/if}
        <div class="mt-3 flex items-center gap-2">
          <button
            type="button"
            onclick={confirmEnrollment}
            disabled={totpSubmitting || totpCode.trim().length < 6}
            class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
            style:border-color="var(--border-accent)"
            style:color="var(--border-accent)"
          >
            {totpSubmitting ? 'verifying…' : 'verify & enable'}
          </button>
          <button
            type="button"
            onclick={() => {
              stage = 'idle';
              totpCode = '';
              totpError = null;
            }}
            class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-fg"
          >
            cancel
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if (stage === 'done' || backupCodes.length > 0) && totpEnabled}
    <div class="mt-2 rounded border border-border bg-recess px-4 py-3">
      <div class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
        backup codes — one-time use, store somewhere safe
      </div>
      <ul class="mt-3 grid grid-cols-2 gap-x-4 gap-y-1 font-mono text-sm">
        {#each backupCodes as c}
          <li>{c}</li>
        {/each}
      </ul>
      <div class="mt-3">
        <button
          type="button"
          onclick={downloadCodes}
          class="font-mono text-2xs uppercase tracking-[0.14em] text-fg-secondary underline-offset-4 hover:underline"
        >
          download as .txt
        </button>
      </div>
    </div>
  {/if}

  {#if totpEnabled}
    <div class="mt-6 grid grid-cols-1 gap-4 md:grid-cols-2">
      <div class="rounded border border-border bg-elev-1 p-4">
        <div class="mb-2 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
          regenerate backup codes
        </div>
        <input
          type="password"
          bind:value={regenPassword}
          placeholder="confirm with password"
          class="mb-2 block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
        {#if regenError}
          <div class="mb-2 font-mono text-2xs" style="color: var(--status-error)">{regenError}</div>
        {/if}
        <button
          type="button"
          onclick={handleRegenBackup}
          disabled={regenSubmitting || !regenPassword}
          class="rounded border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] disabled:opacity-40"
          style:border-color="var(--border-accent)"
          style:color="var(--border-accent)"
        >
          {regenSubmitting ? 'rolling…' : 'regenerate'}
        </button>
      </div>

      <div class="rounded border border-border bg-elev-1 p-4">
        <div class="mb-2 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary">
          disable two-factor
        </div>
        <input
          type="password"
          bind:value={disablePassword}
          placeholder="confirm with password"
          class="mb-2 block w-full rounded border border-border bg-recess px-3 py-2 font-mono text-sm text-fg focus:border-border-accent"
        />
        {#if disableError}
          <div class="mb-2 font-mono text-2xs" style="color: var(--status-error)">{disableError}</div>
        {/if}
        <button
          type="button"
          onclick={handleDisable}
          disabled={disableSubmitting || !disablePassword}
          class="rounded border border-border px-3 py-1.5 font-mono text-2xs uppercase tracking-[0.14em] text-fg-tertiary hover:text-error"
        >
          {disableSubmitting ? 'disabling…' : 'disable 2fa'}
        </button>
      </div>
    </div>
  {/if}
</section>
