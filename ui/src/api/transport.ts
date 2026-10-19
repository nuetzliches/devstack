// The one seam between the dashboard and the outside world. `real.ts` talks to
// the hub over HTTP, and reads the event streams over its WebSocket
// (`/hub/ws`); `mock/` fakes the hub in the
// browser. Everything above this file is identical for both.

/** State of one event stream subscription. */
export type StreamState = 'connecting' | 'open' | 'error';

export type StreamHandlers<T> = {
  onMessage(data: T): void;
  /** Called on every state change; `error` is followed by a reconnect with backoff. */
  onState?(state: StreamState): void;
};

export type Subscription = { close(): void };

/** The answer to an HTTP request. `body` is parsed JSON when the response is JSON, else text (or null). */
export type Response<T = unknown> = {
  status: number;
  ok: boolean;
  body: T | string | null;
};

/** Thrown when the hub cannot be reached at all (network error, hub process gone). */
export class Unreachable extends Error {
  constructor(message = 'devstack hub is not reachable') {
    super(message);
    this.name = 'Unreachable';
  }
}

export interface Transport {
  /** Human-readable name shown in the UI ("live" or "mock"). */
  readonly kind: 'real' | 'mock';
  /**
   * One HTTP request, same-origin with credentials; `body` is sent as JSON.
   * Rejects with `Unreachable` when the network fails; HTTP errors resolve.
   */
  request<T = unknown>(method: string, path: string, body?: unknown): Promise<Response<T>>;
  /**
   * Opens an event stream (SSE, one JSON object per message). The transport
   * reconnects with backoff on errors until `close()` is called.
   */
  subscribe<T = unknown>(path: string, handlers: StreamHandlers<T>): Subscription;
}

/** Paths, relative to the hub origin. */
export const paths = {
  session: '/hub/session',
  hubEvents: '/hub/events',
  hubUpdate: '/hub/update',
  hubUpdateCheck: '/hub/update/check',
  dockerStart: '/hub/docker/start',
  inspect: (path: string) => `/hub/inspect?path=${encodeURIComponent(path)}`,
  stackAdd: '/hub/stacks/add',
  pickFolder: '/hub/pick-folder',
  scanStacks: '/hub/stacks/scan',
  stackInit: '/hub/stacks/init',
  up: (stack: string) => `/hub/stacks/${enc(stack)}/up`,
  down: (stack: string, infra = false) => `/hub/stacks/${enc(stack)}/down?infra=${infra}`,
  forget: (stack: string) => `/hub/stacks/${enc(stack)}`,
  pool: '/hub/pool',
  pools: '/hub/pools',
  poolShared: '/hub/pool/shared',
  poolTelemetry: '/hub/pool/telemetry',
  poolHistory: (pool: string, node: string | undefined, range: string) =>
    `/hub/pool/history?range=${range}${pool !== 'default' ? `&pool=${enc(pool)}` : ''}${node ? `&node=${enc(node)}` : ''}`,
  poolSharedOpen: (lease: string) => `/hub/pool/shared/${enc(lease)}/open`,
  poolShare: '/hub/pool/share',
  poolApprove: (id: string) => `/hub/pool/members/${enc(id)}/approve`,
  poolRemove: (id: string) => `/hub/pool/members/${enc(id)}/remove`,
  poolRole: (id: string) => `/hub/pool/members/${enc(id)}/role`,
  poolPause: (id: string, paused: boolean) => `/hub/pool/nodes/${enc(id)}/${paused ? 'pause' : 'resume'}`,
  poolEnd: (lease: string) => `/hub/pool/leases/${enc(lease)}/end`,
  poolUpdate: (id: string) => `/hub/pool/nodes/${enc(id)}/update`,
  poolPrune: (id: string) => `/hub/pool/nodes/${enc(id)}/prune`,
  poolJoin: '/hub/pool/join',
  poolTags: (id: string) => `/hub/pool/members/${enc(id)}/tags`,
  poolNode: (id: string) => `/hub/pool/members/${enc(id)}/node`,
  poolPolicy: (id: string) => `/hub/pool/nodes/${enc(id)}/policy`,
  /**
   * A daemon path of `stack`. With a `lane` (plan §3.5) it goes to that
   * lane's API, except the lane routes themselves, which the main tree's
   * daemon answers; `null`: the main working tree. The caller names the
   * lane: the stack store knows which one is shown (`useStack().apiLane`).
   */
  api: (stack: string, path: string, lane: string | null = null) => {
    // A lane shared with this machine to view (ADR-0040): read through its node.
    const shared = sharedRef(stack);
    if (shared) return `/hub/pool/shared/${enc(shared.lease)}/api${path}${path.includes('?') ? '&' : '?'}pool=${enc(shared.pool)}`;
    return `/hub/stacks/${enc(stack)}/api${lane && path.startsWith('/v1/') && !path.startsWith('/v1/lanes') ? `/v1/lanes/${enc(lane)}` : ''}${path}`;
  },
};

