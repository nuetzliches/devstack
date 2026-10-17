import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import type { StackEntry } from '@/api/types';
import { FakeTransport, ok } from '@/test/fake';

const t = vi.hoisted(() => ({ current: null as FakeTransport | null }));

vi.mock('@/api', async (importOriginal) => {
  const real = await importOriginal<typeof import('@/api')>();
  return {
    ...real,
    api: () => {
      if (!t.current) throw new Error('no fake transport');
      return t.current;
    },
  };
});

// Imported after the mock, so they get the fake transport.
const { useHub } = await import('./hub');
const { useStack } = await import('./stack');
const { usePool } = await import('./pool');
const { useUi } = await import('./ui');

const entry = (id: string, running = true): StackEntry => ({
  id,
  name: id,
  file: `/src/${id}/devstack.toml`,
  repo: `/src/${id}`,
  running,
  daemon: running ? { pid: 1, port: 7000, started: 0, version: '0.9.8' } : null,
  last_seen: null,
});

let fake: FakeTransport;

beforeEach(() => {
  localStorage.clear();
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
});

describe('hub store', () => {
  it('is paired after a session answer and follows the stack stream', async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    const hub = useHub();
    await hub.init();
    expect(hub.status).toBe('ok');
    expect(hub.version).toBe('0.9.8');
    const s = fake.stream('/hub/events');
    s.handlers.onState?.('open');
    s.handlers.onMessage({ type: 'stacks', stacks: [entry('shop')] });
    expect(hub.reachable).toBe(true);
    expect(hub.entry('shop')?.running).toBe(true);
    // Kept for the offline view.
    expect(JSON.parse(localStorage.getItem('devstack.hub') ?? '{}').stacks).toHaveLength(1);
  });

  it('says so when the browser is not paired', async () => {
    fake.on('/hub/session', { status: 401, ok: false, body: { error: 'not paired' } });
    const hub = useHub();
    await hub.checkSession();
    expect(hub.status).toBe('unpaired');
  });

  it('clears "starting" when the hub reports a failed start', async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    const hub = useHub();
    await hub.init();
    hub.starting['shop'] = true;
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [{ ...entry('shop', false), start_error: 'boom' }] });
    expect(hub.starting['shop']).toBeUndefined();
  });
});

describe('stack store', () => {
  async function shownStack() {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    const hub = useHub();
    await hub.init();
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [entry('shop')] });
    const stack = useStack();
    stack.select('shop');
    return stack;
  }

  it('streams the main tree, then the lane chosen (paths name the lane)', async () => {
    const stack = await shownStack();
    expect(stack.apiLane).toBeNull();
    fake.stream('/hub/stacks/shop/api/v1/events');
    stack.selectLane('feat');
    expect(stack.apiLane).toBe('feat');
    const lane = fake.stream('/hub/stacks/shop/api/v1/lanes/feat/v1/events');
    expect(lane.closed).toBe(false);
    stack.selectLane(null);
    expect(stack.apiLane).toBeNull();
    expect(lane.closed).toBe(true);
  });

  it('a lane not started is shown from the main tree', async () => {
    const stack = await shownStack();
    fake.stream('/hub/stacks/shop/api/v1/events').handlers.onMessage({
      type: 'lanes',
      lanes: [{ name: 'feat', path: '/src/feat', number: 1, active: false, port_offset: 100, owns_main: false, running: 0, services: 3 }],
    });
    stack.selectLane('feat');
    expect(stack.apiLane).toBeNull();
    fake.stream('/hub/stacks/shop/api/v1/events');
  });
});

describe('pool store', () => {
  it('opens a shared service only as a web page (H-4)', async () => {
    const opened: string[] = [];
    vi.spyOn(window, 'open').mockImplementation((url) => {
      opened.push(String(url));
      return null;
    });
    const pool = usePool();
    fake.on(/\/hub\/pool\/shared\/good\/open/, ok({ url: 'http://localhost:4200', port: 4200 }));
    fake.on(/\/hub\/pool\/shared\/bad\/open/, ok({ url: 'ms-msdt://localhost:4200', port: 4200 }));
    await pool.openShared('default', 'good', 'web');
    await pool.openShared('default', 'bad', 'web');
    expect(opened).toEqual(['http://localhost:4200']);
    expect(useUi().toasts.some((x) => x.title.startsWith('Not opening'))).toBe(true);
  });
});
