# Security

## Reporting a vulnerability

Please report security problems privately through GitHub: **Security →
Report a vulnerability** in this repository. Do not open a public issue for
them. You get an answer within a week; a fix and its release are coordinated
with you before anything is published.

## What devstack exposes, and to whom

devstack runs on developer machines and controls processes there, so its
interfaces are deliberately narrow ([ADR-0019](docs/adr/0019-control-api-security.md),
[ADR-0021](docs/adr/0021-a-hub-serves-the-dashboard.md)). The pool, which
runs lanes on other machines, adds two network listeners
([pool.md](docs/pool.md), ADR-0025 to ADR-0037).

| Interface | Bound to | Who may use it |
| --- | --- | --- |
| Daemon API (one per stack) | `127.0.0.1` only, a random port (`daemon.json` names it) | Holders of the per-user token (`devstack` CLI, MCP server, hub). Every route checks `Host` (loopback names only), then the token, then a browser's `Origin` on anything but `GET`; no CORS headers, no cookie |
| Daemon API inside a lane container on a node | `0.0.0.0:7600` in the container, on the lane's own Docker network | The node agent, with that lane daemon's own token. Reachable from every container of that lane's network, protected by the token and the `Host` check. `DEVSTACK_LISTEN` moves the daemon off loopback only together with `DEVSTACK_REMOTE_LANE`, which the node agent sets in the lane container; anywhere else the daemon refuses to start |
| Hub (dashboard) | `127.0.0.1` and `::1`, port 4250 (`DEVSTACK_HUB_PORT`) | A browser paired once through a one-time code (`devstack ui`), and the CLI with the token. Details below |
| `GET /hub/ping` | the hub | Anyone on the machine; it answers name and version only (with `?nonce=`, also a proof that the hub knows the token, see below). Its CORS header names only the landing page's origin (`https://nuetzliches.github.io`, shared by every GitHub Pages site of the organisation, which therefore can read the version too) plus origins in `DEVSTACK_HUB_PING_ORIGINS` |
| `/hub/pick-folder`, `/hub/inspect`, `/hub/stacks/add`, `/hub/stacks/scan`, `/hub/stacks/init` | the hub | The paired browser, from the hub's own page (`GET /hub/inspect` included): opens the system's folder dialog on this machine, inspects a folder the user chose or named, lists the repositories with a stack file below one and registers them, or writes a first `devstack.toml` there, never over an existing file. Local paths only: a network path (`\\host\share`, `//host/share`; on Windows anything but a drive) is refused, as it would make the hub connect to that host as the user |
| `devstack://hub/start` | the operating system | Any web page can open the link; the browser asks first. It can only start the hub, nothing else |
| MCP server (`devstack mcp`) | stdio of the agent that started it | That agent. Commands with a confirmation text run only with `confirm: true`, which an agent must not send without the user's consent |
| Coordinator (`devstack node serve`) | the address the operator passes (`--listen`), TLS | Anyone who reaches the port may ask to join (bounded: a certificate request of normal size, ten requests a minute per address, at most 64 waiting for an admin). Everything else needs mutual TLS with a certificate of the pool's own CA, issued once an admin approved the member; the member's identity comes from the verified certificate, never from the request. Admin routes need the admin role |
| Node agent (`devstack node agent`) | the address the operator passes (`--listen`), TLS | Members of the pool only: mutual TLS with the pool's CA, required for every connection, and the member must be on the coordinator's member list. A lane's API and sync are its requester's; others reach the services of a lane its owner shared with them, nothing else, checked with the coordinator at every connection |

## The hub and the browser

- **Pairing.** `devstack ui` asks the hub for a one-time code (single use, 60
  seconds) and opens `/auth?code=…`; the hub sets an `HttpOnly;
  SameSite=Strict` cookie for a year. The hub stores a hash of the session,
  never the cookie, in `hub-sessions.json` (in the hub's state directory beside
  the token, created `0600`). Deleting that file unpairs every browser with its
  next request.
- **Every request with the cookie must come from the hub's own page**, reads
  included: a change and the WebSocket handshake name the hub's own `Origin`;
  a `GET` names it or says `Sec-Fetch-Site: same-origin` (or `none`, a
  navigation the user typed). `same-site` and `cross-site` are refused. This
  matters because `SameSite=Strict` does **not** separate loopback ports:
  browsers scope it by scheme and host, so a page on `http://127.0.0.1:3000`
  (the app under development, with a compromised dependency, say) is
  same-site with the hub and its requests carry the cookie.
