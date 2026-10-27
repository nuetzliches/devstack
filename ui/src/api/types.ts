// Types of the dashboard API (docs/dashboard-api.md). All JSON is snake_case,
// all times are Unix milliseconds.

// ---------------------------------------------------------------- hub

export type DaemonInfo = {
  pid: number;
  port: number;
  started: number;
  version: string;
  /** A restart carries its lanes over to the next daemon (ADR-0052); older hubs and daemons omit it (it stops them). */
  carries_lanes?: boolean;
};

export type StackEntry = {
  id: string;
  name: string;
  file: string;
  repo: string;
  running: boolean;
  daemon: DaemonInfo | null;
  last_seen: number | null;
  /** Why the last start from the dashboard failed, while the stack is down. Older hubs omit it. */
  start_error?: string;
};

export type HubSession = {
  version: string;
  /** Why the dashboard cannot start a stopped hub: no `devstack://` link on the hub's platform ("not on macOS yet"); null where it can. Older hubs leave it out. */
  link_unavailable?: string | null;
};

/** The Docker engine as the hub sees it (`GET /hub/docker`, `docker` events). */
export type DockerEngine =
  | { state: 'running'; version: string }
  | { state: 'stopped'; starter: string | null; hint: string }
  | { state: 'missing' };

export type HubEvent =
  | { type: 'stacks'; stacks: StackEntry[] }
  | { type: 'docker'; docker: DockerEngine }
  /** A newer release than the hub (`newer`), or none; why the last check failed (`error`; older hubs leave it out). */
  | { type: 'release'; current: string; newer: string | null; error?: string | null }
  /** The dashboard's update (`POST /hub/update`): running, or how the last one ended. Older hubs never send it. */
  | { type: 'update'; update: UpdateState }
  /** Lanes of other members that run on this machine, every pool; older hubs never send it. */
  | { type: 'guests'; guests: Guest[] }
  /** Something happened once in the hub that no request reports (ADR-0050): a toast. */
  | { type: 'notice'; level: NoticeLevel; text: string };

export type NoticeLevel = 'warn' | 'error';
/** A condition the user should know of that no request reports, while it holds (ADR-0050). */
export type Notice = { id: string; level: NoticeLevel; text: string; since: number };

/** A lane of another member on this machine (a lease whose node is this machine). */
export type Guest = {
  pool: string;
  lease: string;
  member: string;
  /** How the pool names the member. */
  owner: string;
  repo: string;
  lane: string;
  /** `offered`: admitted, not started yet. */
  state: 'offered' | 'running' | (string & {});
  /** Unix seconds. */
  since: number;
  /** What it asked for. */
  cpus: number;
  memory: number;
};

/**
 * The dashboard's update. A successful one stops the hub that started it; the
 * new hub reports `installed`. `finished` (ms) tells a fresh outcome from an
 * old one; older hubs send neither.
 */
export interface UpdateState {
  running: boolean;
  /** Why the last update failed: the end of its output. */
  error: string | null;
  /** The version the last update installed. */
  installed?: string | null;
  /** When the last update ended (ms since the epoch). */
  finished?: number | null;
}

// ---------------------------------------------------------------- daemon state

export type KnownServiceKind = 'service' | 'job' | 'devserver' | 'tool' | 'static';
/** The daemon sends a `String`: a newer daemon may name a kind this page does not know. */
export type ServiceKind = KnownServiceKind | (string & {});

export type Status =
  | { state: 'stopped' | 'starting' | 'running' | 'healthy' }
  | { state: 'building'; build_id: string | null }
  | { state: 'unhealthy' | 'failed'; reason: string }
  | { state: 'completed' | 'exited'; exit_code: number };

export type StatusState = Status['state'];

export type KnownCandidateState = 'changed' | 'building' | 'superseded' | 'starting' | 'switching' | 'held' | 'failed';
/** The daemon sends a `String`: a newer daemon may send a state this page does not know. */
export type CandidateState = KnownCandidateState | (string & {});

export type Candidate = {
  state: CandidateState;
  build_id: string | null;
  since: number;
  reason: string | null;
};

/** A dev server's last rebuild, read from its output (ADR-0042). */
export type Rebuild = {
  state: 'rebuilding' | 'ok' | 'failed' | (string & {});
  /** Unix ms: when it started (or ended, for one the tool reports only when done). */
  since: number;
  ms?: number;
  /** `angular`, `vite`, `webpack`, `next`, `dotnet-watch`, `tsc`, `custom`. */
  tool: string;
  errors?: string[];
};

