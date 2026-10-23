import { ref } from 'vue';

export function shortBuild(id: string | null | undefined): string {
  return id ? id.slice(0, 8) : '—';
}

export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** CPU as a share of one core: one decimal up to 100% (so values side by
 *  side read alike, 4.5% next to 23.0%), whole numbers above (several cores). */
export function percent(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return '—';
  if (n === 0) return '0%';
  if (n < 0.1) return '<0.1%';
  return `${n < 100 ? n.toFixed(1) : Math.round(n).toLocaleString('en')}%`;
}

export function duration(ms: number): string {
  if (ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

export function ago(at: number | null | undefined, now: number): string {
  if (!at) return 'never';
  const d = now - at;
  if (d < 5000) return 'just now';
  if (d < 60_000) return `${Math.floor(d / 1000)}s ago`;
  if (d < 3_600_000) return `${Math.floor(d / 60_000)} min ago`;
  if (d < 86_400_000) return `${Math.floor(d / 3_600_000)} h ago`;
  return new Date(at).toLocaleString();
}

export function clock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

export function dateTime(at: number): string {
  return new Date(at).toLocaleString([], { dateStyle: 'medium', timeStyle: 'medium' });
}

/** A shared clock for relative times ("3 min ago"). Not a data poll: it only re-renders. */
const nowRef = ref(Date.now());
let started = false;
export function useNow() {
  if (!started) {
    started = true;
    setInterval(() => (nowRef.value = Date.now()), 1000);
  }
  return nowRef;
}

/**
 * Where a browser opens a service: the daemon's `url`, or plain http for
 * daemons before 0.2.1 (and for a `url` that is no web address, see `webUrl`).
 */
export function serviceUrl(s: { url?: string | null; port: number | null }): string {
  return webUrl(s.url) ?? `http://localhost:${s.port}`;
}

/**
 * `url` when it is a web address (http or https), else `undefined`: a link
 * or a window the dashboard opens never reaches another protocol handler
 * (`javascript:`, `file:`, `ms-msdt:`) of this machine.
 */
export function webUrl(url: string | null | undefined): string | undefined {
  if (!url) return undefined;
  try {
    const u = new URL(url);
    return u.protocol === 'http:' || u.protocol === 'https:' ? url : undefined;
  } catch {
    return undefined;
  }
}
