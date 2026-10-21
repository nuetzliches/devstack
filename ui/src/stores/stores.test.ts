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

  it('follows the lanes of other members on this machine', async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    const hub = useHub();
    await hub.init();
    const guest = { pool: 'default', lease: 'l1', member: 'm1', owner: 'anna', repo: 'shop', lane: 'feat-cart', state: 'running', since: 1, cpus: 2, memory: 2 ** 31 };
    fake.stream('/hub/events').handlers.onMessage({ type: 'guests', guests: [guest] });
    expect(hub.guests.map((g) => g.owner)).toEqual(['anna']);
    fake.stream('/hub/events').handlers.onMessage({ type: 'guests', guests: [] });
    expect(hub.guests).toEqual([]);
  });

  it('says so when the browser is not paired', async () => {
    fake.on('/hub/session', { status: 401, ok: false, body: { error: 'not paired' } });
    const hub = useHub();
    await hub.checkSession();
    expect(hub.status).toBe('unpaired');
  });

  it('asks once what it opens folders with, and names a tree and a program only (ADR-0046)', async () => {
    fake.on('/hub/openers', ok([{ id: 'vscode', label: 'VS Code', kind: 'editor' }]));
    fake.on('/hub/open', ok(null));
    const hub = useHub();
    await hub.loadOpeners();
    await hub.loadOpeners();
    expect(fake.requests.filter((r) => r.path === '/hub/openers')).toHaveLength(1);
    await hub.openWith('shop', '/src/shop', hub.openers![0]);
    expect(fake.requests.find((r) => r.path === '/hub/open')?.body).toEqual({ stack: 'shop', path: '/src/shop', with: 'vscode' });
  });

  it('clears "starting" when the hub reports a failed start', async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    const hub = useHub();
    await hub.init();
    hub.starting['shop'] = true;
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [{ ...entry('shop', false), start_error: 'boom' }] });
    expect(hub.starting['shop']).toBeUndefined();
  });

  it('tells the outcome of an update once, also from the hub it started (R-11)', async () => {
    fake.on('/hub/session', ok({ version: '1.0.0' }));
    const hub = useHub();
    await hub.init();
    const ui = useUi();
    const send = (u: object) => fake.stream('/hub/events').handlers.onMessage({ type: 'update', update: u } as never);
    const installed = { running: false, error: null, installed: '1.0.0', finished: Date.now() };
    send(installed);
    expect(ui.toasts.filter((x) => x.title === 'devstack 1.0.0 is installed')).toHaveLength(1);
    // Another connect (a reload, a second tab): the same outcome, no new toast.
    send(installed);
    expect(ui.toasts.filter((x) => x.title === 'devstack 1.0.0 is installed')).toHaveLength(1);
    // An old outcome the page did not see start is not news.
    send({ running: false, error: 'boom', installed: null, finished: Date.now() - 3600_000 });
    expect(ui.toasts.some((x) => x.title === 'The update failed')).toBe(false);
    // One it saw start is.
    send({ running: true, error: null, installed: null, finished: null });
    expect(hub.updating).toBe(true);
    send({ running: false, error: 'no network', installed: null, finished: Date.now() + 1 });
    expect(hub.updating).toBe(false);
    expect(ui.toasts.some((x) => x.title === 'The update failed' && x.message === 'no network')).toBe(true);
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

  it('a shared lane is read through its node, and changes nothing (ADR-0040)', async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    fake.on('/hub/pools', ok([{ name: 'default', joined: true, state: 'approved', coordinator: 'https://c' }]));
    fake.on('/hub/pool/shared', ok([{ lease: 'ab12', repo: 'crm', lane: 'fix', owner: 'o', owner_name: 'anna', node: { id: 'n', name: 'build', api: 'https://n' }, services: [] }]));
    const hub = useHub();
    await hub.init();
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [] });
    const pool = usePool();
    await pool.load();
    const stack = useStack();
    stack.select('shared~default~ab12');
    await vi.waitFor(() => expect(stack.running).toBe(true));
    expect(stack.readOnly).toBe(true);
    expect(stack.entry?.name).toBe('crm · fix');
    // Its stream goes to the hub's read route for the lane, the pool kept.
    const s = fake.stream('/hub/pool/shared/ab12/api/v1/events?pool=default');
    expect(s.closed).toBe(false);
    await stack.serviceAction('api', 'restart');
    expect(fake.requests.some((r) => r.path.includes('/restart'))).toBe(false);
    expect(useUi().toasts.some((x) => x.title === 'Read only')).toBe(true);
  });

  it("a shared lane's link opens a forward here to the service on its port (ADR-0035)", async () => {
    fake.on('/hub/session', ok({ version: '0.9.8' }));
    fake.on('/hub/pools', ok([{ name: 'default', joined: true, state: 'approved', coordinator: 'https://c' }]));
    const services = [
      { name: 'web', port: 4300, scheme: 'http', here: 4300 },
      { name: 'map', port: 8290, scheme: 'http', here: 8290 },
    ];
    fake.on('/hub/pool/shared', ok([{ lease: 'ab12', repo: 'crm', lane: 'fix', owner: 'o', owner_name: 'anna', node: { id: 'n', name: 'build', api: 'https://n' }, services }]));
    // Its owner's port is taken here: the forward is on another one.
    fake.on(/\/hub\/pool\/shared\/ab12\/open/, ok({ url: 'http://localhost:51000', port: 51000 }));
    const opened: string[] = [];
    vi.spyOn(window, 'open').mockImplementation((url) => {
      opened.push(String(url));
      return null;
    });
    const hub = useHub();
    await hub.init();
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [] });
    await usePool().load();
    const stack = useStack();
    stack.select('shared~default~ab12');
    const click = () => new MouseEvent('click', { cancelable: true });

    const e = click();
    stack.followLink(e, 'http://localhost:8290/ui?x=1');
    expect(e.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(opened).toEqual(['http://localhost:51000/ui?x=1']));
    const sent = fake.requests.filter((r) => r.path.startsWith('/hub/pool/shared/ab12/open'));
    expect(sent.map((r) => r.body)).toEqual([{ service: 'map' }]);

    // A port the lane does not share opens nothing, and says so.
    const none = click();
    stack.followLink(none, 'http://localhost:9999');
    expect(none.defaultPrevented).toBe(true);
    expect(useUi().toasts.some((x) => x.title === 'Not opening localhost:9999')).toBe(true);
  });

  it("one's own stack's links are followed as they are", async () => {
    const stack = await shownStack();
    const e = new MouseEvent('click', { cancelable: true });
    stack.followLink(e, 'http://localhost:8290');
    expect(e.defaultPrevented).toBe(false);
  });

  it('a lane removed while shown gives way to the main tree', async () => {
    const stack = await shownStack();
    const feat = { name: 'feat', path: '/src/feat', number: 1, active: true, port_offset: 100, owns_main: false, running: 2, services: 3 };
    fake.stream('/hub/stacks/shop/api/v1/events').handlers.onMessage({ type: 'lanes', lanes: [feat] });
    stack.selectLane('feat');
    const own = fake.stream('/hub/stacks/shop/api/v1/lanes/feat/v1/events');
    // Its worktree is removed: its own stream breaks, and the main tree no
    // longer lists it (the main tree's stream, which says so, is not followed).
    fake.on('/hub/stacks/shop/api/v1/lanes', ok([]));
    own.handlers.onState?.('error');
    await vi.waitFor(() => expect(stack.lane).toBeNull());
    expect(stack.apiLane).toBeNull();
    expect(own.closed).toBe(true);
    expect(useUi().toasts.some((x) => x.title === 'Lane feat is gone')).toBe(true);
  });

  it('a lane stopped stays shown, with its start', async () => {
    const stack = await shownStack();
    const feat = { name: 'feat', path: '/src/feat', number: 1, active: true, port_offset: 100, owns_main: false, running: 2, services: 3 };
    fake.stream('/hub/stacks/shop/api/v1/events').handlers.onMessage({ type: 'lanes', lanes: [feat] });
    stack.selectLane('feat');
    expect(stack.apiLane).toBe('feat');
    fake.on('/hub/stacks/shop/api/v1/lanes/feat/stop', ok(null));
    fake.on('/hub/stacks/shop/api/v1/lanes', ok([{ ...feat, active: false, running: 0 }]));
    await stack.stopLane('feat');
    expect(stack.lane).toBe('feat');
    expect(stack.apiLane).toBeNull();
  });

  it('shows the services stopped once the daemon is gone', async () => {
    const stack = await shownStack();
    const svc = (name: string, state: 'running' | 'stopped' | 'failed') => ({
      name,
      kind: 'service',
      groups: [],
      port: 5000,
      autostart: true,
      status: state === 'failed' ? { state, reason: 'boom' } : { state },
      build_id: null,
      pid: 42,
      since: 0,
      log: null,
      instance_port: 5001,
      proxy: null,
      candidate: null,
    });
    const view = { name: 'shop', repo: '/src/shop', version: '0.9.8', services: [svc('api', 'running'), svc('job', 'stopped'), svc('web', 'failed')], infra: [], groups: [], commands: [] };
    fake.stream('/hub/stacks/shop/api/v1/events').handlers.onMessage({ type: 'snapshot', stack: view } as never);
    expect(stack.view?.services[0].status.state).toBe('running');
    // `down`: the daemon closes its API, then stops its services, and exits.
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [entry('shop', false)] });
    await vi.waitFor(() => expect(stack.view?.services.map((s) => s.status.state)).toEqual(['stopped', 'stopped', 'failed']));
    expect(stack.view?.services[0].pid).toBeNull();
  });

  it('a stack of containers only stops them too, unless told otherwise', async () => {
    const stack = await shownStack();
    const app = { group: 'app', project: 'shop', containers: [{ service: 'db', name: 'shop-db-1', state: 'running', health: '', ports: [] }], error: null };
    const view = (services: unknown[]) => ({ name: 'shop', repo: '/src/shop', version: '0.9.8', services, infra: [app], groups: [], commands: [] });
    const events = fake.stream('/hub/stacks/shop/api/v1/events');
    const ui = useUi();
    const stopWith = async (checked?: boolean) => {
      const done = stack.stopStack();
      await vi.waitFor(() => expect(ui.confirmRequest).not.toBeNull());
      const option = ui.confirmRequest!.option!;
      const offered = option.checked;
      if (checked !== undefined) option.checked = checked;
      ui.confirmRequest!.resolve(true);
      await done;
      return { offered, path: fake.requests.at(-1)?.path };
    };
    events.handlers.onMessage({ type: 'snapshot', stack: view([]) } as never);
    expect(await stopWith()).toEqual({ offered: true, path: '/hub/stacks/shop/down?infra=true' });
    expect((await stopWith(false)).path).toBe('/hub/stacks/shop/down?infra=false');
    // Services of its own: the containers stay unless asked.
    events.handlers.onMessage({ type: 'snapshot', stack: view([{ name: 'api', status: { state: 'running' } }]) } as never);
    expect(await stopWith()).toEqual({ offered: false, path: '/hub/stacks/shop/down?infra=false' });
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

  describe('a lane started on a node (202, B1)', () => {
    const lane = { name: 'feat', path: '/src/feat', number: 1, port_offset: 100, owns_main: false, running: 0, services: 3 };
    const starting = { ...lane, active: false, moving: { kind: 'start' as const, to: 'build', step: 2, steps: 3, what: 'sync', since: 0 } };

    async function startOnNode() {
      const stack = await shownStack();
      const events = fake.stream('/hub/stacks/shop/api/v1/events');
      events.handlers.onMessage({ type: 'lanes', lanes: [{ ...lane, active: false }] });
      fake.on('/hub/stacks/shop/api/v1/lanes', (method) => (method === 'POST' ? { status: 202, ok: true, body: starting } : ok([starting])));
      fake.on('/hub/stacks/shop/api/v1/lanes/feat/v1/up', { status: 202, ok: true, body: null });
      const done = stack.startLane('feat', 'build');
      await vi.waitFor(() => expect(stack.lanes[0]?.moving?.step).toBe(2));
      return { stack, events, done };
    }
    const ups = () => fake.requests.filter((r) => r.path.endsWith('/feat/v1/up'));

    it('starts its services once it runs there', async () => {
      const { events, done } = await startOnNode();
      expect(ups()).toHaveLength(0);
      events.handlers.onMessage({ type: 'lanes', lanes: [{ ...lane, active: true, node: 'build' }] });
      await done;
      expect(ups()).toHaveLength(1);
    });

    it('says why its start failed, and starts nothing', async () => {
      const { events, done } = await startOnNode();
      events.handlers.onMessage({ type: 'lanes', lanes: [{ ...starting, moving: { ...starting.moving, error: 'no space left', failed_at: 1 } }] });
      await done;
      expect(ups()).toHaveLength(0);
      expect(useUi().toasts.some((x) => x.message === 'no space left')).toBe(true);
    });

    it('forgets the failed start once the stack stops', async () => {
      const { stack, events, done } = await startOnNode();
      events.handlers.onMessage({ type: 'lanes', lanes: [{ ...starting, moving: { ...starting.moving, error: 'no space left', failed_at: 1 } }] });
      await done;
      fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [entry('shop', false)] });
      await vi.waitFor(() => expect(stack.lanes[0]?.moving).toBeUndefined());
      expect(stack.lanes[0]?.name).toBe('feat');
    });
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

  it('grants and takes back the node role of a member (N-4)', async () => {
    const pool = usePool();
    fake.on(/\/hub\/pool\/members\/m1\/node/, (_m, _p, body) => ok({ id: 'm1', name: 'build', state: 'approved', role: 'member', requested_at: 0, node: (body as { node: boolean }).node }));
    await pool.setNode('m1', 'build', true);
    await pool.setNode('m1', 'build', false);
    const sent = fake.requests.filter((r) => r.path.startsWith('/hub/pool/members/m1/node'));
    expect(sent.map((r) => [r.method, r.body])).toEqual([
      ['POST', { node: true }],
      ['POST', { node: false }],
    ]);
    const titles = useUi().toasts.map((x) => x.title);
    expect(titles).toContain("build may run the pool's lanes");
    expect(titles).toContain('build takes no new lanes of the pool');
  });
});