export type ProxyView = {
  port: number;
  upstream: number | null;
  build: string | null;
  pending: string | null;
  active: number;
  served: number;
  last_error: string | null;
};

/** A lane: another git worktree of the repository, running the stack beside the main one (plan §3.5). */
/** What `POST /v1/lanes/{lane}/reset` (and prune) found and did, per place. */
export type Pruned = { place: string; lane: string; bytes: number | null; why: string; removed: boolean; error: string | null };

export type LaneView = {
  name: string;
  path: string;
  /** The branch checked out in it; null: a detached HEAD. Older daemons omit it. */
  branch?: string | null;
  number: number;
  /** Started (`devstack up` in it); a lane not started runs nothing. Older daemons omit it (all started). */
  active?: boolean;
  port_offset: number;
  owns_main: boolean;
  running: number;
  services: number;
  /** Its Compose containers devstack starts that run, and all of them. Older daemons omit them. */
  containers_running?: number;
  containers?: number;
  /** The node of the pool it runs on (ADR-0025); absent: this machine. */
  node?: string;
  /** The pool of that node (ADR-0034). */
  pool?: string;
  /** Whom it is shared with in its pool (ADR-0035): member ids, tags, `*` for everyone. */
  shared_with?: string[];
  /** The profile it runs with (ADR-0041), while it runs. */
  profile?: string;
  /** Its background workers run (`--workers` or its profile), while it runs. Older daemons omit it. */
  workers?: boolean;
  /** What went wrong with it on its node lately (a sync, a renewal). */
  node_error?: string;
  /** Its node or the pool's coordinator runs another devstack than this machine, or does not say which. */
  node_warning?: string;
  /** Of a lane on a node, its containers' published ports this machine does not forward (`5353/udp`): UDP its node does not relay (ADR-0056), another protocol than TCP and UDP. Older daemons omit it. */
  not_forwarded?: string[];
  /** Why they are not (the node's devstack forwards TCP only, with its release). Older daemons omit it. */
  not_forwarded_reason?: string;
  /** A start on a node or a move under way (step of steps), or one that failed lately. */
  moving?: LaneMove;
  /** Its worktree is gone: it stops and its data goes (ADR-0024 point 7); it leaves the list after. */
  removing?: boolean;
  /** Its worktree has no stack file of its own: it runs with the main working tree's (ADR-0055). Older daemons omit it. */
  main_stack_file?: boolean;
  /** The dashboard's own: the daemon no longer lists it, it is shown as it was last. */
  gone?: boolean;
  /** Its notices (ADR-0050): its tree's while it runs here, and what the main tree knows of it (`worktree_missing`). */
  notices?: Notice[];
  /** Of a lane on a node, what `prepare` makes linked into its worktree from a copy on this machine (ADR-0063). Older daemons omit it. */
  links?: NodeLink[];
};

export type NodeLink = {
  path: string;
  service: string;
  state: 'linked' | 'linked (store)' | 'preparing' | 'linked (stale)' | 'not linked';
  why?: string;
};

export type LaneMove = {
  kind: 'start' | 'move';
  /** The node, `pool` (the coordinator chooses) or `local`. */
  to: string;
  step: number;
  steps: number;
  what: string;
  /** Unix ms. */
  since: number;
  error?: string;
  failed_at?: number;
  /** The phases over so far, with their time (older daemons omit it). */
  phases?: { phase: string; ms: number }[];
  /** Unix ms the current phase began. */
  phase_since?: number;
};

export type ServiceView = {
  name: string;
  kind: ServiceKind;
  groups: string[];
  port: number | null;
  /** Where a browser opens it (`https://localhost:4200`); null without a port. Older daemons omit it. */
  url?: string | null;
  autostart: boolean;
  status: Status;
  build_id: string | null;
  pid: number | null;
  since: number;
  log: string | null;
  instance_port: number | null;
  proxy: ProxyView | null;
  candidate: Candidate | null;
  /** A dev server's last rebuild (ADR-0042); older daemons omit it. */
  rebuild?: Rebuild | null;
  /** Runs the toolchain's watcher from the source tree instead of a build (older daemons omit it). */
  hot_reload?: boolean;
  /** The stack file gives the service a hot-reload mode. */
  hot_reload_available?: boolean;
  /** Where its last `prepare` outputs came from (ADR-0062), since the daemon started. Older daemons omit it. */
  prepare?: PrepareView;
  /** The last action a client took on it, and which (ADR-0067). Older daemons omit it. */
  last_action?: Change;
};

