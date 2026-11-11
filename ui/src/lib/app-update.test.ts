import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// `__APP_VERSION__` is '0.0.0-test' in vitest.config.ts.
async function load() {
  vi.resetModules();
  return import('./app-update');
}

function fakeRegistration() {
  return {
    update: vi.fn().mockResolvedValue(undefined),
    unregister: vi.fn().mockResolvedValue(true),
  } as unknown as ServiceWorkerRegistration & { update: ReturnType<typeof vi.fn>; unregister: ReturnType<typeof vi.fn> };
}

describe('app update', () => {
  const reload = vi.fn();
  const deleted: string[] = [];

  beforeEach(() => {
    vi.useFakeTimers();
    sessionStorage.clear();
    reload.mockReset();
    deleted.length = 0;
    vi.stubGlobal('location', { ...window.location, reload });
    vi.stubGlobal('caches', {
      keys: async () => ['devstack-shell', 'workbox-precache'],
      delete: async (k: string) => {
        deleted.push(k);
        return true;
      },
    });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('does nothing when the hub runs the version of this shell', async () => {
    const m = await load();
    const r = fakeRegistration();
    m.rememberRegistration(r);
    m.reconcileShellVersion('0.0.0-test');
    await vi.advanceTimersByTimeAsync(m.STALE_RELOAD_MS + 1);
    expect(r.update).not.toHaveBeenCalled();
    expect(reload).not.toHaveBeenCalled();
  });

  it('does nothing without a registration (mock, dev server)', async () => {
    const m = await load();
    m.reconcileShellVersion('9.9.9');
    await vi.advanceTimersByTimeAsync(m.STALE_RELOAD_MS + 1);
    expect(reload).not.toHaveBeenCalled();
  });

  it('asks for a new worker, then reloads fresh when none took over', async () => {
    const m = await load();
    const r = fakeRegistration();
    m.rememberRegistration(r);
    m.reconcileShellVersion('9.9.9');
    m.reconcileShellVersion('9.9.9'); // a second session check meanwhile
    expect(r.update).toHaveBeenCalledTimes(1);
    expect(reload).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(m.STALE_RELOAD_MS + 1);
    expect(r.unregister).toHaveBeenCalledTimes(1);
    expect(deleted).toEqual(['devstack-shell', 'workbox-precache']);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it('reloads by hand only once per hub version', async () => {
    const m = await load();
    const r = fakeRegistration();
    m.rememberRegistration(r);
    m.reconcileShellVersion('9.9.9');
    await vi.advanceTimersByTimeAsync(m.STALE_RELOAD_MS + 1);
    expect(reload).toHaveBeenCalledTimes(1);
    // The page came back (same tab) still on another shell: no loop.
    const again = await load();
    again.rememberRegistration(r);
    again.reconcileShellVersion('9.9.9');
    await vi.advanceTimersByTimeAsync(again.STALE_RELOAD_MS + 1);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it('looks for a new worker when the window becomes visible again', async () => {
    const m = await load();
    const r = fakeRegistration();
    m.rememberRegistration(r);
    document.dispatchEvent(new Event('visibilitychange'));
    expect(r.update).toHaveBeenCalledTimes(1);
  });
});
