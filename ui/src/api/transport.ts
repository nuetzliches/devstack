// The one seam between the dashboard and the outside world. `real.ts` talks to
// the hub over HTTP and Server-Sent Events; `mock/` fakes the hub in the
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
  dockerStart: '/hub/docker/start',
  inspect: (path: string) => `/hub/inspect?path=${encodeURIComponent(path)}`,
  stackAdd: '/hub/stacks/add',
  pickFolder: '/hub/pick-folder',
  stackInit: '/hub/stacks/init',
  up: (stack: string) => `/hub/stacks/${enc(stack)}/up`,
  down: (stack: string, infra = false) => `/hub/stacks/${enc(stack)}/down?infra=${infra}`,
  forget: (stack: string) => `/hub/stacks/${enc(stack)}`,
  api: (stack: string, path: string) => `/hub/stacks/${enc(stack)}/api${path}`,
};

export const v1 = {
  stack: '/v1/stack',
  events: '/v1/events',
  usage: '/v1/usage?history=true',
  up: '/v1/up',
  logStream: (service: string, tail = 500, build?: string) =>
    `/v1/services/${enc(service)}/logs/stream?tail=${tail}${build ? `&build=${enc(build)}` : ''}`,
  buildLog: (service: string) => `/v1/services/${enc(service)}/build-log`,
  logFiles: (service: string) => `/v1/services/${enc(service)}/logs/files`,
  builds: (service: string, limit = 50) => `/v1/services/${enc(service)}/builds?limit=${limit}`,
  keptLog: (service: string, file: string, tail = 2000) => `/v1/services/${enc(service)}/logs?file=${enc(file)}&tail=${tail}`,
  serviceAction: (service: string, action: ServiceAction) => `/v1/services/${enc(service)}/${action}`,
  groupAction: (group: string, action: GroupAction) => `/v1/groups/${enc(group)}/${action}`,
  infraRestart: (group: string, service: string) => `/v1/infra/${enc(group)}/${enc(service)}/restart`,
  infraLogs: (group: string, service: string, tail = 1000) => `/v1/infra/${enc(group)}/${enc(service)}/logs?tail=${tail}`,
  command: (name: string) => `/v1/commands/${enc(name)}`,
  commandRuns: (name: string) => `/v1/commands/${enc(name)}/runs`,
  run: (id: number) => `/v1/runs/${id}`,
  runLogStream: (id: number) => `/v1/runs/${id}/log/stream`,
};

export type ServiceAction = 'start' | 'stop' | 'restart' | 'promote' | 'rollback' | 'hot-reload-on' | 'hot-reload-off';
export type GroupAction = 'start' | 'stop' | 'restart';

function enc(s: string): string {
  return encodeURIComponent(s);
}
