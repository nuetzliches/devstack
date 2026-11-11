import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { StreamState } from './transport';
import { RealTransport } from './real';

/** A WebSocket the test opens and closes by hand. */
class FakeSocket {
  static all: FakeSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: MessageEvent<string>) => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.all.push(this);
  }
  open() {
    this.onopen?.();
  }
  drop() {
    this.onclose?.();
  }
  close() {
    this.onclose?.();
  }
}

describe('RealTransport.subscribe', () => {
  let states: StreamState[];

  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.all = [];
    states = [];
    vi.stubGlobal('WebSocket', FakeSocket);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const subscribe = () => new RealTransport().subscribe('/hub/events', { onMessage: () => {}, onState: (s) => states.push(s) });

  it('reconnects a stream that held at once, without an error', () => {
    const sub = subscribe();
    FakeSocket.all[0].open();
    vi.advanceTimersByTime(60_000);
    FakeSocket.all[0].drop();
    expect(FakeSocket.all).toHaveLength(2);
    FakeSocket.all[1].open();
    expect(states).toEqual(['connecting', 'open', 'connecting', 'open']);
    sub.close();
  });

  it('reports an error when the reconnect fails, and waits before the next', () => {
    const sub = subscribe();
    FakeSocket.all[0].open();
    vi.advanceTimersByTime(60_000);
    FakeSocket.all[0].drop();
    FakeSocket.all[1].drop();
    expect(states.at(-1)).toBe('error');
    expect(FakeSocket.all).toHaveLength(2);
    vi.advanceTimersByTime(1500);
    expect(FakeSocket.all).toHaveLength(3);
    sub.close();
  });

  it('waits after a stream that dropped right after it opened', () => {
    const sub = subscribe();
    FakeSocket.all[0].open();
    FakeSocket.all[0].drop();
    expect(states.at(-1)).toBe('error');
    expect(FakeSocket.all).toHaveLength(1);
    sub.close();
  });

  it('cuts a backoff short when the window comes back into view', () => {
    const sub = subscribe();
    FakeSocket.all[0].drop();
    expect(states.at(-1)).toBe('error');
    document.dispatchEvent(new Event('visibilitychange'));
    expect(FakeSocket.all).toHaveLength(2);
    expect(states.at(-1)).toBe('connecting');
    sub.close();
  });
});
