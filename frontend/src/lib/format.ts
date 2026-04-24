// Formatters for bytes, rates, percentages, and relative time.
// Output stays compact so values fit card cells without wrapping.

export function percent(v: number, digits = 1): string {
  if (!Number.isFinite(v)) return '—';
  return v.toFixed(digits);
}

export function bytes(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v) || v < 0) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0;
  let n = v;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  const digits = n >= 100 ? 0 : n >= 10 ? 1 : 2;
  return `${n.toFixed(digits)} ${units[i]}`;
}

export function bitsPerSec(v: number | null | undefined): string {
  // Network is conventionally reported in bytes/s on ops dashboards.
  return `${bytes(v)}/s`;
}

export function usagePct(used: number, total: number): number {
  if (!total) return 0;
  return (100 * used) / total;
}

const relFormatter = new Intl.RelativeTimeFormat('en', { numeric: 'auto', style: 'short' });

/**
 * Convert a past ISO timestamp to a compact age (e.g. "3m ago", "5h ago").
 * Returns "just now" for things < 5 seconds old.
 */
export function ageFromIso(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return '—';
  const then = new Date(iso).getTime();
  const seconds = Math.max(0, Math.round((now - then) / 1000));
  if (seconds < 5) return 'just now';
  if (seconds < 60) return relFormatter.format(-seconds, 'second');
  if (seconds < 3600) return relFormatter.format(-Math.round(seconds / 60), 'minute');
  if (seconds < 86_400) return relFormatter.format(-Math.round(seconds / 3600), 'hour');
  return relFormatter.format(-Math.round(seconds / 86_400), 'day');
}

export function uptimeFromSeconds(s: number | null | undefined): string {
  if (s == null || !Number.isFinite(s)) return '—';
  if (s < 60) return `${Math.round(s)}s`;
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}`;
  return `${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}`;
}
