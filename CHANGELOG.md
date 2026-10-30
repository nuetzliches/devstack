# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as far as a 0.x release can.
Releases before 0.10.0 are summarised in [plan.md §7.4](docs/plan.md#74-phase-2-status)
and listed with their assets on the
[releases page](https://github.com/nuetzliches/devstack/releases).

## [Unreleased]

## [0.33.0] - 2026-10-31

A stack has a page of its own, above its lanes: its daemon, who serves the
main ports, its lanes with where they run and what they take, the services
that run once per machine, and its ports drawn as a graph, with the ports
another stack on this machine uses too in red. Every lane, the main
working tree included as `main`, has the same view, and the lane is in the
URL. A daemon lists every port its stack holds (`/v1/ports`), the hub
every stack's (`/hub/ports`), and `devstack doctor` warns where two
stacks meet (ADR-0080, ADR-0081, ADR-0082). `singleton = true` replaces
`lanes = false`; `[compose.<group>] shared` is deprecated. Fixed: a node
leaves no gone lane's containers running, and a hub that stopped answering
no longer blocks every start.

### Changed

- **`singleton = true` replaces `lanes = false`** (ADR-0080): a service that
  runs once per machine, in the main working tree only. `lanes = false`
  still works and means the same; `devstack check` reports it as
  deprecated.
- **Every lane has the same view, `main` included** (ADR-0081): the main
  working tree is the lane `main`, with the same header, actions and start
  card as any lane; what has no meaning for it (Cold, Discard data, Remove
  worktree, Move, Share, a start in the pool, another profile) is shown
  disabled, with the reason. *Serve main ports* is the same button in
  every lane that does not hold them. *Stop stack* now stops every lane and
  the daemon; `main`'s own Stop stops the main working tree only.
- **The lane is in the URL**: `#/s/<stack>/l/<lane>/...` (`main` for the
  main working tree), in the links of `devstack status`, `up`, `ui
  --link`, notifications and the tray too. Links of the form
  `#/s/<stack>/services/api?lane=feat` still open that lane.

### Deprecated

- **`[compose.<group>] shared`** (ADR-0080): it works as before, and
  `devstack check` and `devstack doctor` report it; it goes with
  `version = 2`. It ties a lane to the main tree, reaches only native
  services (a lane's containers do not find it by name), behaves
  differently for a lane on a node, and mixes the lanes' telemetry. Drop
  it: every lane starts its own containers; a profile's `skip` leaves out
  what a lane can do without.

### Added

- **`/v1/lanes/main/v1/...`** answers as the main working tree's `/v1/...`
  (ADR-0080), so a client addresses every tree as a lane.
- **A stack has a page of its own** (ADR-0081), above its lanes, at
  `#/s/<stack>` (the sidebar's stack row): its daemon (pid, port, uptime)
  and the restart banner, Start, Stop and Restart stack, who serves the
  main ports and the control to give them to another lane or back to
  main, its lanes with branch, place, state, port offset and what each
  uses, and the services that run once per machine. A stopped stack's page
  shows its start and its worktrees.
- **`ServiceView.singleton`** (ADR-0080, ADR-0081): whether a service runs
  once per machine, in the main working tree only.
- **The ports of a machine** (ADR-0082): `GET /v1/ports` lists every port
  a stack holds or uses on this machine, per lane: port, protocol, lane,
  what it is (a service's fixed port, an instance's, a container's
  published or held port, a lane on a node's forward), what holds it, how
  (proxy, forward, the process itself, Docker), where it leads and whether
  it listens. The main tree's daemon includes its lanes'. The hub's `GET
  /hub/ports` joins every registered stack's (a stopped stack's from what
  its stack file declares) and marks the ports two stacks share;
  `devstack doctor` lists them.
- **The stack page draws its ports** (ADR-0082): a Ports card with the
  machine's fixed ports of the stack, the lane each leads to (the one
  serving the main ports marked, a lane on a node by its forwards) and
  each lane's services and containers with their own ports. A port
  another stack uses too is red and names that stack; instance ports show
  on request; on a narrow window it is a grouped list.

### Fixed

- **A hub that stopped answering no longer blocks every start** (#398): it
  held its lock, so `devstack up`, `devstack ui`, `devstack hub start` and
  the MCP server each failed with "another devstack instance owns …\hub\lock"
  until it was ended by hand. A hub that runs, holds the lock, started more
  than a minute ago and does not answer on its port is now ended by the
  next start, which takes its place (`hub_hung` in the hub's log).
- **A node no longer leaves a gone lane's containers running** (#394): a
  slot removed without its purge (one without `slot.json`, or by an agent
  of an earlier release) left its Compose projects and networks running
  for good, until the engine had no network left. Every ten minutes the
  agent now removes the Compose projects of its lanes whose slot is gone
  (their volumes stay), only its own (they ran under its slots). A lane
  that cannot get a network because Docker's address pools are used up
  says so; `docs/pool.md` says how to give Docker more.

## [0.32.0] - 2026-10-30

A pool's project skills stay in their repository: a skill bound with
`metadata.repos` goes into the agents' skill folders of that repository's
working trees only, each folder ignored by git, and a skill set can give
each of its folders to other members (ADR-0079). devstack's own generated
skills are local to the working tree too and are no longer committed; the
hub keeps them current. The Skills page's groups fold away. Fixed: an MCP
upstream's tools show right after its login, and a stopped stack's main
tree says why it does not start in the pool.

### Added

- **A pool's project skills stay in their repository** (ADR-0079): a skill
  with `metadata.repos: [owner/name]` in its frontmatter goes into the
  agents' skill folders of the working trees whose `origin` is that
  repository (`.claude/skills`, `.agents/skills`, ...), not into the user
  folders, so sessions in other repositories neither see it nor pay for
  its description. Each such folder holds a `.gitignore` that ignores it:
  git never sees it. The Skills page shows a skill's repositories and the
  working trees it is in.
- **A skill set of several folders, each for its own members**
  (ADR-0079): `devstack pool skills add team --github org/skills --path
  skills --to '*' --path projects/shop/skills --to shop --repos -
  --repos org/shop`, or as rows in the pool area. A member reads and
  downloads only the skills of its folders.
- **The Skills page's groups fold away**: a click on a group's heading
  hides its skills and shows them again, remembered in this browser. While
  a filter is on, every group with a match shows.

### Changed

- **devstack's generated skills are local to the working tree**
  (ADR-0079): each of their folders ignores itself, and the hub writes
  them into the skill folders of the agents switched on on its Skills
  page, in every stack and worktree, whenever devstack or the stack file
  changes. Commit them no more: `devstack skills install` lists the copies
  git still tracks with the `git rm -r --cached` that takes them out, and
  the upgrade prompt does it (a migration step that goes again in a later
  release, #389). `[agents] skills` still names where
  `devstack skills install` writes.

### Fixed

- **An MCP upstream's tools show right after its login**: for up to a minute
  after logging in, giving a token or logging out, agents still got what the
  hub had asked with the old credential (a `<prefix>__login` instead of the
  tools); a change of the credential now forgets it.
- **The pool's MCP upstreams card no longer says it keeps nothing secret**:
  it can hold an OAuth app's client secret (never shown) since 0.31.0, and it
  says where a member uses or logs in to an upstream.
- **A stopped stack's main tree says why it does not start in the pool**:
  its pool and node buttons said "No node can run it" (the wording for a
  lane without a lane image); now they say only a worktree's lane starts
  there, hovering them lights no machine, and *Start* says it starts the
  stack.

## [0.31.0] - 2026-10-30

The hub follows every stack's worktrees, so a new one shows in the
sidebar at once, and a stopped stack's worktrees can be started as lanes
straight away (ADR-0078). An MCP upstream that logs in with OAuth can carry
an OAuth app's client secret, which GitHub's MCP server needs; a pool can
name one for its members (ADR-0069). Lanes on nodes survive a reboot or a
sleep of the machine that started them. Fixed: `devstack up --on` and the
MCP tool `up` move a started lane, a lane on a node picks up edits, and the
pool's capacity history survives a restart of the coordinator.

### Added

- **An OAuth app's client secret for an MCP upstream** (ADR-0069 addendum
  2026-10-30): GitHub's MCP server wants one even with PKCE, so an OAuth
  upstream with a client id may carry it. The hub sends it only to the
  token endpoint; the user's own goes into the keychain, a pool's comes
  with its list (`devstack pool upstreams add --client-secret … --scope …`),
  and the dashboard, the CLI and the audit log only say that one is set.
  Both upstream forms in the dashboard gain the client secret and the
  scopes (an edit there no longer drops the scopes). A token endpoint that
  refuses with `200` and an `error`, as GitHub's does, is taken as a
  refusal.

- **Worktrees show at once, and a stopped stack's lanes too**
  ([ADR-0078](docs/adr/0078-the-hub-follows-every-stacks-worktrees.md)):
  the hub follows every stack's worktrees and tells the dashboard when one
  is added, removed or switches branch, so the sidebar shows it without
  opening its stack first. A stack that does not run lists its worktrees
  as lanes not started; one opens a start card, and *Start here* or *Start
  in the pool* brings its daemon up and starts the lane, the main working
  tree's services left stopped. A lane the branch picker makes for a
  stopped stack opens there.
- **A stack's dot in the sidebar sums up its trees**: folded, it shows the
  worst of its trees' dots (a failed start first); unfolded it has none, as
  its trees show theirs. The main working tree's and a lane's dot are red
  when one of their services fails (as last seen), yellow for trouble on a
  lane's node. A stack whose daemon runs
  but nothing of it says `idle`, as one without a daemon says `offline`.
- **The sidebar keeps its order**: stacks by name, and a stack's trees
  with the main working tree first, then by name; a start or a stop no
  longer moves anything.
- **The tab follows you**: opening another stack or lane keeps the tab
  shown (Infrastructure, Commands, Usage, Activity), also coming back from
  the pool area, instead of going back to Overview.

### Fixed

- **`devstack up --on` moves a started lane**: a lane started on a node
  stayed there after `devstack down` (its lease and place stay, only its
  services stop), and `devstack up --on local` started it there again.
  `--on local` now brings it here, and `--on <node>` takes it to that node,
  as `devstack lane move` does; without `--on` it stays where it runs.
- **The MCP tool `up` does as `devstack up`**: in a worktree without `on`
  it starts the lane where this machine starts lanes by default (*Where
  lanes start*), not always here, and an `on` that names another place
  than where a started lane runs moves it there.
- **A lane on a node picks up edits**: its dev servers ran from a copy of
  the worktree that its daemon there never brought up to date, so hot
  reload showed the code of the lane's start; the file watcher took the
  lane's daemon on the node for a main tree, which runs in place.
- **Lanes on nodes survive a restart of this machine** (#379): a node
  ended a lane two minutes after its requester went quiet, so a reboot or a
  sleep lost every lane on nodes and the work running in them. A lease now
  lives an hour without renewals on a dedicated node, 15 minutes on another
  (`lease_grace_minutes` in a node's policy), and the daemon follows its
  lanes there again when it starts.
- **The pool's capacity history survives a restart of the coordinator**
  (ADR-0036): it keeps it in `history.json` and writes it every minute.
  The chart puts each point at its time, so a short history fills only its
  part of the hour or day, and a time without points stays a gap.

## [0.30.0] - 2026-10-29

The skills of a pool's set can be taken one by one: each switched on or
off, a group at once, grouped and tagged by their own frontmatter, and the
Skills page shows each set in its own card with its skills, when they were
written here and into which agents (ADR-0077). The pool area has tabs and
each machine a page of its own, and starting a lane shows where it would
go (ADR-0076). Fixed: a GitHub skill set's commit, an agent installed
since not showing, and dashboard details.

### Added

- **Skills one by one** ([ADR-0077](docs/adr/0077-a-sets-skills-are-grouped-and-switched-one-by-one.md)):
  each skill of an accepted set can be switched off and on again, a group
  at once; all are on when a set is accepted, and a skill the set gains
  later too. A skill's `metadata.group` and `metadata.tags` in its
  frontmatter group and tag it. The Skills page lists every skill as a
  row by group, with search, filters by set, state and group, when it was
  written here and into which agents, and its detail on a click.

### Changed

- **The pool area has tabs, and each machine a page** (ADR-0076): a pool
  shows *Overview*, *Machines*, *Lanes* and, for admins, *Admin* (requests
  to join, provisioning, the pool's MCP upstreams and skill sets, the admin
  log), instead of fifteen cards on one page. A machine's page holds its
  membership and its node side together, with what it reports, its policy,
  its capacity over time, the lanes on it and every admin action on it;
  the nodes and members tables are one list. *This machine* shows what
  holds in every pool: what this machine has and has free, its capacity
  over time, how each pool sees it, the lanes of others it runs, its own
  lanes on other machines, and where its lanes start. The overview's
  capacity shows each node as starting a lane does, and the pool's history
  stacked machine by machine, one colour each, against what the pool
  offered; a machine's page shows its own the same way. A badge sits where
  what waits for an admin is handled. The pool and the tab are in the URL
  (`#/pool/<pool>/machines/<id>`); `#/pool` still opens the pool shown
  last.
- **Starting a lane shows where it would go**: its machines are grouped by
  pool in their coordinator's order; a card opens the machine's page and
  its own *Start* starts the lane there; a pool's start names its pick, and
  pointing at a start button lights the machines it means.

### Fixed

- **A lane not started shows no tabs**: its start options stand in for its
  views, so Overview, Infrastructure and the others no longer all led to
  the same start page.
- **A tree not started says so first**: a dashed box across the page
  above its start options, which no running tree shows; the stack's
  offline banner no longer repeats it.
- **The pool's devstack column** no longer shows a stray "–" after a
  node's version when no update runs.
- **A GitHub skill set names its commit**: the set showed
  `pax_global_header` where its commit belongs, the name of the first
  entry of GitHub's archive (ADR-0074).
- **An agent installed since shows on the Skills page**: the hub looks
  again which agents are on the machine when the page opens, instead of
  only after another change.
- **"Check for updates" is hidden once an update is found**, beside
  "Update to X" in the version popover (#370).

## [0.29.0] - 2026-10-29

A pool hands out agent skills: an admin names skill sets (a GitHub
repository read with the coordinator's own token, or a folder on it), and
each member's hub installs the ones its user accepted into the user skill
folders of the coding agents switched on, never into a repository; a push
reaches every member within seconds (ADR-0074). Members no longer poll the
coordinator: one stream tells them which parts of the pool changed
(ADR-0073). Where a lane can start says what it takes once up beside its
peak (ADR-0075), and a lane without a lane image is refused before its
lease. Fixed: PowerShell from the dashboard, Zed on Windows, the MCP
upstreams page after a login, and dashboard details.

### Added

- **A pool hands out agent skills** ([ADR-0074](docs/adr/0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md)):
  an admin names skill sets (`devstack pool skills add`, or the pool
  area): a GitHub repository, read with the coordinator's own token, or a
  folder on the coordinator, given to every member, member ids or tags.
  Each member's hub offers them on the new *Skills* page and, once
  accepted, installs them into the user skill folders of the coding agents
  switched on there (Claude Code, Codex, OpenCode, Copilot, Cursor and
  some sixty more): no repository is touched. Updates come at once or wait
  for the user, per set; a skill of the user's own, or one changed by
  hand, is left alone. A push reaches every member within seconds through
  a signed `POST /v1/skills/refresh` (Hookaido's signature). "Open with"
  takes its agents from the same agent table.

### Changed

- **The pool is live, and nobody polls the coordinator** (ADR-0073): the
  coordinator tells its members over one stream (`GET /v1/events`) which
  parts of the pool changed, and each hub reads a part only then. The
  pool area, the capacity charts, where a lane can start, the guest lanes
  on this machine and the pool's MCP upstreams follow within about two
  seconds instead of every 10 s to a minute; a member removed from the
  pool loses the pool's upstreams at once. Update the coordinator first:
  with one of an earlier release the pool area is read when it opens and
  says it is not live.
- **A lane's need says what it takes once up** (ADR-0075): beside the
  peak, which includes its build, a node measures the 95th percentile of a
  lane's CPU and memory after it is up. Where a lane can start, the
  dashboard and `devstack pool placement` say "starts with up to 8.7 CPU
  and 7.2 GB, its build included, then runs on about 0.8 CPU and 2.1 GB".
  Admission is unchanged. Shown once a node of this release measured a run.

### Fixed

- **The MCP upstreams page follows a login**: after an OAuth login in the
  other tab, the list shows the upstream logged in by itself (it asks the
  hub every two seconds, and at once when you come back to the tab),
  instead of only after a reload.
- **Zed opens the folder on Windows**: the path menu's Zed opened an
  empty window there, as the link handed it `/D:/...`; it now hands
  `D:/...`.
- **PowerShell from the dashboard stays open**: the folder menu's
  PowerShell closed again at once, as it inherited the hub's empty input;
  it is started as a double-click starts it now (#359).
- **A lane without a lane image is refused before its lease**: a stack
  file that names neither `[lanes] image` nor `[lanes] dockerfile` cannot
  run on a node; the start in the pool is refused at once, placement marks
  every node not eligible with why, and the dashboard offers a prompt that
  names a fitting image (#366). devstack's own stack file names one (#364).
- **Dashboard details**: CPU says it is a share of one core (400 % is four
  busy), the lane card's figures have icons, the pending count takes the
  colour of what is pending, and a service's or container's detail closes
  with ×, Escape or a second click on its row (#361).

## [0.28.0] - 2026-10-28

A restart is warm, a rebuild or cold, from every restart button and the
command palette, and a lane restarts where it runs, on a node too
(ADR-0072); a group's start and stop take its containers along. A pool
can name MCP upstreams for its members, which each member's hub offers
and uses once its user accepted them, with that user's own login
(ADR-0069 option B). `devstack up` waits for the containers too.
Fixed: a lane's journal on a node, the dashboard's row columns, a node's
refusal that did not say what other starts hold, and logins lost in the
Windows Credential Manager.

### Added

- **Restart, Rebuild, Cold** ([ADR-0072](docs/adr/0072-a-restart-is-warm-a-rebuild-or-cold.md)):
  every restart button in the dashboard (a service row, a group's head, a
  lane's header) is a split button. A click restarts warm, from the build
  there is for the inputs, the data kept; its arrow offers Rebuild (built
  again even for inputs built before, the data kept) and, for a lane,
  Cold (built again and seeded again; asks first). The command palette has
  them too.
- **A lane restarts where it runs**: `POST /v1/lanes/{lane}/restart` with
  `{"mode": "warm" | "rebuild" | "cold"}`. On a node a warm restart and a
  rebuild go through the lane's own API, so its lease, node and ports
  stay; a cold restart stops it, throws its data away and starts it again
  at the same place. A group has `rebuild` too
  (`POST /v1/groups/{name}/rebuild`, `devstack restart --rebuild <group>`).
- **A lane's actions in one menu**: the header's Restart (Start for a
  stopped lane) has Rebuild, Cold…, Stop, Discard data… and Remove
  worktree… in its menu; the same menu opens from a lane's "…" button or a
  right click on its row in the branch picker. The separate "Stop
  worktree" and "Discard data" buttons are gone.
- **A pool names MCP upstreams for its members** (ADR-0069 option B):
  `devstack pool upstreams add|remove` (admins) or the pool area of the
  dashboard keep a list of URLs on the coordinator, never a secret or a
  command, each change in the audit log. Each member's hub offers them on
  its *MCP upstreams* page, from which pool, and uses one only once its
  user accepted it, with that user's own login; a new URL is accepted
  again, the user's own upstreams go first, and an entry the pool drops
  goes with its login.

### Changed

- **A group's start and stop take its containers along**, as its restart
  did: `devstack start|stop <group>` and the group's buttons start its
  containers before its services and stop them after (a lane leaves the
  main tree's `shared` ones alone). A group of containers only gets its
  buttons in the overview now. A lane on a node needs a node with devstack
  0.28.0 or later for this.

### Fixed

- **A lane on a node names its requester in its journal**: what the
  requester's daemon asks of the lane's daemon (a restart of its
  services, its start) showed as "by an unknown client" in that lane's
  *Activity*; it now shows as "by devstack". The client a person used is
  in the requester's journal.
- **A node that cannot take a lane says what other starts hold there.**
  For a minute after it is offered, a lease for another lane holds its
  room on its node; the node's figures in the dashboard do not count it,
  so a node with enough free read "cannot take it: offers 3.9 CPU …". The
  reason now adds "4.8 CPU and 1802 MiB more are held for a lane starting
  there".
- **A lane on a node keeps its journal** (#287): the requester's take of
  the lane daemon's telemetry lines, every 30 seconds, went into that
  daemon's journal as "take the telemetry lines by an unknown client" and
  pushed the lane's real changes out within about four hours; its log
  line was also the next take's telemetry. The take is no longer
  journaled or logged as a request.
- **The dashboard's rows line up**: a service's or container's usage,
  pin, state and actions stand in fixed columns, so they line up from row
  to row and under the group's usage in its head, whatever the ports and
  names; a row without usage keeps the column. The *Infrastructure* page
  shows its Compose groups one under the other at full width instead of
  in narrow columns that cut the container names short.
- **`devstack up` waits for the containers too** (#66): it waited for the
  autostart services only, so a stack of Compose groups alone answered
  right away while its images were still built and its containers started.
  It now follows every container its groups start (the group's
  `services`, or every container of its project; not those a profile
  skips, nor in a lane the `shared` ones) until it runs and, with a health
  check, is healthy, or ran to completion. A container not ready 300 s
  after the services, or a group whose Docker error holds for 10 s, fails
  the `up` with its state. The MCP tools `up` and `lane_start` wait the
  same way.
- **The dashboard's MCP upstreams page** has the spacing of the other
  cards: a header with the Add button, the text and the form inside the
  card's padding instead of on its border.
- **Logins in the Windows Credential Manager** can no longer be lost when
  two are written or removed at once (seen in the tests; parallel tool
  calls refreshing tokens could do it): the hub makes one keychain call at
  a time.

## [0.27.1] - 2026-10-28

0.27.0 as tagged: its release stopped before publishing, at advisories
that came out the same day. Fixed: the release's dependency check.

### Fixed

- **The release's advisory check** passes again: RUSTSEC-2026-0370,
  -0371 and -0372 mark zbus 5's `zvariant`, `zvariant_derive` and
  `zbus_names` unmaintained (they went into zbus 6; no vulnerability).
  Only the Linux keychain pulls them in (keyring 4.2 through
  secret-service 5.2, ADR-0069), which has no release on zbus 6 yet;
  `deny.toml` ignores the three until it has.

## [0.27.0] - 2026-10-28

A pool grows by machines a provisioner makes, for members an admin
allowed and within caps (ADR-0051). The MCP servers a user works with are
configured once in the hub, which logs in to them and keeps the login in
the keychain; `devstack mcp` offers their tools in every coding agent
(ADR-0069). The dashboard's Activity page shows the live events and the
journal in one timeline, with who changed what (ADR-0067), and the
dashboard writes no file of a repository any more: a change there is a
prompt for a coding agent (ADR-0071). A stack starts sooner: services
build while the containers start, what is independent starts side by
side, and a node keeps a member's build artifacts for the next lane. The
card of a lane not started compares this machine with the pool's nodes.

### Added

- **Where a lane starts, compared** (ADR-0036 addendum): the card of a
  lane not started shows this machine first, beside the pool's nodes, and
  per machine one chart for CPU and one for memory on the scale of all it
  has: the last hour with what a node offers (dashed), and now with the
  zone the lane would take on top, red where it does not fit. The lane's
  need is the coordinator's (its footprint in the pool, else the
  estimate): `GET /hub/pool/placement` gives it as `need` and `need_from`.
- **A pool grows by machines a provisioner makes** (ADR-0051): with
  `devstack node serve --provisioner <url>`, a lane the pool places that
  no node can take makes a machine through an HTTP service of the pool's
  own (it holds the cloud credentials, devstack none), for a member an
  admin allowed (`devstack node members provision <id> on`) and within
  caps (`devstack node provisioning`). The start waits for it as a phase
  of its own (`provision`, at most 10 minutes). The machine enrols once
  with a single-use token, only from the address the provisioner named
  (`devstack join --enrol`); a second use, or one from elsewhere, revokes
  it, deletes the machine and alarms the admins. Optionally an admin
  confirms each machine (`--confirm admin`, `devstack node confirm`), or a
  machine runs one repository's lanes (`--per repo`). Placement fills
  provisioned nodes before another is made; an idle one is given back
  after 30 minutes (`--idle-minutes`), deletions retried until confirmed,
  and the provisioner is told of every machine and lease to bill from. A
  member keeps its lanes off them with `devstack pool provisioned off`:
  whoever runs the provisioner can read the lanes there. The contract is
  in [pool.md](docs/pool.md#grow-by-provisioned-nodes); `devstack node
  mock-provisioner` (hidden) tries it on one machine.
- **MCP upstreams in the hub** (ADR-0069): the dashboard's new
  page *MCP upstreams* keeps the MCP servers a user works with once, per
  user: a URL, a prefix, which tools, and how it logs in. The hub logs in
  itself, with OAuth (it finds the authorization server, registers where
  the server allows it, PKCE; the browser comes back to the hub) or a
  token the user gives it, and keeps the login in the keychain (a file on
  a Linux machine without a Secret Service). `devstack mcp` offers their
  tools in every coding agent as `<prefix>__<tool>`, beside its own, and
  calls them through the hub, which starts when it does not run; the token
  never leaves the hub. One that is logged out or unreachable keeps the
  tools it listed last and answers with the dashboard's page; a change of
  the tools is announced (`notifications/tools/list_changed`). Without a
  hub (`DEVSTACK_NO_HUB`), upstreams without a login or with a token from
  the environment are asked directly. An upstream can also be a command the
  hub starts (stdio, `npx -y …`), as the user, with a base environment and
  only the variables set for it there, a token the hub keeps given in a
  variable of its choice; never a command from a repository or the pool.
- **The dashboard shows who changed the stack** (ADR-0067, addendum
  2026-10-27; #331): the Events page is now *Activity*, one timeline of
  the live events and the journal, which it loads on opening and so shows
  changes from before the page connected, each with its client (CLI, MCP
  server, dashboard, tray, devstack itself) and, refused, with the reason.
  Filters by client and lane. A lane on the main ports says since when and
  through what it holds them; a service's details its last action.
  `/v1/events` carries each new journal entry as a `journal` event, and
  whoever views a shared lane may read its `/v1/journal`.

### Changed

- **A stack starts sooner** (measured in the pool's telemetry: a lane on a
  node took about 154 s, of which 51 s waiting for its containers before
  any build, and 37 s building what an earlier lane had built). The
  services are built while the containers start, Compose groups that no
  `after` orders start side by side (here and on a node), and services
  whose `after` is done start side by side. A node keeps the member's
  build artifacts with its store (ADR-0062 addendum): the member's next
  lane with the same inputs copies them instead of building.
- **The dashboard writes no file of a repository** (ADR-0071): "Refresh
  skills", "Create draft" in "Add a stack" and "Copy the main tree's" for a
  worktree without a stack file are gone. Each is a prompt for a coding
  agent now (the new `skills` prompt, `setup`, `lane-file`), started in the
  user's agent or copied; the agent reads what it writes. While the stack
  file is behind, its banner speaks for the agent skills too, and hiding it
  hides both until the next minor devstack (before, the skills banner came
  up in its place). The hub's `POST /hub/stacks/init`,
  `POST /hub/stacks/{id}/skills` and `POST /hub/stacks/{id}/worktrees/stack-file`
  are gone. `devstack init` and `devstack skills install` stay in the CLI.

## [0.26.0] - 2026-10-27

Setting a repository up offers devstack's MCP server, and the dashboard
says when a stack's agents lack it. The dashboard gets a Usage tab that a
lane switch keeps, and a quieter "stack file behind" banner that can be
hidden until the next minor; agent skills count as outdated only when their
text changed. `devstack up --workers` runs the workers of a lane on a pool
node too (#314). Fixed: a lane's memory check looks at Docker Desktop's VM
(#309), a refused start on the pool says how to go on (#310), an emptied
linked `node_modules` is prepared again and the skills warn about it
(#311), and the upgrade prompt checks the version and asks before a
restart (#312).

### Added

- **Setting a repository up offers devstack's MCP server**: the setup
  prompt of "Add a stack" has the agent ask whether to register
  `devstack mcp` in the project's agent configuration (`.mcp.json`,
  `.vscode/mcp.json`, `.codex/config.toml`), suggesting yes; the upgrade
  prompt offers it where no entry is there. The stack's head in the
  dashboard shows *no MCP* when neither the project's nor the user's
  configuration registers it (`GET /hub/stacks/{id}/mcp`); its menu holds a
  prompt for a coding agent that adds the entry, or hides the label for the
  stack.
- **The dashboard's Usage view has a tab** beside Commands and Events. A
  switch to another lane of the same stack (or back to the main working
  tree) keeps the tab shown: from Usage of one lane to Usage of the next.
  A service's or container's detail goes to the overview, a command's run
  to the command list, since both belong to the lane left.

### Changed

- **The dashboard's "stack file behind" banner is one quiet line and can be
  hidden**: neutral instead of blue, one "Bring up to date" button whose
  menu holds the other agents, the copy and the prompt, and it says that the
  agent skills are older too instead of a second banner. Hidden, it stays
  hidden for that stack until the next minor devstack is installed; a patch
  release does not bring it back.
- **The agent skills count as outdated only when their text changed**: a new
  devstack whose skills read the same, apart from the version in their marker
  line and docs links, no longer asks for a refresh (dashboard, `status`,
  `doctor`, `skills check`).

### Fixed

- **`devstack up --workers` runs the workers of a lane on a node too**
  (#314): the flag never reached the node, so a worker gated by
  `{is_main}` stayed off there and its jobs stayed queued, with nothing
  saying so. It goes with the lane's start to the node's agent and its
  daemon (a profile's `workers` too), stays with the lane when it moves or
  the stack restarts, and `devstack lane list` says which lanes run their
  workers. A node before this release does not take it: `up` warns.
  `up --workers` on a lane that runs already says it applies at its next
  start, on a node as here.

- **A lane's memory check looks at Docker Desktop's VM** (#309): on Windows
  and macOS the check compared the whole need with the host's free memory,
  while the containers run in Docker Desktop's VM with a limit of its own;
  a lane that did not fit there started and the engine hung. The
  containers' part is now checked against what is free in the VM, the
  services' part against the host, and the refusal names the limit.
  `devstack doctor` shows the VM's memory, what the running containers
  use of it and the host's free memory.
- **A refused start on the pool says how to go on** (#310): the error now
  names `devstack pool placement`, another node and `devstack up --on
  local`, and when the pool is the machine's default, that running here is
  the user's call. `--on <this machine's name or id>` runs the lane here
  instead of being refused as a node not granted lanes. The skill's rows
  say the same.
- **The skills warn about a linked `node_modules`** (#311): a directory
  `prepare.link` puts in a worktree is the lane's copy; reinstalling or
  deleting it there empties what the lane runs from. The `devstack` and
  `devstack-config` skills say so and how to get one's own. devstack
  counts the entries of a linked directory once prepared; one that lost
  more than a tenth of them is prepared again (a local lane at its
  service's next start, with a notice and `prepare_copy_emptied` in the
  log; a lane on a node at the next look, into its other copy).
- **The upgrade prompt checks the version and asks before a restart**
  (#312): it starts with `devstack --version`, and before restarting the
  stack it has the agent look for lanes and other sessions and ask; the
  restart keeps the containers (`down --keep-infra`).

## [0.25.0] - 2026-10-27

A stop takes the stack's containers too, as stopping a lane already did;
`--keep-infra` leaves them running (ADR-0068). Who changed the stack at
runtime, and through which client, is on record: `devstack journal`, and
`devstack status` says who gave a lane the main ports (ADR-0067). Fixed: a
lane on a node keeps a whole `node_modules` while new inputs are prepared
(#305), a measured CPU need no longer locks a repository out of the pool
(ADR-0066), the dashboard's banners are spaced apart.

### Added

- **Who changed the stack at runtime is on record** (ADR-0067): the CLI,
  the MCP server, the tray and the dashboard (through the hub) say which
  client they are, and the daemon keeps the last 500 changes with it in
  `.devstack/journal.ndjson`: `devstack journal`, the MCP tool `journal`,
  `GET /v1/journal`. `devstack status` says who gave a lane the main ports
  and when, and who stopped a service last. An agent can tell a user's
  decision from a fault; the skill tells it to look there first.

### Changed

- **A stop takes the containers too** (ADR-0068): `devstack down`, the
  dashboard's stop and the MCP tool `down` remove the stack's containers
  along with its services, as stopping a lane already did; their volumes,
  and with them the data, stay. `devstack down --keep-infra` (the box "Keep
  its containers running" in the dashboard, `keep_infra` on the MCP tool,
  `infra=false` on the API) leaves them running for a faster next start.
  `--infra` is still accepted and does what `down` does now. **Breaking**
  for a script that relied on `down` keeping the containers: it adds
  `--keep-infra`. A restart of the stack still keeps them.
- **Lanes that run keep only what they use** (ADR-0068): the main tree's
  stop while lanes run used to keep every one of its containers, also with
  `--infra`. It now removes them except the `shared` ones a lane on this
  machine uses (ADR-0030) and those a `lanes = false` service comes
  `after`, and says which stayed.

### Fixed

- **A lane on a node keeps a whole `node_modules` while new inputs are
  prepared** (#305, ADR-0063 addendum): when the lockfile changed in its
  worktree, `prepare` ran `npm ci` in the very copy the worktree was linked
  to, which empties `node_modules` first; a `prepare` that then failed left
  the worktree with a few packages and nothing in the log. The new inputs
  are prepared in a second copy and the link moves once it is whole; a
  failure keeps the copy it had (`linked (stale)`, with why) and is logged
  (`prepare_failed`).
- **A measured CPU need no longer locks a repository out of the pool**
  (ADR-0066): a build takes every core that is free, so a lane's measured
  CPU peak rose with each run on an idler node until it was more than any
  node offers; then no lane of the repository started, and with no run the
  footprint never came down. A node now counts at most half of the most it
  offers its lanes (its share of its cores, at most `--max-cpus`), and the coordinator
  counts CPU from such runs only: a repository locked out already starts
  again once the coordinator is updated, with no edit of `footprints.json`.
- **The dashboard's banners are spaced apart** (#301): notices, the upgrade
  and the skills banner above a stack sat directly on top of each other.

## [0.24.1] - 2026-10-27

Fixes of warming the store (ADR-0065): a second service with the same
`prepare` warms and shares the first one's entry instead of failing on its
`version`, and a warm lease on a node gets the ref's files, so a stack with
`[lanes] dockerfile` builds its lane image there.

### Fixed

- **A second service with the same `prepare` warms, and shares the entry**
  (#297): a service whose `prepare` never ran here had no log directory, so
  its `version` (`node --version`) failed with "path not found" and it was
  not warmed, again at every commit. Its log directory is made first. A
  store key now covers the `outputs` too, and a service finds the entry
  another service made with the same `run`, `cwd`, inputs, outputs and
  `version`, instead of running `npm ci` once more. Entries made before
  have another key and are not hit once more; the sweep removes them.
- **A warm lease on a node gets the ref's files** (#298): its checkout lies
  under `.devstack/`, which git ignores, so the sync sent nearly nothing,
  and a stack with `[lanes] dockerfile` failed to build its lane image
  ("dockerfile not found"). The files come from the checkout's own index.

## [0.24.0] - 2026-10-26

A lane on a pool node now gets what its `prepare` makes without a second
install: its worktree here is linked to a copy from this machine's store, so
a pre-commit hook with `npx eslint` runs there (ADR-0063), and the node keeps
a store per member and repository, so the next lane with the same lockfile
copies `node_modules` instead of running `npm ci` (ADR-0062). With
`[prepare] warm = ["origin/main"]` the store is filled ahead for the main
branch, here and on the nodes the repository's lanes use (ADR-0065), and the
dashboard shows where each `prepare` came from and what the store holds.
The daemon in a lane's container sends its own lines through its requester,
under the member's consent (ADR-0037 addendum).

### Added

- **The store is warmed for a ref before a lane needs it** (ADR-0065,
  #290): `[prepare] warm = ["origin/main"]`. devstack reads the ref every
  minute (it never fetches); a new commit, quiet for two minutes, is
  checked out under `.devstack/warm/` as git checks it out, with an index
  of its own, and each service's `prepare` with `outputs` is made into the
  store unless it has the key, in the background. The nodes this
  repository's lanes ran on get a warm lease that runs only `prepare` into
  the member's store there and ends; a node's policy can refuse them
  (`warm = false`), a node before 0.24.0 is not asked. A file with `warm`
  says `requires = "0.24.0"`.
- **The dashboard shows the store of what `prepare` makes** (ADR-0062,
  #291). A service's detail view says where its last `prepare` outputs came
  from: copied out of the store (the key, the copy's time, what `prepare`
  took when it made the entry) or a run (its time, and why the store gave
  nothing: no entry for these inputs, copying slower than running, `version`
  failed, an output git tracks). The usage page has a line with the store's
  entries and size, which opens the list (service, key, outputs, size, last
  used, which lanes use it) with a remove action per entry. The pool view
  shows a node's store beside what it keeps. API: `ServiceView.prepare`,
  `GET /v1/store`, `DELETE /v1/store/{service}/{key}`, and `kept.store_bytes`
  in a node's report (older nodes omit it).

- **A pool node keeps a store of what `prepare` makes** (ADR-0062, #277),
  per member and repository: a lane on the node reads it (read-only) and
  copies an entry with the same inputs, platform and lane image instead of
  running `npm ci`; what it prepares goes into its slot's outbox, and the
  node takes it into the store within 15 s. Entries go after the node's
  retention for kept lanes, count with the member's kept lanes (the least
  recently used go first over the room) and go with the member. Needs the
  node on this version.

- **The worktree of a lane on a node gets `prepare.link` too** (ADR-0063,
  #277): a pre-commit hook with `npx eslint` there no longer needs a
  hand-run `npm ci`. With `outputs`, devstack checks the worktree's inputs
  every 15 s; when their key is new it copies this machine's store entry
  into `.devstack/lane-<name>/linked/` or runs `prepare` there once, keeps
  the result in the store, and links the worktree to it (built for this
  machine, not the node's platform). `devstack lane list`, `status` and the
  dashboard's lane header say per path `linked`, `linked (store)`,
  `preparing` or `not linked` with why. The links go when the lane stops,
  moves or goes.

- **A lane on a node is in the pool's telemetry from the inside** (ADR-0037
  addendum, #287): its own daemon sent nothing, having no membership. It now
  keeps its diagnostic lines (the newest 5,000), and the daemon that started
  the lane takes them every 30 s and sends them with its own, under its
  consent, as lines of that lane on its node (`place` `remote`): its
  services, builds, `prepare`, notices and failed requests. Needs the node
  on this version too; an older node's lane answers `404` and is skipped.

## [0.23.0] - 2026-10-26

One `npm ci` per lockfile and machine instead of one per lane: what
`prepare` makes is kept in a store per machine and repository, keyed by its
inputs, the platform and the toolchain's version, and the next lane or build
with the same key copies it (ADR-0062; on Windows 5 s instead of 23 s for a
`node_modules` of 24,500 files). Compose files are told the lane they run
for, and a lane on a node gives its native services its name (ADR-0064), so
a login cookie stays apart per tree in containers too. Fixed: a lane on a
node keeps its lease through a long first sync and is said when a reboot
lost it; a Linux tray menu shows no stale item.

### Added

- **`prepare.outputs` and `prepare.version`** (ADR-0062, #277), checked by
  `devstack check`: the directories `prepare` makes, inside the repository,
  and a command whose output keys them (`node --version`), for the store
  that keeps them per machine and repository so a second lane with the same
  lockfile skips `npm ci`. With `outputs`, each `link` is one of them or
  inside one; `version` needs `outputs`. A file with either says
  `requires = "0.23.0"`.
- **What `prepare` makes is kept in a store** (ADR-0062, #277): with
  `outputs`, a lane or build whose `prepare` has the same `run`, `cwd`,
  inputs, platform and `version` as one before copies its directories from
  `.devstack/store/` instead of running it, 32 files at once. Measured on
  Windows: a `node_modules` of 24,500 files copies in 5 s where `npm ci`
  took 23 s (NTFS), and in 2 to 3 s where it took 45 s (Dev Drive). Two
  copies preparing the same key prepare it once. Entries no copy used for
  14 days go; `devstack lane prune --older-than` removes them sooner.
  Lanes on a node do not use a store yet.
- **Compose files are told the lane** (ADR-0064, #280): every `docker
  compose` call devstack makes for a group, and every `exec` step of a
  command, gets `DEVSTACK_LANE` (`main` in the main tree) and
  `DEVSTACK_LANE_SUFFIX` (empty there, `_<lane>` in a lane, as
  `{lane_suffix}`), on a node with the lane's name as its developer calls
  it. A container keeps a login cookie apart per tree with
  `app_refresh${DEVSTACK_LANE_SUFFIX:-}`, and the files give the same value
  without devstack. `devstack check` warns about either read without a
  default and about a `DEVSTACK_LANE_*` devstack does not set. No stack-file
  key: no `requires`.

### Fixed

- **A lane on a node gives its native services its name** (ADR-0064
  addendum, #284): `{lane}`, `{lane_suffix}` and a native process's
  `DEVSTACK_LANE` were `main`, empty and `main` there, because the lane's
  daemon runs as a main tree, while its Compose calls were told the lane's
  name. They now have the values a local lane of that name has, in services
  and in commands, so a login cookie named with `{lane_suffix}` no longer
  collides with the main tree's on `localhost`. `{is_main}`,
  `DEVSTACK_SLOT` and `{compose.<group>.project}` stay the node's own.
- **A pool call that only reads survives a connection the other side
  broke**: `devstack join`, asking whether this machine is admitted, and
  what a member reads from the coordinator ask once more when the
  connection broke before an answer, instead of failing. On macOS this
  showed as `Invalid argument (os error 22)`.
- **A lane on a node keeps its lease through a long first sync** (#276):
  a start whose first sync took 90 s lost its lease, since nothing renewed
  it between the grant and the first renewal of the followed lane; it is
  renewed from the grant on. A file that changed between hashing and upload
  no longer makes the node refuse the whole sync, a sync the lane's end
  overtakes is dropped instead of reported as failed, and a lane being
  removed (or whose worktree lost its `.git`) syncs no more, so a worktree
  on its way out is not mirrored to the node as deleted.
- **A lane on a node lost to a reboot is said** (#275, ADR-0052 point 6):
  after the developer's machine restarted, the next daemon showed such a
  lane as merely stopped. It now finds the note the lane left while it was
  followed, releases the lease if it still runs, and says with the lane why
  it is gone and how to start it again. The first sync to a node hashes
  files without a cached hash on several threads (22 to 90 s for 7,200
  files on a machine with a slow virus scanner), and `node_lease_ended`
  says why: released, expired or ended by an admin.
- **A Linux tray menu item keeps no label of the item before it** (#272):
  when a stack started and its submenu grew, GNOME's AppIndicator extension
  showed a second, greyed `Quit Tray`; each new menu layout now numbers its
  items after the old one's.

## [0.22.1] - 2026-10-24

Fixes of the tray: it comes back after an update on Linux, its menu on
Windows has no empty column and even marks, and what it opens comes to the
front on Wayland where the browser lets it. The overview's last row keeps
the card's round corners.

### Fixed

- **What the tray opens comes to the front on Wayland**, where the
  browser lets it: the tray takes the activation token GNOME and KDE offer
  with a click (`ProvideXdgActivationToken`) and hands it to the program
  the click starts (`XDG_ACTIVATION_TOKEN`, `DESKTOP_STARTUP_ID`), once,
  and only within ten seconds of the click. A dashboard opened in a browser
  tab now comes forward instead of GNOME saying "… is ready". The installed
  app already open in the background still gets that notice: Chromium
  applies a handed-over token to new tabs only, not to an app window it
  reuses. *Open Folder*, which the hub opens, is not covered yet.
- **The overview's last row in a group keeps the card's round corners**:
  a selected or hovered row at the end of a group (a group of one service,
  say) ended square in the card's rounded bottom.
- **The tray's menu on Windows: no empty column, even marks.** The menu
  kept a column for check marks it never shows, and the marks before each
  stack (`●`, `○`, ...) came from different fonts, in different sizes. A
  stack's state is a dot in the icon's colours now, in a column of its own;
  the other menus have no column. Linux and macOS keep the marks as text.
- **The tray comes back after an update on Linux.** The tray of the old
  devstack hands over to the hub's new one by starting itself again, but
  Linux names a binary replaced while it runs `<path> (deleted)`: the start
  failed ("No such file or directory"), and the new hub's tray had already
  given way to the old one, so none was left. It starts the binary at its
  path now; and a hub whose tray gave way to an older one starts its own
  once that one is gone, so an update from 0.22.0 or earlier keeps the
  tray too. On 0.22.0, `devstack hub tray on` brings it back.

## [0.22.0] - 2026-10-24

The dashboard opens in its installed app (ADR-0061): with the dashboard
installed in Chrome, Edge, Brave or Chromium, the tray, `devstack ui` and a
notification open it there, on Linux and Windows, instead of a browser tab.
Fixed: the tray's menu on Windows follows dark mode; a lane its owner threw
away on a node no longer comes back.

### Added

- **The dashboard opens in its installed app** (ADR-0061). With
  the dashboard installed as an app in Chrome, Edge, Brave or Chromium, the
  tray's *Open Dashboard*, `devstack ui` and a click on a notification open
  it there instead of in a browser tab: on Linux found by its launcher (the
  applications folder and the desktop folder), on Windows by the browser
  profile it is installed in; not on macOS yet. The app is recognised by
  the id Chromium derives from the hub's origin. The manifest asks for the
  open window to be reused (`launch_handler`), and names its `id`.
  `--browser`, `DEVSTACK_BROWSER` and `BROWSER` still win.

### Fixed

- **A lane its owner threw away on a node no longer comes back.** When a
  lane was leased again while the end of its previous lease was still
  taking it down, the second lease's end marked the slot free too early: a
  purge then removed it, and the first end wrote it back. The node counts
  what keeps a slot busy now.
- **The tray's menu on Windows follows dark mode.** It was light with the
  system's apps set to dark. It takes the mode of apps now, and follows a
  change while it runs; before Windows 10 1809 it stays light.

## [0.21.0] - 2026-10-23

A tray icon on Windows, Linux and macOS, a client of the hub (ADR-0060). A
profile can leave out containers, whole Compose groups and services
(ADR-0058); a service's `stop` command ends what its `run` started outside
its process tree, a `docker run` container (ADR-0059). The dashboard's
sidebar can be hidden and resized. Fixed: a lane carried over a restart
warned against the previous release; the dashboard's prompts ask which
branch a commit goes on.

### Added

- **The dashboard's sidebar can be hidden and resized.** A button in the
  top bar, Ctrl+B (Cmd+B) or the command palette shows and hides it; on a
  wide screen it stays as it was left, on a narrow one it opens as a drawer
  over the page and closes again when a stack or worktree is chosen, on
  Escape or a click beside it. Its edge is dragged for its width (arrow keys
  too, a double click sets it back), kept across reloads.
- **A tray icon on Windows, Linux and macOS** (ADR-0060, #1). The hub
  starts `devstack tray`: the dashboard's mark in the notification area
  (Windows), as a StatusNotifierItem (Linux: KDE, Xfce, GNOME with the
  AppIndicator extension; the binary stays static) or in the menu bar
  (macOS, from the menu bar client contributed in #248). Green when
  something runs, amber while a stack starts or stops, red when something
  needs attention, grey when nothing runs. Its menu opens the dashboard,
  lists every stack with what needs attention (a click opens that service
  in the dashboard), and starts, stops or opens a stack's folder. It is a
  client of the hub, which says what it shows (`GET /hub/summary`, the same
  as what notifies); quitting it stops nothing. `devstack hub tray off`
  keeps it off, `on` brings it back; `DEVSTACK_NO_TRAY` too.
- **A profile can leave out containers, whole Compose groups and services**
  (ADR-0058, #244). `[profiles.<name>] skip = ["compose.search",
  "compose.app.scanner", "indexer"]` makes a light profile for daily work:
  what it skips does not start with the tree, a skipped container an earlier
  start left running is stopped, and each still starts on demand (`devstack
  start compose.app.scanner`). A service that starts with the profile and
  uses a skipped container's address is an error in the profile's check. A
  lane on a node does not create what its profile skips. A file with `skip`
  wants `requires = "0.21.0"`.
- **A service's `stop` command** (ADR-0059, #245): `[services.<name>]
  stop = "docker rm -f api{lane_suffix}-{port}"` ends what `run` started
  outside its process tree, a container of a `docker run` service. It
  runs whenever an instance ends (a stop, a replacement, a failed start,
  one that ended by itself), before the graceful signal, with the
  instance's placeholders, at most 30 s; after a crash the next start runs
  the ones recorded. `docs/stack-file.md` describes a container as a
  service (`--init`, `--rm`, a name per instance). A file with `stop` says
  `requires = "0.21.0"`.

### Fixed

- **A lane on a node carried over a restart is warned about against the
  devstack that runs it now.** The warning that the node or the coordinator
  runs another release was made once, when the lane was leased, and carried
  over a restart as a text (ADR-0052): after an update to 0.20.0 the lane
  still read "the node … runs devstack 0.20.0, this machine devstack
  0.19.0" while every side ran 0.20.0. The daemon now carries the
  coordinator's release instead and makes the warning again; a lane carried
  by 0.20.0 loses the coordinator's part, which only that daemon knew.
- **The dashboard's prompts ask which branch a commit goes on.** They said
  to ask before committing, not where: a coding agent took the branch
  checked out, which may be another piece of work's. Each prompt now has
  the agent name the branch when it asks.

## [0.20.0] - 2026-10-23

macOS as a developer platform in preview, installed with Homebrew on macOS
and Linux, with arm64 Linux builds (ADR-0054). A lane on a node relays its
UDP ports (ADR-0056); a port a container announces moves with its lane
(ADR-0057); commands reach the tree's own containers
(`{compose.<group>.project}`). A lane whose branch has no stack file says
so (ADR-0055); whoever a lane is shared with connects to it (ADR-0053); the
dashboard's upgrade goes by `[stack] reviewed`. Fixed: `restart_on` for the
files in a folder, ports held on `0.0.0.0` on Windows, the hub and a node
agent reporting over each other, a job run twice for one edit.

### Added

- **macOS, in preview** (ADR-0054, #66): release builds for Apple Silicon
  and Intel, and for arm64 Linux; `install.sh` takes them, and Homebrew
  installs devstack on macOS and Linux (`brew install
  nuetzliches/tap/devstack`), updated through `brew upgrade` by `devstack
  update` and the dashboard. CI runs the checks and the daemon's integration
  test on macOS. On macOS the file watcher takes the real paths FSEvents
  reports (`/private/tmp`, a symlinked project folder) back under the
  repository, and a port another process listens on with every address
  (`*:port`) counts as taken.
- **A lane whose branch has no stack file says so, and can be given one**
  (ADR-0055). Such a lane ran with the main working tree's stack file
  without saying so, and failed on a node of the pool (`the stack file
  'devstack.toml': …`), since the node found none in the synced worktree.
  Now the main tree's file is synced to the node as well, so a lane runs
  with the same file there as here. The branch picker marks a branch without
  one (`stack_file` in `GET /hub/stacks/{id}/branches`), and the lane's page
  says that the lane runs with the main tree's file (`main_stack_file` in
  `GET /v1/lanes`). From there the main tree's file can be copied into the
  worktree (`POST /hub/stacks/{id}/worktrees/stack-file`; never over a
  file, nothing committed), or a coding agent can be started to adapt it to
  the branch (the prompt `lane-file`). A stack file below the repository's
  root (`-f config/devstack.toml`) is now found in a lane at the same path,
  not only at the worktree's root.
- **UDP ports of a lane on a node are relayed** (ADR-0056, #219). A
  container port published as UDP (`53/udp`, a game's voice port) on a lane
  running on a pool node now reaches `localhost:<port>` here like its TCP
  ports. Each client gets a tunnel of its own through the node, which ends
  after 60 s without a datagram; datagrams that arrive before the container
  answers are lost, as on a network, and the client's next one tries again.
  A node with a devstack before 0.20.0 relays no UDP: the lane's page and
  `devstack lane list` name such ports as not forwarded and say why
  (`not_forwarded_reason` in `GET /v1/lanes`). The main slot still moves TCP
  ports only.
- **A port a container announces moves with its lane** (ADR-0057, #222). A
  server that tells its clients its own port (a WebRTC media server's ICE
  candidates, a broker's advertised listeners) sent a lane's clients to the
  main tree's server, since a lane moved only the host side of a published
  port. Name such ports in the Compose group, `announced_ports =
  ["media:7881", "media:7882/udp"]`, each published 1:1 (`7881:7881`): a
  lane moves them inside the container too (`7981:7981`). Every `docker
  compose` call of the group and every `exec` step of a command gets
  `DEVSTACK_PORT_MEDIA_7881` with the number clients reach in this tree;
  pass it to the server's listen and announce settings with the files' port
  as default (`${DEVSTACK_PORT_MEDIA_7881:-7881}`), so the files still run
  without devstack. `{compose.app.media.7881.port}` is `7981` in the first
  lane; a lane on a node works the same. `devstack check` rejects entries
  not published 1:1 and warns about variables misspelt, unused or read
  without a default. A file that uses the key needs `requires = "0.20.0"`.
- **`{compose.<group>.project}`: a Compose group's project as this tree
  runs it** (#222): `<project>` in the main working tree, `<project>-<lane>`
  in a lane. `docker compose -p {compose.app.project} exec -T db ...` in a
  command acts on the lane's own database in a lane; with the project name
  written out it acted on the main tree's. The reference's `reset-db`
  example uses it now. `devstack check` warns about a dev server whose
  folder has a `package.json` and no `prepare`.

### Changed

- **`devstack status` lists Compose containers apart from services**
  (#184): containers have their own `CONTAINER` / `STATE` / `PORTS` table,
  by group; the `SERVICE` table shows only when the stack has
  `[services.*]` entries, which is what "service" means in the stack file
  and in "main ports".
- **The dashboard's upgrade goes by the stack file, `[stack] reviewed`, not
  by the agent skills** (ADR-0046 amended). "Refresh skills" rewrote the
  version the skills name, and the banner that offered a coding agent the
  upgrade went away with nothing done to the stack file. The stack file
  now says which devstack it was last brought up to date with (`reviewed =
  "0.20.0"` in `[stack]`); the upgrade prompt has the agent set it at the
  end, and `devstack init` writes it. A file without it is offered the
  upgrade, from the skills' version while they are older. The two banners
  are apart: the upgrade's, and the skills' with "Refresh skills". New in
  the hub's API: `GET /hub/stacks/{id}/upgrade`. A devstack before 0.20.0
  does not know the key: `devstack check` warns while `requires` is lower.
- **The release adds no second winget pull request for the new package**:
  while `nuetzliches.devstack` is not in the winget catalog and a pull
  request for it is open, a release does not submit another one (each did:
  0.16.0 to 0.19.0 opened four). A version already in the catalog or
  already submitted is not submitted again. The title says *New package*
  until the package is in, *New version* after. A new `winget` workflow
  submits a published release by hand (`gh workflow run winget.yml -f
  tag=vX.Y.Z`), for the version then current once the first pull request is
  merged; `force` submits despite an open one. The steps live in
  `packaging/winget/render.ps1` and `submit.ps1`, used by both workflows.
- **Whoever a lane is shared with connects to it, and disconnects again**
  (ADR-0053, ADR-0035 point 3 amended). Seeing or viewing a shared lane
  opens no port on this machine. *Connect* (in the lane's header, in the
  pool area, `devstack pool connect <lane>`) forwards every service it
  shares at once, each on its owner's port if free here, so its services
  reach each other as on the owner's machine; *Disconnect* (`devstack pool
  disconnect <lane>`) closes them, connections in flight included, and
  frees the ports. Opening a service (a link, `devstack open`) connects its
  lane. Until now each click forwarded one service, on the owner's port,
  until the hub stopped, unseen: the member's own `devstack up` could fail
  on that port. The sidebar dims a shared lane that is not connected; the
  header names each service's port here and one that is not the owner's.
  A lane no longer shared with this machine is disconnected.

### Fixed

- **`restart_on` restarts a dev server for every file in a folder it
  names, and says why** (#221). `restart_on = ["src/api"]` matched only the
  folder's own path, so a file created, saved or deleted in it restarted
  nothing until the OS happened to report the folder itself: on Windows
  seconds to minutes later, on Linux never. A folder in `restart_on` (dev
  servers and Compose services) now covers what it holds, as `src/api/**`
  does, and the restart follows within the 2 s debounce. A change made while
  the dev server starts or restarts restarts it once more when it is up;
  Python's caches (`__pycache__`, `.pytest_cache`, `.mypy_cache`,
  `.ruff_cache`) never count. The daemon log has a `restart_on` line per
  restart with the paths, when the first changed and how long it waited.
- **On Windows, a port another program holds on every address counts as
  taken** (#226). A dev server on `0.0.0.0` or a port Docker Desktop
  publishes held the port unnoticed: Windows lets a bind on `127.0.0.1`
  succeed beside it, and the service devstack started then failed with
  "address in use". devstack reads the system's table of listeners (well
  under a millisecond, no port opened, no firewall prompt).
- **The hub and a node agent on the same machine no longer report over each
  other** (#170). Both reported to the coordinator as the same member; after
  each of the hub's reports the machine showed no node agent and its leases
  came back new, without their sharing. The agent holds `node/lock` while it
  runs, the hub does not report for that pool then (`agent` in `GET
  /hub/pool/share`), and a second agent of the same member is refused.
- **Lanes: commands and dev servers** (#222). A command's `start`, `stop` or
  `restart` step on a `lanes = false` service is skipped in a lane, with a
  line in the run's log, instead of failing the command. When the installed
  devstack is new enough but the main tree's daemon is older, a stack file
  that needs the new one says to restart that daemon ("Restart stack"), not
  to update devstack. A dev server without `prepare` that ends in a lane
  before it is ready says the lane's copy has no `node_modules`. The
  reference adds that `{port}` in a dev server's `env` works in a lane, and
  covers cookies shared across ports on `localhost` and Origin checks.
- **One edit makes one candidate per service, at the same moment** (#233).
  The watcher reports a change more than once (macOS above all), and a
  service could start building 200 ms after another for the same edit: two
  services after the same job then ran the job twice. A change reported
  while `devstack restart` of its service is still under way waits for it,
  and is dropped when the restart built it already.
- **A lane on a node keeps its data across a quick release and lease**: the
  node wrote `slot.json` in place, and the next lease could read it half
  written and start fresh. It is written whole now.
- **The overview's usage card names the lane shown**: for a lane on a node
  it said "Lane main" (the lane's own daemon there calls its tree `main`).
  CPU reads with one decimal up to 100% (4.5% next to 23.0%), whole numbers
  above.
- **A hub that is still starting is no longer taken for another user's
  program.** The hub bound its port before its setup, of which a first
  `docker version` alone may take 5 s on a busy machine or while Docker
  Desktop starts; the port took connections it did not answer, and a CLI
  command in that time (`devstack ui`, `devstack hub status`, `devstack
  open`), which waits 3 s for the hub's answer, said "port … answers, but
  not as a devstack hub of this user" and gave up. The hub now binds its
  port last: what accepts a connection answers it.

## [0.19.0] - 2026-10-22

A restart of the stack carries its lanes over to the next daemon
(ADR-0052). The agent skills go where the stack file says (`[agents]
skills`); the dashboard says when a repository was set up with an older
devstack and starts a coding agent with its prompt (ADR-0046 amended). A
node's policy says what part of its free CPU and memory it offers. A lane
takes the main ports with a service stopped; the hub's failures that only
reached its log are shown; a lane on a node says which UDP ports are not
forwarded.

### Added

- **A node's policy says what part of its free CPU and memory it offers**
  (`share_percent`, 1 to 100; ADR-0031 point 3 amended). Until now a node
  offered half of what is free, below its caps: a dedicated server nobody
  works on can now offer more. An admin sets it in the dashboard's policy
  form ("Offers of what is free (%)") or with `devstack node members policy
  <node> --share-percent 80`; a node's own `policy.json` can too, and the
  smaller holds. `--max-cpus` and `--max-memory-gib` still cap it.

- **`[agents] skills` in the stack file: where the agent skills go.**
  `devstack skills install` writes the generated skills into each folder it
  names: `.claude/skills` (Claude Code, still the default), `.agents/skills`
  (Codex, Gemini CLI and most others), or both. `devstack init` fills it in
  from what the repository shows (`CLAUDE.md`, `AGENTS.md`, `.cursor/`, ...).
  A generated skill in another agent's folder (one written with `--dir`) is
  now found too: `devstack status`, `doctor`, `skills check` and the
  dashboard report it when it is out of date, and `skills install`
  refreshes it there. Skills are named by their folder in those reports
  (`.agents/skills/devstack`). A stack file that uses `[agents]` needs
  `[stack] requires` at this version.

- **The dashboard says when a repository was set up with an older
  devstack**, and hands a coding agent a prompt to bring it up to date. The
  generated agent skills name the devstack that wrote them; when they no
  longer match the one installed, a banner on the stack names both versions
  and offers the prompt (the changelog in between, the stack file against
  the reference, `devstack check` and `doctor`, the skills, a restart) and a
  plain "Refresh skills" (`devstack skills install`, by the hub). Skills that
  only no longer match the stack file get the refresh alone; skills a newer
  devstack wrote are left as they are. New in the hub's API:
  `GET`/`POST /hub/stacks/{id}/skills`.
- **A coding agent started with the dashboard's prompt** (ADR-0046,
  amended). Where the dashboard has a prompt, it also offers the agents it
  found that take one: Claude Desktop and Claude Code open in the folder
  with the prompt filled in (you send it), Orca starts on it at once (asked
  first). The hub writes every prompt; the page only names which. New
  prompts: a stack file that does not load ("Add a stack"), and a stack
  whose start from the dashboard failed (with the end of `devstack up`'s
  output and its log). New in the hub's API: `GET /hub/prompt`,
  `POST /hub/agent`, and `prompt` in `GET /hub/openers`.

### Changed

- **"Restart stack" switches the daemon to the installed devstack while
  lanes run too** (ADR-0052). Before, a restart with a lane running only
  restarted the main tree's services in the old daemon, so the banner
  "<stack> runs devstack X, devstack Y is installed" stayed until every lane
  was stopped. Now the daemon leaves its lanes to the next one: a lane on a
  node keeps running there (the next daemon renews its lease and forwards
  its ports again), a lane here starts again from the builds it ran, and
  the lane that served the main ports gets them back. The confirmation says
  which lanes go where; a daemon of an earlier release cannot do it, and
  then the confirmation says its lanes stop. New: `restart=true` on
  `POST /hub/stacks/{id}/down` and `POST /v1/shutdown`, `carries_lanes` on
  a stack's daemon. Telemetry: `lane_carried`, `lane_not_carried`,
  `main_slot_carried`, `daemon_stopping` with what a restart carries, and
  `daemon_outdated` from the hub (once per stack and release of its daemon).
- **A service's build list shows the newest ten**, the rest behind "Show
  more". The list is read from the diagnostic log (5 MB, one rotated file);
  what builds keep on disk is bounded apart from it: the newest five
  artifacts per service and whatever a tree or lane runs or may go back to,
  and the newest ten build logs. Each build now says what of it is still
  there (`artifact`, `log` in `/v1/services/{name}/builds`): a kept artifact
  is marked, and a build whose log is gone is shown faint and opens nothing.

### Fixed

- **A lane starting on a node no longer reads that the node cannot take
  it.** The start view ranks the pool's nodes for the lane, and its own
  lease, just admitted, counted against the node's offer for three minutes:
  the node it was starting on said "cannot take it: offers 2.1 CPU …, the
  lane needs 9.2 CPU". `GET /v1/placement` now counts the room a lane's own
  leases hold as the lane's, and the node it starts on says "the lane is
  starting here". The dashboard's policy form keeps the fields it does not
  edit (`keep_lanes_hours`, `keep_lanes_gib`) instead of dropping them.
- **A lane on a node forwards its TCP ports only, and says which UDP ports
  are not forwarded** (#192). The requester listened on a UDP port's number
  as TCP; a service's `53/tcp` and `53/udp` were one entry. The dashboard
  (the lane's header, the container row and page) and `devstack lane list`
  name a UDP port as not forwarded; `not_forwarded` on a container and a
  lane in the API. devstack forwards TCP only.
- **The hub's failures that only reached its log are shown** (#202): a
  failed check for a newer release in the version popover (`error` in the
  `release` event) instead of looking up to date; devstack's name not
  registered with Windows, and a click on a notification that could not
  open the dashboard (Linux), in the notifications popover (`trouble` in
  `GET /hub/notify`, and `devstack hub notify`).
- **A lane takes the main ports when one of its services does not run.**
  `promote --lane` (the dashboard's "Serve main ports") refused while a
  single service of the lane was stopped or failed ("does not run
  worker"); now that service's main port moves too and answers
  once it runs in the lane (ADR-0024 point 5, noted). The `main_slot` line
  names what did not run yet.
- **Stopping the main tree while lanes run keeps what serves them.** A
  service with `lanes = false` runs only in the main tree and serves every
  lane; `devstack down` (and "Stop stack") stopped it too, and the lanes
  lost it. It runs on now, with what it starts `after`; `devstack down`
  names it, `POST /v1/down` reports it as `kept`.
- **Giving the main ports back from a lane runs what the main tree ran
  before**, not a candidate held meanwhile (ADR-0013): the main tree started
  its services anew, which built its tree as it is and took a held build
  (one with a schema change) into the main slot without its migration. Each
  service stopped for the lane now starts again from the build it ran.
- **The hub's Docker check no longer reports a busy engine as stopped or
  missing.** One `docker version` that failed or took longer than 5 s on a
  busy machine made the dashboard say "Docker is not running" (or "not
  installed", for any error starting `docker`); now only a `docker` that is
  not found is missing, and a running engine counts as gone when two looks
  in a row say so. The `docker` line says why and how long the look took;
  a single failed look is `docker_look_failed`.
- **One changed file makes one `changed` candidate event**, not one per
  file: a checkout or an install sent hundreds of events (1925 in a minute)
  to the dashboard and the log.
- **A sync to a node skips a file Windows is deleting** ("access denied"
  until its last handle closes, a folder a package manager removes) as it
  skips one that is gone, instead of failing the whole sync.
- **A refused `promote --lane` says what each missing service is**
  (`worker (failed: build failed)`), and advises `up` only when
  they are stopped.
- **devstack raises its limit of open files to the hard limit** (Linux,
  macOS): a lane's daemon on a node ran out at the container's soft limit
  of 1024 ("No file descriptors available"). `daemon_start` says the limit.
- Telemetry: an OS error's code survives the cut to the first line when
  Windows wraps its text (`… (os error 10053)`); a Windows exit code of an
  exception reads in hex as well (`-532462766 (0xE0434352)`, in a failed
  service's reason and in `service_exited`); `remote_lane_synced` says how
  many files were read to hash them and where the time went.
- **A build left building, starting or switching is `interrupted`**: one
  the daemon's stop, or a stop or restart, cut short stayed in that state in
  the build list for good ("switching", 20 h). Only the candidate's build is
  still on its way.
- **A lane of a stack of Compose containers only is no longer yellow** in
  the dashboard's sidebar while its containers run ("0 of 0 services
  running"). A lane's containers now count as its services do, and its
  tooltip names them (`3 of 3 containers running`); `GET /v1/lanes` reports
  `containers_running` and `containers`, a lane on a node's too.

## [0.18.0] - 2026-10-21

Notices: what no request reports reaches the dashboard (ADR-0050). A lane
whose worktree is removed elsewhere shows it while it goes, and stays shown
until left (ADR-0024 point 7 amended). The stack header's path reads as a
menu whose programs can be ordered and hidden; the branch picker groups
lanes, local and remote branches; the sidebar marks what serves the main
ports. `down --all --infra` removes every container.

### Added

- **Notices: what no request reports reaches the dashboard**
  ([ADR-0050](docs/adr/0050-what-no-request-reports-reaches-the-dashboard-as-a-notice.md)).
  A condition the user should know of is listed in the tree's `notices`
  while it holds, shown as a banner, marked on the lane in the sidebar, and
  printed by `devstack status`: a lane's worktree missing (the lane goes
  after 10 minutes, with its containers and volumes), a worktree that gets
  no lane for want of ports, a lane's copy not brought up to date (its dev
  servers run old code), and `prepare` paths not linked into the worktree.
  Something that happened once is a toast and an entry on the Events page:
  the main ports taken back from a lane that is gone (and which services
  did not start again), a shared lane's tunnel refused, a desktop
  notification not shown. Before, all of these were only in the diagnostic
  log. The log stays as it is; the dashboard does not show it.
- **A lane whose worktree is removed shows it, and stays shown until left**
  (ADR-0024 point 7 amended). Removed from GitHub Desktop, an IDE or an
  agent, a running lane took some twenty seconds to go (its containers and
  volumes removed) and looked live meanwhile. The daemon now lists it with
  `removing` from the start (`devstack lane list` too); the dashboard shows
  "removing", then "removed", read only, with the lane as it was last,
  instead of switching to the main tree, and offers to make the worktree
  again on its branch while the branch is there (a fresh lane: the data
  went with the old one). `?mock=laneremoved` shows it in the mock.
- **The path menu's programs can be ordered and hidden**: "Manage
  programs…" at its end, kept in this browser (ADR-0046's list itself is
  unchanged).

### Changed

- **The stack header's path reads as a menu**: a chip like the branch's,
  with a folder and a caret; a long path is cut in its folder, so the
  worktree's name stays readable.
- **The branch picker groups the branches**: those with a worktree (the
  main tree's first), then the other local ones, then the remote ones, each
  by name. "New branch from" is always shown, as a ghost button.
- **The sidebar marks what serves the main ports**, the main working tree
  too.
- **The overview** gives the list more of the width, leaves out the states
  nothing is in, and shows the active filter in the accent with a "Reset
  filter" button.

### Fixed

- **`devstack down --all --infra` removes every container**, the lanes' and
  the main tree's (and `POST /hub/stacks/{id}/down?infra=true&all=true`).
  It stopped the lanes but left all containers running ("lanes were using
  it"): the containers were judged while the lanes still ran. The lanes now
  stop first, each with its own containers (a lane on the main ports gives
  them back first, a lane on a node is released), then the main tree's
  containers go (`POST /v1/down?infra=true&all=true`).
- **A lane reset says where its data stayed.** When removing a lane's
  containers and volumes on this machine failed, `POST /v1/lanes/{lane}/reset`
  still answered `removed: true`, and the dashboard said the lane starts
  fresh. That place now answers `removed: false` with the error, and the
  dashboard names every place that kept the lane's data.
- **A file watcher that misses edits is shown.** `watcher_error` (the watcher
  could not be set up, or lost events) was in `/v1/stack` but nothing showed
  it: edits then made no candidates without a word. The dashboard shows it as
  a banner and on the Events page (a new `watcher` event keeps it current),
  `devstack status` as a line.
- **Main services that do not start again are logged** when a lane on a node
  that served the main ports is gone (`main_slot_back_failed`); the error was
  dropped.
- **A stack file that is gone is no change**: a lane's worktree being
  removed took its stack file first, and the dashboard offered "Restart
  stack" for a change that applies after a restart.
- The branch chip no longer names a lane's branch twice; service details and
  the disk legend no longer break inside a value; a CPU share of 1,000 % and
  more gets its thousands separator.

## [0.17.0] - 2026-10-21

The dashboard makes a lane from a branch, and the main working tree's
checkout is never changed (ADR-0047); containers can serve the main ports,
so a lane of a stack of containers can take them over (ADR-0048); a lane's
need is measured where it ran, disk included (ADR-0049). A tree opens in
GitHub Desktop, and programs the dashboard opens come to the front on
Windows; a shared lane's links open its services.

### Added

- **A lane from a branch, in the dashboard** (ADR-0047): the branch in the
  stack header opens the repository's branches, local and remote, each with
  its lane. A click shows a branch's lane, or makes one: the hub runs `git
  worktree add`, beside the repository's other worktrees (else in
  `<repo>.worktrees/`), and the lane shows, not started. **+** makes a new
  branch from one, and a lane on it; a stopped lane's worktree can be removed
  (`git worktree remove`, which refuses one with changes). **Fetch** runs
  `git fetch --all --prune`, without a prompt. The main working tree's
  checkout is never changed: a branch runs in a lane, which can then serve
  the main ports. The CLI and the MCP server leave worktrees to git, as
  before.
- **Serve main ports** is not offered for a stack with nothing on main
  ports (#184): `StackView` says `no_main_ports`.
- **Containers can serve the main ports** (ADR-0048, #184):
  `[compose.<group>] main_ports = ["web"]` names containers whose published
  TCP ports devstack holds in the main tree. The main tree's container moves
  to a loopback port Docker assigns, devstack listens on the fixed port (on
  the Compose file's address) and forwards it there, and `promote --lane`
  points it at the lane's container. A stack of containers only can serve
  its main ports from a lane now. UDP ports stay Docker's (#192).
- **A lane's need is measured where it ran, disk included** (ADR-0049).
  A node measures each lane it runs (the peak of its memory, of its CPU
  over a minute, of its slot and of its images) and the coordinator keeps
  the last five runs per repository and profile: a lane is placed by the
  most each part took in them, or, before any ran in the pool, by what the
  requester estimates from its main tree (no longer the main tree's memory
  of the moment and 0.5 CPU for every stack). Disk is counted per node
  (nothing more where the lane's data is kept, its own part where the
  repository's images are) against what is free and what kept lanes take,
  held for each lease until it has written it, and kept lanes go before a
  lane that needs their room starts. `[lanes.need]` and a profile's `need`
  set a floor (`cpus`, `memory`, `disk`, `gpus`); a lane asking for GPUs
  goes only to a node that offers them. The lease says the need and where it
  came from; the dashboard shows the disk a lane needs on each node. A node
  short of disk says so with every renewal. A lane on this machine checks
  the disk of its worktree's drive too (`--force` starts it anyway).
- **Open a tree in GitHub Desktop** (Windows, ADR-0046): the path's menu
  in the stack header lists it when it is installed, in a group of its own
  for Git clients. A repository it does not know yet is added to its list.

### Fixed

- **Serving the main ports refuses a stack with nothing to move** (#184): a
  stack of Compose containers only answered that the lane served the main
  ports and moved nothing, as a container's published port is Docker's
  unless `main_ports` names it. It now says so (`409`), in the dashboard
  too.
- A link to a service or container of a lane shared with you (its port, in
  the list and on its page) opens it: a click opens the forward to it on
  your machine, as **Open** in the pool area does, and keeps the link's
  path. Before, it pointed at `localhost:<port>` of the lane's node, where
  nothing on your machine listens.
- **A tree opened in a program comes to the front on Windows** (ADR-0046):
  Explorer, the terminals, the editors, the agents and Orca opened behind
  the browser, as the hub runs in the background. The hub takes the
  foreground first and hands it to the program.

## [0.16.0] - 2026-10-21

The hub tells the desktop what needs attention (a failed build, a held
candidate, a crashing service, a lane of another member starting here) and
shows whose lanes run on this machine; Windows installs and updates through
winget; the dashboard opens a stack's folder in one's own programs, hands
agents links to a stack or lane, and stops a stack with its containers. The
share switch is gone: it never let a lane run (see Removed).

### Removed

- **The share switch is gone from the dashboard** (sidebar and pool area):
  it reported capacity but never started a node agent, so no member could
  start a lane on a machine that only switched it on; on Windows none can
  run at all (#168). The hub reports the machine (online, what it has, its
  devstack) and offers nothing; `PUT /hub/pool/share` is gone, `GET` says why
  (`unavailable`). A machine runs other members' lanes with `devstack node
  agent`, granted by an admin; a switch that starts it is #171.

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
- Dashboard: the overview's state filters are small badges above the list
  instead of a row of tiles; the detail pane shows the lane's CPU, memory and
  disk as the usage page does, with a link to it.

### Added

- **Stop stack can take the containers down too**: the dashboard's stop
  dialog (header and sidebar) offers "Stop its containers too", as
  `devstack down --infra` does; their volumes stay. Checked when the stack
  runs nothing but containers, where a stop without them changed nothing
  but the daemon. Not offered while lanes run: they keep the containers
  they share (ADR-0030).
- **Links to a stack's or a lane's dashboard page, for agents to hand on**:
  `devstack status`, `devstack up` and `devstack lane start` end with
  `dashboard: http://127.0.0.1:4250/#/s/<id>?lane=<lane>`; the MCP tools
  `status`, `up` and `lane_start` too, and `lanes` names each lane's page.
  `devstack ui` opens the dashboard on the stack it runs in, in a worktree on
  its lane (`--lane <name>` for another); `devstack ui --link` prints the page
  and the `devstack://open` link that pairs the browser first. The `devstack`
  skill tells agents to give the user that page after starting a stack or a
  lane, or creating a worktree (run `devstack skills install` again).
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
- **winget**: `winget install nuetzliches.devstack` (once winget's catalogue
  has the release). Each release submits its manifests
  (`packaging/winget/`) to `microsoft/winget-pkgs`; the release workflow
  needs the secret `WINGET_TOKEN` for it. `devstack update` (and the
  dashboard's update) on a devstack winget installed runs `winget upgrade`,
  moving the running binary out of winget's way first, since winget cannot
  replace a file that runs; `devstack self uninstall` leaves that binary to
  `winget uninstall`.
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

- A lane's failed start on a node no longer stays on its start page after
  the lane or the stack stopped: stopping the lane, stopping the main tree
  (`devstack down`) or starting the lane here clears it, and the dashboard
  drops it once the stack's daemon is gone. Before, it stayed ten minutes
  whatever happened.
- After **Stop stack** the dashboard shows the stack's services stopped.
  They did stop, but the daemon closes its API before it stops them and
  exits, so the page kept showing them as running.
- Stopping a lane keeps it shown, with its start, instead of switching to
  the main working tree, where the same button is "Stop stack". A lane whose
  worktree was removed still gives way to the main tree.
- A lane's dot in the sidebar follows its services: the daemon announces
  the lanes when their counts change (a lane on a node is read every 5 s),
  and the shown lane counts from its own stream. Before, a lane started on
  a node stayed yellow until the page switched to another tree.
- A daemon's last telemetry is sent as it exits (2 s at most): what it said
  in its last minute, its stop included, was dropped before (ADR-0037).
- A lane on a node no longer stops syncing when a file goes while it is
  sent (a package manager rewriting `node_modules`): the file is left out
  and the next sync sends what is there then.
- A browser or dev server closing a connection through a lane's tunnel is
  no longer a warning in the diagnostic log (Windows reports it as
  "aborted", 10053).
- Files of a write in progress no longer show among the paths that made a
  candidate: atomic writes of editors and agents' tools
  (`db.rs.tmp.<pid>.<hex>`), `sed -i`'s `sedXXXXXX`, vim's `4913` and swap
  files, Emacs' `.#` and `~` files.
- A dev server's failed rebuild goes to the diagnostic log with its error
  lines, not only with its tool's last line: Angular on Windows prints them
  after that line, and the log line now waits for them (2 s at most after
  the last).

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

[Unreleased]: https://github.com/nuetzliches/devstack/compare/v0.33.0...HEAD
[0.33.0]: https://github.com/nuetzliches/devstack/compare/v0.32.0...v0.33.0
[0.32.0]: https://github.com/nuetzliches/devstack/compare/v0.31.0...v0.32.0
[0.31.0]: https://github.com/nuetzliches/devstack/compare/v0.30.0...v0.31.0
[0.30.0]: https://github.com/nuetzliches/devstack/compare/v0.29.0...v0.30.0
[0.29.0]: https://github.com/nuetzliches/devstack/compare/v0.28.0...v0.29.0
[0.28.0]: https://github.com/nuetzliches/devstack/compare/v0.27.1...v0.28.0
[0.27.1]: https://github.com/nuetzliches/devstack/compare/v0.27.0...v0.27.1
[0.27.0]: https://github.com/nuetzliches/devstack/compare/v0.26.0...v0.27.0
[0.26.0]: https://github.com/nuetzliches/devstack/compare/v0.25.0...v0.26.0
[0.25.0]: https://github.com/nuetzliches/devstack/compare/v0.24.1...v0.25.0
[0.24.1]: https://github.com/nuetzliches/devstack/compare/v0.24.0...v0.24.1
[0.24.0]: https://github.com/nuetzliches/devstack/compare/v0.23.0...v0.24.0
[0.23.0]: https://github.com/nuetzliches/devstack/compare/v0.22.1...v0.23.0
[0.22.1]: https://github.com/nuetzliches/devstack/compare/v0.22.0...v0.22.1
[0.22.0]: https://github.com/nuetzliches/devstack/compare/v0.21.0...v0.22.0
[0.21.0]: https://github.com/nuetzliches/devstack/compare/v0.20.0...v0.21.0
[0.20.0]: https://github.com/nuetzliches/devstack/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/nuetzliches/devstack/compare/v0.18.0...v0.19.0
[0.18.0]: https://github.com/nuetzliches/devstack/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/nuetzliches/devstack/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/nuetzliches/devstack/compare/v0.15.0...v0.16.0
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
