# ADR-0021: A hub on a fixed port serves the dashboard for every stack

- **Status:** Accepted (2026-10-15)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0018](0018-dashboard-is-a-browser-page.md) (amended here), [ADR-0019](0019-control-api-security.md), [ADR-0007](0007-one-http-api-is-the-control-surface.md)

## Context

ADR-0018 has each stack's daemon serve the dashboard. Three wishes for the
dashboard do not fit that:

1. **It survives a daemon shutdown.** A progressive web app caches its shell and
   keeps working offline, but only for a stable origin. A daemon picks a free
   port at every start, so its origin changes.
2. **It switches between projects.** Daemons are per stack and know nothing of
   each other; a page served by one cannot list or start the others.
3. **It never polls.** Live state comes from event streams.

## Decision

1. **A hub process (`devstack hub`) on a fixed loopback port** (default
   `127.0.0.1:4250`, `DEVSTACK_HUB_PORT` overrides) serves the dashboard as a
   progressive web app. It is started on demand by `devstack up` and
   `devstack ui`, runs without a window, and lives until the user logs out or
   runs `devstack hub stop`. It also starts at logon, and a `devstack://hub/start`
   link starts it from the dashboard: while the hub is down nothing listens on
   its port, so the page has to ask the system. Both are registered in the
   user's profile (Windows: `HKCU` Run key and URL class, through
   `conhost.exe --headless` so no window opens; Linux: XDG autostart and a
   `x-scheme-handler/devstack` desktop file). `devstack up` registers them once,
   the installers always, `devstack hub uninstall` removes them and keeps `up`
   from adding them again. A link can do nothing but start the hub.
2. **A registry of stacks** in the user's state directory (next to the token):
   every daemon registers its stack (id, name, stack file, repository) when it
   starts. The id is derived from the stack file's absolute path.
3. **The hub forwards to the daemons:** `/hub/stacks/{id}/api/v1/...` goes to
   that stack's daemon with the user's token, event streams included. It also
   starts a stopped stack (`up`) and stops one, which is what lets the dashboard
   outlive a daemon.
4. **Event streams only.** The hub publishes the list of stacks and their
   running state as a stream (`/hub/events`); a daemon's stream starts with a
   snapshot of its state, then sends changes; logs are streams too. The
   dashboard holds no timers that ask for state.
5. **Security per ADR-0019**, now on the hub: loopback only, `Host` checked,
   the browser authenticates by pairing (`devstack ui` gets a one-time code from
   the hub with the user's token and opens `/auth?code=…`, which sets an
   `HttpOnly; SameSite=Strict` cookie), mutating requests need the hub's own
   `Origin`, no CORS. The daemons keep their own token-only API; the browser
   never talks to them directly. A pairing outlives a hub restart (an update,
   a new login): the hub keeps a hash of each session cookie in
   `hub/hub-sessions.json` next to the token, never the cookie, for the
   cookie's lifetime of a year. Deleting the file unpairs every browser.
6. **One public question: `GET /hub/ping`.** The landing page
   (`https://dev.nuetzliche.it`, built from `site/`) asks the local hub whether
   it runs and which version, to offer "open the dashboard", "update available"
   or the install command. The endpoint needs no session, answers name and
   version only, changes nothing, and its CORS header names only the landing
   page (more origins through `DEVSTACK_HUB_PING_ORIGINS`, for a team hosting
   its own copy); it also answers Chrome's private-network preflight. The rest
   of the hub keeps ADR-0019: no CORS, cookie and own `Origin`. The dashboard
   itself is never loaded from that site.
7. **Stopping does not wait for the dashboard.** Every open tab holds event
   streams that never end on their own, and a graceful shutdown waits for open
   requests. Hub and daemon give requests two seconds after a stop, then exit;
   the dashboard sees the connection drop and shows the last known state.

ADR-0018 stands otherwise: a browser page, no desktop wrapper, a tray later.
The tray belongs to the hub, which is the long-lived process now.

## Alternatives considered

- **Each daemon serves the dashboard (ADR-0018 as written).** No offline shell,
  no project switch, no way to start a stopped stack from the page.
- **A fixed port per daemon.** Two stacks would need two configured ports, and
  the project switch would still need something that knows all of them.
- **The first daemon to start binds a shared dashboard port.** The dashboard
  would die with whichever stack happened to start first.

## Consequences

- One more process, small and idle most of the time.
- The hub must stay compatible with daemons of the same minor version; it
  shows a daemon's version and warns on a mismatch.
- The dashboard's offline view shows the last state it saw, marked as such.

## Enforced by

- `crates/devstack-hub` (registry, pairing, forwarding, serving the embedded
  dashboard), `devstack hub`, `devstack ui`.
- `ui/` (Vue 3, Vite, TypeScript), built into the binary.
- [`docs/dashboard-api.md`](../dashboard-api.md): the contract between them.
