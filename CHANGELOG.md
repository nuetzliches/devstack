# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as far as a 0.x release can.
Releases before 0.10.0 are summarised in [plan.md §7.4](docs/plan.md#74-phase-2-status)
and listed with their assets on the
[releases page](https://github.com/nuetzliches/devstack/releases).

## [Unreleased]

### Changed

- **A lane started on a node opens its pages as soon as its daemon runs
  there** (#164, ADR-0025 noted 2026-10-21): the node creates the lane's
  containers, starts the lane container and answers, then brings the
  containers up under the running daemon, a Compose group at a time. The
  lane's page shows them created, starting and healthy, and its services
  start once their infrastructure is up, as on this machine. A container
  that does not get healthy within 300 s shows on the lane's page with its
  output and its group's error instead of failing the start.
- While a lane's containers come up on a node, its worktree syncs again;
  Compose builds nothing then (`--no-build`), since images are built before
  the lane runs (ADR-0033, noted 2026-10-21).
- `devstack node run-lane` waits for the lane's services past their
  infrastructure, and stops on a Compose group that failed.

### Added

- **Open a stack's folder from the dashboard**
  ([ADR-0046](docs/adr/0046-the-hub-opens-a-worktree-in-the-users-programs.md)):
  the path in the stack header is a menu. It copies the path, or opens the
  shown tree (the main working tree or a lane's worktree) in Explorer or the
  file manager, Windows Terminal, PowerShell or a terminal, VS Code, Cursor,
  Windsurf, Zed, Claude Desktop, Claude Code or Orca: whichever of them the
  hub finds on this machine (an editor or agent by the URL scheme it
  registered). The hub opens only a registered stack's own trees, with a
  program from that list (`GET /hub/openers`, `POST /hub/open`). A lane
  shared with you offers the copy only.
- **Desktop notifications** ([ADR-0045](docs/adr/0045-the-hub-notifies-the-desktop.md), #156):
  the hub follows every running stack and each of its started lanes (on a
  node too) and tells the desktop when a build failed, a candidate is held
  for `promote`, a service exited by itself or failed to start, or a lane's
  start or move on a node failed; on request also when a lane took the main
  ports or a candidate went live. Only changes notify, and the same thing for
  the same service once in five minutes (a crash loop is one notification).
  Windows shows a toast, Linux uses the desktop's notification service over
  D-Bus (the binary stays static; without a desktop session nothing shows).
  A click opens the dashboard on that stack, lane and service. Each kind on
  or off, and a mute per stack or lane, behind the bell in the dashboard's
  top bar or with `devstack hub notify [on|off <kind>…|test]`;
  `DEVSTACK_NO_NOTIFY` switches them off.
- **Lanes of other members on this machine are shown**: a banner below the
  dashboard's top bar (hidden until a lane comes or goes), a tile in the
  sidebar, and *On this machine* in the pool area (whose, repository,
  since when, what it reserved); a lane that starts running here notifies
  (`guest_lane`, on by default). The hub reads them from the pool's leases
  every 15 s and sends them on `/hub/events` (`guests`).
- A `devstack://open?stack=…&lane=…&service=…` link opens the dashboard
  there, paired; `/auth` takes `next`, a dashboard route, and keeps the
  session of a browser that is paired already.
- The node's pool telemetry reports each Compose group of a lane brought up
  or failed (`node_lane_infra_up`, `node_lane_infra_failed`, with its
  duration and Compose's reason).
- **`prepare.link`** (ADR-0044, #159): directories `prepare` installs in a
  lane's copy (`link = ["web/node_modules"]`) are linked into the lane's
  worktree, a junction on Windows, so git hooks there (`npx eslint` in a
  pre-commit hook) find them without a second `npm ci`. Only where the
  worktree has nothing of that name; only when git ignores the link (write
  `node_modules`, not `node_modules/`); removed when the lane stops. Stop the
  lane before `git worktree remove`, which on Windows trips over a junction.

### Fixed

- `--browser` / `DEVSTACK_BROWSER` on Windows: an address with `&` was cut
  there by `cmd` (none had one before `/auth?…&next=`).
- The dashboard, opened on a lane before it knew whether the stack runs (a
  link, a first visit), showed the lane without its list of lanes.
- The daemon log says why a lane whose worktree went is not removed at once:
  `lane_forget_deferred` said "it is being started" also when a removal of
  the same lane was under way already, or a move.
- Docs: a viewer of a shared lane reads its containers' logs, as it has
  since 0.11.0 (the node answers them itself); the docs and notes of 0.15.0
  said otherwise.

## [0.15.0] - 2026-10-21

A release of fixes from a review of the whole code base: the pool node runs
what it checked and nothing else, several node agents share a Docker engine,
the supervisor keeps edits and stops it lost before, and the stack file
keeps its paths in the repository. Some stack and Compose files that loaded
before are refused now (see Changed); devstack has no users outside its
maintainers yet, so there is no migration.

### Security

- **A node runs the Compose model it checked**, written beside the lane's
  mirror, and never reads the lane's files again: nothing the lane changes
  between the check and the start, or later, reaches Compose. Bind mounts of
  the mirror become `subpath` mounts of a volume bound to it (Docker refuses
  a link out of it at every container start); secret and config files are
  copied. A node takes no sync for a lane while it starts (`409`) and stops
  what of the lane still runs before it checks its files.
- Compose and Docker on a node run with only what Docker needs of the
  agent's environment; a service's or build's variable without a value stays
  unset, and secrets or configs `from environment` are refused.
- A node refuses Compose keys it does not know, and `use_api_socket`,
  `provider`, `oom_kill_disable`, a negative `oom_score_adj`, logging drivers
  that send elsewhere, privileged lifecycle hooks, non-GPU device
  reservations, `build.tags`, `build.cache_to` and a local
  `build.cache_from`; a build's secrets are checked like a service's.
- Two node agents on one Docker engine (one per pool) no longer remove each
  other's lane containers and network rules: each agent tags its containers,
  lane images and iptables chains (`devstack.agent=<tag>`,
  `ds-<tag>-<lease>-f/-i`) and sweeps only its own. Leftovers of an earlier
  agent (`dsl-<lease>` chains, untagged lane containers) are removed with
  their lease or by the next sweep. Update all agents on one engine together.
- Telemetry: paths on this machine no longer leave it inside error texts
  (the repository, a lane's worktree, devstack's state and the home directory
  go out as `<repo>`, `<worktree>`, `<devstack>`, `~`); the masking of
  `/users/` no longer touches URLs; a collector that answers `404` stops the
  sending at once.
- Releases come only from a tag on a commit of `main`. The installers check
  who built the archive (`gh attestation verify --signer-workflow
  …/release.yml`) and show gh's reason when it fails, so a network or login
  problem no longer reads like a tampered archive.

### Changed

- **Stack file paths stay in the repository** (ADR-0002): `cwd`, `output`,
  `prepare.cwd` and `hot_reload.cwd` are refused when absolute, on another
  drive or with `..` (`output` may start with `{repo}`); a build whose `cwd`
  or expanded `output` leaves its snapshot fails. Also refused now:
  `health.timeout = "0s"`, and `{port}`, `{artifact}` or `{is_main}` in a
  profile's `env` (commands get it too).
- A profile that changes a service's `kind` drops what only the old kind
  takes: a `kind = "static"` profile over a dev server with `rebuild` or
  `restart_on` (ADR-0041 over ADR-0042) made the whole stack file fail to
  load.
- `devstack build` no longer takes `--lane` (it always failed); a lane builds
  in its daemon (`devstack restart <service>` in its worktree).
- `devstack self uninstall` names the pool memberships it removes, refuses
  while the per-user state holds a pool coordinator unless
  `--with-coordinator`, and refuses while another devstack process of the
  user runs (a node agent, a coordinator, an MCP server).
- The static server caches only files whose name carries a content hash;
  every other file is sent with `no-cache` and an `ETag` (`304` while
  unchanged), so a promoted build shows at the next load.
- MCP `up`: `timeout_s` is one deadline for Docker, the daemon, the lane and
  its services.
- CI builds with `--locked` and checks the declared minimum Rust version.
- Docs: SECURITY.md says that a lane's uploads and data outlive its lease
  (ADR-0038), that releases are checked by checksum, not signature (ADR-0039,
  amended), and that the Windows binary is not code-signed. ADR-0017 is
  amended for ADR-0041's profiles. `docs/pool.md` says that
  images a node pulls are pulled with its credentials and can be run by any
  lane there.

### Added

- `devstack check` warns when one of the first three lanes would run a
  service on another service's port (`[lanes] port_step`).
- Pool: admins can withdraw a node update while it waits for its lanes
  (`DELETE /hub/pool/nodes/{id}/update`, **Cancel update** in the pool area);
  the node reports `cancelled`.

### Fixed

- **Pool node**
  - `devstack lane stop`, the end of a lease, purging a kept lane and
    removing a member remove the lane's infrastructure containers (and, on
    purge, its volumes) on the node; before, every group was skipped once it
    had started.
  - A `data = "fresh"` profile on a node no longer recreates the lane's
    networks without the node's flags and egress rules.
  - Admission no longer over-commits: a node checks and records a lease in
    one step, and the coordinator counts offered, not yet admitted leases
    against a node's capacity.
  - A node counts its kept lanes' volumes against a member's room, removes
    the images built for a lane with its kept data and lane images with
    their member or when disk runs short, and refuses a lane with less than
    5 GiB of disk free.
  - A node answers large syncs off its async workers, limits container log
    streams per member (16), asks the coordinator about unknown certificates
    at most every 5 s, and drops connections that send no request within
    30 s. Lease repository and lane names are bounded in length and
    characters; a `:` in a synced path is accepted on Linux nodes.
  - An update an admin orders gives up after eight hours of waiting for
    lanes; a node whose new release does not start keeps running the old
    one; the update replaces the binary the agent runs (the one its service
    unit starts again), not a copy in `~/.local/bin`.
- **Supervisor**
  - A first start after a daemon start that fails for a passing reason (a
    port still held, infra not reachable yet) no longer uses up the restore:
    the next start runs the build last promoted again, instead of building
    the tree into main past the promotion gate and the migrator.
  - An edit made while a service that exited by itself waits for its
    restart, or while it starts, is no longer lost: it follows as a
    candidate once the service runs again.
  - `devstack stop` while a candidate's migrator runs is final: the
    candidate no longer starts the stopped service afterwards; the same for
    a `restart` in flight.
  - "serving without background workers" stays in the status and the
    dashboard; the next health check no longer turns it into `healthy`.
  - Two services after the same job run it once for one change, not once
    per candidate.
  - A build ended by `devstack stop` is reported as stopped, not "superseded
    by a newer change". A build whose inputs changed while it ran is no
    longer published and then deleted.
  - After `devstack promote --lane main`, a service that does not run in the
    main tree answers "`x` is stopped" instead of "lane `y` is not
    answering".
  - A lane whose worktree is removed while the lane starts is forgotten
    after the start, not during it. On Windows, a lane's state directory
    that cannot be deleted at once is retried, logged, and swept at the next
    daemon start.
- **Builds and the stack file**
  - The build id follows a `${VAR}` in `build` and `output`: with another
    value devstack reused the old artifact. Ids of lines without `${VAR}`
    are unchanged.
  - A Docker call over its time budget ends with everything it started:
    before, only the `docker` CLI was ended and Compose's plugin kept
    running.
  - Publishing a build follows symbolic links and junctions
    (`node_modules/.bin`, pnpm layouts) instead of failing.
  - Env files: a byte order mark (Windows PowerShell 5.1) no longer hides
    the first variable; `${A:-${B}}`, `${VAR:+alt}` and `${VAR+alt}` work as
    in Compose. `{{port}}` in a dev server's `run` no longer counts as
    `{port}`.
- **CLI, daemon and dashboard**
  - `devstack down` against a daemon that refuses the token or hangs shows
    why instead of "no daemon is running"; `down --all` in the main tree
    ends it by its pid. The MCP tools pass that error on. `down` without
    `--all` leaves the daemon running for a lane started meanwhile.
  - A second `devstack up` while the first daemon is still starting waits
    for it instead of failing.
  - `devstack lane start --profile … --run` checks the profile's command
    before the lane starts.
  - `devstack node agent --print-unit` quotes paths with spaces, `%` and `$`
    for systemd; `--pool` is checked before it goes into a hub URL.
  - A lane on a node no longer hides a port that cannot be forwarded behind
    a sync that worked.
  - `self uninstall` no longer says "uninstalled" when the per-user state
    could not be removed; `logs --follow | head` no longer says the daemon
    ended the stream; `doctor` without a stack file warns about a missing
    Docker instead of failing.
  - A refused request whose JSON answer is larger than 64 KiB reaches the
    dashboard and the CLI whole; the request logs of the daemon and the hub
    no longer send an empty body for it.
  - The hub closes a dashboard WebSocket after two pings without an answer,
    so a connection that went away (a laptop that slept) no longer holds its
    daemon stream until the system's TCP timeout.
  - Opening a service of a lane shared with this machine with
    `?pool=default` and without `pool` uses one forward, not two.
  - A lane shared with this machine shows no control that would change it,
    the command palette included; before, the controls were only hidden by
    style rules.

## [0.14.1] - 2026-10-21

The release of 0.14.0 stopped at the advisory check: three advisories of
that day against the TLS certificate checks. 0.14.1 is 0.14.0 with them
fixed; 0.14.0 was tagged but never published.

### Security

- `rustls-webpki` 0.103.16 (RUSTSEC-2026-0382), `webpki-root-certs` and
  `webpki-roots` 1.0.10 (RUSTSEC-2026-0383 and its sibling): certificate
  paths through the distrusted emSign intermediates are refused. They
  check the TLS of the update and release checks (`ureq`); the pool's
  mutual TLS trusts its own CA only and was not affected.

## [0.14.0] - 2026-10-21

### Added

- **Where a lane starts without `--on` is the machine's choice**
  ([ADR-0025](docs/adr/0025-a-lane-can-run-on-another-node.md), amended).
  `devstack pool default-on pool|<pool>|<node>|local` (or the pool area)
  sets it; it is consent to send worktrees there without being asked, so it
  is never set by joining. `devstack up` in a worktree, `devstack lane
  start` and the MCP `up` and `lane_start` use it when no place is named;
  `--on local` keeps a lane here. Resolved by the client, so a daemon of an
  older release is asked as before.
- **`devstack pool placement <lane>` and the MCP tool `placement`**: before
  choosing where a lane runs, this machine's free CPU and memory and each
  node's load, free disk, lanes and the coordinator's rank and reason
  (computed from the nodes' reports when the coordinator is older).
- **The pool places a lane where it starts warm**
  ([ADR-0026](docs/adr/0026-placement-by-the-requester-admission-by-the-node.md)
  point 3, as decided and not built until now). With `--on pool` the
  coordinator prefers, with room to spare, a node that keeps this lane's
  data, then one the repository ran on (its images and builds are there),
  then the most free capacity for the lane's need (CPU and memory, not
  memory alone); a node with less than 5 GiB of disk free takes none. The
  lease says why (`remote_lane_placed`, the start's first step), and
  `GET /hub/pool/placement` gives the order with reasons. Nodes report
  their slots and free disk.
- **A lane's start on a node shows its phases**: while it has no pages yet
  (its daemon does not run on the node before its infrastructure is up),
  its start lists each phase over so far with its time (lease, sync, lane
  image, each Compose group's images, containers and health, the lane's
  daemon) and the current one with its time running. `moving` carries
  `phases` and `phase_since`.
- **Where to start a lane shows how busy each node is**: free CPU and
  memory, what its lanes use of what it has (with the last hour), its
  lanes, kept lanes and GPUs, and which node the pool would pick.

### Fixed

- A node's `node_lane_started` carries the start's whole time; it said `0`
  (the time since its last phase).
- A command run's line says how long it ran and, when it failed, why
  (`ms`, `error`); the pool's telemetry counts its time by command and
  result. Before, a failed run said only `failed`.

## [0.13.1] - 2026-10-20

### Fixed

- A service stopped while its health check ran no longer shows
  "unhealthy" in the dashboard: the check's result is dropped once the
  instance it checked is gone, and a service's events go out in the order
  its states were set (a stop's `stopped` could be overtaken by an
  `unhealthy` from just before it, and stayed on the page while the
  daemon knew it stopped).

## [0.13.0] - 2026-10-20

### Changed

- **A dev server's fixed port is devstack's**
  ([ADR-0043](docs/adr/0043-a-dev-servers-fixed-port-is-devstacks.md)). A
  dev server whose `run` takes `{port}` runs on a port of its own; devstack
  holds the fixed port and forwards it byte for byte (TLS, WebSockets, HMR
  unchanged). A restart (`restart_on`, the restart button) keeps the
  address: a request made meanwhile waits for the new instance instead of
  being refused; `promote --lane` switches the forward instead of binding
  the port anew; a stale process can block an instance port only. The view
  shows the fixed and the instance port, as for a proxied service.
  `proxy = false` keeps the dev server on the fixed port.

### Fixed

- A failed rebuild of a dev server is logged once (`devserver_rebuild` with
  `ms` and `result`); each error line that followed its marker logged it
  again, and the pool's telemetry counted it as often.

## [0.12.0] - 2026-10-20

### Added

- **A dev server's rebuilds show like builds**
  ([ADR-0042](docs/adr/0042-a-dev-servers-rebuilds-show-like-builds.md)).
  devstack reads a dev server's output (and a service's in hot-reload mode)
  for its rebuilds: the service row and page show "rebuilding", "rebuilt in
  11.9 s" or "rebuild failed" with the dev server's error lines, as a built
  service shows its candidate; a failed rebuild is no longer hidden behind
  "healthy". The tool is recognised from its lines (Angular CLI, Vite,
  webpack's dev server, Next.js, `dotnet watch`, `tsc --watch`); `rebuild`
  in the stack file names one, turns it off or gives a tool's own lines.
  Each finished rebuild is a `devserver_rebuild` line with `ms` and
  `result`, in `daemon.log` and the pool's telemetry.
- **A node's own lines reach the pool's telemetry** (ADR-0037 point 3): a
  lease admitted or refused (with the reason), a lane started on it (with
  its time) or not started (with the phase and the reason), stopped, its
  lease ended, an update refused or failed. Before, a node sent only its
  capacity, and why a lane did not start there was in nobody's log.
- A request a daemon or the hub refuses is logged with the first line of
  its `error`, in `daemon.log` and the telemetry alike: a `409` alone
  explained nothing afterwards.

### Changed

- **Breaking (CLI, API, MCP): sharing a lane includes viewing it**
  ([ADR-0040](docs/adr/0040-a-shared-lane-can-be-watched-like-ones-own.md),
  amended 2026-10-20). Whoever a lane is shared with opens its services and
  views it read only (state, logs, events, command runs, usage; never its
  environment). The separate grant is gone: `devstack lane share --view`,
  `view` in `POST /v1/lanes/{lane}/share` (ignored now), `view` on the MCP
  `lane_share` and `viewed_by` in `LaneView`. Viewers a 0.11 owner kept
  apart (`lane-views.json`) join the lane's sharing once. An owner still
  sends its list as `view` too, and the coordinator still lists shared lanes
  with `view: true`, so 0.11 coordinators, nodes and dashboards in the same
  pool let them view as well.
- **A shared lane looks like your own stacks in the sidebar.** Under
  "Shared with you" each one is a row as a stack is (`repo · lane`, its
  owner faint); a click opens its pages, read only. Its services open from
  there and from the pool area, where the lane's name links to it too. The
  share menu says what they see.

### Fixed

- **A server that cannot update itself says so** (ADR-0039): a node agent
  in a container, with `remote_update = false` in its policy or without a
  way to install reports why in `no_update`; the dashboard disables its
  **Update** and names the reason. Before, the order was accepted, refused
  by the node and shown as "update refused" only, and the refusal was in no
  log.
- A port another process holds names that process (`port 8091 is already
  in use by another process (node.exe, pid 4711)`, from `netstat`/`tasklist`,
  `ss` or `lsof`): a leftover of an earlier run is told apart from another
  program.
- A node sends its capacity every minute on average; it drifted to every
  67 s.
- **A lane on a node restarts its own containers** ([#128](https://github.com/nuetzliches/devstack/issues/128),
  [ADR-0025](docs/adr/0025-a-lane-can-run-on-another-node.md)). `devstack
  restart <group>` with Compose members ran `docker compose` with the main
  tree's project and files inside the lane container on the node, which has
  no Docker (and with one in reach would have hit the main tree's
  containers). The node's agent now restarts the lane's own containers (its
  project `<project>-<slot>`), for the lane's owner, only those the group
  starts, and the lane's daemon never runs Compose for them. A node older
  than this release is refused with what to update.
- **`devstack restart` (and `start`, `stop`) takes a Compose service**:
  `app.web`, the member form `compose.app.web`, or `web` when one compose
  group starts it, locally, in a lane and in a lane on a node (it answered
  `no service`). The same for `POST /v1/services/{name}/…` and the MCP's
  `restart_service`.
- **A file a node cannot write no longer stops a lane's sync.** The node
  writes the others and names it; the lane's trouble shows it, and
  `remote_lane_sync_failed` goes to the daemon's log (and the pool's
  telemetry), at most once per lane every ten seconds with how many were
  left out. It is tried again with the next change or full comparison, not
  every tick. Every failed sync is logged so, not only shown in the
  dashboard.

## [0.11.1] - 2026-10-19

### Fixed

- **A shared lane on a node opens** ([ADR-0035](docs/adr/0035-lanes-can-be-shared-with-the-pool.md)).
  Its owner told the pool the main tree's port of each service, but a lane
  on a node listens on its lane ports there, as it does on the owner's
  machine; every connection was refused with `port … is no service's or
  container's port in this stack`. The owner sends the lane's ports now
  (the change reaches the pool with its next renewal, once the owner's
  daemon runs this release).
- **A shared lane shares its Compose containers with a web page too.** Only
  services with a `port` were shared, so a container's web interface stayed
  out of reach for those it was shared with. Shared now: the containers the
  owner's dashboard links as a web page (a `urls` entry with `http`/`https`,
  or a published port without an entry), on the lane's ports; `urls = { db =
  "" }` keeps one out, a group's `shared` containers stay the main tree's.
- Dashboard: a lane's share menu opens to the left of its button, inside the
  window (it ran off the screen's right edge).

## [0.11.0] - 2026-10-19

### Added

- **Profiles: a tree runs for a purpose** ([#115](https://github.com/nuetzliches/devstack/issues/115),
  [ADR-0041](docs/adr/0041-a-profile-says-how-a-lane-runs-for-a-purpose.md)).
  `[profiles.<name>]` in the stack file changes how a lane or the main tree
  runs, chosen at start: background workers, fresh data (the tree's own
  volumes go first; on a node its slot's), variables for every service and
  command with the tree's addresses, which services start, per service how
  it runs (a frontend as a `static` build instead of a dev server), and the
  command it is for. `devstack up --profile <name> [--run]`, `devstack lane
  start <lane> --profile <name> [--run]`, the MCP `up` and `lane_start`
  with `profile` and `run`, and the dashboard's lane start offer them; the
  tree runs with it until it stops (`status`, `lane list`, the stack
  header). Each profile is checked as the stack it makes. A lane takes the
  profiles of its own worktree's stack file. A file with `[profiles]`
  needs this release (`requires = "0.11.0"`).
- **An admin updates a server's devstack from the dashboard**
  ([ADR-0039](docs/adr/0039-an-admin-can-update-a-server-node.md)).
  "Update" in the pool area, for a node of the kind server that is online:
  at its next report the node takes no new lane, waits until its lanes
  have ended, installs the latest release (checked against its
  `SHA256SUMS`) and starts its agent again on it; the version column says
  how it goes. The node's own `policy.json` can refuse it (`remote_update:
  false`), a node in a container refuses, and an older release needs
  `downgrade`. `devstack node agent ... --print-unit` prints a systemd unit
  that brings the agent back whatever happens. "Prune" removes the stopped
  lanes a node keeps (ADR-0038). Every order is in the audit log.
- **"Discard data" for a stopped lane in the dashboard** (ADR-0038): its
  containers and volumes go, here and on the nodes it ran on, so its next
  start is fresh (`devstack lane reset`).
- **A lane shared with you to view looks like your own** (ADR-0040). Under
  "Shared with you" it is a row like your lanes; it opens the same pages
  (overview, services and containers with their logs, infrastructure,
  commands' runs, events, usage), live, read through its node, with its
  owner and node named and nothing that would change it.
- **A shared lane can be shared to view** ([ADR-0040](docs/adr/0040-a-shared-lane-can-be-watched-like-ones-own.md)).
  `devstack lane share <lane> --view <member|tag|*>` (API: `view` beside
  `with`; MCP `lane_share` with `view`) lets them read the lane as its
  owner does: its state, events, services' and containers' logs, command
  runs and usage, never its environment, changing nothing. The node lets
  only those reads through (`GET /v1/leases/{id}/view/…`), checked with the
  coordinator at least every 5 s; the hub reads through it
  (`/hub/pool/shared/{lease}/api/…`). Shared lanes are listed with `view`.
- **`devstack lane reset` and `devstack lane prune`**
  ([ADR-0038](docs/adr/0038-a-lanes-data-outlives-its-lease.md)). `reset
  <lane>` throws a stopped lane's data away, here and on the nodes it ran
  on, so its next start is fresh. `prune [--older-than 7d] [--dry-run]`
  removes what lanes whose worktree is gone left behind: on this machine
  their Compose projects (only those whose files are in this repository's
  lane state), on the pool's nodes this machine's kept lanes of the
  repository, and with `--older-than` those not used for that long. A lane
  whose worktree is removed takes its kept data on the nodes it ran on
  along (`lane_data_removed`); a node that does not answer keeps it until
  its retention ends. API: `POST /v1/lanes/{lane}/reset`, `POST
  /v1/lanes/prune`.
- **Kept lanes are shown** (ADR-0038). A node reports what its stopped
  lanes take (`Report.kept`), and the pool area shows it per node. A lane
  that starts from data a node kept says so with its age (`devstack up
  --on`, `remote_lane_kept` in `daemon.log`), since it may be from another
  branch; `devstack lane reset` starts fresh.
- **A lane on a node keeps its data for its next start there**
  ([ADR-0038](docs/adr/0038-a-lanes-data-outlives-its-lease.md)). The end
  of a lease no longer removes the lane's mirror, builds, toolchain caches
  and volumes: they stay in a slot of the node for that member, repository
  and lane, and the next lease of the lane finds them (a sync of only what
  changed, a warm build, its databases). One lease at a time per lane. The
  node keeps a stopped lane a week if dedicated, a day otherwise, at most
  20 GiB per member (`keep_lanes_hours`, `keep_lanes_gib` in its policy),
  removes the kept lanes of a member the pool no longer lists, and on start
  what an earlier agent left behind. `GET /v1/slots` and `DELETE
  /v1/slots?repo=&lane=` on the node list and remove a member's own.
  `[lanes] keep_data = false` keeps nothing.
- **Check for updates in the dashboard.** The version in the top bar opens
  a popover with the hub's and the shown stack's devstack, a "Check for
  updates" button (`POST /hub/update/check`: the hub asks GitHub now instead
  of at its next 12-hour look) and, when there is one, the update to install.
- **Lanes can be started from the main tree**
  ([#103](https://github.com/nuetzliches/devstack/issues/103)). `devstack
  lane start <lane> [--on <node>]` and the MCP tools `lane_start` and
  `lane_stop` start and stop another worktree's lane from wherever they run,
  so an agent in the main tree can run another branch or commit in
  isolation instead of building a separate environment. `devstack status`
  (and the MCP `status`) in the main tree names the lanes in one line. The
  `devstack` skill has the recipe "run a commit beside the main stack".
- **The pool knows which devstack each party runs**
  ([ADR-0031](docs/adr/0031-a-coordinator-places-lanes.md) point 8;
  [pool.md](docs/pool.md#versions-in-a-pool)). Nodes and members
  report their version and a pool protocol number, the coordinator answers
  with its own and passes a node's on with a lease offer and a shared lane.
  The coordinator places no lane on a node with another protocol and names
  both releases when one is asked for by name. A node of another version, or
  one too old to say, gets the lane with a warning: in `daemon.log`
  (`pool_version_mismatch`), under the lane in the dashboard and `devstack
  lane list`, and from `devstack up --on`. Opening a shared lane warns the
  same way (dashboard and `devstack open`). The pool area shows each node's
  version and the coordinator's.

### Changed

- **Dashboard sidebar.** Every worktree of a running stack is listed (no
  more "N not started" fold), the main working tree as its own row first,
  and each tree's state is a dot, as a stack's is (the counts and the place
  are in its tooltip). A stack has a menu (its "…" button, or a right
  click): open, start, stop, copy its path, and remove it from the
  dashboard (`DELETE /hub/stacks/{id}`; nothing on disk changes). "Add a
  stack" is the "+" next to "Stacks". The top bar shows the hub's
  connection only; whether the shown tree is live is in its header.
- **API: `POST /v1/lanes` with `on` answers `202` once the lane is leased**
  (was `201` when it ran, after minutes on a first start), and syncs and
  starts it on the node in the background; the lane's `moving` shows the
  steps, and is gone once it runs there or carries the error
  ([ADR-0025](docs/adr/0025-a-lane-can-run-on-another-node.md), amended
  2026-10-19). A request held open for 4.4 minutes was cut off by proxies
  and clients with shorter timeouts. A missing worktree, a clash with where
  the lane runs and a refused lease are still answered at once (`409`).
  `devstack up --on`, the MCP `up` and the dashboard follow the start, so
  `up` still ends when the lane runs or fails with the reason; an
  interrupted `up --on` run again follows the start under way. A client of
  the API that starts lanes on nodes must follow `moving` before it calls
  the lane's `/v1/up`. `devstack lane list` shows a start or move under
  way, or one that failed lately.
- Each phase of a lane's start on a node is timed: the node measures its
  phases (`LaneState.phases`), each Compose group split into checking,
  images (pull, build) and containers, starting, and waiting until healthy;
  the requester logs every phase, the lease and the sync included, as
  `remote_lane_phase` with `ms`, which the pool's telemetry sends as the
  `devstack.duration` histogram labelled with `devstack.phase`.
  `devstack node run-lane` prints the phase times.
- A tunnel that fails (`remote_lane_tunnel`, `shared_lane_tunnel`) says what
  the node answered and names both versions when the node runs another
  release; the lane daemon's "no service's or container's port" names its
  version. Before, a 0.10 member opening a lane on a 0.9.8 node saw only a
  port refused.

### Fixed

- **The dashboard lets go of a lane whose worktree was removed.** The
  daemon removed such a lane (`lane_removed`), but a dashboard showing it
  followed the lane's own event stream, which does not carry the lanes:
  it said "reconnecting" for good and kept the lane in its lists. When the
  shown lane's stream breaks, the dashboard now asks the main tree for the
  lanes, and shows the main working tree with a note once the lane is gone.
- **Stopping the main working tree no longer stops its lanes**
  ([ADR-0024](docs/adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md)).
  `devstack down` in the main tree, and "Stop stack" in the dashboard, shut
  the daemon down and with it every lane: a lane on a node lost its lease,
  one serving the main ports lost them without a word, and the next start
  began each lane cold. While a lane runs, they now stop the main tree's
  services only; the daemon and the infrastructure the lanes share stay,
  and a lane serving the main ports keeps them. `devstack down --all` (hub:
  `POST /hub/stacks/{id}/down?all=true`) stops everything as before. The
  main tree's `POST /v1/down` answers `200 {"lanes": [...]}` then and skips
  `infra`; `POST /v1/shutdown?unless_lanes=true` refuses with `409`. A
  service stopped in the main tree while a lane serves the main ports no
  longer takes its port from the lane, and a daemon that stops no longer
  takes the main slot back for services it stops anyway. The dashboard
  offers "Start stack" for a main tree whose daemon stayed for its lanes.
- **A Compose port is no longer taken for a lost forward while the server
  inside starts** ([#98](https://github.com/nuetzliches/devstack/issues/98)).
  Docker Desktop's proxy accepts a connection and closes it until something
  listens inside, which looked like a lost port forward: `status` warned
  about ports that worked, and a database still starting could be
  restarted. A container is judged once it has been ready (running, healthy
  if it has a check) for 20 s, a port is lost only when three probes a
  second apart say so, and one found lost is probed again every few seconds
  until it answers ("reachable from this machine again"). A restart from
  devstack, or a change of the container's state, clears the verdict, so
  `status` no longer shows one from before a restart. devstack restarts a
  container for a lost forward only when it has been ready for 20 s, and at
  most once in 10 minutes.
- **A lane on a node shows its containers' output and load, and they can be
  restarted** ([#101](https://github.com/nuetzliches/devstack/issues/101)).
  The lane's daemon has no Docker (ADR-0025 point 2), so its container log
  console failed (with a `docker:cli` lane image: `tcp://docker:2375`),
  its containers showed `0 B / 0%`, and start, stop and restart were
  refused. The node's agent now answers those routes itself, for the lane's
  owner and on the containers of the projects it started for the lease
  only (by their Compose labels, no Compose file of the mirror read), and
  writes the containers' states, health and load into the lane's file every
  5 s; the lane's daemon reports them as a local lane's (`top`, the
  dashboard), and says when a container's state changes. A node with an
  older devstack says it is too old instead of a Docker error. A lane
  started before the node was updated shows its containers once it is
  started again.
- **A lane's `[compose.<group>] urls` links follow its ports**
  ([#101](https://github.com/nuetzliches/devstack/issues/101), comment).
  In a lane, a link to a port on this machine (`localhost`, `127.0.0.1`)
  pointed at the main tree's port; it moves by the lane's offset now, with
  its path, as every port of the lane does. Another host stays as written.
- **A long-running service that exits by itself is started again**
  ([#103](https://github.com/nuetzliches/devstack/issues/103), comment). A
  lane's dev server exited with code 0 a minute after it was healthy, and
  nothing brought it back: tests against the lane found its port closed. A
  `service`, `devserver` or `static` that exits without a stop, restart or
  switch asking for it now starts again from the same build, after 1, 2,
  4, … s (at most a minute); after five such exits in a row it is
  `failed` with the reason, and an instance that ran for five minutes
  starts the count again. A `tool` stays exited (it may have been closed on
  purpose). `daemon.log` says why: `service_exited` (exit code, how long it
  ran, what of its process tree still ran), `service_restarting`,
  `service_given_up`; and `copy_synced` says what a sync changed in a
  lane's copy under what runs from it.
- **A lane on a node started again uploads only what changed**
  ([#99](https://github.com/nuetzliches/devstack/issues/99),
  [ADR-0038](docs/adr/0038-a-lanes-data-outlives-its-lease.md), first step).
  The end of a lease removed all of the member's uploaded files on the node,
  so stopping a lane and starting it again sent every file once more. They
  stay now, at most 8 GiB per member: what no lease needs goes once that
  would be exceeded (never what was uploaded in the last ten minutes, which
  also closes a race in which a new lease's uploads could be removed before
  it applied them), and all of a member's files go once the coordinator no
  longer lists the member.
- **A lane on a node is checked for what it starts, not for the whole
  Compose file** ([ADR-0033](docs/adr/0033-a-node-decides-what-runs-on-it.md)
  point 7). A group that names its `services` was refused for a service it
  never starts (a Docker socket or a host path mounted by a production-only
  service, an external network only such a service joins). The node now
  checks the named services, what they need (`depends_on`, `links`, a
  service's network or volumes) and the networks, volumes, secrets and
  configs those use; a group that names none is checked whole, as before.
- The file watcher honours the repository's ignore rules (`.gitignore` at
  any depth, `.git/info/exclude`, the global excludes): a change to an
  ignored file starts no candidate and no longer updates a lane's copy, as
  it is not in the snapshot a build runs in. A dev server starting up (Vite
  writing its prebundled dependencies into `.angular/cache`) no longer
  rebuilds every built service. The caches of frontend tools (`.angular`,
  `.vite`, `.next`, `.nuxt`, `.turbo`, `.cache`, `.svelte-kit`,
  `.parcel-cache`) never count, with or without a `.gitignore`.
  `restart_on` still sees ignored files.

- The dashboard offered stop, start and restart for every container of a
  Compose group's project, the routes knew only the services in the group's
  `services`: a container of the project the group does not start (started by
  hand, of a profile, left from an earlier stack file) showed a stop button
  that answered `404`. The routes now accept every container the view shows:
  such a container (`managed: false`) can be stopped, `start` and `restart`
  answer `409` with the reason, and the dashboard disables them with that
  reason as tooltip. A `404` names why.

- A request through the hub to a stack that is stopping or restarting is a
  `503` with `Retry-After` and a message that says so, not a `502`. A `down`
  from the dashboard let the daemon take new requests for as long as its
  services took to stop, then cut what it held 2 s after: a request in
  between (a new lane, say) ended as `502`. Now the daemon stops taking
  requests as soon as it is told to stop; the hub looks at `daemon.json` for
  every request, sends one that certainly never reached the daemon (and a
  `GET`) again to the daemon that replaced it, and answers `503` for one that
  was cut. The dashboard shows that message instead of "stack not running".
- `up` right after `down` (from the dashboard or the CLI) waits for the old
  daemon to be gone instead of attaching to it as it stops, or starting one
  that gives up on the state lock after 5 s.
- The hub no longer takes a stack for running when its `daemon.json` was
  left by a killed daemon and another process now has that port.

- **Telemetry** ([ADR-0037](docs/adr/0037-telemetry-goes-to-the-pools-collector-with-consent.md)
  amended), after a review of three days of a pool's telemetry:
  - A process's first lines (daemon start, restores, early errors, the main
    slot) were lost: lines were kept only once the first round, ten seconds
    in, had found a pool that collects. They are kept from the start now and
    sent only if that round finds a pool with this machine's consent, else
    dropped.
  - Durations promised by the ADR are measured now: lane stops (local and
    remote), local lane starts, service starts from the spawn to ready or
    failed (with `result` and `restarts`; a service without a health check
    reports its fixed wait as `settle_ms`, not as a duration), builds
    (`build`, with `result`), tunnel connections (duration and bytes, and
    tunnels open).
  - Log records carry `os.type` and `devstack.repository`; duration
    histograms carry repository, lane, node, place, service and result
    besides the event. The main working tree's lane is `main` in the log,
    not an empty name.
  - Nested values (`status`, `candidate`) go out as dotted attributes
    (`devstack.status.state`) instead of a JSON string; lists of plain values
    as OTLP arrays. Log records carry `observedTimeUnixNano`.
  - Histogram buckets add 180 s, 600 s, 900 s, 1200 s and 1800 s.
  - `scripts/telemetry/analyze.py` reads the old and the new shape.
  - A `prepare` line sent its command line (`run`) to the collector. Fields
    that hold a command line, an environment, process output or a path on
    the machine (`run`, `cmd`, `command_line`, `args`, `env`, `output`,
    `repo`, `worktree`, `file`, `cwd`, `dir`) now stay in `daemon.log`, at
    any depth; user names in home paths and credentials in URLs are masked
    in every text that leaves. Lane lines name the worktree's path
    `worktree` (was `path`).

## [0.10.1] - 2026-10-18

### Fixed

- A command may use `{compose.<group>.<service>.<port>}` (with `.host`,
  `.port`, `.address`): it resolves to the published address as it does for
  services. 0.10.0 refused stack files with such a command, so a running
  stack would not restart on 0.10.0; before, the command failed only when it
  ran.

## [0.10.0] - 2026-10-18

The fixes of a review of the whole repository at 0.9.8 (2026-10-17): the
pool's boundaries, two core promises (ADR-0013, ADR-0016) and the hub's
origin rules. Read **Changed** before updating: some stack files that loaded
before are refused now, and a pool needs its nodes granted.

### Security

- **Lanes on a node are confined** ([ADR-0033](docs/adr/0033-a-node-decides-what-runs-on-it.md)
  amended). The node reads a lane's Compose model in a throwaway container
  that sees only the lane's mirror and refuses privileged services, added
  capabilities or devices, host namespaces, `userns_mode`, `cgroup_parent`,
  `security_opt`, `sysctls`, `volumes_from` a container, mounts, builds and
  env files outside the mirror, external volumes, volume `driver_opts`,
  foreign external networks and non-bridge drivers. Before, a member's
  Compose file was root on the node.
- The mirror sync no longer follows symbolic links (writes through
  `O_NOFOLLOW`, a link in the way is replaced), so a lane cannot overwrite
  the agent's binary or files outside its mirror; the stack file, the lane
  Dockerfile and the lane daemon's token are read without following links.
  Compose and env files of the stack file must resolve beneath the mirror.
- Every container of a lane gets the lease's memory and CPU limits and a
  process limit; the lane container runs with `--cap-drop ALL` and
  `no-new-privileges`. Network rules go in before any container starts;
  `none` gets the `internet` rules plus a final drop.
- **Only members an admin granted are nodes**
  ([ADR-0032](docs/adr/0032-the-pool-is-administered-in-the-dashboard.md)
  amended): before, any member could report itself as a node with any
  capacity and receive `--on pool` lanes with their worktrees.
- Uploaded files are kept per member (8 GiB at most, removed with the
  lease); joins are bounded (request size, ten a minute per address, 64
  waiting); a node trusts its last member list for an hour at most while the
  coordinator does not answer; a node applies its own `policy.json` on top of
  the admin's.
- **Hub:** every request with the session cookie must come from the hub's own
  page, `GET`s included (`Origin`, or `Sec-Fetch-Site: same-origin|none`):
  `SameSite=Strict` does not separate loopback ports, so a page on another
  port of `127.0.0.1` could make the hub inspect a path. Paths are local
  only (no `\\host\share`, which made the hub connect to that host as the
  user). Every answer carries a `Content-Security-Policy` without inline
  script and with `frame-ancestors 'none'`, `X-Frame-Options: DENY`,
  `nosniff` and `no-referrer`
  ([ADR-0019](docs/adr/0019-control-api-security.md) amended).
- The CLI sends the token only to a hub that proves it knows it
  (`/hub/ping?nonce=`): a program of another local user on port 4250 learns
  nothing.
- `hub-sessions.json` is written `0600` and read again when it changes, so
  deleting it unpairs at once; any session cookie of the hub's name counts
  (another loopback server cannot lock a browser out); pairing codes expire.
- Shared services, service links and `devstack open` use `http` and `https`
  only; another scheme would reach a protocol handler of the machine.
  `POST /hub/pool/join` takes `https://host[:port]` only and passes on a
  coordinator's own error only.
- The daemon's token file is created `0600` from the start and never in a
  shared temporary directory; `DEVSTACK_LISTEN` is refused outside a lane
  container.
- What leaves the machine for an OTLP or a pool's collector has every text
  cut to its first line (a failed job's reason carried its output); the
  coordinator strips identity attributes from records and data points too.
- MCP: the `lane` argument is checked like every path segment, so
  `promote_lane` cannot reach other lane routes.
- **Releases** attest the build provenance of the archives, `SHA256SUMS` and
  the installers; the installers verify it with a logged-in `gh` and stop if
  it fails. Actions are pinned by commit, Dependabot proposes updates,
  `cargo deny check advisories` and `npm audit` must pass for a release.
  TLS to GitHub (update, release check) uses the system's certificate store.
  The landing page has a Content-Security-Policy.

### Changed

- **Breaking, stack file:** files that use any of the following no longer
  load (no outside users yet, so no migration and no `requires` bump; the
  frozen compatibility files still load):
  compose files, compose env files, `[stack] env_files` and `[lanes]
  dockerfile` outside the repository (absolute, a drive, `..`);
  placeholders without a value (`{port}` without `port`, `{artifact}`
  without `build`, anything but `{repo}` in `build`/`output`, slot
  placeholders in a command); placeholders in `cwd`, `hot_reload.cwd`,
  `prepare.cwd`, `prepare.run`; globs that do not compile; port 0,
  `port_step` above 10000, a port that leaves no room for a lane; Windows
  device names (`con`, `nul`, ...) as names; Compose project, service and
  network names Compose would misread; step keys of another action. The
  rules are in [stack-file.md](docs/stack-file.md).
- **Every service builds once after the update:** the build id now covers
  `cwd` and `prepare` (run, cwd, the content of its inputs).
- **Jobs time out** after 30 minutes unless they set `timeout`; `stop` ends a
  start in progress at once; a stopping daemon waits at most 90 s for its
  services.
- **`POST /v1/down` may answer `409`**, naming what did not stop, instead of
  `204` regardless.
- **A pool's nodes need an admin grant** (`devstack node members node <id>
  on`, `POST /v1/admin/members/{id}/node`); until then the coordinator
  ignores the node's API and the agent says why. Grant existing nodes after
  updating the coordinator.
- **Nodes with network `internet` or `none` build images without a network**
  (`[lanes] dockerfile`, Compose `build`); downloads in `RUN` need a `lan`
  node or a prebuilt `[lanes] image`. Their lane networks have no IPv6.
- A lane is taken to need at least 0.25 CPU and 256 MiB on a node.
- A daemon restart starts the build last promoted to main without building
  ([ADR-0016](docs/adr/0016-a-daemon-restart-is-a-clean-restart.md));
  changes made meanwhile arrive as a candidate.
- One build job per daemon, shared by its lanes (ADR-0020).
- Commands of a lane run in the lane's copy, `{repo}` is that copy (ADR-0024).
- Every process gets `DEVSTACK_LANE`; `DEVSTACK_SLOT` follows the slot.
- `devstack env --export dotenv` writes Compose's dialect (`$$`, escapes in
  double quotes) and env files are read the same way.
- Docker calls run with a time budget and are ended past it.
- A worktree whose directory is missing is forgotten (with `compose down
  --volumes`) only after ten minutes.
- A lane on a node syncs on change (a watcher, within two seconds) and
  compares everything every 30 s; sync, renewal and state reads no longer
  wait for each other.
- `devstack logs --follow` reads the daemon's log stream (with `--grep`) and
  needs a service name; `devstack skills check` exits 1 when the skills are
  outdated.
- `DEVSTACK_NO_HUB_INTEGRATION` works with both one-line installers and is
  kept: `devstack up` does not register the hub later either.
  `devstack update` leaves a `cargo install` build alone.
- The dashboard's pool area polls the coordinator (the one exception to "no
  polling", ADR-0021 amended).

### Added

- `timeout` for jobs; `POST /v1/runs/{id}/cancel`; `StackView.watcher_error`.
- `devstack node members node <id> on|off`, and the same grant in the
  dashboard's member list (`POST /hub/pool/members/{id}/node`).
- The outcome of an update started from the dashboard is reported by the new
  hub (installed version, or the failure with the end of `hub/update.log`).
- Unit tests for the dashboard (`npm test` in `ui/`, vitest).
- [SECURITY.md](SECURITY.md) describes the pool's listeners, what leaves the
  machine and the pool's trust model; this changelog.

### Fixed

- A held candidate (ADR-0013) no longer slips into main with the next
  change; `promote` takes the newest held build; the gate survives a daemon
  restart.
- `promote --lane` against a start in flight no longer leaves workers running
  in two trees (ADR-0008).
- `restart --rebuild` of a stopped or unproxied service or a job runs the new
  build, not the old artifact; `restart` and a take-over do not build twice.
- A monitor of an instance that was switched out leaves the slot alone; two
  services after the same job run it once; a chosen artifact is not pruned
  before it runs; a build whose inputs changed during it is discarded and
  built again.
- An event stream that falls behind gets a fresh snapshot instead of
  drifting; logs are read from their end, off the async workers.
- The proxy pools upstream connections, drops hop-by-hop headers, clears its
  last error and lets a body of known length finish when draining.
- `stop` no longer reports a graceful end when the process query failed
  (Windows and Linux); a process that cannot be resumed is killed.
- Snapshots: a case-only rename on NTFS no longer loses the file, a path that
  changed between file and directory no longer blocks syncs, checked-out
  submodules are included, copies are streamed.
- `build` and `output` are interpolated like the rest of the stack file.
- An admin's end of a lease works with several reporting machines; leases
  are known again after a coordinator restart; `members.json` changes hold a
  file lock across processes.
- Compose: nulls read as empty, external networks the group does not own are
  left alone, a published port's address is kept.
- CLI: the daemon no longer holds the caller's directory (a worktree could
  not be removed on Windows); a hanging daemon is reported, a stale
  `daemon.json` ignored, two `up` at once wait for each other; MCP `up`
  starts Docker Desktop like the CLI.
- `devstack self uninstall` removes the install directory on Windows;
  `devstack update` and `install.ps1` replace the binary without a
  half-written file; the PATH entry keeps its registry type.
- The file watcher judges an event by its path before it opens anything, and
  on Windows no longer lists new directories: `pnpm install` failed with
  `EBUSY` in about one run in four while a daemon watched the repository.
  A folder moved into the tree on Windows is reported as itself; its files
  count when they change.
- A daemon that lost the start race against another daemon for the same
  stack ends at once instead of running beside it.
- A lane on a node re-hashes only files whose size or modification time
  changed, and uploads stream; the node writes them to disk as they arrive.
- Publishing an artifact on Windows waits out a scanner holding one of its
  files instead of failing the candidate.
- Dependencies: `toml` 1, `rand` 0.10, `windows-sys` 0.61.
- Dashboard: an unknown candidate state no longer breaks every service row;
  a paused log view keeps its line limit; the folder dialog no longer stays
  "open" after the page gave up waiting.

[Unreleased]: https://github.com/nuetzliches/devstack/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/nuetzliches/devstack/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/nuetzliches/devstack/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/nuetzliches/devstack/compare/v0.13.1...v0.14.0
[0.13.1]: https://github.com/nuetzliches/devstack/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/nuetzliches/devstack/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/nuetzliches/devstack/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/nuetzliches/devstack/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/nuetzliches/devstack/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/nuetzliches/devstack/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/nuetzliches/devstack/compare/v0.9.8...v0.10.0
