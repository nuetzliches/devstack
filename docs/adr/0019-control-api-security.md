# ADR-0019: The control API trusts the user's own processes, never a web page

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0018](0018-dashboard-is-a-browser-page.md), plan §5

## Context

The daemon's API can start and stop processes, run arbitrary commands from the
stack file and expose logs that contain development secrets. It listens on a
developer machine on which a browser runs, with tabs from arbitrary origins.

Binding to loopback keeps other machines out, but not other content on the same
machine. A web page can send requests to `http://127.0.0.1:<port>`: simple
requests (form posts, `GET`s) even without CORS, and a DNS-rebinding page can
make the browser treat the daemon as same-origin. Other local users on a shared
machine are a second, smaller concern.

The clients are the CLI, the MCP server (both processes of the same user) and
the dashboard served by the daemon itself.

## Decision

1. **Loopback only.** The daemon binds `127.0.0.1` (and `::1`), never a wildcard
   address. Remote use goes through an SSH tunnel or a port forward the user
   sets up deliberately.
2. **Every request carries a credential**, reads included:
   - **CLI and MCP** read a random token from a file in the user's profile,
     readable only by that user (`%LOCALAPPDATA%\devstack\token` on Windows,
     `$XDG_STATE_HOME/devstack/token` with mode 0600 elsewhere), and send it as
     `Authorization: Bearer`.
   - **The dashboard** never sees that token. `devstack ui` asks the daemon for a
     one-time code (single use, valid 60 seconds) and opens
     `http://127.0.0.1:<port>/auth?code=…`; the daemon exchanges it for a session
     cookie `HttpOnly; SameSite=Strict; Path=/`. Nothing is kept in
     `localStorage`.
3. **The `Host` header must be `127.0.0.1:<port>`, `[::1]:<port>` or
   `localhost:<port>`**, which defeats DNS rebinding.
4. **Mutating requests from a browser must carry an `Origin` equal to the
   daemon's own origin**; a request with a foreign `Origin` is rejected even with
   a valid cookie.
5. **No CORS headers are sent.** Nothing outside the daemon's origin is meant
   to call it from a browser.

## Alternatives considered

- **Loopback without credentials.** Common for local developer tools and the
  reason "a web page can call localhost" keeps being found in them.
- **Token in the dashboard's `localStorage` or URL.** Readable by any script in
  the page, logged in browser history; the cookie keeps it out of JavaScript.
- **Unix domain socket / named pipe only.** Strong local isolation, but a
  browser cannot reach it, and ADR-0018 makes the browser the dashboard.

## Consequences

- Every client needs the token file; `devstack up` creates it on first start
  and rotates it on request (`devstack token rotate`).
- The MCP server runs as a separate process of the same user and reads the same
  file; agents never see the token in their context.
- Tests of the API use the same file; there is no "test mode without auth".

## Enforced by

- API middleware (planned): `Host` check, credential check, `Origin` check on
  mutating methods, in that order, for every route including static files.
- `devstack ui` (planned): one-time code flow.
