# ADR-0021: A hub on a fixed port serves the dashboard for every stack

- **Status:** Accepted (2026-10-15), amended 2026-10-15 (the dashboard's streams come over WebSocket), amended 2026-10-18 (the pool area polls; cookie, sessions file and ping in detail)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0018](0018-dashboard-is-a-browser-page.md) (amended here), [ADR-0019](0019-control-api-security.md), [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0006](0006-dashboard-vue-same-origin.md) (who serves the dashboard, replaced here)

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
   dashboard holds no timers that ask for state. **The dashboard receives
   every stream over a WebSocket** (`/hub/ws?path=<stream>`): the hub opens
   the server-sent event stream at that path, its own or a daemon's, and sends
   each event's data as one message. The streams themselves stay server-sent
   events, for the CLI and for anything else that reads them.
5. **Security per ADR-0019**, now on the hub: loopback only, `Host` checked,
   the browser authenticates by pairing (`devstack ui` gets a one-time code from
   the hub with the user's token and opens `/auth?code=…`, which sets an
   `HttpOnly; SameSite=Strict` cookie), mutating requests need the hub's own
   `Origin`, no CORS; so does the WebSocket handshake, although it is a
   `GET`, because a page of any origin may open one. The daemons keep their own
   token-only API; the browser
   never talks to them directly. A pairing outlives a hub restart (an update,
   a new login): the hub keeps a hash of each session cookie in
   `hub/hub-sessions.json` next to the token, never the cookie, for the
   cookie's lifetime of a year. Deleting the file unpairs every browser.
6. **One public question: `GET /hub/ping`.** The landing page
   (`https://nuetzliches.github.io/devstack/`, built from `site/`) asks the local hub whether
   it runs and which version, to offer "open the dashboard", "update available"
   or the install command. The endpoint needs no session, answers name and
   version only, changes nothing, and its CORS header names only the landing
   page's origin `https://nuetzliches.github.io` (shared by the organisation's
   Pages sites, which is acceptable for a version number; more origins through
   `DEVSTACK_HUB_PING_ORIGINS`, for a team hosting its own copy); it also answers Chrome's private-network preflight. The rest
   of the hub keeps ADR-0019: no CORS, cookie and own `Origin`. The dashboard
   itself is never loaded from that site.
7. **Stopping does not wait for the dashboard.** Every open tab holds event
   streams that never end on their own, and a graceful shutdown waits for open
   requests. Hub and daemon give requests two seconds after a stop, then exit;
   the dashboard sees the connection drop and shows the last known state.

*Amended 2026-10-15.* The dashboard read its streams as `EventSource`s.
A browser keeps at most six HTTP/1.1 connections per host, across every tab
and installed-app window of a profile, and each dashboard window holds two to
four streams open. Two or three windows used the six up; every request after
that, a restart click included, waited in the browser's queue without end
while the window showed "Connecting…". Measured with eight windows: a request
answered in 2 ms over WebSockets, and not within 8 s once each window held one
`EventSource` as well. WebSockets do not count against that limit.

*Amended 2026-10-18, after a review.* Four points in detail:

- **Point 4, the pool area polls.** The pool's state lives in its
  coordinator, which has no event stream; the hub reads it with this
  machine's member certificate on request. The pool area reads
  `/hub/pool...` every 10 s while it is shown, and the parts other pages show
  (where a lane can start or move, the capacity chart) every 30 s. The hub
  polling the coordinator in the dashboard's stead and pushing the result
  over `/hub/events` would move the timer, not remove it. This is the one
  exception to "no timers that ask for state"; it ends when the coordinator
  gets a stream.
- **Point 5, the session cookie is no boundary by itself.** `SameSite=Strict`
  does not separate loopback ports, so every request with the cookie must
  come from the hub's own page, reads included (ADR-0019 as amended), and the
  hub forbids framing and inline script. Any cookie of the hub's name that is
  a session counts: another server on a loopback port can set one of the same
  name, and the browser then sends both.
- **Point 5, deleting `hub-sessions.json` unpairs at once.** The hub reads
  the file again when it changed, before it checks a cookie, so a deleted
  file unpairs every browser with its next request, not at the next start. A
  WebSocket already open stays until it reconnects. The file is written for
  the user alone (`0600` on Unix).
- **Point 6, the ping proves the hub to the CLI.** With `?nonce=` it also
  answers a keyed hash of the nonce under a key derived from the user's
  token. The CLI sends the token only to a hub that answers it right, since a
  TCP answer on the port proves nothing on a machine several people use. The
  landing page's view is unchanged: without a nonce the answer is name and
  version.

ADR-0018 stands otherwise: a browser page, no desktop wrapper, a tray later.
The tray belongs to the hub, which is the long-lived process now. (Amended
2026-10-23 by [ADR-0060](0060-the-tray-is-a-client-of-the-hub.md): the hub
starts the tray as a process of its own and tells it what to show.)

## Alternatives considered

- **Each daemon serves the dashboard (ADR-0018 as written).** No offline shell,
  no project switch, no way to start a stopped stack from the page.
- **A fixed port per daemon.** Two stacks would need two configured ports, and
  the project switch would still need something that knows all of them.
- **The first daemon to start binds a shared dashboard port.** The dashboard
  would die with whichever stack happened to start first.
- **HTTP/2 instead of WebSockets** (many streams over one connection). Browsers
  speak HTTP/2 only over TLS; a certificate for a loopback hub is a setup step
  of its own and a trust problem.
- **One multiplexed stream per window, or one per browser** (a shared
  worker). Fewer connections, still counted against the six, and a second
  protocol on top of the streams.
- **Streams paused in hidden tabs.** Helps, but two visible windows with a log
  open still reach the limit.

## Consequences

- One more process, small and idle most of the time.
- The hub must stay compatible with daemons of the same minor version; it
  shows a daemon's version and warns on a mismatch.
- The dashboard's offline view shows the last state it saw, marked as such.

## Enforced by

- `crates/devstack-hub` (registry, pairing, forwarding, serving the embedded
  dashboard; `ws.rs`, the WebSocket bridge), `devstack hub`, `devstack ui`.
- `ui/src/api/real.ts` (`subscribe` over WebSocket); `crates/devstack/tests/hub.rs`
  (`live_streams_come_over_websocket`).
- `ui/` (Vue 3, Vite, TypeScript), built into the binary.
- [`docs/dashboard-api.md`](../dashboard-api.md): the contract between them.