- **No framing, no inline script.** Every answer carries a
  `Content-Security-Policy` (`script-src 'self'`, `connect-src` the hub
  itself, `frame-ancestors 'none'`), `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
- **Links open only `http` and `https`.** A service's address, a container's,
  and a lane someone in the pool shares are opened by the dashboard and by
  `devstack open` only with those schemes; the coordinator and the daemon
  refuse to keep a shared service with another one. Another scheme would reach
  a protocol handler of this machine.
- What the hub forwards from a daemon loses `Set-Cookie` and hop-by-hop
  headers on the way.

## The token

The token lives in the user's profile (`%LOCALAPPDATA%\devstack\token`,
`$XDG_STATE_HOME/devstack/token`, by default `~/.local/state/devstack/token`),
created readable by that user only (`0600` from the start), never in a shared
temporary directory: without a profile directory the daemon refuses to start.
Anything running as the same user can read it; devstack does not defend
against code that already runs under your account.

The CLI sends the token to the hub (`devstack ui`, `hub stop`, `update`,
`open`, the pool commands) only after the hub proved it knows it: `GET
/hub/ping?nonce=<fresh>` answers a keyed hash of the nonce under a key
derived from the token. A program of another local user that took port 4250
while your hub was down learns nothing. A hub older than that proof counts
only when this user's hub holds its state lock and `hub.json` names the port.

There is no `devstack token rotate`. To replace the token, stop every stack's
daemon (`devstack down` in each repository) and the hub (`devstack hub
stop`), delete the token file and start again; the first daemon or hub
creates a new one. Delete `hub/hub-sessions.json` beside it to unpair the
browsers as well.

## What leaves the machine

- **Release checks** to GitHub: the hub at start and every 12 hours (and when
  a dashboard connects and the last answer is over an hour old), `devstack
  doctor` and `devstack update`; `devstack update` and the installers then
  download from GitHub's release. TLS to GitHub is verified against the
  system's certificate store. `DEVSTACK_NO_UPDATE_CHECK=1` turns the check off.
- **Docker** pulls the images your Compose files name. A node agent also
  pulls `docker:cli` (it reads a lane's Compose model in it;
  `DEVSTACK_NODE_COMPOSE_IMAGE` names another image) and builds a small
  `alpine` helper with `iptables`.
- **Diagnostic log**, only with `DEVSTACK_OTLP_ENDPOINT` set: daemon and hub
  send their diagnostic log to that OTLP collector. It holds service names,
  build ids, paths of your repository and the reasons of state changes; every
  text is cut to its first line before it leaves (the process output a failed
  job's reason carries stays in `daemon.log`).
- **The pool**, only once you joined one (`devstack join`):
  - `devstack up --on <node>` / `--on pool` copies the worktree to that node:
    every file git does not ignore, tracked or not (an untracked `.env` that
    is not ignored goes too), plus the ignored files `[lanes] sync` names
    (local certificates, an ignored env file), from the worktree or else the
    main tree. The node's owner can read all of it, and the lane's output and
    containers run there.
  - Your machine reports to the coordinator what it shares (CPU, memory, GPUs,
    load) when the share switch is on, and a node reports its lanes.
  - **Telemetry** to the pool's collector only with your consent, per pool,
    off by default (`devstack join --telemetry`, `devstack pool telemetry
    on|off`, the pool area): devstack's diagnostic log, cut to first lines as
    above, and durations and outcomes of lane starts, syncs, moves and stops,
    service starts, builds and tunnel connections (with bytes through them).
    Lines from a process's first seconds are kept on the machine until it
    knows whether a pool with your consent collects, and dropped if none does.
    Never file contents, environment values, command lines or service output.
    The coordinator stamps your identity from your certificate. A node sends
    what concerns the lanes it runs with the coordinator's setting alone.

## The pool's trust model

A pool is a group of machines that trust each other to a stated degree
([ADR-0027](docs/adr/0027-trust-between-nodes.md),
[ADR-0033](docs/adr/0033-a-node-decides-what-runs-on-it.md)):

- **Nodes are granted by an admin.** Only a member an admin granted
  (`devstack node members node <id> on`) runs the pool's lanes; `--on pool`
  and named placement use granted nodes only. A member cannot make itself a
  node by reporting capacity.
- **Placing a lane on a node trusts the node's owner** with the worktree
  (secrets in env files included) and with what the lane does there. Keep
  lanes local whose secrets must not leave your machine.
- **A node trusts the members it runs lanes for with its kernel and its Docker
  engine.** Lanes share the node's engine; a container escape is not
  contained, and there is no engine per lane. Within that, a node confines
  what a lane may ask:
  - a lane's Compose model is read in a throwaway container that sees only
    the lane's mirror, then refused if a service is privileged, adds
    capabilities or devices other than GPUs, shares a host namespace (`pid`,
    `ipc`, `uts`, `cgroup`, `network_mode`), sets `userns_mode`,
    `cgroup_parent`, `security_opt`, `sysctls` or `oom_kill_disable`, uses
    `volumes_from` a container, `use_api_socket`, a `provider` or a logging
    driver that sends elsewhere, or mounts, builds from or reads an env file
    outside the mirror; build `tags`, `cache_to` and local `cache_from`,
    secrets and configs from the environment, external volumes, volume
    `driver_opts`, foreign external networks, non-`bridge` drivers and
    service keys the node does not know are refused too;
  - the node runs the model it checked, written outside the mirror, never
    the lane's files again; bind mounts of the mirror become `subpath`
    mounts of a volume bound to it (Docker refuses a link out of it at every
    container start), secret and config files are copied, values left unset
    stay unset, and Compose runs with only what Docker needs of the agent's
    environment; no sync is taken while a lane starts, and whatever of the
    lane still runs is stopped before the check;
  - compose files, env files, the lane Dockerfile and the stack file must
    resolve beneath the mirror; the mirror sync never follows a symbolic link
    (a link in the way is replaced), so a lane's own code cannot make the
    agent write outside its mirror; the lane container mounts its mirror, a
    home directory of its own and, read-only, the agent's devstack binary;
  - uploaded files are kept per member, capped at 8 GiB per member; the end
    of a lease does **not** remove them, nor the lane's mirror, builds and
    volumes ([ADR-0038](docs/adr/0038-a-lanes-data-outlives-its-lease.md)):
    a stopped lane stays on the node for a week on a dedicated node, a day
    otherwise, at most 20 GiB of kept lanes per member (the node's policy
    sets both), unless its stack says `[lanes] keep_data = false`. The node
    removes them at once when the coordinator no longer lists the member,
    and when the lane's worktree is removed (best effort, if the node is
    reachable). Until then, the node's owner can read them;
  - every container of a lane gets the lease's memory and CPU limits and a
    process limit; the lane container drops every capability and cannot
    gain privileges, and gets no Docker socket;
  - network by the node's policy: `lan` (default) reaches the internet and the
    local network; `internet` the internet but no private address range and
    not the node itself (iptables rules per lane, IPv4; the lane's networks
    have no IPv6); `none` only its own containers. On `internet` and `none`
    nodes, images are built without a network.
- **A node's own `policy.json`** (in its state) holds on top of what an admin
  sets: both must admit a lane, the smaller limits and the stricter network
  win. The node asks the coordinator for its policy before its first lease.
- **Removing a member revokes it.** Nodes take the member list from the
  coordinator; while the coordinator does not answer, a node trusts its last
  list for at most an hour, then refuses every member until it answers again.
- **Certificates are not renewed.** The pool's CA is valid for 20 years, the
  coordinator's server certificate for 10, a member's for 5; when one
  expires, that member (or for the CA, the pool) is set up again. The CA's key
  is in the coordinator's directory: back it up and keep it private.
- Pools do not trust each other: a machine in several has a certificate per
  pool ([ADR-0034](docs/adr/0034-a-machine-can-belong-to-several-pools.md)).

## Release integrity

- Each release lists the SHA-256 of its archives in `SHA256SUMS`. The
  installers and `devstack update` check the download against it. That proves
  the archive is the one the release lists (not damaged or swapped on the
  way), not who built it: both come from the same release.
- Releases since 0.10.0 carry a build provenance attestation of the
  archives, `SHA256SUMS` and the installers. With the
  [GitHub CLI](https://cli.github.com) logged in, the installers verify it
  (`gh attestation verify <archive> --repo nuetzliches/devstack
  --signer-workflow nuetzliches/devstack/.github/workflows/release.yml`) and
  stop if it fails, showing gh's reason; you can run the same command
  yourself.
- The winget package `nuetzliches.devstack` names the release's archive on
  GitHub and its SHA-256, taken by the release workflow from the archive it
  built and published; winget checks the download against it. Its manifests
  (`packaging/winget/`) go through the review of `microsoft/winget-pkgs`
  before winget offers a release. A devstack winget installed is updated
  through winget, never replaced behind its back.
- `devstack update`, and a server node an admin updates
  ([ADR-0039](docs/adr/0039-an-admin-can-update-a-server-node.md)), check
  only `SHA256SUMS`: the trust is GitHub's TLS and control of this
  repository. Releases are not signed with a key the binary knows.
- The Windows executable is not Authenticode-signed: SmartScreen or an
  endpoint protection product may warn about, or hold back, a freshly
  downloaded `devstack.exe`. The checksum and the attestation above are how
  to check it.
- The release workflow pins every action by commit, releases only a tag on a
  commit of `main`, and requires `cargo deny check advisories` and
  `npm audit` to pass.

## Hub integration on the machine

The installers register the hub to start at logon and the `devstack://` link.
`DEVSTACK_NO_HUB_INTEGRATION=1` (or `-NoHubIntegration` for `install.ps1` run
as a file) skips both, and the choice is kept: the installer records it with
`devstack hub uninstall`, so a later `devstack up` does not register them
either. `devstack hub install` adds them again.

## Out of scope

- Processes devstack starts run with your rights. A stack file is code: run
  `devstack up` only in repositories you trust, as you would `npm install`.
- Network access of those processes on your machine is not restricted (see
  the proposal [ADR-0022](docs/adr/0022-network-egress-sandbox.md); a node's
  network modes above are something else).
- Other code running as your user, and anyone who can read your profile, is
  you as far as devstack is concerned.