/** Who changed something at runtime, and when (ADR-0067). */
export type Change = { via: Client; at: number; action?: string };
/** The client a change came through; `auto`: devstack itself. */
export type Client = 'cli' | 'mcp' | 'dashboard' | 'tray' | 'auto' | 'unknown';

/** A change the daemon's journal recorded (ADR-0067): a request that changed something, refused ones included, or devstack's own (`AUTO`). */
export type JournalEntry = {
  /** Unix ms. */
  at: number;
  via: Client;
  /** The lane it was for; absent: the main working tree (and every entry of a lane's daemon on a node). */
  lane?: string;
  /** `POST`, `DELETE`; `AUTO` for devstack's own. */
  method: string;
  path: string;
  status: number;
  error?: string;
};

/** Where a service's last `prepare` outputs came from (ADR-0062). */
export type PrepareView = {
  /** Copied out of the store, or `prepare` ran. */
  source: 'store' | 'run' | (string & {});
  /** Unix ms, when it was done. */
  at: number;
  /** How long: the copy, or the run. */
  ms: number;
  /** The store's key: of the entry copied, or of the one the run put. */
  key?: string;
  /** Of a copy: how long the `prepare` that made the entry took. */
  prepare_ms?: number;
  /** Of a run: why the store gave nothing; absent: the `prepare` names no `outputs`. */
  miss?: string;
  /** Of a run: what it made went into the store. */
  stored?: boolean;
};

/** `GET /v1/store`: the store of what `prepare` makes (ADR-0062). */
export type StoreView = {
  /** The daemon writes it; false: a node's store, seen from a lane there (read-only). */
  owned: boolean;
  bytes: number;
  entries: StoreEntry[];
};

export type StoreEntry = {
  service: string;
  key: string;
  outputs: string[];
  bytes: number;
  files: number;
  /** Unix ms. */
  created: number;
  /** Unix ms of the last copy out of it, else of its making. */
  last_used: number;
  prepare_ms: number;
  copy_ms?: number;
  /** Lane names whose last `prepare` came from it or went into it; `""` the main tree. */
  used_by: string[];
};

export type ContainerView = {
  /** Published ports Docker no longer forwards (accepted, closed at once); older daemons omit it. */
  unreachable?: string[];
  /** Of a lane on a node, its published ports the developer's machine does not reach (`5353/udp`): another protocol than TCP, UDP too unless the requester relays it (ADR-0056). Older daemons omit it. */
  not_forwarded?: string[];
  service: string;
  name: string;
  state: string;
  health: string;
  /** Exit code once it is not running; 0/130/137/143 is a stop. Older daemons omit it. */
  exit_code?: number;
  ports: string[];
  /** Links for the dashboard (stack file `urls`, or http://localhost:<port> per published port). Older daemons omit it. */
  urls?: string[];
  /** The group starts it (in its `services`); false: another container of the project, which can only be stopped. Older daemons omit it. */
  managed?: boolean;
  /** Its fixed ports devstack holds (`main_ports`, ADR-0048): `upstream` the instance port, or the lane's while a lane serves the main ports. */
  held?: { port: number; upstream: number | null; active: number; served: number }[];
};

export type InfraView = { group: string; project: string; containers: ContainerView[]; error: string | null };

export type GroupView = { name: string; label: string; order: number | null; members: string[] };

/**
 * `GET /hub/stacks/{id}/skills`: the agent skills `devstack skills install`
 * wrote into the repository, held against what this devstack would write.
 */
export type SkillsState = {
  found: string[];
  outdated: string[];
  /** The devstack that wrote them (the oldest, when they differ). */
  from: string | null;
  current: string;
  /** Written by a newer devstack than this one: refreshing would go back. */
  ahead: boolean;
  /** After `POST`: skills of the same name the project wrote itself, left alone. */
  kept?: string[];
};

/**
 * `GET /hub/stacks/{id}/upgrade`: the devstack the stack file was last brought
 * up to date with (`[stack] reviewed`), apart from the agent skills.
 */
