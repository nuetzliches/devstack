# ADR-0091: Public links can go through the user's own named tunnel, at stable addresses; the hub runs its one connector

- **Status:** Accepted (card in the pull request); built; its step 3, the pool, built by [ADR-0092](0092-public-links-through-the-pool.md)
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
