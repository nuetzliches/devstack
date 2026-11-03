# ADR-0090: A service can get a public link, by its owner's hand: a Cloudflare Quick Tunnel to its fixed port

- **Status:** Accepted (card in the pull request); built; extended by [ADR-0091](0091-public-links-through-the-users-own-named-tunnel.md) (stable addresses through the user's own named tunnel)
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md) (sharing with the pool, which rejected links with a secret for *pool members*; a public link is not a share), [ADR-0043](0043-a-dev-servers-fixed-port-is-devstacks.md) and [ADR-0010](0010-promotion-mechanism.md) (the fixed port it goes to), [ADR-0009](0009-the-daemon-owns-every-process-tree.md) (the daemon owns `cloudflared`), [ADR-0019](0019-control-api-security.md), [ADR-0067](0067-a-change-at-runtime-says-who-made-it.md) (the journal), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Showing a running service to someone who has no stack, no pool membership
and no VPN (a customer, a phone, a webhook sender) meant leaving devstack:
running `cloudflared` or `ngrok` by hand, against a port that a restart or
`promote --lane` changes under it, and forgetting it afterwards. Pool
sharing (ADR-0035) reaches members with a certificate only.

A Cloudflare Quick Tunnel (`cloudflared tunnel --url`, no account) gives a
random `https://<words>.trycloudflare.com` address for one local origin,
without a login in front, with no SSE and at most 200 requests in flight.

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
seven, with step 1 (Quick Tunnels on the developer's machine); while
building, point 7 was built for services only:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | A public link is opened only by a click or a CLI call of the stack's owner, never by the stack file; it ends with a stop of the service by hand, the tree's `down` and the daemon, and does not outlast a daemon restart. Wrong, a dev service with seed data is on the internet without a login. | Opening it from the stack file. | Low while nothing builds on it. |
| 2 | trust | devstack's own ports (the hub's, the daemon's API) are never tunnelled. Wrong, the control API is on the internet. | "Any port that speaks HTTP". | Low: one check in the daemon. |
| 3 | trust | Only a lane's owner opens a link to it, not a member it is shared with. Wrong, a member publishes someone else's work. | "Whoever is connected may". | Medium: the pool's rules would change. |
| 4 | concept | A public link is a state of a service in its tree; the daemon owns the `cloudflared` process, and the link goes to the fixed port. | The hub owning the process; the instance's port. | High once the status and the API build on it. |
| 5 | contract | `POST`/`DELETE /v1/services/{name}/public-link`, `public_link: {state, url, since, error}` in the service's view, the CLI `devstack tunnel <service> [--stop]`. | The dashboard only. | High: scripts and agents read it. |
| 6 | term | The dashboard calls it a **public link** (`public_link`), not a tunnel: "tunnel" is the forward to a node (ADR-0029, ADR-0056). | "Tunnel" everywhere. | Medium: words travel into docs and the UI. |
| 7 | contract | `tunnel = false` on a service keeps it from a link; by default one is offered for every service with a `port`. *Built for `[services.*]` only: Compose containers get no link yet, so their `urls` need no such key.* | An opt-in list. | High: stack file compatibility (ADR-0023). |

Decided while building, not on the card: `cloudflared` comes from the
daemon's `PATH` (or `DEVSTACK_CLOUDFLARED`), devstack downloads nothing;
the origin is `localhost:<port>` (a dev server may bind IPv6 loopback only)
with `--http-host-header localhost:<port>`, so a dev server's host check
lets it through, and `--no-tls-verify` for an `https` service; the address
is read from `cloudflared`'s output, the link is `open` at its first
"Registered tunnel connection" and `failed` after 45 s without one or when
`cloudflared` ends; the dashboard asks before opening one, saying who
reaches it; no MCP tool opens one (the agent skill says to only on the
user's word); `devstack doctor` says whether `cloudflared` is installed.

As built:

1. **The daemon** (`supervisor/publiclink.rs`) runs `cloudflared` as an
   owned tree with a persisted record (ADR-0016's cleanup), its output in
   the service's log folder (`public-link.log`), and follows it on a thread
   of its own. A failed link stays in the view until it is closed or opened
   again. `down_keeping` closes the links of what it stops; the `stop`
   action closes the service's; restarts, rebuilds and promotions go
   through `stop` inside the supervisor and keep it.
2. **The API** answers `409` for a stopped service, one without a port,
   `tunnel = false`, the hub's or the daemon's port, `cloudflared` not
   found, and a daemon that runs a lane on a node. Each change is a
   `public_link` event; the journal records the requests as any other.
3. **The CLI** prints the address on stdout once registered (the rest on
   stderr); `devstack status` lists open, starting and failed links first.
4. **The dashboard** has a globe button on a service's row; an open link is
   a line under it (the address, copy, close), its toast says the service
   is public. A guest's view of a shared lane is read only and has none.

## Consequences

- Anyone with the address reaches the service, its seed data and whatever
  its environment exposes; the owner decides by opening it, and the
  dashboard says so before.
- A front end that calls its API on `localhost:<port>` works from outside
  only when it proxies the API itself; several origins behind one address
  need a named tunnel (an account), built by ADR-0091.
- A lane on a node gets no link yet: its daemon runs on the node, and the
  link would leave from there (step 3, the node's policy, ADR-0033).

## Enforced by

`crates/devstack-daemon/src/supervisor/publiclink.rs` (the process, the
checks of points 1 to 3), `crates/devstack-daemon/src/api.rs`
(`/v1/services/{name}/public-link`, a stop closes it),
`crates/devstack-daemon/src/supervisor/lifecycle.rs` (`down` closes them),
`crates/devstack-config/src/{model,validate}.rs` (`tunnel`,
`TUNNEL_SINCE`), `crates/devstack/src/client.rs` (`devstack tunnel`, the
status lines), `crates/devstack/src/doctor.rs`, `ui/src/components/ServiceRow.vue`,
`ui/src/stores/stack.ts`, `crates/devstack/tests/daemon_lifecycle.rs`
(`a_public_link_goes_to_the_fixed_port_and_ends_with_a_stop`, with a fake
`cloudflared`), `docs/stack-file.md` ("Public links").
