// In-browser fake of the hub and three stack daemons. No network.
//
// URL switches for testing the edge screens:
//   ?mock=unpaired   GET /hub/session answers 401
//   ?mock=hubdown    the hub is unreachable (toggle at runtime: window.devstackMock.hubDown(false))
import type { HubEvent, LogStreamEvent, RunLogEvent, StackEvent, StackEntry } from '../types';
import type { Response, StreamHandlers, Subscription, Transport } from '../transport';
import { Unreachable } from '../transport';
import { persist } from '@/lib/persist';
import { crmSeed, docsSeed, failedReason, shopSeed, stamp } from './data';
import { Abort, MockStack, sleep, type Listener } from './stack';

type Route = { stop(): void } | null;

class MockHub {
  stacks: MockStack[] = [];
  hubSubs = new Set<Listener<HubEvent>>();
  hubDown = false;
  unpaired = false;
  private streams = new Set<{ reconnect(): void }>();

  constructor() {
    const now = Date.now();
    const shop = new MockStack(shopSeed(now));
    const crm = new MockStack(crmSeed(now));
    const docs = new MockStack(docsSeed(now));
    this.stacks = [shop, crm, docs];
    for (const s of this.stacks) {
      s.onEntryChange = () => this.publish();
      if (s.running) s.start();
    }
    seedRuns(shop, crm);
    // A last-seen state for the offline stack, as if the dashboard had seen it before.
    if (!persist.stack(docs.entry.id)) {
      persist.saveStack(docs.entry.id, { view: JSON.parse(JSON.stringify(docs.view)), at: docs.entry.last_seen ?? now });
    }
    void shopScript(shop);
    void crmScript(crm);

    const q = new URLSearchParams(location.search).get('mock') ?? '';
    this.hubDown = q.includes('hubdown');
    this.unpaired = q.includes('unpaired');
  }

  entries(): StackEntry[] {
    return this.stacks.map((s) => ({ ...s.entry, last_seen: s.running ? Date.now() : s.entry.last_seen }));
  }

  publish(): void {
    const ev: HubEvent = { type: 'stacks', stacks: this.entries() };
    for (const l of this.hubSubs) l.send(ev);
  }

  stack(id: string): MockStack | undefined {
    return this.stacks.find((s) => s.entry.id === id);
  }

  setHubDown(down: boolean): void {
    this.hubDown = down;
    for (const s of [...this.streams]) s.reconnect();
  }

  track(s: { reconnect(): void }): () => void {
    this.streams.add(s);
    return () => this.streams.delete(s);
  }
}

function seedRuns(shop: MockStack, crm: MockStack): void {
  shop.seedRun('seed', 50 * 60_000, 'succeeded', ['Seeding customers... 120', 'Seeding products... 48', 'Seeding orders... 300', 'done in 2.1 s']);
  shop.seedRun('lint', 32 * 60_000, 'failed', ['eslint src/', "src/web/cart.ts:14:7  error  'total' is assigned a value but never used  no-unused-vars", '1 problem (1 error, 0 warnings)'], 'exit code 1');
  shop.seedRun('lint', 12 * 60_000, 'succeeded', ['eslint src/', 'no problems']);
  crm.seedRun('reindex', 70 * 60_000, 'succeeded', ['Reindexing 18,204 contacts', 'done in 1.9 s']);
}

/** The scripted timeline of the "shop" stack: a change that goes live, a superseded build, a failed build, a held one. */
async function shopScript(s: MockStack): Promise<void> {
  for (;;) {
    try {
      if (!s.running) {
        await sleep(1000);
        continue;
      }
      await sleep(4000);
      // 1. a change to the api goes live
      s.log('api', `${stamp()} info: devstack[0] change detected in src/Api (2 files)`);
      await s.candidateFlow('api');
      await sleep(5000);

      // 2. the worker: a newer change supersedes the running build
      s.cand('worker', 'changed');
      await sleep(1000);
      s.cand('worker', 'building');
      s.buildsActive++;
      await sleep(2200);
      s.buildsActive--;
      s.cand('worker', 'superseded', null, 'a newer change arrived; the build was cancelled');
      await sleep(1500);
      await s.candidateFlow('worker', { changed: false, buildMs: 3000 });
      await sleep(4000);

      // 3. a broken edit: the build fails, the running api stays
      await s.candidateFlow('api', { fail: failedReason, buildMs: 3600 });
      await sleep(9000);

      // an infrastructure blip
      s.infra('data', 'queue', 'restarting', 'starting', 'queue restarting');
      await sleep(2500);
      s.infra('data', 'queue', 'running', 'healthy', 'queue healthy');
      await sleep(4000);

      // 4. the fix touches a migration: built, health-checked, then held
      if (s.svc('api').candidate?.state === 'failed' || !s.svc('api').candidate) {
        await s.candidateFlow('api', {
          hold:
            'changes schema inputs (src/Data/Migrations/20261015_AddOrderNotes.cs matches promote.hold_if_changed). ' +
            'The candidate passed its health check against the current schema; promote it to run the migrate job and switch: devstack promote api',
        });
      }
      // wait until someone promotes (or discards) it
      while (s.running && s.svc('api').candidate?.state === 'held') await sleep(1000);
      await sleep(8000);
    } catch (e) {
      if (!(e instanceof Abort)) console.warn('[mock] script', e);
      await sleep(1000);
    }
  }
}

