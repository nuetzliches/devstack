// The app shell comes from the hub, cached by the service worker for offline
// use. A new devstack brings a new shell; this makes an open tab pick it up.
//
// - On load: navigations go to the network first (see vite.config.ts), so a
//   reload shows the version the hub serves now, the cache only when it is down.
// - With the tab open: whenever the hub stream comes back after a break (a hub
//   restart, often an update), ask the browser to look for a new service
//   worker. With `autoUpdate` a new one activates at once and reloads the page.

let registration: ServiceWorkerRegistration | undefined;

export function rememberRegistration(r: ServiceWorkerRegistration | undefined): void {
  registration = r;
}

/** One request for `sw.js`; nothing happens when it did not change. */
export function checkForAppUpdate(): void {
  void registration?.update().catch(() => {
    /* hub gone again: the next reconnect asks again */
  });
}
