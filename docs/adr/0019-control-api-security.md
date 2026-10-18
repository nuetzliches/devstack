# ADR-0019: The control API trusts the user's own processes, never a web page

- **Status:** Accepted (2026-10-14), amended 2026-10-18 (the own page for reads too; the hub proves itself to the CLI; IPv4 loopback and `DEVSTACK_LISTEN`; the cookie is the hub's, no `devstack token rotate`)
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

*Amended 2026-10-18, for the hub that holds the cookie now (ADR-0021).*
`SameSite=Strict` is no boundary between loopback ports: browsers scope it by
site (scheme and host), so a page on `http://127.0.0.1:3000`, say the app under
development with a compromised dependency, is same-site with the hub, and its
`<img>` and `fetch` requests carry the cookie. Point 4 therefore covers reads
too: **every request with the cookie must come from the hub's own page.** A
mutation and the WebSocket handshake name the hub's own `Origin`; a `GET`
names it or says `Sec-Fetch-Site: same-origin` (or `none`, a navigation the
user typed); a browser that says `same-site` or `cross-site` is refused
whatever else it sends. Every current browser sends `Sec-Fetch-Site`. The hub
also forbids framing and inline script (`Content-Security-Policy` with
`frame-ancestors 'none'` and `script-src 'self'`, `X-Frame-Options: DENY`),
so a paired dashboard cannot be laid under another page's clicks. And a path
the browser hands the hub must be local: a network path would have the hub
connect to that host as the user.

The second concern of the context, other local users, is narrowed for the
token's way out: the CLI sends the token only to a hub that proves it knows it
(`GET /hub/ping?nonce=` answers a keyed hash of the nonce under a key derived
from the token), so a program of another user that took the hub's port while
it was down learns nothing. What stays: another local user who can read this
user's profile, or run code as this user, is this user.

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
- Since the amendment: `crates/devstack-hub/src/lib.rs` (`guard`,
  `from_own_page`, `page_headers`, `stack_paths`, `who_answers`);
  `crates/devstack/tests/hub.rs` (`pairing_cookie_and_checks`,
  `the_token_goes_only_to_this_users_hub`).

## Addendum 2026-10-18: IPv4 loopback, and `DEVSTACK_LISTEN`

1. The daemon listens on `127.0.0.1` only, on a free port, which
   `daemon.json` names; it does not also bind `::1` (the port chosen on IPv4
   may be another process's on IPv6, and every client connects to
   `127.0.0.1`). The `Host` check still accepts `[::1]:<port>`, which costs
   nothing. Point 1's "(and `::1`)" reads as "never anything but loopback".
2. `DEVSTACK_LISTEN=<address:port>` binds another address, for one case: the
   daemon inside a lane container on a node (ADR-0025), which the node agent
   reaches over the lane's network. It is accepted only together with
   `DEVSTACK_REMOTE_LANE` (the lane container's marker, set by the node
   agent); anywhere else the daemon refuses to start with an error. In the
   lane container the API is then reachable from every container on the
   lane's network, protected by its token and the `Host` check alone.

## Addendum 2026-10-18: the cookie is the hub's; there is no `token rotate`

1. **Point 2's cookie flow runs in the hub, not in the daemon**
   ([ADR-0021](0021-a-hub-serves-the-dashboard.md)). `devstack ui` sends the
   token to the hub (`POST /hub/pair`, only to a hub that proved it knows the
   token, see the amendment above), gets a one-time code (single use, 60
   seconds) and opens `http://127.0.0.1:<hub port>/auth?code=…`; the hub sets
   `HttpOnly; SameSite=Strict; Path=/` with a lifetime of a year and keeps a
   hash of it in `hub-sessions.json` (0600; deleting the file unpairs every
   browser with its next request). A daemon has no cookie, no `/auth` and
   no static files: every request to it carries the token, including the
   ones the hub forwards for the dashboard. Its `Host`, token and `Origin`
   checks (points 3 and 4) are as decided. The context's "dashboard served
   by the daemon itself" and the cookie of point 2 read as the hub's.
2. **`devstack token rotate` (Consequences) does not exist.** To replace
   the token: stop every stack's daemon (`devstack down` in each repository)
   and the hub (`devstack hub stop`), delete the token file
   (`%LOCALAPPDATA%\devstack\token`, `$XDG_STATE_HOME/devstack/token`, by
   default `~/.local/state/devstack/token`), then start again; the first
   daemon or hub creates a new one, readable by this user only. Paired
   browsers stay paired (their cookie is not derived from the token); delete
   `hub/hub-sessions.json` beside the token file to unpair them too.

*Paths as built:* `crates/devstack-daemon/src/api.rs` (`guard`: `Host`,
token in constant time, `Origin` on every method but `GET`, for every route
and the fallback; no CORS headers), `crates/devstack-daemon/src/token.rs`
(`load_or_create`: created 0600, never in a shared temporary directory),
`crates/devstack-daemon/src/lib.rs` (`listen_addr`), `crates/devstack-hub/src/lib.rs`
(`pair`, `auth`, `guard`), `crates/devstack/src/hubcmd.rs` (`devstack ui`).