async function crmScript(s: MockStack): Promise<void> {
  for (;;) {
    try {
      await sleep(15000);
      if (!s.running) continue;
      await s.candidateFlow('contacts', { buildMs: 5000 });
      await sleep(25000);
      if (!s.running) continue;
      s.infra('core', 'search', 'running', 'healthy', 'search healthy');
    } catch (e) {
      if (!(e instanceof Abort)) console.warn('[mock] script', e);
    }
  }
}


function json<T>(status: number, body: T): Response<T> {
  return { status, ok: status >= 200 && status < 300, body: JSON.parse(JSON.stringify(body)) as T };
}

function text(status: number, body: string): Response<never> {
  return { status, ok: status >= 200 && status < 300, body };
}

const empty = (status: number): Response<never> => ({ status, ok: true, body: null });

function ready(state: string): boolean {
  return state === 'healthy' || state === 'running';
}

export class MockTransport implements Transport {
  readonly kind = 'mock' as const;
  private hub = new MockHub();

  constructor() {
    (window as unknown as { devstackMock: unknown }).devstackMock = {
      hubDown: (down = true) => this.hub.setHubDown(down),
      stacks: () => this.hub.stacks,
    };
  }

  async request<T = unknown>(method: string, path: string, body?: unknown): Promise<Response<T>> {
    await sleep(80 + Math.random() * 120);
    if (this.hub.hubDown) throw new Unreachable('mock: hub is down');
    return (await this.route(method.toUpperCase(), path, body as Record<string, unknown> | undefined)) as Response<T>;
  }

  private async route(method: string, path: string, body?: Record<string, unknown>): Promise<Response> {
    const url = new URL(path, 'http://hub');
    const p = url.pathname;
    const hub = this.hub;
    if (p === '/hub/session') return hub.unpaired ? json(401, { error: 'not paired' }) : json(200, { version: '0.2.0' });
    if (hub.unpaired) return json(401, { error: 'not paired' });
    if (p === '/hub/stacks' && method === 'GET') return json(200, hub.entries());

    const m = /^\/hub\/stacks\/([^/]+)(\/.*)?$/.exec(p);
    if (!m) return json(404, { error: `no route ${method} ${p}` });
    const stack = hub.stack(decodeURIComponent(m[1]));
    if (!stack) return json(404, { error: 'unknown stack' });
    const rest = m[2] ?? '';

    if (rest === '/up' && method === 'POST') {
      if (!stack.running) void stack.goUp();
      return empty(202);
    }
    if (rest === '/down' && method === 'POST') {
      setTimeout(() => stack.goDown(), 600);
      return empty(202);
    }
    if (rest === '' && method === 'DELETE') {
      hub.stacks = hub.stacks.filter((s) => s !== stack);
      hub.publish();
      return empty(204);
    }
    if (!rest.startsWith('/api/')) return json(404, { error: 'no route' });
    if (!stack.running) return json(503, { error: 'stack not running' });
    return this.daemon(stack, method, rest.slice(4), url.searchParams, body);
  }

