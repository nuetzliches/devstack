# ADR-0043: A dev server's fixed port is devstack's, forwarded to its instance

- **Status:** Accepted (2026-10-20)
- **Date:** 2026-10-20
- **Deciders:** project owner
- **Related:** [ADR-0010](0010-promotion-mechanism.md) (the proxy, which this extends to dev servers), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (`promote --lane`), [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md), plan §3.4a

## Context

The fixed port is what clients know: a browser tab, an OAuth redirect URI,
a CORS rule, another service's proxy configuration. ADR-0010 put a
service's fixed port behind devstack's HTTP proxy, which forwards to the
instance's own port: a new build takes over without a dropped request,
`promote --lane` flips the proxy's upstream, and a start that fails leaves
the address answering with why.

A dev server was left out: it binds the fixed port itself. So

- a restart (`restart_on`, the restart button) leaves the address closed
  for as long as the dev server needs to start: connections are refused,
  and a browser's live-reload socket gives up;
- `promote --lane` has to stop the main tree's dev server before it can put
  a forward of its own on the port;
- a process that outlived its run (a dev server's child, a stale one)
  holds the address itself, and every later start fails with "port in
  use";
- the view has no fixed and instance port for it, unlike every other
  service with a port.

The HTTP proxy is the wrong layer for it: dev servers speak TLS
(`ng serve --ssl`), WebSockets (live reload, HMR) and HTTP/2, and the proxy
would have to terminate TLS. A forward of bytes needs none of that.

## Decision

1. **A dev server with a `{port}` in its `run` sits behind a forward of
   devstack's on its fixed port** (TCP, byte for byte: TLS, WebSockets and
   HTTP/2 pass unchanged). The dev server runs on a free port of its own,
   which `{port}` names; the forward listens on the fixed port from the
   first start on, on IPv4 and IPv6 loopback, as `promote --lane`'s forward
   does.
2. **The forward has three states**, as the proxy has:
   - *hold* while the dev server starts or restarts: a new connection
     waits until it answers (up to its health check's timeout), then goes
     through;
   - *forward* to the running instance;
   - *refuse* while it is stopped or exited: a new connection is closed at
     once.
   A restart keeps the address: the browser waits instead of being refused.
3. **`promote --lane` flips the forward** to the lane's port, as it flips a
   proxy's upstream; the main tree's dev server stops as before (one dev
   server per fixed port, ADR-0008's reasoning for workers holds for its
   watchers), and starts again behind the forward when the main ports come
   back.
4. **Opt out**: `proxy = false` on a dev server, or a `run` without
   `{port}`, keeps today's behaviour (the dev server binds the fixed port
   itself). The stack file's meaning of `proxy` widens: "devstack holds the
   fixed port" (HTTP proxy for a service, a forward for a dev server).
5. **The view shows it** like a proxied service's: the fixed port, the
   instance port, the connections open through it.

## Consequences

- Restarts, `restart_on` and `promote --lane` keep a dev server's address
  open; a stale process can block only an instance port, which is chosen
  anew at every start.
- A dev server prints its instance port (`http://localhost:51234/`) in its
  own log; the address to open is the fixed one, which the dashboard
  links.
- A dev server that builds absolute URLs from the port it listens on (an
  HMR client port set explicitly, a `--public-host`) names the instance
  port; such a stack sets those to the fixed port or uses `proxy = false`.
  Angular and Vite take the page's own address by default.
- devstack holds the fixed port while the dev server is stopped (it refuses
  connections there), as it does for a proxied service.

## Alternatives considered

- **Put dev servers behind the HTTP proxy.** It would have to terminate TLS
  for `--ssl` dev servers and re-encrypt; a forward needs no certificate.
- **Keep binding the fixed port and only retry faster.** The address stays
  closed during a restart, and a stale process still blocks it.

## Enforced by

`crates/devstack-daemon/src/tcpfwd.rs` (`Front`: hold, forward, refuse),
`crates/devstack-daemon/src/supervisor/` (the front on a dev server's start,
stop and exit; `tcp_forward` flips it for `promote --lane`),
`devstack_config::model::Service::fronted`.
