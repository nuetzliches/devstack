import type { Response, StreamHandlers, StreamState, Subscription, Transport } from './transport';
import { Unreachable } from './transport';

/** Reconnect delays after a stream error: 1 s, 2 s, 4 s ... capped at 15 s, with jitter. */
function backoff(attempt: number): number {
  const base = Math.min(15000, 1000 * 2 ** Math.min(attempt, 4));
  return base * (0.8 + Math.random() * 0.4);
}

/** How long a stream must have been open for a drop to reconnect at once, without an error. */
const STABLE_MS = 5000;

export class RealTransport implements Transport {
  readonly kind = 'real' as const;

  async request<T = unknown>(method: string, path: string, body?: unknown): Promise<Response<T>> {
    let res: globalThis.Response;
    try {
      res = await fetch(path, {
        method,
        credentials: 'same-origin',
        cache: 'no-store',
        headers: body !== undefined ? { 'Content-Type': 'application/json', Accept: 'application/json' } : { Accept: 'application/json, text/plain' },
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
    } catch (e) {
      throw new Unreachable(e instanceof Error ? e.message : undefined);
    }
    const type = res.headers.get('content-type') ?? '';
    let parsed: T | string | null = null;
    if (res.status !== 204) {
      const text = await res.text().catch(() => '');
      if (type.includes('json') && text) {
        try {
          parsed = JSON.parse(text) as T;
        } catch {
          parsed = text;
        }
      } else {
        parsed = text || null;
      }
    }
    return { status: res.status, ok: res.ok, body: parsed };
  }

  /**
   * A live stream over WebSocket (`/hub/ws?path=`): the hub carries the
   * server-sent event stream at `path`, one message per event. Not as an
   * EventSource, because a browser keeps at most six HTTP/1.1 connections
   * per host across all tabs and app windows; a few dashboard windows with
   * their streams used them up, and every request after that hung in the
   * browser's queue. WebSockets do not count against that limit.
   */
  subscribe<T = unknown>(path: string, handlers: StreamHandlers<T>): Subscription {
    let closed = false;
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    const setState = (s: StreamState) => handlers.onState?.(s);
    const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/hub/ws?path=${encodeURIComponent(path)}`;

    const connect = () => {
      timer = null;
      if (closed) return;
      setState('connecting');
      const socket = new WebSocket(url);
      ws = socket;
      let openedAt: number | null = null;
      socket.onopen = () => {
        openedAt = Date.now();
        attempt = 0;
        setState('open');
      };
      socket.onmessage = (ev: MessageEvent<string>) => {
        let data: T;
        try {
          data = JSON.parse(ev.data) as T;
        } catch {
          return; // a malformed message; ignore
        }
        handlers.onMessage(data);
      };
      // An error is always followed by close: reconnect there, once, with a
      // growing delay, so a 401/503 does not end the stream for good.
      socket.onclose = () => {
        if (ws !== socket) return;
        ws = null;
        if (closed) return;
        // A connection that held for a while and dropped (the window was in
        // the background, the machine slept): try again at once, still
        // "connecting"; only a reconnect that fails is an error. One that
        // dropped right after it opened waits like a failed one, so a hub
        // that closes every stream does not make a busy loop.
        if (openedAt !== null && Date.now() - openedAt >= STABLE_MS) {
          connect();
          return;
        }
        setState('error');
        timer = setTimeout(connect, backoff(attempt++));
      };
    };

    // Back in view or online: do not sit out the rest of a backoff, which
    // a background window's throttled timers stretch to a minute or more.
    const wake = () => {
      if (timer === null || closed || document.visibilityState === 'hidden') return;
      clearTimeout(timer);
      connect();
    };
    document.addEventListener('visibilitychange', wake);
    window.addEventListener('online', wake);

    connect();
    return {
      close() {
        closed = true;
        document.removeEventListener('visibilitychange', wake);
        window.removeEventListener('online', wake);
        if (timer) clearTimeout(timer);
        const socket = ws;
        ws = null;
        socket?.close();
      },
    };
  }
}
