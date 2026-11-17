# ADR-0110: A static service forwards paths to another server, as a reverse proxy in front of it would

- **Status:** Accepted (card in the pull request); built; amended 2026-11-17 (connections to the target are kept, #556)
- **Date:** 2026-11-17
- **Deciders:** project owner
- **Related:** [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) (profiles), [ADR-0023](0023-stack-file-compatibility.md) (a new key is optional), [ADR-0010](0010-promotion-mechanism.md) (the service proxy whose header handling it shares), [ADR-0058](0058-a-profile-can-leave-out-containers-and-services.md) (`skip`), [ADR-0090](0090-a-service-can-get-a-public-link-by-hand.md) (public links), issue #551

## Context

Profiles (ADR-0041) are meant to replace a hand-written acceptance setup.
A single-page app that calls its API on its own origin (a relative `/api`,
as in production behind a reverse proxy) did not fit: `kind = "static"`
served the bundle's files only, so every `/api/...` request got the app's
`index.html` or a 404. In development the dev server's proxy config covers
this (`ng serve --proxy-config`, Vite `server.proxy`); a profile that turns
that dev server into `kind = "static"` lost the proxy, and the stack file
had nothing to put in its place. A downstream stack kept a separate Compose
stack with a reverse proxy in front only for this.

## Decision

The owner approved the review card in the conversation on **2026-11-17**,
all six points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A new optional key `forward` maps a path prefix to a target (`{ "/api" = "{service.api.url}" }`); only for `kind = "static"`, also in `[profiles.<p>.services.<s>]`; elsewhere a check error; `FORWARD_SINCE` with its warning for an older `requires`. | A proxy service of its own per repository (Caddy, Node); an absolute API URL plus CORS | A published key goes only as a deprecated alias (ADR-0023) |
| 2 | concept | The longest prefix wins, at a segment boundary only (`/api` takes `/api`, `/api/…`, `/api?…`, not `/apix`); path and query go on unchanged; a forwarded path never falls back to a file or `index.html`. | A plain string prefix that would take `/apix` routes | Low while no one uses it |
| 3 | contract | A target is an origin `http://host:port`, without a path or https; it takes the placeholders of `env` and gets their values when the instance starts; `check` refuses a path, a service with `insecure_tls` and a container the profile `skip`s. | Path rewriting (`strip`) and TLS to the target now | Loosening later is compatible; tightening would be a break |
| 4 | trust | As a reverse proxy: `Host` stays as the browser sent it, `X-Forwarded-For`/`-Host`/`-Proto` are added, hop-by-hop headers stay on their hop, answers are streamed (SSE, chunked), a WebSocket upgrade passes; as the service proxy (ADR-0010). | Rewriting `Host` to the target (Vite's `changeOrigin`) | A header change that targets checking `Host` notice |
| 5 | concept | An unreachable target is a `502` at once, naming the target; no holding. The static service's health check stays on its own files, not the targets. | Holding requests until the target is ready, as the service proxy does | Adding holding later is compatible |
| 6 | trust | Forwards are reachable wherever the static port is: the lane's port, and a public link (`devstack tunnel`) to the static service reaches the API too, as in production behind a reverse proxy. | Limiting the tunnel to the files, which breaks the app there | Switching forwards off in the tunnel: a visible change for whoever shares that way |

As built:

1. **The key** (`crates/devstack-config`): `Service::forward` and
   `ServiceProfile::forward` (`BTreeMap<String, String>`); a profile's
   entries are added to the service's, and a profile that changes the kind
   away from `static` drops them, as it drops `spa`. The rules live in
   `forward.rs` (`prefix`, `matches`, `pick`, `origin`), shared by the check
   and the server. `validate` checks each prefix, checks each target's
   placeholders as `env`'s, and, where no `${VAR}` leaves it open, its form
   with a probe value per placeholder (`{service.x.url}` with the service's
   scheme, so one with `insecure_tls` is refused). A skipped container's
   `{compose.…}` in a target is an error, as in `env`.
   `FORWARD_SINCE = "0.49.0"`, with its warning for the service's and each
   profile's `forward`.
2. **The server.** `devstack serve-static` takes `--forward PREFIX=URL`
   (repeatable) and checks it with the same rules at the start. The daemon
   appends one per entry to the static service's generated `run`
   (`with_static_servers`), so its placeholders get their values like the
   rest of `run`, per lane.
3. **Passing a request** (`staticsrv::pass`): an HTTP/1.1 connection to
   the target from its idle pool, or a new one (amendment below), the
   request target in origin form, the client's
   `Host` kept (or taken from an HTTP/2 authority), `X-Forwarded-For`
   appended to, `X-Forwarded-Host` and `-Proto` set where a proxy in front
   (a tunnel) has not set them; `proxy::strip_hop_by_hop` both ways; the
   answer's body passed as a stream; a `101` joins both upgraded
   connections.

Decided while building, not on the card:

- The forward runs inside the static service's own process, not in the
  daemon: it starts, stops and moves with the instance.
- ~~No connection pool to the target: one connection per request is enough
  on loopback and keeps the server small.~~ Wrong, see the amendment.
- A target without a port is port 80; an IPv6 host is written in brackets.
- Prefixes take letters, digits and `-._~` only, so they need no quoting in
  the generated command line.

## Alternatives considered

- **A proxy service per repository** (`kind = "service"` running Caddy or a
  small Node server in front of the build output): works today, but every
  repository writes the same proxy again, and the stack file no longer says
  what the service is.
- **An absolute API URL in the build plus CORS on the API:** changes the
  artifact under test and the API's configuration; the run no longer tests
  what ships.
- **Forwarding in the daemon's service proxy** (ADR-0010) instead of the
  static server: the proxy sits only in front of a service with `proxy`
  on, and the forwards belong to the build's way of being served, not to
  the switch between instances.

## Consequences

- A profile can turn a dev server with a proxy config into a static build
  without a second stack for the reverse proxy.
- A target that is still starting answers `502` until it is up; an
  end-to-end suite started with `--run` waits for the tree to be ready, so
  it does not see that.
- HTTPS targets and path rewriting are not there; both can be added without
  breaking a file that works now.

## Amendment 2026-11-17: connections to the target are kept (#556)

One connection per request was wrong. devstack closed each one first, so
each left a socket in `TIME_WAIT` on this machine for two minutes; an
end-to-end suite (four workers, about 1000 tests in 11 minutes) left about
7000 and drained Windows' ephemeral ports, and the browser and the forward
failed with `ERR_ADDRESS_IN_USE` and `502`. A reverse proxy keeps its
upstream connections, and so does a dev server's proxy.

Each target now keeps up to 32 idle connections. A request takes a ready
one or opens a new one; one the target closed while idle is sent again on a
new connection, as the service proxy does (ADR-0010). A connection goes back
once its answer is read to its end (for a body of known length: its last
frame, which hyper does not poll past); an upgraded one (WebSocket) is the
upgrade's, and a stream the browser leaves early, or one that does not end
(server-sent events), keeps or drops its connection. 2000 requests four at
a time over new browser connections now open 4 connections to the target
instead of 2001; a unit test bounds the count. No point of the card
changes: the pool was decided while building.

The pool is `devstack_node::connpool` (`Pool`, and `GiveBack`, the body that
returns the connection), shared with the hub's way to its daemons, which
had the same flaw. Two places keep their own way: the service proxy's pool
is tied to the drain of an instance it switches away from, and the public
link router (`linkrouter.rs`) still opens a connection per request, since
its `Dial` is also its check that a host is still open; a kept connection
would pass a link its owner closed. (It keeps its connections since #559,
with that check made for every request: ADR-0091, amendment.)
