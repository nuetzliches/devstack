import type { Response, StreamHandlers, StreamState, Subscription, Transport } from './transport';
import { Unreachable } from './transport';

/** Reconnect delays after a stream error: 1 s, 2 s, 4 s ... capped at 15 s, with jitter. */
function backoff(attempt: number): number {
  const base = Math.min(15000, 1000 * 2 ** Math.min(attempt, 4));
  return base * (0.8 + Math.random() * 0.4);
}

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

  subscribe<T = unknown>(path: string, handlers: StreamHandlers<T>): Subscription {
    let closed = false;
    let es: EventSource | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    const setState = (s: StreamState) => handlers.onState?.(s);

    const connect = () => {
      if (closed) return;
      setState('connecting');
      es = new EventSource(path, { withCredentials: true });
      es.onopen = () => {
        attempt = 0;
        setState('open');
      };
      es.onmessage = (ev: MessageEvent<string>) => {
        let data: T;
        try {
          data = JSON.parse(ev.data) as T;
        } catch {
          return; // a keep-alive or a malformed line; ignore
        }
        handlers.onMessage(data);
      };
      es.onerror = () => {
        // EventSource retries on its own only for some failures; take over so
        // that the delay grows and a 401/503 does not end the stream for good.
        es?.close();
        es = null;
        if (closed) return;
        setState('error');
        timer = setTimeout(connect, backoff(attempt++));
      };
    };

    connect();
    return {
      close() {
        closed = true;
        if (timer) clearTimeout(timer);
        es?.close();
        es = null;
      },
    };
  }
}