export type UpgradeState = {
  reviewed: string | null;
  /** The devstack to read the changes after: `reviewed`, or for a file without it the agent skills' version when older. */
  from: string | null;
  current: string;
  /** `reviewed` is older than this devstack, or not set; `false` for a file that does not load. */
  behind: boolean;
};

/**
 * `GET /hub/stacks/{id}/mcp`: the agent configuration files (the project's,
 * then the user's) that register `devstack mcp` for the repository.
 */
export type McpState = { registered: boolean; files: string[] };

export type CommandView = { name: string; description: string; confirm: string | null; requires: string[] };

export type StackView = {
  name: string;
  repo: string;
  version: string;
  services: ServiceView[];
  infra: InfraView[];
  groups: GroupView[];
  commands: CommandView[];
  /** The branch checked out in this working tree. Older daemons omit it. */
  branch?: string | null;
  /** The stack file changed since the daemon started; applies with the next start. Older daemons omit it. */
  config_changed?: boolean;
  /** The profile this tree runs with (ADR-0041). */
  profile?: string;
  /** The stack file's profiles, for a start with one. */
  profiles?: ProfileView[];
  /** Nothing moves with the main slot (Compose containers only, #184): serving the main ports is refused. */
  no_main_ports?: boolean;
  /** The lane on the main ports and who put it there (ADR-0067); `at` absent when the journal does not say. Older daemons omit it. */
  main_served?: { lane: string; via: Client; at?: number };
  /** The file watcher could not be set up, or lost events: edits make no candidates (or every built service got one). */
  watcher_error?: string | null;
  /** What no request reports, while it holds (ADR-0050). Older daemons omit it. */
  notices?: Notice[];
};

/** A branch of a stack's repository (ADR-0047), `GET /hub/stacks/{id}/branches`. */
export type BranchView = {
  /** `feat/login`, or `origin/feat/login` for a remote-tracking one. */
  name: string;
  /** The remote of a remote-tracking branch. */
  remote?: string;
  /** What a local branch tracks. */
  upstream?: string;
  /** The worktree that has it checked out. */
  worktree?: string;
  /** That worktree is the main working tree. */
  main: boolean;
  /** Its last commit, Unix seconds. */
  date: number;
  /** It has a stack file at the main one's path (ADR-0055): in its worktree, else in its last commit. Older hubs omit it. */
  stack_file?: boolean;
};

export type Branches = { branches: BranchView[]; fetched?: number };

/** A profile of the stack file (ADR-0041). */
export type ProfileView = { name: string; description?: string; fresh?: boolean; command?: string };

// ---------------------------------------------------------------- usage

export type Load = { cpu: number; memory: number };
export type Point = { at: number; cpu: number; memory: number };

export type ServiceUsage = { name: string; cpu_time_ms?: number; processes?: number; history?: Point[] } & Load;
export type BuildsUsage = { name: 'builds'; cpu_time_ms?: number; processes?: number; history?: Point[] } & Load;
export type ContainerUsage = { group: string; service: string; history?: Point[] } & Load;
export type GroupUsage = { name: string; history?: Point[] } & Load;
export type LaneDisk = { snapshot: number; artifacts: number; logs: number; volumes: number | null };
export type LaneUsage = { name: string; disk?: LaneDisk; history?: Point[] } & Load;

export type UsageReport = {
  at: number;
  services: ServiceUsage[];
  builds: BuildsUsage;
  containers: ContainerUsage[];
  groups: GroupUsage[];
  lanes: LaneUsage[];
  docker_error?: string;
};

// ---------------------------------------------------------------- events

export type RunStatus = 'running' | 'succeeded' | 'failed';

export type StackEventBody =
  | { type: 'snapshot'; stack: StackView }
  | { type: 'service'; service: string; status: Status; build_id: string | null }
  | { type: 'candidate'; service: string; candidate: Candidate | null }
  | { type: 'devserver_rebuild'; service: string; rebuild: Rebuild | null }
  | { type: 'infra'; group: string; message: string }
  | { type: 'command'; command: string; run: number; status: RunStatus }
  | { type: 'config'; changed: boolean }
  /** The file watcher failed or lost events, or works again (`null`). */
  | { type: 'watcher'; error: string | null }
  /** This tree's notices changed: all of them (ADR-0050). */
  | { type: 'notices'; notices: Notice[] }
  /** Something happened once that no request reports (ADR-0050): a toast. */
  | { type: 'notice'; level: NoticeLevel; text: string }
  /** A worktree added or removed, a branch checked out, a lane started or stopped, the main ports moved. */
  | { type: 'lanes'; lanes: LaneView[] }
  /** A change the journal recorded (ADR-0067): the main tree's stream carries every entry, a lane's those of its lane. */
  | { type: 'journal'; entry: JournalEntry }
  | ({ type: 'usage' } & UsageReport);

