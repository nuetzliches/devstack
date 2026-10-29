# ADR-0039: An admin can have a server node update its devstack, from the dashboard

- **Status:** Accepted (2026-10-19); amended 2026-10-20 (what the release check proves; which binary is replaced; a drain ends, and can be withdrawn)
- **Date:** 2026-10-19
- **Deciders:** project owner
- **Related:** [ADR-0031](0031-a-coordinator-places-lanes.md) (point 8: versions in a pool), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (extended here), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (point 4: dedicated nodes), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md)

## Context

Since ADR-0031 point 8 every node reports its devstack version, and the pool
area shows it. A pool where the node's version differs from the members' gets
a warning on each lane. Another protocol gets no lanes at all. The way out is
"update the older one", and for a server that means someone with SSH access to
it running `devstack update` and restarting the agent, which nothing does for
them: there is no service unit, `devstack update` restarts only the hub, and a
running `devstack node agent` keeps the old binary.

The admin who sees the warning in the dashboard is usually not the one with
the shell. The coordinator never calls a node. Its one channel to a node is
the answer to the node's report every 15 seconds, which already carries
"end this lease" (ADR-0032 point 4).

## Decision

1. **An admin orders an update of one node** (`POST /v1/admin/nodes/{id}/update`,
   optionally a version, else the latest release). The coordinator accepts it
   only when all of these hold:
   - the caller is an admin;
   - the node is a **server** (granted as a node, its agent started with
     `--dedicated`, ADR-0025 point 4);
   - it is online.

   The coordinator records the order in the audit log (ADR-0032 point 5).
   Developer machines are never updated this way: their owner runs
   `devstack update`, and the dashboard tells them so.