  private async daemon(s: MockStack, method: string, p: string, q: URLSearchParams, body?: Record<string, unknown>): Promise<Response> {
    const view = () => json(200, s.view);
    if (p === '/v1/stack' && method === 'GET') return view();
    if (p === '/v1/usage' && method === 'GET') return json(200, s.usageWithHistory());
    if (p === '/v1/up' && method === 'POST') return empty(202);

    let m = /^\/v1\/services\/([^/]+)\/(start|stop|restart|promote|rollback|build-log|logs)$/.exec(p);
    if (m) {
      const name = decodeURIComponent(m[1]);
      const action = m[2];
      const svc = s.view.services.find((x) => x.name === name);
      if (!svc) return json(404, { error: `no service ${name}` });
      if (action === 'build-log') return text(200, s.buildLog(name));
      if (action === 'logs') return text(200, '');
      try {
        switch (action) {
          case 'start':
            if (ready(svc.status.state)) return json(409, { error: `${name} is already running` });
            if (svc.kind === 'job') {
              s.setStatus(name, { state: 'running' });
              await sleep(2500);
              s.setStatus(name, { state: 'completed', exit_code: 0 });
              return view();
            }
            s.setStatus(name, { state: 'starting' });
            await sleep(1600);
            s.startInstance(svc, svc.build_id ?? '0000000000000000');
            return view();
          case 'stop':
            svc.pid = null;
            if (svc.proxy) svc.proxy = { ...svc.proxy, upstream: null, active: 0 };
            s.setStatus(name, { state: 'stopped' });
            return view();
          case 'restart': {
            if (s.failedBuild.has(name) && name === 'api') {
              s.cand(name, 'building');
              s.buildsActive++;
              await sleep(2600);
              s.buildsActive--;
              s.cand(name, 'failed', null, failedReason);
              return json(409, { error: failedReason });
            }
            await s.candidateFlow(name, { changed: false, buildMs: 2800 });
            return view();
          }
          case 'promote': {
            const c = svc.candidate;
            if (!c || c.state !== 'held' || !c.build_id) return json(409, { error: `${name} has no held candidate to promote` });
            const build = c.build_id;
            s.cand(name, 'switching', build, 'running the migrate job');
            if (s.view.services.some((x) => x.name === 'migrate')) {
              s.setStatus('migrate', { state: 'running' });
              s.log('migrate', `${stamp()} info: Migrations[0] Applying 20261015_AddOrderNotes`);
              await sleep(1800);
              s.log('migrate', `${stamp()} info: Migrations[0] Done, 1 migration applied`);
              s.setStatus('migrate', { state: 'completed', exit_code: 0 });
            }
            await s.switchTo(name, build);
            return view();
          }
          case 'rollback': {
            const prev = s.previous[name];
            if (!prev || prev === svc.build_id) return json(409, { error: `${name} has no previous build to roll back to` });
            await s.switchTo(name, prev);
            return view();
          }
        }
      } catch (e) {
        if (e instanceof Abort) return json(503, { error: 'stack not running' });
        throw e;
      }
    }

    m = /^\/v1\/infra\/([^/]+)\/([^/]+)\/restart$/.exec(p);
    if (m && method === 'POST') {
      const group = decodeURIComponent(m[1]);
      const service = decodeURIComponent(m[2]);
      s.infra(group, service, 'restarting', 'starting', `${service} restarting`);
      setTimeout(() => s.infra(group, service, 'running', /cache|collector/.test(service) ? '' : 'healthy', `${service} running`), 2600);
      return empty(204);
    }

    m = /^\/v1\/commands\/([^/]+)(\/runs)?$/.exec(p);
    if (m) {
      const name = decodeURIComponent(m[1]);
      const cmd = s.view.commands.find((c) => c.name === name);
      if (!cmd) return json(404, { error: `no command ${name}` });
      if (m[2]) return json(200, s.runs.filter((r) => r.command === name));
      if (method !== 'POST') return json(405, { error: 'method not allowed' });
      if (cmd.confirm && body?.confirm !== true) return json(428, { confirm: cmd.confirm });
      for (const r of cmd.requires) {
        const svc = s.view.services.find((x) => x.name === r);
        const ctr = s.view.infra.flatMap((g) => g.containers).find((c) => c.service === r);
        const state = svc ? svc.status.state : ctr ? (ctr.state === 'running' ? 'running' : ctr.state) : 'missing';
        if (!ready(state)) return json(409, { error: `${name} requires ${r}, which is not ready (${state})` });
      }
      const run = s.startRun(name, commandPlan(name));
      return json(202, { run: run.id });
    }

    m = /^\/v1\/runs\/(\d+)$/.exec(p);
    if (m) {
      const run = s.runs.find((r) => r.id === Number(m![1]));
      return run ? json(200, run) : json(404, { error: 'no such run' });
    }
    void q;
    return json(404, { error: `no route ${method} ${p}` });
  }

