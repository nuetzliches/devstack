# Architecture Decision Records

devstack is developed ADR-driven ([ADR-0001](0001-development-is-adr-driven.md)):
an architectural decision is written down, with the alternatives it beat,
**before** code depends on it.

## What belongs here

An ADR records a constraint that

1. is enforced in more than one place, so no single file explains it,
2. has a plausible alternative someone will propose again, and
3. is expensive to reverse (code, published contract, user workflow).

Reasoning that fails this test goes into a comment at the decision site or the
[changelog](../../CHANGELOG.md).

## Status values

| Status | Meaning |
|---|---|
| `Proposed` | Written, not yet agreed. Code may be spiked against it, not merged. |
| `Accepted` | Agreed. Code builds on it. |
| `Accepted, provisional` | Agreed for now, with a named point at which it is confirmed or superseded. Code builds on it; until that point it may change without a migration path. |
| `Open` | A recorded question with the options laid out. **Nothing builds on either side** until it moves to `Proposed`/`Accepted`. |
| `Superseded by ADR-NNNN` | Reversed. The file stays; its outcome is never edited. |
| `Decided by ADR-NNNN` | An `Open` question that a later ADR answered. The file stays as the record of the options. |

An ADR describes a decision at the moment it was taken. To change the outcome,
write a new ADR and supersede the old one.

A refinement that keeps the outcome (a detail found while building it, a
point another ADR replaced) is an **amendment**: a dated paragraph
(`*Amended YYYY-MM-DD.*`) or section (`## Addendum YYYY-MM-DD: …`) added to the
file, the original text left as it was, and named in the status line and in
the index below. A later ADR that replaces or details a point of an earlier one
is linked from the earlier one too (its status or `Related` line).

## Numbering and format

