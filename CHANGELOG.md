# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as far as a 0.x release can.
Releases before 0.10.0 are summarised in [plan.md §7.4](docs/plan.md#74-phase-2-status)
and listed with their assets on the
[releases page](https://github.com/nuetzliches/devstack/releases).

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

[0.10.0]: https://github.com/nuetzliches/devstack/compare/v0.9.8...v0.10.0
