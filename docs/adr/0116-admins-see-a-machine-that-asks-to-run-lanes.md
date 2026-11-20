# ADR-0116: Admins see a machine that asks to run lanes, and are told when one starts

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-20
- **Deciders:** project owner
- **Related:** [ADR-0113](0113-the-share-switch-runs-the-node-agent.md) (the share switch), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (the pool area, what only admins see), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (desktop notifications, amended by this one), [ADR-0073](0073-a-pool-tells-its-members-what-changed-over-one-stream.md) (the pool's stream), [#599](https://github.com/nuetzliches/devstack/issues/599)

## Context

A member that switches sharing on (ADR-0113) runs a node agent that reports
to the coordinator, but the pool runs lanes there only once an admin granted
it (`devstack node members node <id> on`, "Runs the pool's lanes" on the
machine's page). The coordinator kept such a report and placed nothing, and
the admins saw nothing: the Admin tab listed join requests only, no
notification, no mark among the machines. An admin had to be told by other
means. On 2026-11-19 a member's Windows machine waited this way, and a
second member entry of the same machine, from an earlier setup by hand in
WSL (`<name>-wsl`), already had the grant, which made it easy to grant the
wrong one.

## Decision

The owner approved the review card in the conversation on **2026-11-20**,
points 1 to 5:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | term | A machine that reports with sharing on and has no grant shows under Admin as **"Asks to run lanes"** next to the join requests: its name, its offer (CPU, memory, GPUs), its last report and a "Grant" button. | The admin learns of it only when told | Low: display only |
| 2 | contract | The dashboard derives "asks" from what the pool overview has (`report.sharing && !member.node`), with no new field in the API. | A new field in the coordinator | Low |
| 3 | concept | Such machines count in the pool's badge and the Admin tab's, and are marked "asks to run lanes" in Machines. | A badge that counts join requests only | Low |
| 4 | trust | An admin's hub notifies the desktop (a new kind, `machine_asks`, switchable like the others) when a machine starts asking. | No notification | Medium: a kind the pool's stream triggers |
| 5 | term | Where another member has the same address or a name that is the first one's with a suffix (`<name>-wsl`), the request says "same machine as X?" and whether X may run lanes already. | Granting the wrong entry | Low; a heuristic |

As built:

1. **Who asks** (`ui/src/lib/asks.ts`, `asking`): an approved member
   without `node` whose node is online and reports `sharing`. The same rule
   in the hub (`crates/devstack-hub/src/asks.rs`, `of`), where only an
   admin of the pool gets any.
2. **The Admin tab** (`PoolAdmin.vue`): the section shows only while a
   machine asks; "Grant" is the machine page's `setNode(id, name, true)`.
   The offer is the report's CPU, memory and GPUs (`sharedText`).
3. **Badges and Machines** (`stores/pool.ts` `due`, `PoolMachines.vue`):
   asking machines add to `admin`, which the pool tab and the sidebar's
   "Pools" count; the row's Role column shows "asks to run lanes" (warn)
   where "runs lanes" would stand.
4. **The notification** (`asks.rs`, `share::asks`, `poolfeed.rs`,
   `notify` `machine_asks`): read when the pool's stream says its members
   or nodes changed, as the guest lanes are; a pool's first reading is the
   baseline; a machine is told once each time it starts asking (granted or
   switched off, then asking again, is told again). A click opens the pool
   area.
5. **Same machine** (`asks.ts`, `sameMachine`): another approved member with
   the same `address` (admins see it), or the same name once a suffix such
   as `-wsl`, `-ubuntu…`, `-linux` is taken off.

Decided while building, not on the card:

- "Last report" instead of "since when it asks": the coordinator keeps no
  time of a member's first report while it has no grant.
- The hub reads the member list for this only where the machine's own
  membership is approved; it asks the coordinator only on a change of
  members or nodes, never on a timer.
- An offline machine does not ask: its last report may be days old.

## Alternatives considered

- **A grant request the agent sends** (a new endpoint, a state "asks"): the
  coordinator has what it needs in the report already; a new state would
  be one more thing to keep in step.
- **Granting with the join approval** (approve as node): the owner decides
  about sharing later, on their machine; the approval is about membership.

## Consequences

- An admin sees and grants a waiting machine from the Admin tab; nobody
  needs to be told by other means.
- The same-machine hint is a guess: two machines behind one NAT share an
  address. It only says "same machine as X?", it changes nothing.
- Verified by unit tests (who asks, the hub's baseline and once-per-start,
  the same-machine hint) and in the mock dashboard (Admin tab, badges,
  Machines, Grant).
