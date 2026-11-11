// The app shell comes from the hub, cached by the service worker for offline
// use. A new devstack brings a new shell; this makes an open tab or an
// installed app pick it up.
//
// - On load: navigations go to the network first (see vite.config.ts), so a
//   reload shows the version the hub serves now, the cache only when it is down.
// - With the tab open: whenever the hub stream comes back after a break (a hub
//   restart, often an update), when the window becomes visible again or the
//   browser is back online, ask the browser to look for a new service worker.
//   With `autoUpdate` a new one activates at once and reloads the page.
// - The page knows two versions: the hub's (shown in the top bar) and its own
//   (`__APP_VERSION__`, the shell it runs). When they differ the shell is
//   stale whatever the events did or did not deliver: look for a new worker,
//   and when none takes over, drop the worker's caches and reload once.

let registration: ServiceWorkerRegistration | undefined;
let listening = false;
let pending = false;

/** How long a new worker may take to reload the page before we reload by hand. */
export const STALE_RELOAD_MS = 5000;
/** sessionStorage: the hub version a forced reload was already tried for. */
const FORCED = 'shellReloadFor';

export function rememberRegistration(r: ServiceWorkerRegistration | undefined): void {
  registration = r;
  if (r && !listening && typeof document !== 'undefined') {
    listening = true;
    // A resumed app (a phone, a laptop that slept) missed the hub's restart.
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible') checkForAppUpdate();
    });
    window.addEventListener('online', checkForAppUpdate);
  }
}

/** One request for `sw.js`; nothing happens when it did not change. */
export function checkForAppUpdate(): void {
  void registration?.update().catch(() => {
    /* hub gone again: the next reconnect asks again */
  });
}

function forcedFor(): string | null {
  try {
    return sessionStorage.getItem(FORCED);
  } catch {
    return null;
  }
}

async function reloadFresh(hubVersion: string): Promise<void> {
  try {
    sessionStorage.setItem(FORCED, hubVersion);
  } catch {
    /* no storage: the worker's own reload is all we have */
    return;
  }
  try {
    await registration?.unregister();
    for (const key of await caches.keys()) await caches.delete(key);
  } catch {
    /* reload anyway */
  }
  window.location.reload();
}

/**
 * The hub says which version it is. If the shell this page runs is another
 * one, get the new shell: ask for a new worker, and reload by hand when none
 * took over in time. Once per hub version, so a build that never matches
 * (a development build) cannot reload in a loop.
 */
export function reconcileShellVersion(hubVersion: string | null | undefined): void {
  if (!registration || !hubVersion || hubVersion === __APP_VERSION__) return;
  if (forcedFor() === hubVersion || pending) return;
  pending = true;
  checkForAppUpdate();
  setTimeout(() => {
    pending = false;
    // A new worker reloads the page itself; if this one is still here, it did not.
    void reloadFresh(hubVersion);
  }, STALE_RELOAD_MS);
}
