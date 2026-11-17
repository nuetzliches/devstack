# ADR-0091: Public links can go through the user's own named tunnel, at stable addresses; the hub runs its one connector

- **Status:** Accepted (card in the pull request); built; its step 3, the pool, built by [ADR-0092](0092-public-links-through-the-pool.md); amended 2026-11-17 (the router keeps its connections to services, checked per request, #559)
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0090](0090-a-service-can-get-a-public-link-by-hand.md) (public links as Quick Tunnels, which this extends; its points 1 to 3 hold here too), [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (the hub's keychain), [ADR-0053](0053-whoever-a-lane-is-shared-with-connects-to-it.md) (connecting a lane as a whole), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (the hub follows every tree's events), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A Quick Tunnel (ADR-0090) has a new random address each time: no bookmark,
OAuth redirect URI or CORS rule can name it, and a front end cannot be told
where its API is. A Cloudflare Tunnel of the user's own account, under a
wildcard `*.<domain>`, has stable hosts and can sit behind Cloudflare
Access. Two connectors of one tunnel share its requests at random, so one
connector must carry every link of a machine.

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
seven:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | devstack gets the credentials of one tunnel only, never `cert.pem` or an API token. Wrong, an attacker with the machine changes the account's DNS and zone. | An API token with which devstack makes tunnels and DNS itself | Medium |
| 2 | trust | The credentials are in the hub's keychain, as ADR-0069's; no route hands them out. | A file in `~/.cloudflared` | Low |
| 3 | concept | The named tunnel is the hub's, one connector per machine; its router lets the hosts of open links through, every other host is answered 404. | A `cloudflared` per daemon | High |
| 4 | contract | The host is `{service}-{lane}-{stack}.{domain}`, without the lane for `main`, one level below the wildcard (its certificate covers one level). | Random names; several levels | High: bookmarks and redirects |
| 5 | contract | `{service.<name>.public_url}` gives that address, the local URL without a tunnel set up. | No placeholder | High: the stack file (ADR-0023) |
| 6 | term | **Make public** opens every service of a lane at once, as connecting does (ADR-0053); a front end then reaches its API at its `public_url`. | Services one by one | Medium |
| 7 | trust | devstack manages no Cloudflare Access: the user puts a login before `*.{domain}` in Cloudflare's dashboard; the confirmation names the domain. | Access applications through the API, which needs a token | Low |

Decided while building, not on the card: the setup is `devstack tunnel
--setup <credentials.json> --domain <d>` (and `--remove-setup`; `devstack
tunnel` shows the state), the hub's `PUT`/`DELETE
/hub/public-links/tunnel`; the domain and the tunnel's id (no secret) are in
`named-tunnel.json` beside the token, which daemons read; `cloudflared`
gets the credentials in `TUNNEL_CRED_CONTENTS`, not on its command line, and
a config file of the hub's own (`tunnel`, one catch-all ingress to the
router), so the user's `~/.cloudflared/config.yml` does not apply; the
connector starts with the first open link and ends 20 s after the last;
a host two trees claim is answered 404 (two stacks of one name); a label
over 63 characters is cut and ended with a hash; the router sends the
`Host` a browser here sends (`localhost:<port>`, as a Quick Tunnel's
`--http-host-header`), `X-Forwarded-Host` and `X-Forwarded-Proto`, accepts
any certificate of an `https` service (a development one), and passes
WebSockets; `devstack tunnel --all` for *Make public*.

As built:

1. **The hub** (`publiclinks.rs`) binds the router on a loopback port at
   start, follows the links from the events `notify` already reads (the
   snapshot's ports and links, `public_link` events), and clears a tree's
   when its stream ends. `GET /hub/public-links` says the tunnel, the
   connector's state and the hosts it carries.
2. **A daemon** with the tunnel set up opens a link as `starting` at its
   stable address, runs nothing, and sets it `open` once the hub says its
   connector runs and carries the host (`failed` after 45 s, or 5 s
   without an answer from the hub). `StackView.public_link_domain` names
   the domain. `POST`/`DELETE /v1/public-link` open and close a tree's.
3. **The placeholder** resolves in services' environments, commands and
   `devstack env`, from the tunnel's domain, the stack's name and the lane.
4. **The dashboard** has *Make public* / *Public (n)* in a lane's header;
   both confirmations name the domain and Access.

## Consequences

- A link needs the hub running; a Quick Tunnel needs none.
- A tunnel set up or removed while links are open changes nothing about
  them until they are closed; a service's `public_url` changes at its next
  start.
- Two stacks with the same `[stack] name` share hosts: whichever opens a
  link second makes both unreachable until one closes.
- A pool (one tunnel per member, its DNS by a token on the coordinator, a
  node's own connector) is step 3, not built.

## Enforced by

`crates/devstack-hub/src/publiclinks.rs` (keychain, connector, router,
API; points 1 to 3), `crates/devstack-hub/src/notify/mod.rs` (the events it
follows), `crates/devstack-daemon/src/namedtunnel.rs` (the shared file, the
host, point 4), `crates/devstack-daemon/src/supervisor/publiclink.rs`
(links through the tunnel, the lane at once), `crates/devstack-config/src/interpolate.rs`
and `crates/devstack-env/src/lib.rs` (point 5), `crates/devstack/src/hubcmd.rs`
(`--setup`), `ui/src/components/StackHeader.vue` and `ui/src/stores/stack.ts`
(points 6, 7), `crates/devstack/tests/daemon_lifecycle.rs`
(`a_named_tunnel_gives_stable_addresses_and_needs_the_hub`),
`docs/stack-file.md` ("Stable addresses").

## Amendment 2026-11-17: the router keeps its connections, checked per request (#559)

The router (`linkrouter.rs`, shared by the hub and a node) opened a new
connection to the service for every request and closed it first, leaving
each in `TIME_WAIT` for two minutes: the class #556 fixed for a static
service's `forward`. Its `Dial` was both the way to the service and the
owner's check that the host is open, so a pool keyed by host would have
passed a link its owner closed.

The owner approved the review card in the conversation on **2026-11-17**,
all three points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | Every request passes the owner's check before a byte goes to a service; the check (`Dial`) opens nothing, it says whether the host is open and where it goes, and only then a kept or new connection is taken. | A pool by host that skips the check for a kept connection | Low |
| 2 | trust | Kept connections belong to the target (here: port and TLS; on a node: lease, port, TLS), never to the host name; one that no open link leads to idles until the service closes it, unreachable without passing the check. | Dropping a link's connections when it closes (an event per close, protecting nothing point 1 does not) | Low |
| 3 | concept | The rules of #556: up to 32 idle per target, given back at the end of the answer; upgrades and streams that do not end keep their own; on a node the lane daemon's tunnel is reused with its connection. | Keeping the TCP connection and opening the tunnel each time | Low |

As built: `Dial` returns a `Way` (`key`, `port`, `tls`, and `open`, the
future that connects: TCP here, the lane's tunnel on a node) and is called
for every request. The router keeps a `connpool::Pool` by `Way::key`, sends
on a kept connection or opens one along `open` (TLS on top where the
service speaks it), resends a request a kept connection handed back unsent
on a new one, and gives a connection back through `GiveBack`. The hub's
`route` and the node's registered-host and lease check stay where they
were; the node's look-up of the lane's daemon moved into `open`. Tests:
`linkrouter::tests::the_router_reuses_its_connections_to_a_service` (20
requests, at most 2 connections; red with one per request), and in it a
request after the link closed is a `404` while a connection to the service
is kept and idle, which serves again once the link is opened again.