export type StackEvent = { seq: number; at: number } & StackEventBody;

export type LogStreamEvent = { type: 'line'; text: string } | { type: 'instance'; build_id: string | null };

export type RunLogEvent =
  | { type: 'line'; text: string }
  | { type: 'end'; status: 'succeeded' | 'failed'; error: string | null };

// ---------------------------------------------------------------- commands

export type Run = {
  id: number;
  command: string;
  started: number;
  finished: number | null;
  status: RunStatus;
  error: string | null;
  log: string | null;
};

// ---------------------------------------------------------------- pool (ADR-0031, ADR-0032)

export type PoolRole = 'member' | 'admin';
export type PoolMemberState = 'pending' | 'approved' | 'removed';

/** A member of the pool. Times here are Unix seconds, as the coordinator keeps them. */
export type PoolMember = {
  id: string;
  name: string;
  state: PoolMemberState;
  role: PoolRole;
  requested_at: number;
  /** Where the request came from; only an admin sees it. */
  address?: string;
  /** Set by admins (ADR-0033): `team:backend`, `customer:acme`. */
  tags?: string[];
  /** As a node: what it lets run on it (ADR-0033). */
  policy?: NodePolicy;
  /** An admin granted it to run the pool's lanes (a node); absent: no. */
  node?: boolean;
  /** An admin let its lanes cause provisioning (ADR-0051 point 3); absent: no. */
  provision?: boolean;
  /** It keeps its lanes off provisioned nodes (ADR-0051 point 10); absent: it does not. */
  no_provisioned?: boolean;
  /** A node a provisioner made (ADR-0051). */
  provisioned?: { request: string; machine: string; address: string; confirmed?: boolean };
};

export type NodePolicy = {
  /** Member ids or tags that may start lanes there; empty: everyone. */
  allow?: string[];
  /** Repositories (`[stack] name`) it runs; empty: any. */
  repos?: string[];
  /** A lane's memory limit: its expected need times this (at least 2 GiB). */
  memory_headroom?: number;
  /** The most CPU cores one lane may use. */
  max_cpus?: number;
  /** What of its free CPU and memory the node offers the pool, percent (1 to 100); absent: half. */
  share_percent?: number;
  /** `lan`: the internet and the local network; `internet`: not the local network; `none`: only the lane's own containers. */
  network?: 'lan' | 'internet' | 'none';
};

export type PoolNode = {
  id: string;
  name: string;
  online: boolean;
  /** Seconds since its last report. */
  seen?: number;
  report?: {
    sharing: boolean;
    cpus: number;
    memory: number;
    api?: string;
    /** What the machine has, what its lanes use (ADR-0036). */
    total?: PoolAmount;
    used?: PoolAmount;
    gpus?: { name: string; memory: number; memory_used: number; load: number; offered: boolean }[];
    /** Stopped lanes it keeps for their next start (ADR-0038); older nodes omit it. */
    kept?: { lanes: number; bytes: number; /** Of `bytes`, the members' stores of what `prepare` makes (ADR-0062); older nodes omit it. */ store_bytes?: number };
    /** How the last update an admin ordered goes (ADR-0039). */
    update?: { at: number; state: 'draining' | 'installing' | 'restarting' | 'done' | 'refused' | 'failed' | 'cancelled' | (string & {}); target?: string; error?: string };
    /** Why it would refuse an ordered update (in a container its image is updated instead); absent: it updates itself. */
    no_update?: string;
    /** The devstack it runs; absent from a release before version reporting. */
    devstack?: PoolRelease;
  };
  /** Paused by an admin: no new lanes. */
  paused?: boolean;
  /** `server` (dedicated node), `node` (runs lanes, may reclaim), `machine` (asks for lanes), `unknown` (never reported). */
  kind?: 'server' | 'node' | 'machine' | 'unknown';
  /** A node a provisioner made (ADR-0051 point 9); absent: an enrolled machine of someone's. */
  provisioned?: ProvisionedNode;
};

