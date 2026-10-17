// A transport for unit tests: answers by path, records what was asked, and
// lets a test push stream messages by hand.
import type { Response, StreamHandlers, Subscription, Transport } from '@/api/transport';

type Handler = (method: string, path: string, body?: unknown) => Response | Promise<Response>;

export type Stream = { path: string; handlers: StreamHandlers<unknown>; closed: boolean };

export class FakeTransport implements Transport {
  readonly kind = 'mock' as const;
  readonly requests: { method: string; path: string; body?: unknown }[] = [];
  readonly streams: Stream[] = [];
  private routes: [string | RegExp, Handler][] = [];

  /** Answer requests whose path is `path` (or matches it). */
  on(path: string | RegExp, h: Handler | Response): this {
    this.routes.push([path, typeof h === 'function' ? h : () => h]);
    return this;
  }

  async request<T = unknown>(method: string, path: string, body?: unknown): Promise<Response<T>> {
    this.requests.push({ method, path, body });
    const route = this.routes.find(([p]) => (typeof p === 'string' ? p === path : p.test(path)));
    const res = route ? await route[1](method, path, body) : { status: 404, ok: false, body: { error: 'no route' } };
    return res as Response<T>;
  }

  subscribe<T = unknown>(path: string, handlers: StreamHandlers<T>): Subscription {
    const s: Stream = { path, handlers: handlers as StreamHandlers<unknown>, closed: false };
    this.streams.push(s);
    return { close: () => (s.closed = true) };
  }

  /** The newest open stream at `path`. */
  stream(path: string): Stream {
    const s = [...this.streams].reverse().find((x) => x.path === path && !x.closed);
    if (!s) throw new Error(`no open stream at ${path}; open: ${this.streams.map((x) => x.path).join(', ')}`);
    return s;
  }
}

export const ok = (body: unknown): Response => ({ status: 200, ok: true, body: body as Response['body'] });
