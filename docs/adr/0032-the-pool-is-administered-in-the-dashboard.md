# ADR-0032: The pool is administered in the dashboard, by members with the admin role

- **Status:** Accepted (2026-10-16), amended 2026-10-17 (admins grant which members are nodes)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0031](0031-a-coordinator-places-lanes.md) (point 2 extended here), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0019](0019-control-api-security.md), [ADR-0006](0006-dashboard-vue-same-origin.md)

## Context

ADR-0031 has an administrator approve a member on the coordinator itself
(`devstack node members approve <id>`, over SSH). That works, but it means the
person who admits a colleague needs shell access to the coordinator's host,
and the pool has a second place to look at besides the dashboard: who asked to
join, which machines are online, what they share, which lanes run where.
Once lanes run on nodes, there is more to manage there: take a node out of
placement for a while, end a lane someone forgot, remove a machine that left.

The coordinator already knows every caller: each request carries the client
certificate of an approved member (ADR-0031 point 2). What it does not know is
who may change the pool.

## Decision

1. **Members have a role:** `member` (the default) or `admin`. Only an admin
   may approve or remove members, change roles, and manage nodes and leases.
   The coordinator checks the role on every such request; the dashboard only
   hides what the role does not allow.
2. **The first admin is made on the coordinator** (`devstack node members
   approve <id> --admin`, or `devstack node members role <id> admin`, over
   SSH). From then on admins make admins in the dashboard. The coordinator
   refuses to demote or remove the last admin.
3. **Authentication is the member's certificate.** The dashboard never holds
   it: the browser talks to the hub (paired, same origin, ADR-0021), the hub
   calls the coordinator with this machine's certificate, and the coordinator
   decides by that member's role. An admin is therefore a machine, not a
   person; on a developer's own PC the two are the same.
4. **The dashboard has a pool area.** Every member sees the pool: members,
   online or not, what each shares and uses, which lane runs where, and this
   machine's share switch (ADR-0031 points 3 and 7). An admin also sees
   pending requests (with a badge where the pool shows in the header or
   sidebar) and can approve or reject them, remove members, change roles,
   pause a node (no new placements, running lanes stay), and end a lease.
5. **Every admin action is recorded** on the coordinator (who, what, when),
   and the pool area shows that log to admins.
6. **Approving shows what is approved:** the requesting machine's name, its
   member id, when it asked and from which address, so an admin can tell a
   colleague's request from someone else's before accepting it.

*Amended 2026-10-17.* **Admins grant which members run the pool's lanes.**
Until now every member whose node agent reported an API was a node, and
`--on pool` picked among them by what each said it had free: a member's
laptop, or a machine taken over, could claim a terabyte and receive every
placed lane with its worktree and its untracked environment files. The
requester who asks for `pool` has not chosen whom to trust (ADR-0027
point 6). So being a node is granted, like the admin role: an admin
grants it (`devstack node members node <id> on`, or the admin API), and
takes it back the same way; the change is in the admin log. The
coordinator takes a node's API from its report only when the member is
granted, places `--on pool` and named leases on granted members only, and
says to a node agent that is not granted why. A member's share switch
alone (the hub) never made it a node and still does not.

## Alternatives considered

- **SSH only (ADR-0031 as written).** Nothing to build, but admitting a
  colleague needs shell access to the coordinator's host, and the pool has no
  place in the dashboard where its state is changed.
- **Sign-in as a person.** An admin would be a person on any machine, and a
  stolen laptop would not be an admin by itself. Someone has to vouch for who
  that person is: an identity provider the team already has (any OpenID
  Connect provider, such as its Git server or its office suite), or passkeys
  registered on the coordinator itself. Either needs a login flow in the hub
  and tokens next to the certificates, for a pool of a handful of machines
  whose owners are their admins. Can come later on top: the role stays on the
  coordinator, only how a caller proves who they are would change.
- **Admin by a separate admin certificate.** A second certificate per admin
  machine for admin requests only. Distinguishes "this machine" from "this
  machine acting as admin", but the hub would hold both, so it protects
  against nothing the role does not.

## Consequences

- An admin machine's certificate is as sensitive as the coordinator's SSH
  access was. A lost admin machine is removed by another admin, or over SSH.
- The coordinator gets an admin API and an audit log; the hub a proxy for the
  pool; the dashboard a pool area that changes with the role.
- A pool always has an admin; the last one cannot leave without naming
  another.

## Enforced by

Will live in: `crates/devstack-node` (`store`: role, audit log;
`coordinator`: `/v1/admin/*`, role check, last-admin guard),
`crates/devstack/src/poolcmd.rs` (`--admin`, `members role`),
`crates/devstack-hub` (`/hub/pool/*`, calling the coordinator with this
machine's certificate), `ui/` (the pool area).

*Paths as built, noted 2026-10-18.* All of the above:
`crates/devstack-node/src/store.rs` (roles, `set_node`, the audit log
`audit.jsonl`, `members.json` changed under a file lock),
`crates/devstack-node/src/coordinator.rs` (`/v1/admin/*` including
`/v1/admin/members/{id}/node`, the role check, the last-admin guard),
`crates/devstack/src/poolcmd.rs`, `crates/devstack-hub/src/pool.rs`
(`/hub/pool/*`), `ui/src/views/PoolView.vue` and `ui/src/stores/pool.ts`;
tests in `crates/devstack-node/tests/pool.rs`. The node grant of the
amendment: `/v1/admin/members/{id}/node` on the coordinator,
`devstack node members node <id> on|off`, `POST /hub/pool/members/{id}/node`
and the "Allow lanes" toggle of the member list in the dashboard.

## What this ADR does not say

- How a node pauses or a lease ends on the node itself: that is the lifecycle
  of milestone M4 (ADR-0026, ADR-0031 point 6). This ADR says who may ask.
- Anything about local stacks: the dashboard's control of this machine's
  stacks stays as ADR-0019 has it.