Four digits, monotonic, never reused. Copy [`0000-template.md`](0000-template.md),
keep it to about a page. `Enforced by` lists every place the constraint lives;
before code exists it names where it *will* live. Once it is built, or the code
moves, a dated note under `Enforced by` (`*Paths as built, noted
YYYY-MM-DD.*`) names the real paths and what is not built; the original lines
stay.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](0001-development-is-adr-driven.md) | Development is ADR-driven | Accepted |
| [0002](0002-processes-run-from-immutable-artifacts.md) | Processes run from immutable artifacts, never from the build output of the source tree | Accepted, amended (services without `build`; a build whose inputs changed meanwhile, checked before it is published) |
| [0003](0003-changes-flow-through-candidates.md) | Changes flow through candidates; the main slot only holds a stack that built and passed health | Accepted |
| [0004](0004-infrastructure-is-delegated-to-compose.md) | Infrastructure comes from the project's existing Compose files plus a dev overlay | Accepted |
| [0005](0005-rust-for-daemon-and-cli.md) | Rust for daemon and CLI | Accepted |
| [0006](0006-dashboard-vue-same-origin.md) | The dashboard is a Vue 3 SPA served same-origin by the daemon | Accepted; who serves it replaced by ADR-0021 (the hub) |
| [0007](0007-one-http-api-is-the-control-surface.md) | One HTTP API is the control surface; CLI, MCP and dashboard are its clients | Accepted |
| [0008](0008-background-workers-run-only-in-the-active-slot.md) | Background workers run only in the active slot | Accepted |
| [0009](0009-the-daemon-owns-every-process-tree.md) | The daemon owns every process tree it starts | Accepted; what a service starts outside its tree is ended by its `stop` command, ADR-0059 |
| [0010](0010-promotion-mechanism.md) | Promotion mechanism: restart on the fixed port or switch a reverse proxy | Accepted (option B, the reverse proxy; measured) |
| [0011](0011-platform-windows-native-first.md) | Windows-native first; WSL is decided by measurement | Accepted; macOS added as a preview by ADR-0054 |
| [0012](0012-shared-infrastructure-between-lanes.md) | Which infrastructure may be shared between lanes | Decided by ADR-0030 |
| [0013](0013-schema-changes-hold-promotion.md) | A build that changes schema inputs is not promoted automatically | Accepted, amended (how long a candidate stays held) |
| [0014](0014-native-services-take-their-configuration-from-compose.md) | Native services take their configuration from the Compose service they replace | Accepted |
| [0015](0015-devstack-is-optional.md) | devstack accelerates a stack, it is never required to run one | Accepted |
| [0016](0016-a-daemon-restart-is-a-clean-restart.md) | A daemon restart is a clean restart from the last promoted artifacts | Accepted |
| [0017](0017-stack-file-is-toml.md) | The stack file is TOML, without profiles in version 1 | Accepted, amended 2026-10-20 (ADR-0041's profiles are not the environments ruled out) |
| [0018](0018-dashboard-is-a-browser-page.md) | The dashboard is a browser page; native integration is a tray in the daemon, not a desktop wrapper | Accepted; who serves it amended by ADR-0021; notifications from the hub by ADR-0045; the tray a client process the hub starts by ADR-0060 |
| [0019](0019-control-api-security.md) | The control API trusts the user's own processes, never a web page | Accepted, amended (own page for reads too; the hub proves itself; IPv4 loopback and `DEVSTACK_LISTEN`; the cookie is the hub's, no `token rotate`) |
| [0020](0020-builds-share-one-long-lived-build-job.md) | Builds share one long-lived build job; a cancel ends only the cancelled build's tree | Accepted |
| [0021](0021-a-hub-serves-the-dashboard.md) | A hub on a fixed port serves the dashboard for every stack | Accepted, amended (streams over WebSocket; the pool area polls); the tray is the hub's client, ADR-0060 |
| [0022](0022-network-egress-sandbox.md) | Outbound network calls of supervised services are visible and need an allow rule | Proposed |
| [0023](0023-stack-file-compatibility.md) | Stack files stay readable; migrations once devstack has outside users | Accepted |
| [0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) | A lane is a supervisor in the main working tree's daemon | Accepted, amended twice (lanes are the worktrees, run from a copy); point 4 replaced by ADR-0030; point 1 replaced in part for the hub by ADR-0047; point 7 amended 2026-10-21 (the removal is shown while it runs); point 6 amended by ADR-0052 (a restart carries the lanes over) |
| [0025](0025-a-lane-can-run-on-another-node.md) | A lane can run on another node, as a Linux container with its own infrastructure | Accepted; point 10 (showing and moving a lane) amended; amended 2026-10-19 (a start on a node answers once leased; its phases are timed); noted 2026-10-21 (its daemon starts first, its containers come up under it); points 5 and 7 amended by ADR-0038; point 7 amended by ADR-0052 (a restart keeps the lanes on nodes) |
| [0026](0026-placement-by-the-requester-admission-by-the-node.md) | The requester places a lane; the node admits it, under a lease | Accepted; points 1, 3 replaced by ADR-0031 |
| [0027](0027-trust-between-nodes.md) | Nodes and requesters trust each other by pinned keys, never by network location | Accepted; points 1, 2 replaced by ADR-0031; point 5 amended |
| [0028](0028-lane-images.md) | A remote lane runs in a stack-provided toolchain image; the node supplies devstack | Accepted, amended (a Dockerfile the node builds); point 3 noted 2026-10-26 (`prepare`'s results are in ADR-0062's store) |
| [0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) | A remote lane works on a mirror of the requester's tree; ports and API come back over one connection | Accepted; point 2 amended (`[lanes] sync`); Consequences built by ADR-0038; point 4 extended to UDP by ADR-0056 |
| [0030](0030-each-lane-has-its-own-infrastructure.md) | Each lane has its own infrastructure | Accepted; point 2 detailed by ADR-0057 (an announced port moves inside the container too) |
| [0031](0031-a-coordinator-places-lanes.md) | A coordinator places lanes and admits members; nodes admit leases; data goes directly | Accepted, amended (one binary: `devstack node`), amended 2026-10-19 (versions in the pool), amended 2026-10-21 (no share switch) |
| [0032](0032-the-pool-is-administered-in-the-dashboard.md) | The pool is administered in the dashboard, by members with the admin role | Accepted, amended (admins grant nodes); extended by ADR-0039 (updating a server) |
| [0033](0033-a-node-decides-what-runs-on-it.md) | A node decides who runs lanes on it, and what a lane there may do | Accepted, amended (what Compose files may ask; limits; the node's own policy; amended 2026-10-20: the node runs the model it checked, known keys only, a lane's projects end with it; noted 2026-10-21: what holds while a lane's containers start under its running daemon) |
| [0034](0034-a-machine-can-belong-to-several-pools.md) | A machine can belong to several pools; pools do not trust each other | Accepted; noted 2026-10-20 (several agents on one Docker engine) |
| [0035](0035-lanes-can-be-shared-with-the-pool.md) | The pool sees the lanes its members share, and can use them | Accepted; point 4 extended by ADR-0040 (sharing for viewing; since 2026-10-20 sharing includes viewing); point 3 amended by ADR-0053 (a member connects to a lane and disconnects) |
| [0036](0036-the-pool-shows-its-capacity-over-time.md) | The pool shows its capacity, GPUs included, and how it changed | Accepted |
| [0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md) | Telemetry goes to the pool's collector, if the pool has one and the member agrees | Accepted |
| [0038](0038-a-lanes-data-outlives-its-lease.md) | A lane's mirror and data outlive its lease on a node, for a while, and can be pruned | Accepted; noted 2026-10-26 (a member's store, ADR-0062, counts towards the cap) |
| [0039](0039-an-admin-can-update-a-server-node.md) | An admin can have a server node update its devstack, from the dashboard | Accepted, amended 2026-10-20 (a checksum from the release, not a signature; the running binary is replaced; a drain ends, and can be withdrawn) |
| [0040](0040-a-shared-lane-can-be-watched-like-ones-own.md) | A shared lane can be shared for viewing, and is then shown as one's own lane is | Accepted; amended 2026-10-20 (sharing a lane includes viewing it) |
| [0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) | A profile says how a lane (or the main tree) runs for a purpose, chosen when it starts | Accepted; point 1 extended by ADR-0058 (`skip`) |
| [0042](0042-a-dev-servers-rebuilds-show-like-builds.md) | A dev server's rebuilds show like builds, read from its output | Accepted |
| [0043](0043-a-dev-servers-fixed-port-is-devstacks.md) | A dev server's fixed port is devstack's, forwarded to its instance | Accepted |
| [0044](0044-a-lanes-prepared-directories-can-be-linked-into-its-worktree.md) | A lane's prepared directories can be linked into its worktree | Accepted; point 5 replaced for lanes on a node by ADR-0063; `link` ⊆ `outputs` by ADR-0062 |
| [0045](0045-the-hub-notifies-the-desktop.md) | The hub notifies the desktop of what needs attention, without a tray | Accepted, amended 2026-10-21 (`guest_lane`) |
| [0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) | The hub opens a stack's worktree in the user's own programs, from a fixed list | Accepted, amended 2026-10-21 (the program opens in front on Windows; GitHub Desktop), 2026-10-22 (an agent started with a prompt the hub writes) |
| [0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) | The dashboard makes a lane from a branch; the main tree's checkout stays the user's | Accepted; a branch without a stack file by ADR-0055 |
| [0048](0048-a-containers-main-port-can-move-with-the-main-slot.md) | A container's main port can be devstack's, and then moves with the main slot | Accepted; point 2 detailed by ADR-0057 (an announced port is not held) |
| [0049](0049-a-lanes-need-is-measured-disk-included.md) | A lane's need is measured where it ran, disk included; the stack file sets a floor | Accepted, amended 2026-10-21 (point 9: what a lane here checks) |
| [0050](0050-what-no-request-reports-reaches-the-dashboard-as-a-notice.md) | What no request reports reaches the dashboard as a notice, not as a log view | Accepted |
| [0051](0051-a-pool-can-grow-by-nodes-a-provisioner-makes.md) | A pool can grow by nodes a provisioner makes for one member's lane, and gives back when it is removed | Proposed |
| [0052](0052-a-restart-carries-the-lanes-over.md) | A restart of the stack carries its lanes over to the next daemon | Accepted; point 6 added 2026-10-26 (a lane on a node lost to an unclean end is said) |
| [0053](0053-whoever-a-lane-is-shared-with-connects-to-it.md) | Whoever a lane is shared with connects to it, and disconnects again | Accepted |
| [0054](0054-macos-is-a-developer-platform-in-preview.md) | macOS is a developer platform in preview, installed by Homebrew, checked by CI and by its testers | Accepted |
| [0055](0055-a-lane-without-a-stack-file-says-so-and-can-be-given-one.md) | A lane whose branch has no stack file runs with the main tree's, says so, and can be given one | Accepted |
| [0056](0056-a-lane-on-a-node-relays-its-udp-ports-through-a-tunnel-per-session.md) | A lane on a node relays its UDP ports, a tunnel per client, a datagram per frame | Accepted |
| [0057](0057-a-port-a-container-announces-moves-inside-it-too.md) | A port a container announces moves inside the container too, and Compose is told its number | Accepted |
| [0058](0058-a-profile-can-leave-out-containers-and-services.md) | A profile can leave out containers, whole Compose groups and services | Accepted |
| [0059](0059-a-services-stop-command-ends-what-its-run-started.md) | A service's `stop` command ends what its `run` started outside its process tree | Accepted |
| [0060](0060-the-tray-is-a-client-of-the-hub.md) | The tray is a client of the hub, a process of its own, on every desktop | Accepted |
| [0061](0061-the-dashboard-opens-in-its-installed-app.md) | The dashboard opens in its installed app, when there is one | Accepted |
| [0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md) | What `prepare` makes is kept in a store per machine and repository, keyed by its inputs | Accepted |
| [0063](0063-a-lane-on-a-node-links-its-worktree-to-a-copy-from-the-local-store.md) | A lane on a node links its worktree to a copy from the local store | Accepted |
| [0064](0064-every-compose-call-is-told-the-lane-it-runs-for.md) | Every Compose call is told the lane it runs for | Accepted; addendum 2026-10-26 proposed (a lane on a node: native `{lane}`, `{lane_suffix}`, `DEVSTACK_LANE` follow its name) |