/** What the coordinator tells of a provisioned node (ADR-0051 point 9). */
export type ProvisionedNode = {
  /** The provider's id of the machine. */
  machine: string;
  /** The address it enrolled from. */
  address: string;
  /** The SHA-256 of the node certificate's key it enrolled with (ADR-0051 point 6). */
  key?: string;
  size?: PoolAmount;
  /** When it enrolled (Unix seconds). */
  since: number;
  /** Seconds until it is given back, while it has no lane. */
  idle_left?: number;
  /** It admits no lane until an admin confirms it. */
  unconfirmed?: boolean;
  /** The repository it runs lanes of, when the pool provisions per repository. */
  repo?: string;
};

/** A machine the coordinator asked a provisioner for (ADR-0051), kept until it is deleted. */
export type ProvisionMachine = {
  request: string;
  /** The provider's id; empty until the provisioner answered. */
  machine: string;
  address: string;
  size?: PoolAmount;
  repo?: string;
  state: 'provisioning' | 'ready' | 'returning' | (string & {});
  /** The member whose lane caused it. */
  requester: string;
  created: number;
  deadline: number;
  /** The member it enrolled as. */
  member?: string;
  idle_since?: number;
  /** A second use of its enrolment token, or one from another address. */
  alarm?: string;
  /** Why the last deletion failed. */
  error?: string;
};

/** `GET /hub/pool/provisioning` (admins): the provisioner, the pool's settings, the machines. */
export type Provisioning = {
  /** The provisioner's address; absent: the coordinator has none. */
  provisioner?: string;
  settings: { per: 'pool' | 'repo'; confirm: 'none' | 'admin'; idle_minutes: number; max_nodes: number; max_cpus?: number; max_memory?: number };
  machines: ProvisionMachine[];
};

/** A lane on a node, as the coordinator knows it. */
export type PoolLease = {
  id: string;
  node: string;
  member: string;
  repo: string;
  lane: string;
  state: 'offered' | 'running';
  created: number;
  expires_in?: number;
};

export type PoolAuditEntry = {
  at: number;
  by: string;
  /** The coordinator's `String`: these today, others from a newer coordinator. */
  action: 'approve' | 'reject' | 'remove' | 'role' | 'pause' | 'resume' | 'end' | 'tags' | 'policy' | 'node' | 'provision' | 'confirm' | 'enrol' | (string & {});
  member: string;
  name: string;
  detail?: string;
};

/** `GET /hub/pool`: what this machine sees of its pool. */
export type PoolOverview = {
  joined: boolean;
  me?: { id: string; name: string; state: PoolMemberState; role?: PoolRole; coordinator: string; /** Its lanes stay off provisioned nodes (ADR-0051 point 10). */ no_provisioned?: boolean };
  members?: PoolMember[];
  nodes?: PoolNode[];
  leases?: PoolLease[];
  /** What another machine runs to join this pool. */
  join?: { coordinator: string; fingerprint: string; command: string };
  /** Admins only. */
  audit?: PoolAuditEntry[];
  /** Admins only, from a coordinator that knows provisioning (ADR-0051). */
  provisioning?: Provisioning;
};

/** Which devstack a party of the pool runs; `protocol` changes when releases no longer work together. */
export type PoolRelease = { version: string; protocol: number };

/** CPU cores and memory (bytes). */
export type PoolAmount = { cpus: number; memory: number };
/** `GET /hub/pool/share`: what this machine shares (ADR-0031 point 3). */
export type ShareStatus = {
  total: PoolAmount;
  free: PoolAmount;
  /** An approved member of a pool: only then is anything reported. */
  member: boolean;
  /** Unix ms of the last report that reached the coordinator. */
  reported_at: number | null;
  error: string | null;
  /** A node agent of the same membership runs here and reports the machine, not the hub (#170); absent from an older hub. */
  agent?: boolean;
  /** The devstack the coordinator runs, from its answer to the last report; absent from an older one. */
  coordinator?: PoolRelease;
  /** Why this machine offers the pool nothing (a node agent runs lanes; #171, on Windows #168). */
  unavailable: string;
};