2. **The order travels as an answer to the node's report**: `update: {version,
   by, at}`, beside `end`. The coordinator never connects to a node, and the
   node opens no new port. An older agent ignores the field.
3. **The node decides** (ADR-0033):
   - it refuses when its policy says `remote_update = false`;
   - it refuses while it holds leases, unless the order says `drain`: then it
     takes no new lease and updates once the last one ended;
   - it refuses when it runs in a container: that update is a new image, and
     the agent says so.
4. **The update is the release, checked as `devstack update` checks it**: the
   GitHub release asset for its platform, against `SHA256SUMS`. A URL or a
   binary from the coordinator is never run. A dev build or a `cargo install`
   copy refuses, as `devstack update` does.
5. **The agent restarts itself.** Under a service manager it exits with a
   code that asks to be restarted (`devstack node agent install` writes a
   systemd unit with `Restart=always`). Started by hand, it re-executes itself
   with the same arguments. The next report carries the new version, and that
   ends the order.
6. **Progress and failure are reported**: `Report.update: {target, state,
   error?}`. The pool area shows them in the node's version cell, beside an
   **Update** button that only an admin sees, and only for a server that is
   online. The button is highlighted when the node is older than the hub. The
   dashboard only hides the button; the coordinator decides (ADR-0032
   point 1).

## Alternatives considered

- **Only show the command** (`ssh <node> devstack update`, copied). No new
  right, but the warning stays until someone with the shell has time. That
  is what happens now.
- **The coordinator calls the node.** It would need a client certificate and
  a route into every node, and so a new trust direction (ADR-0027). The
  report channel already exists and works through NAT.
- **Updates for every node, developer machines too.** A developer machine's
  devstack is its owner's: it runs their hub and stacks. An admin of a pool
  is not an admin of a colleague's machine.
- **Automatic updates on a server** (it follows the latest release by itself).
  It is simpler, but a pool then changes its version without anyone deciding
  it, and members get warnings at random.

## Consequences

- The admin role gains a right over servers: replacing the code they run,
  limited to signed releases. A stolen admin certificate can force a
  server to a release it did not run, an older one included. A node may
  refuse versions older than its own unless the order says `downgrade`.
- `devstack node agent install` (a systemd unit) has to be built. Without
  it, re-executing loses a supervisor the operator set up by other means.
- Leases on the node end if the order says to update without draining.
  Lanes on it then restart cold on their requesters' next `up` (ADR-0025
  point 7). That is why `drain` is the default in the dashboard.
- The coordinator's own update is not covered.

## Enforced by

*Paths as built, noted 2026-10-19:* the order and the checks in
`crates/devstack-node/src/coordinator.rs` (`admin_update`, `orderable`,
`ReportAnswer.update`, `Report.update`); the node's part in
`crates/devstack-node/src/node.rs` (`carry_out`, `restart_on`, admission
refused while draining) with `NodeConfig::on_update`; the release in
`crates/devstack/src/update.rs` (`for_node`); `remote_update` in
`crates/devstack-node/src/policy.rs`, read from the node's own policy only;
the hub's `POST /hub/pool/nodes/{id}/update`; the button and the state in
`ui/src/views/PoolView.vue`. The unit is printed, not installed: `devstack
node agent ... --print-unit` (installing it needs root). Not built: the CLI
for an admin (`devstack node members update`), which needs the running
coordinator rather than its store.

Where it will live:

- `crates/devstack-node/src/coordinator.rs`: the admin route, the checks, the
  order in `ReportAnswer`, the audit entry.
- `crates/devstack-node/src/node.rs`: acting on the order (policy, leases,
  container), `Report.update`, re-exec or the exit code.
- `crates/devstack/src/update.rs`: the download and check, shared with the
  agent (no hub restart there).
- `crates/devstack-node/src/policy.rs`: `remote_update`.
- `crates/devstack/src/poolcmd.rs`: `devstack node agent install`,
  `devstack node members update <id> [--version] [--drain]`.
- `crates/devstack-hub/src/pool.rs`: `POST /hub/pool/nodes/{id}/update`.
- `ui/src/views/PoolView.vue`, `ui/src/stores/pool.ts`: the button and the
  state.
- `docs/pool.md`, `docs/dashboard-api.md`.

*Paths as built, noted 2026-10-29.* The pool area moved from `ui/src/views/PoolView.vue` to `ui/src/views/pool/` (ADR-0076): the machine's page (`PoolMachine.vue`) holds what a node row held, the Admin tab (`PoolAdmin.vue`) the admin's lists, the Lanes tab (`PoolLanes.vue`) the lanes shared with this machine.

## What this ADR does not say

- That nodes update without an order. They do not.
- How the coordinator is updated. Its host is still operated by hand.

*Noted 2026-10-20.* A node that would refuse an order says so before one
is given: its report's `no_update` names why (its own policy, an agent in a
container, no way to install), and the dashboard disables **Update** with
that reason. A refusal is logged (`node_update_refused`).

## Addendum 2026-10-20: what the release check proves, and which binary

- **"Limited to signed releases" (Consequences) is not what was built.** The
  node takes a release of this repository from GitHub and compares the
  archive with the `SHA256SUMS` of the same release (point 4). That proves
  the download is the file the release lists, not who built it: the chain of
  trust is GitHub's TLS and control of the repository. No key is compiled
  into the binary, and the agent does not check the release's build
  provenance (the installers do, with `gh`). A signature the binary checks
  (a key in devstack, a signature file in each release) would close that; it
  is not built. SECURITY.md says the same.
- **The binary replaced is the one the agent runs** (`current_exe`),
  wherever it lives: a service unit starts that path again. `devstack
  update` on a developer machine still replaces the installers' copy.
- **`devstack node agent install` is not built** (Consequences): the unit is
  printed with `--print-unit`, and the operator installs it.

## Addendum 2026-10-20: a drain ends, and can be withdrawn

Point 3's drain had no end: a node whose lanes never stopped took no lane
again, and the admin could not take the order back. As amended:

- A node waits at most eight hours for its lanes (`DRAIN_FOR`). Then the
  update fails (`failed`, with why) and the node takes lanes again; the
  admin orders it again, or without draining.
- An admin withdraws an order the node has not begun to install
  (`DELETE /v1/admin/nodes/{id}/update`, the hub's
  `DELETE /hub/pool/nodes/{id}/update`, **Cancel update** in the pool area,
  logged as `update cancelled`). The coordinator no longer sends it; a
  draining node whose report is answered without its order stops waiting,
  reports `cancelled` and takes lanes again. A coordinator that restarted
  forgot its orders (they are kept in memory), with the same effect.
  `409` once the node installs.
- Point 5, as built: when starting the new binary fails, the agent goes on
  with the release it runs and reports `failed` (it no longer exits). There
  is still no rollback of the installed binary.
