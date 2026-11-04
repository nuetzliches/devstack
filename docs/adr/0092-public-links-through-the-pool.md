# ADR-0092: A pool can give its members public links below a zone of its own; the coordinator holds the token, each machine gets its own tunnel

- **Status:** Accepted (card in the pull request); points 1 to 5 built; point 6 built for links carried by the owner's machine, the node's own connector and point 7 not yet
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0091](0091-public-links-through-the-users-own-named-tunnel.md) (the user's own named tunnel, which a pool's works like), [ADR-0090](0090-a-service-can-get-a-public-link-by-hand.md) (its points 1 to 3 hold), [ADR-0051](0051-a-pool-can-grow-by-nodes-a-provisioner-makes.md) (the provisioner's token, kept the same way), [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (option B: a pool offers, a member accepts), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (a node's policy), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A named tunnel of one's own (ADR-0091) needs a Cloudflare account and a
domain per person. A team wants one domain, a login in front of every
link, and an admin who sees what is public. One tunnel cannot be shared:
two connectors of it share its requests at random, so every machine needs
a tunnel of its own, and a wildcard can point at one tunnel only, so each
link needs a DNS record. Something with a token has to make both.

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
seven:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | The Cloudflare API token (Tunnel: Edit, DNS: Edit of one zone, Access: Read) is only on the coordinator's host (a file or `DEVSTACK_CLOUDFLARE_TOKEN`, as the provisioner's); no route takes or hands it out. Wrong, an attacker with the coordinator controls that zone's DNS; so a zone of its own. | Entering it in the dashboard; a token on each member | Medium |
| 2 | trust | The coordinator gives each machine a tunnel of its own and only its credentials, and makes and removes a DNS record per open link, only for hosts with that member's suffix. Wrong, a member takes over another's addresses. | One shared team tunnel; a wildcard per member | High |
| 3 | trust | A member uses the pool's tunnel only after accepting it in the dashboard (its links' traffic then passes the pool's Cloudflare account); its own tunnel (ADR-0091) goes first. | Using it unasked | Low |
| 4 | contract | The host is `{service}-{lane}-{stack}--{member}.{domain}`, one level below the zone; the coordinator gives the member part and keeps it unique. | Two levels (a paid certificate); the node instead of the member | High |
| 5 | term | An admin switch **Public links in the pool: off / only with Access / open** (default *only with Access*, checked through Access: Read for `*.{domain}`), and a tag a member needs; the admin sees the open links and can close one (its record goes). | No switch; devstack making Access itself | Medium |
| 6 | concept | A lane on a node gets a public link, carried by its owner's hub (its ports are forwarded there); with the node's policy allowing it, by the node's own connector instead, also while the owner's machine is off. | Only through the owner; only through the node | Medium |
| 7 | trust | A node carries public links only if its own policy allows it (a new switch in ADR-0033, off by default). Wrong, a node operator's machine serves the internet without their consent. | The pool deciding for the node | Low |

Decided while building, not on the card: the token also needs *Zone: Read*
unless the settings name the zone's and the account's ids; tunnels are
made "locally managed", with a secret the coordinator makes and keeps in
`public-links.json` (readable by its user only); the suffix is the
member's name as a label, cut at 20, with four characters of its id when
another member has it; a hub reports its hosts whenever they change and
every minute, the coordinator removes what is no longer reported and all
of a machine's after five minutes without a report or when it leaves the
pool; a closed host stays refused to its member until the member stops
reporting it; a record of the same name left from before is taken over;
a changed domain removes the old domain's records and forgets the
tunnels; `devstack tunnel --pool <name>` accepts from the CLI; the
member's acceptance names what the pool's admins and account can see.

As built (points 1 to 5):

1. **The coordinator** (`publiclinks.rs` in `devstack-node`) answers
   `GET /v1/public-links` (the offer), `POST /v1/public-links/tunnel` (the
   machine's tunnel, audited), `PUT /v1/public-links/hosts`, and for
   admins `GET`/`POST /v1/admin/public-links` and `POST
   /v1/admin/public-links/close` (audited). `DEVSTACK_CLOUDFLARE_API`
   points it at another API (tests).
2. **The member's hub** takes the tunnel on `PUT /hub/public-links/pool`
   into the keychain and `named-tunnel.json` (with `pool` and `suffix`),
   reports its hosts, and routes only those the coordinator gave a record;
   a refused host fails the daemon's link with the coordinator's reason.
3. **The dashboard**'s *Public links* card in the pool area: the offer and
   *Use for my public links* for every member; the settings, the token's
   presence, Access and the open links with *Close* for admins.

As built (point 6, through the owner's machine): the owner's daemon passes
a public-link request for a lane on a node on only with a named tunnel set
up here and its hub answering, with the tunnel's domain and suffix in
`x-devstack-public-link-tunnel`; the lane's daemon names the host, opens
the link and runs nothing; the owner's hub, which follows the lane's
events, routes the host to the lane's ports forwarded to it (and in a
pool's tunnel reports it). A Quick Tunnel is refused for a lane on a node:
it would leave from the node.

## Consequences

- A pool's admins see every member's open links, and whoever holds the
  pool's Cloudflare account could read what passes them; a member accepts
  that by taking the tunnel.
- A coordinator that is down makes no new records; links already open keep
  theirs until it sweeps again.
- Cloudflare keeps the tunnel of a member that left (it can be deleted
  once its connections are gone); its records are removed.

## Enforced by

`crates/devstack-node/src/publiclinks.rs` (points 1, 2, 4, 5; its tests
against a fake Cloudflare API), `crates/devstack-node/src/coordinator.rs`
(the routes, the audit, the sweep, a removed member),
`crates/devstack-hub/src/publiclinks.rs` (point 3: acceptance, the report,
routing only what has a record), `crates/devstack-hub/src/pool.rs` (the
admin's calls), `crates/devstack-daemon/src/namedtunnel.rs` (the host with
the suffix), `ui/src/components/PoolPublicLinks.vue`, `docs/pool.md`
("Public links through the pool").
