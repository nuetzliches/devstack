// Mutating requests: same-origin, JSON, with the two answers that need the
// user: 409 {"error"} (shown in a readable panel, e.g. compiler errors) and
// 428 {"confirm"} (ask, then send again with confirm: true).
import { api, Unreachable } from '@/api';
import { useUi } from '@/stores/ui';

export type ActResult<T> = { ok: boolean; status: number; body: T | null };

export type ActOptions<T> = {
  /** "Restart api": used in toasts, dialog titles. */
  label: string;
  method?: string;
  path: string;
  body?: Record<string, unknown>;
  /** Style the 428 confirmation as destructive (default true: a confirm text means "this is serious"). */
  danger?: boolean;
  confirmLabel?: string;
  /** Known confirmation text: ask before the first request (the 428 path stays as a fallback). */
  confirmText?: string;
  /** Toast on success: text, function of the body, or false for none. */
  success?: string | ((body: T | null) => string) | false;
  /** Explanation shown under a 409 message. */
  conflictHint?: string;
};

function errorText(body: unknown): string | null {
  if (body && typeof body === 'object' && 'error' in body && typeof (body as { error: unknown }).error === 'string') return (body as { error: string }).error;
  if (typeof body === 'string' && body.trim()) return body.trim();
  return null;
}

export async function act<T = unknown>(opts: ActOptions<T>): Promise<ActResult<T>> {
  const ui = useUi();
  const method = opts.method ?? 'POST';
  let body = opts.body;
  if (opts.confirmText) {
    const ok = await ui.confirm({ title: opts.label, text: opts.confirmText, danger: opts.danger ?? true, confirmLabel: opts.confirmLabel ?? 'Run anyway' });
    if (!ok) return { ok: false, status: 428, body: null };
    body = { ...(body ?? {}), confirm: true };
  }
  for (let round = 0; round < 2; round++) {
    let res;
    try {
      res = await api().request<T>(method, opts.path, body);
    } catch (e) {
      ui.toast('error', `${opts.label}: hub not reachable`, e instanceof Unreachable ? 'Run `devstack ui` in the project to start it.' : String(e));
      return { ok: false, status: 0, body: null };
    }
    if (res.ok) {
      const b = (typeof res.body === 'object' ? res.body : null) as T | null;
      if (opts.success !== false) {
        const msg = typeof opts.success === 'function' ? opts.success(b) : (opts.success ?? `${opts.label}: done`);
        ui.toast('success', msg);
      }
      return { ok: true, status: res.status, body: b };
    }
    if (res.status === 428 && round === 0) {
      const text =
        res.body && typeof res.body === 'object' && 'confirm' in res.body ? String((res.body as { confirm: unknown }).confirm) : 'This action needs confirmation.';
      const ok = await ui.confirm({ title: opts.label, text, danger: opts.danger ?? true, confirmLabel: opts.confirmLabel ?? 'Run anyway' });
      if (!ok) return { ok: false, status: 428, body: null };
      body = { ...(body ?? {}), confirm: true };
      continue;
    }
    const msg = errorText(res.body) ?? `HTTP ${res.status}`;
    if (res.status === 409) {
      ui.showProblem({ title: `${opts.label} was refused`, message: msg, hint: opts.conflictHint });
    } else if (res.status === 503) {
      // "stack not running", or the hub's word that it is starting, stopping
      // or restarting (with Retry-After): then try again in a moment.
      if (msg.startsWith('stack not running')) ui.toast('warning', `${opts.label}: stack not running`, 'Start the stack first.');
      else ui.toast('warning', `${opts.label}: try again in a moment`, msg);
    } else if (res.status === 401) {
      ui.toast('error', `${opts.label}: not paired`, 'Run `devstack ui` in the project.');
    } else {
      ui.toast('error', `${opts.label} failed`, msg);
    }
    return { ok: false, status: res.status, body: null };
  }
  return { ok: false, status: 428, body: null };
}
