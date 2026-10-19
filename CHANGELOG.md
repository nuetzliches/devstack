# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as far as a 0.x release can.
Releases before 0.10.0 are summarised in [plan.md §7.4](docs/plan.md#74-phase-2-status)
and listed with their assets on the
[releases page](https://github.com/nuetzliches/devstack/releases).

## [Unreleased]

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

[Unreleased]: https://github.com/nuetzliches/devstack/compare/v0.11.0...HEAD
[0.11.0]: https://github.com/nuetzliches/devstack/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/nuetzliches/devstack/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/nuetzliches/devstack/compare/v0.9.8...v0.10.0