/** The id under which the dashboard shows a lane shared with this machine to view (ADR-0040). */
export function sharedId(pool: string, lease: string): string {
  return `shared~${pool}~${lease}`;
}

/** The pool and lease of a [`sharedId`], or `null` for a stack of this machine. */
export function sharedRef(id: string | null | undefined): { pool: string; lease: string } | null {
  const m = id ? /^shared~([^~]+)~(.+)$/.exec(id) : null;
  return m ? { pool: m[1], lease: m[2] } : null;
}

export const v1 = {
  stack: '/v1/stack',
  lanes: '/v1/lanes',
  laneMain: (lane: string) => `/v1/lanes/${enc(lane)}/main`,
  lane: (lane: string) => `/v1/lanes/${enc(lane)}`,
  laneStop: (lane: string) => `/v1/lanes/${enc(lane)}/stop`,
  laneReset: (lane: string) => `/v1/lanes/${enc(lane)}/reset`,
  laneMove: (lane: string) => `/v1/lanes/${enc(lane)}/move`,
  events: '/v1/events',
  usage: '/v1/usage?history=true',
  up: '/v1/up',
  down: '/v1/down',
  logStream: (service: string, tail = 500, build?: string) =>
    `/v1/services/${enc(service)}/logs/stream?tail=${tail}${build ? `&build=${enc(build)}` : ''}`,
  buildLog: (service: string) => `/v1/services/${enc(service)}/build-log`,
  logFiles: (service: string) => `/v1/services/${enc(service)}/logs/files`,
  builds: (service: string, limit = 50) => `/v1/services/${enc(service)}/builds?limit=${limit}`,
  keptLog: (service: string, file: string, tail = 2000) => `/v1/services/${enc(service)}/logs?file=${enc(file)}&tail=${tail}`,
  serviceAction: (service: string, action: ServiceAction) => `/v1/services/${enc(service)}/${action}`,
  groupAction: (group: string, action: GroupAction) => `/v1/groups/${enc(group)}/${action}`,
  infraAction: (group: string, service: string, action: 'start' | 'stop' | 'restart') => `/v1/infra/${enc(group)}/${enc(service)}/${action}`,
  infraLogs: (group: string, service: string, tail = 1000) => `/v1/infra/${enc(group)}/${enc(service)}/logs?tail=${tail}`,
  infraLogStream: (group: string, service: string, tail = 500) => `/v1/infra/${enc(group)}/${enc(service)}/logs/stream?tail=${tail}`,
  command: (name: string) => `/v1/commands/${enc(name)}`,
  commandRuns: (name: string) => `/v1/commands/${enc(name)}/runs`,
  run: (id: number) => `/v1/runs/${id}`,
  runLogStream: (id: number) => `/v1/runs/${id}/log/stream`,
};

export type ServiceAction = 'start' | 'stop' | 'restart' | 'rebuild' | 'promote' | 'rollback' | 'hot-reload-on' | 'hot-reload-off';
export type GroupAction = 'start' | 'stop' | 'restart';

function enc(s: string): string {
  return encodeURIComponent(s);
}
