// Types of the dashboard API (docs/dashboard-api.md). All JSON is snake_case,
// all times are Unix milliseconds.

// ---------------------------------------------------------------- hub

export type DaemonInfo = { pid: number; port: number; started: number; version: string };

export type StackEntry = {
  id: string;
  name: string;
  file: string;
  repo: string;
  running: boolean;
  daemon: DaemonInfo | null;
  last_seen: number | null;
};

export type HubSession = { version: string };

/** The Docker engine as the hub sees it (`GET /hub/docker`, `docker` events). */
export type DockerEngine =
  | { state: 'running'; version: string }
  | { state: 'stopped'; starter: string | null; hint: string }
  | { state: 'missing' };

export type HubEvent =
  | { type: 'stacks'; stacks: StackEntry[] }
  | { type: 'docker'; docker: DockerEngine }
  /** A newer release than the hub (`newer`), or none. */
  | { type: 'release'; current: string; newer: string | null };

// ---------------------------------------------------------------- daemon state

export type ServiceKind = 'service' | 'job' | 'devserver' | 'tool';

export type Status =
  | { state: 'stopped' | 'starting' | 'running' | 'healthy' }
  | { state: 'building'; build_id: string | null }
  | { state: 'unhealthy' | 'failed'; reason: string }
  | { state: 'completed' | 'exited'; exit_code: number };

export type StatusState = Status['state'];

export type CandidateState = 'changed' | 'building' | 'superseded' | 'starting' | 'switching' | 'held' | 'failed';

export type Candidate = {
  state: CandidateState;
  build_id: string | null;
  since: number;
  reason: string | null;
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

/** A lane: another git worktree running the stack beside the main one (plan §3.5). */
export type LaneView = {
  name: string;
  path: string;
  number: number;
  port_offset: number;
  owns_main: boolean;
  running: number;
  services: number;
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
  /** Runs the toolchain's watcher from the source tree instead of a build (older daemons omit it). */
  hot_reload?: boolean;
  /** The stack file gives the service a hot-reload mode. */
  hot_reload_available?: boolean;
};

export type ContainerView = {
  /** Published ports Docker no longer forwards (accepted, closed at once); older daemons omit it. */
  unreachable?: string[];
  service: string;
  name: string;
  state: string;
  health: string;
  ports: string[];
  /** Links for the dashboard (stack file `urls`, or http://localhost:<port> per published port). Older daemons omit it. */
  urls?: string[];
};

export type InfraView = { group: string; project: string; containers: ContainerView[]; error: string | null };

export type GroupView = { name: string; label: string; order: number | null; members: string[] };

export type CommandView = { name: string; description: string; confirm: string | null; requires: string[] };

export type StackView = {
  name: string;
  repo: string;
  version: string;
  services: ServiceView[];
  infra: InfraView[];
  groups: GroupView[];
  commands: CommandView[];
  /** The stack file changed since the daemon started; applies with the next start. Older daemons omit it. */
  config_changed?: boolean;
};

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
  | { type: 'infra'; group: string; message: string }
  | { type: 'command'; command: string; run: number; status: RunStatus }
  | { type: 'config'; changed: boolean }
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