/** A pool this machine belongs to or asked to join (ADR-0034), `GET /hub/pools`. */
export type PoolEntry = {
  name: string;
  joined: boolean;
  state?: 'pending' | 'approved' | 'removed' | null;
  coordinator?: string | null;
};

/** A service of a lane someone shares (ADR-0035). `forwarded`: its port here while this machine is connected to the lane (ADR-0053). */
export type SharedService = { name: string; port: number; scheme: string; here: number; forwarded?: number };

/** A lane another member shares with this machine, `GET /hub/pool/shared`. */
export type SharedLane = {
  lease: string;
  repo: string;
  lane: string;
  owner: string;
  owner_name: string;
  node: { id: string; name: string; api: string; devstack?: PoolRelease };
  services: SharedService[];
  /** The owner's devstack, from its last report. */
  owner_devstack?: PoolRelease;
};

/** Where the coordinator would start a lane, best first (ADR-0026 point 3), `GET /hub/pool/placement`.
 * `disk`: what the lane needs there beyond what is there already, `disk_room`: what the node has for lanes (ADR-0049). */
export type Placement = {
  node: string;
  name: string;
  eligible: boolean;
  why: string;
  cpus: number;
  memory: number;
  disk?: number;
  disk_room?: number;
  /** What the lane would hold there, and where that comes from (its footprint, or the estimate); absent from an older coordinator. */
  need?: { cpus: number; memory: number };
  need_from?: string;
};

/** A node's or the pool's capacity at one moment (ADR-0036), `GET /hub/pool/history`. */
export type CapacityPoint = {
  at: number;
  total_cpus: number;
  total_memory: number;
  offered_cpus: number;
  offered_memory: number;
  used_cpus: number;
  used_memory: number;
  gpus: number;
  gpus_offered: number;
  gpu_load: number;
  gpu_memory: number;
  gpu_memory_used: number;
};

/** A program this machine can open a stack's folder with (`GET /hub/openers`, ADR-0046). */
export type Opener = {
  id: string;
  label: string;
  kind: 'files' | 'terminal' | 'editor' | 'git' | 'agent';
  /** An agent started with a prompt the hub writes: shown for the user to send, or sent at once. */
  prompt?: 'prefill' | 'send';
};

/** The prompts the hub writes for a coding agent (`GET /hub/prompt`, ADR-0046 amendment 2026-10-22). */
export type PromptKind = 'setup' | 'fix-file' | 'upgrade' | 'start-failed' | 'lane-file' | 'mcp' | 'skills';
/** What a prompt is about: a registered stack, a folder before it is one, or a worktree of a stack (`lane-file`). */
export type PromptTarget = { stack: string } | { path: string } | { stack: string; path: string };

/** A kind of desktop notification and whether it is on (ADR-0045). */
export type NotifyKind = { kind: string; label: string; on: boolean; default: boolean };

/** `GET /hub/notify`: what notifies, and whether the desktop shows it. */
export type NotifySettings = {
  available: boolean;
  /** Why nothing shows (no desktop session, switched off in the system's settings). */
  unavailable: string | null;
  /** What does not work as it should while notifications show (devstack's name not registered with Windows, a click that could not open the dashboard); older hubs leave it out. */
  trouble?: string | null;
  kinds: NotifyKind[];
  /** Stack ids, and `<stack id>/<lane>`. */
  muted: string[];
};

/** How an MCP upstream logs in (ADR-0069). */
export type McpUpstreamAuth = { kind: 'none' } | { kind: 'token'; env?: string } | { kind: 'oauth'; client_id?: string; scopes?: string[] };

/** An MCP upstream as the hub lists it: never a secret. */
export type McpUpstream = {
  name: string;
  /** Its endpoint (Streamable HTTP); absent for one that is a command. */
  url?: string;
  /** The command the hub starts for it (stdio), program and arguments. */
  command?: string[];
  /** The variables that command gets besides a base; no secret. */
  env?: Record<string, string>;
  /** For a command with a token the hub keeps: the variable it is given in. */
  token_var?: string;
  /** Its tools are `<prefix>__<tool>`. */
  prefix: string;
  /** The tools offered; absent or empty: all. */
  allow?: string[];
  auth: McpUpstreamAuth;
  logged_in: boolean;
};

/** `GET /hub/upstreams`. `file`: no keychain on this machine (Linux without a Secret Service). */
export type McpUpstreams = { store: 'keychain' | 'file'; upstreams: McpUpstream[] };