  subscribe<T = unknown>(path: string, handlers: StreamHandlers<T>): Subscription {
    const hub = this.hub;
    let closed = false;
    let route: Route = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    const fail = () => {
      route?.stop();
      route = null;
      if (closed) return;
      handlers.onState?.('error');
      timer = setTimeout(open, Math.min(8000, 800 * 2 ** Math.min(attempt++, 4)));
    };
    const send = (d: unknown) => {
      if (!closed) handlers.onMessage(d as T);
    };
    const listener = <E,>(): Listener<E> => ({ send: (d: E) => send(d), kill: () => fail() });

    const open = () => {
      if (closed) return;
      handlers.onState?.('connecting');
      timer = setTimeout(() => {
        if (closed) return;
        route = hub.hubDown ? null : this.attach(path, listener);
        if (!route) return fail();
        attempt = 0;
        handlers.onState?.('open');
      }, 120);
    };

    const untrack = hub.track({
      reconnect: () => {
        if (closed) return;
        if (timer) clearTimeout(timer);
        if (hub.hubDown) fail();
        else if (!route) open();
      },
    });
    open();
    return {
      close() {
        closed = true;
        if (timer) clearTimeout(timer);
        route?.stop();
        untrack();
      },
    };
  }

  /** Connects a stream to the fake hub; null = the server would refuse it (401/503/404). */
  private attach(path: string, listener: <E>() => Listener<E>): Route {
    const hub = this.hub;
    if (hub.unpaired) return null;
    const url = new URL(path, 'http://hub');
    const p = url.pathname;
    if (p === '/hub/events') {
      const l = listener<HubEvent>();
      l.send({ type: 'stacks', stacks: hub.entries() });
      hub.hubSubs.add(l);
      return { stop: () => hub.hubSubs.delete(l) };
    }
    const m = /^\/hub\/stacks\/([^/]+)\/api(\/.*)$/.exec(p);
    if (!m) return null;
    const s = hub.stack(decodeURIComponent(m[1]));
    if (!s || !s.running) return null;
    const rest = m[2];
    if (rest === '/v1/events') {
      const off = s.subscribeEvents(listener<StackEvent>());
      return { stop: off };
    }
    let mm = /^\/v1\/services\/([^/]+)\/logs\/stream$/.exec(rest);
    if (mm) {
      const tail = Number(url.searchParams.get('tail') ?? '200');
      const off = s.subscribeLogs(decodeURIComponent(mm[1]), tail, listener<LogStreamEvent>());
      return { stop: off };
    }
    mm = /^\/v1\/runs\/(\d+)\/log\/stream$/.exec(rest);
    if (mm) {
      const off = s.subscribeRunLog(Number(mm[1]), listener<RunLogEvent>());
      return off ? { stop: off } : null;
    }
    return null;
  }
}

function commandPlan(name: string): { lines: string[]; fail?: string; stepMs?: number } {
  switch (name) {
    case 'reset-db':
      return {
        lines: [
          'Stopping api and worker',
          'DROP DATABASE shop',
          'CREATE DATABASE shop',
          'Running migrate: 14 migrations',
          '  20260901_Initial ... ok',
          '  20260915_AddInventory ... ok',
          '  20261015_AddOrderNotes ... ok',
          'Seeding demo data ... 468 rows',
          'Starting api and worker',
          'done in 6.2 s',
        ],
        stepMs: 550,
      };
    case 'seed':
      return { lines: ['Seeding customers... 120', 'Seeding products... 48', 'Seeding orders... 300', 'done in 2.0 s'], stepMs: 500 };
    case 'lint':
      return Math.random() < 0.5
        ? { lines: ['eslint src/', 'stylelint src/**/*.css', 'no problems'] }
        : {
            lines: ['eslint src/', "src/web/checkout.ts:88:3  warning  Unexpected console statement  no-console", "src/web/cart.ts:14:7  error  'total' is assigned a value but never used  no-unused-vars", '1 problem (1 error, 0 warnings)'],
            fail: 'exit code 1',
          };
    default:
      return { lines: [`running ${name}`, 'working...', 'done'] };
  }
}
