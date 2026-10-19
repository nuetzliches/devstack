// In-browser fake of the hub and three stack daemons. No network.
//
// URL switches for testing the edge screens:
//   ?mock=unpaired   GET /hub/session answers 401
//   ?mock=empty      no stack registered (a fresh install)
//   ?mock=configchanged  the shop stack's file changed since it started
//   ?mock=newer      a newer release (0.5.0) is out
//   ?mock=nodocker   the Docker engine is stopped; "Start Docker" brings it up after 4 s
//   ?mock=nopool     this machine is in no pool; ?mock=poolmember: a member, not an admin
//   ?mock=hubdown    the hub is unreachable (toggle at runtime: window.devstackMock.hubDown(false))
import type { DockerEngine, HubEvent, ShareSettings, LogStreamEvent, RunLogEvent, StackEvent, StackEntry } from '../types';
import type { Response, StreamHandlers, Subscription, Transport } from '../transport';
import { Unreachable } from '../transport';
import { persist } from '@/lib/persist';
import { crmSeed, docsSeed, failedReason, shopSeed, stamp } from './data';
import { Abort, MockStack, sleep, type Listener } from './stack';
import { MockPool } from './pool';

type Route = { stop(): void } | null;

/** Where a mock lane was moved to (`local` or a node). */
const laneNodes = new Map<string, string>();

class MockHub {
  stacks: MockStack[] = [];
  hubSubs = new Set<Listener<HubEvent>>();
  hubDown = false;
  unpaired = false;
  empty = false;
  docker: DockerEngine = { state: 'running', version: '28.4.0' };
  newer: string | null = null;
  pool = new MockPool(new URLSearchParams(location.search).get('mock') ?? '');
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
    if (q.includes('newer')) this.newer = '0.5.0';
    if (q.includes('configchanged')) shop.view.config_changed = true;
    // A fresh install: no stack registered yet (the welcome page with the scan).
    this.empty = q.includes('empty');
    if (q.includes('nodocker')) this.docker = { state: 'stopped', starter: 'Docker Desktop', hint: 'start Docker Desktop' };
  }

  entries(): StackEntry[] {
    if (this.empty) return [];
    return this.stacks.map((s) => ({ ...s.entry, last_seen: s.running ? Date.now() : s.entry.last_seen }));
  }

  startDocker(): void {
    setTimeout(() => {
      this.docker = { state: 'running', version: '28.4.0' };
      for (const l of this.hubSubs) l.send({ type: 'docker', docker: this.docker });
    }, 4000);
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
/** A lane shared to view (ADR-0040) reads like the mock's crm stack. */
function viewedAs(path: string): string {
  const m = /^\/hub\/pool\/shared\/[^/]+\/api(\/[^?]*)(\?.*)?$/.exec(path);
  if (!m) return path;
  const q = (m[2] ?? '').replace(/[?&]pool=[^&]*/, '').replace(/^&/, '?');
  return `/hub/stacks/crm-81c0d2/api${m[1]}${q}`;
}

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
    return (await this.route(method.toUpperCase(), viewedAs(path), body as Record<string, unknown> | undefined)) as Response<T>;
  }

  private async route(method: string, path: string, body?: Record<string, unknown>): Promise<Response> {
    const url = new URL(path, 'http://hub');
    const p = url.pathname;
    const hub = this.hub;
    if (p === '/hub/session') return hub.unpaired ? json(401, { error: 'not paired' }) : json(200, { version: '0.2.0' });
    if (hub.unpaired) return json(401, { error: 'not paired' });
    if (p === '/hub/stacks' && method === 'GET') return json(200, hub.entries());
    if (p === '/hub/docker' && method === 'GET') return json(200, hub.docker);
    if (p === '/hub/pool/telemetry')
      return json(200, {
        consent: method === 'PUT' ? !!(body as { enabled?: boolean } | undefined)?.enabled : false,
        collects: true,
        what: ["devstack's diagnostic log", 'durations and outcomes of lane starts, syncs, moves and stops, service starts and builds', 'never: file contents, environment values'],
      });
    if (p === '/hub/pool/history' && method === 'GET') {
      const now = Math.floor(Date.now() / 1000);
      const node = url.searchParams.get('node');
      const n = 120;
      return json(
        200,
        Array.from({ length: n }, (_, i) => {
          const w = Math.sin(i / 9) * 0.5 + 0.5;
          return {
            at: now - (n - i) * 30,
            total_cpus: node ? 32 : 48,
            total_memory: (node ? 128 : 192) * 2 ** 30,
            offered_cpus: 12 + 4 * w,
            offered_memory: (48 + 8 * w) * 2 ** 30,
            used_cpus: 2 + 6 * w,
            used_memory: (10 + 14 * w) * 2 ** 30,
            gpus: 1,
            gpus_offered: 1,
            gpu_load: 10 + 60 * w * w,
            gpu_memory: 24 * 2 ** 30,
            gpu_memory_used: (2 + 8 * w) * 2 ** 30,
          };
        }),
      );
    }
    if (p === '/hub/pool/shared' && method === 'GET')
      return json(
        200,
        url.searchParams.get('pool')
          ? []
          : [
              {
                lease: '0d3a5b6c7e8f9012',
                repo: 'crm',
                lane: 'agent-1242',
                owner: '1122334455667788',
                owner_name: 'laptop-anna',
                node: { id: '9f8e7d6c5b4a3921', name: 'build-server', api: 'https://10.0.0.14:4261' },
                services: [
                  { name: 'api', port: 5000, scheme: 'http', here: 5100 },
                  { name: 'web', port: 4200, scheme: 'https', here: 4300 },
                ],
                view: true,
              },
            ],
      );
    if (/^\/hub\/pool\/shared\/[^/]+\/open$/.test(p) && method === 'POST') return json(200, { url: 'http://localhost:5100', port: 5100 });
    if (p === '/hub/pools' && method === 'GET')
      return json(200, [
        { name: 'default', joined: true, state: 'approved', coordinator: 'https://10.0.0.14:4260' },
        { name: 'customer', joined: true, state: 'approved', coordinator: 'https://172.20.1.5:4260' },
      ]);
    if (p === '/hub/pool' && method === 'GET' && url.searchParams.get('pool') === 'customer')
      return json(200, {
        joined: true,
        me: { id: 'c0ffee0123456789', name: 'dev-pc', state: 'approved', role: 'member', coordinator: 'https://172.20.1.5:4260' },
        nodes: [
          { id: 'c0ffee0123456789', name: 'dev-pc', online: true, seen: 3, kind: 'machine', report: { sharing: false, cpus: 0, memory: 0 } },
          { id: 'feedbeef00112233', name: 'gpu-box', online: true, seen: 5, kind: 'server', report: { sharing: true, cpus: 24, memory: 96 * 2 ** 30, api: 'https://172.20.1.9:4261' } },
        ],
        leases: [],
        members: [
          { id: 'c0ffee0123456789', name: 'dev-pc', state: 'approved', role: 'member', requested_at: 1760000000 },
          { id: 'feedbeef00112233', name: 'gpu-box', state: 'approved', role: 'admin', requested_at: 1760000000 },
        ],
        join: { coordinator: 'https://172.20.1.5:4260', fingerprint: 'ab'.repeat(32), command: `devstack join https://172.20.1.5:4260 --fingerprint ${'ab'.repeat(32)} --pool customer` },
      });
    if (p === '/hub/pool' && method === 'GET') return json(200, hub.pool.overview());
    if (p === '/hub/pool/share' && method === 'GET') return json(200, hub.pool.share());
    if (p === '/hub/pool/share' && method === 'PUT') return json(200, hub.pool.setShare(body as unknown as ShareSettings));
    const pn = /^\/hub\/pool\/nodes\/([^/]+)\/(pause|resume)$/.exec(p);
    if (pn && method === 'POST') {
      const r = hub.pool.pause(decodeURIComponent(pn[1]), pn[2] === 'pause');
      return json(r.status, r.body);
    }
    const pe = /^\/hub\/pool\/leases\/([^/]+)\/end$/.exec(p);
    if (pe && method === 'POST') {
      const r = hub.pool.end(decodeURIComponent(pe[1]));
      return json(r.status, r.body);
    }
    const pt = /^\/hub\/pool\/members\/([^/]+)\/tags$/.exec(p);
    if (pt && method === 'POST') {
      const r = hub.pool.tags(decodeURIComponent(pt[1]), (body as { tags?: string[] } | undefined)?.tags ?? []);
      return json(r.status, r.body);
    }
    const pg = /^\/hub\/pool\/members\/([^/]+)\/node$/.exec(p);
    if (pg && method === 'POST') {
      const r = hub.pool.grantNode(decodeURIComponent(pg[1]), (body as { node?: boolean } | undefined)?.node === true);
      return json(r.status, r.body);
    }
    const pp = /^\/hub\/pool\/nodes\/([^/]+)\/policy$/.exec(p);
    if (pp && method === 'POST') {
      const r = hub.pool.policy(decodeURIComponent(pp[1]), body as never);
      return json(r.status, r.body);
    }
    if (p === '/hub/pool/join' && method === 'POST') {
      const r = hub.pool.join();
      return json(r.status, r.body);
    }
    const pm = /^\/hub\/pool\/members\/([^/]+)\/(approve|remove|role)$/.exec(p);
    if (pm && method === 'POST') {
      const r = hub.pool.act(decodeURIComponent(pm[1]), pm[2], body as { role?: 'member' | 'admin' } | undefined);
      return json(r.status, r.body);
    }
    if (p === '/hub/inspect' && method === 'GET') {
      const path = url.searchParams.get('path') ?? '';
      if (!/^([a-z]:\\|\/)/i.test(path)) return json(400, { error: 'an absolute path, e.g. C:\\src\\shop or /home/me/src/shop' });
      const has = !/empty|new/i.test(path);
      return json(200, { repo: path, is_dir: !/missing/i.test(path), has_stack_file: has, valid: has ? { Ok: null } : null, registered: null, id: 'b1a2c3d4e5f6' });
    }
    if (p === '/hub/stacks/scan' && method === 'POST') {
      await sleep(600);
      const root = String((body as { path?: string } | undefined)?.path ?? 'C:\\src');
      const add = !!(body as { add?: boolean } | undefined)?.add;
      return json(200, [
        { repo: `${root}\\shop`, id: 'shop-3f9a1c', name: 'shop', error: null, registered: true, added: false },
        { repo: `${root}\\billing`, id: 'b111', name: 'billing', error: null, registered: add, added: add },
        { repo: `${root}\\legacy`, id: 'l222', name: null, error: 'devstack.toml: unknown field `biuld`', registered: false, added: false },
      ]);
    }
    if (p === '/hub/update/check' && method === 'POST') return json(200, { current: '0.2.0', newer: hub.newer });
    if (p === '/hub/update' && method === 'POST') {
      // As if installed: the hub would restart on the new version.
      const send = (ev: HubEvent) => hub.hubSubs.forEach((l) => l.send(ev));
      send({ type: 'update', update: { running: true, error: null } });
      setTimeout(() => {
        hub.newer = null;
        send({ type: 'update', update: { running: false, error: null, installed: '0.5.0', finished: Date.now() } });
        send({ type: 'release', current: '0.5.0', newer: null });
      }, 2500);
      return empty(202);
    }
    if (p === '/hub/pick-folder' && method === 'POST') return json(200, { path: 'C:\\src\\new-shop' });
    if (p === '/hub/stacks/add' && method === 'POST') return json(200, { id: hub.stacks[0].entry.id, name: hub.stacks[0].entry.name });
    if (p === '/hub/stacks/init' && method === 'POST')
      return json(200, { file: `${String(body?.path)}\\devstack.toml`, services: ['api', 'web'], compose_files: ['deploy/compose.yml'], todos: 9, id: null });
    if (p === '/hub/docker/start' && method === 'POST') {
      if (hub.docker.state === 'running') return json(200, { started: null });
      hub.startDocker();
      return json(202, { started: 'Docker Desktop' });
    }

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
    // Lanes (plan §3.5): the shop repository has two other worktrees, one
    // started; a lane's API is the stack's, here.
    const lanes = () =>
      s.view.name === 'shop'
        ? [
            { name: 'agent-login-fix', path: 'C:\\work\\shop-agent-login-fix', branch: 'agent/login-fix', number: 1, port_offset: 100 },
            { name: 'review-cart', path: 'C:\\work\\shop-review-cart', branch: 'review-cart', number: 2, port_offset: 200 },
            // Worktrees agents opened and left: lanes, not started.
            { name: '1242', path: 'C:\\work\\_wt\\1242', branch: 'agent/checkout-totals', number: 3, port_offset: 300 },
            { name: '1333', path: 'C:\\work\\_wt\\1333', branch: 'agent/i18n-de', number: 4, port_offset: 400 },
            { name: 'bisect', path: 'C:\\work\\shop-bisect', branch: null, number: 5, port_offset: 500 },
          ].map((l) => {
            const active = l.name === 'agent-login-fix' || s.startedLanes.has(l.name);
            const owns = l.name === 'agent-login-fix' && s.laneOwnsMain;
            // review-cart runs on a node of the pool once started (ADR-0025);
            // a lane moved goes where it was moved.
            const placed = laneNodes.get(l.name) ?? (l.name === 'review-cart' ? 'build-server' : 'local');
            const node = active && placed !== 'local' ? { node: placed } : {};
            return { ...l, active, owns_main: owns, running: active ? (l.name === 'agent-login-fix' ? 3 : 5) : 0, services: 5, ...node };
          })
        : [];
    if (p === '/v1/lanes' && method === 'GET') return json(200, lanes());
    if (p === '/v1/lanes' && method === 'POST') {
      await sleep(600);
      const l = lanes().find((x) => x.path === body?.path);
      if (!l) return json(409, { error: 'no worktree there' });
      s.startedLanes.add(l.name);
      // `on`: a node, or `pool` (the coordinator picks build-server here).
      if (body?.on) laneNodes.set(l.name, body.on === 'pool' ? 'build-server' : String(body.on));
      else laneNodes.set(l.name, 'local');
      // On a node the daemon answers 202 and goes on in the background; here
      // it has already started by the time the dashboard looks.
      return json(body?.on ? 202 : 201, { ...l, active: true, running: 5 });
    }
    const lm = /^\/v1\/lanes\/([^/]+)\/main$/.exec(p);
    if (lm && method === 'POST') {
      await sleep(1200);
      s.laneOwnsMain = lm[1] !== 'main';
      return json(200, []);
    }
    const mv = /^\/v1\/lanes\/([^/]+)\/move$/.exec(p);
    if (mv && method === 'POST') {
      await sleep(2500);
      laneNodes.set(decodeURIComponent(mv[1]), String(body?.to ?? 'local'));
      return json(200, lanes());
    }
    const stop = /^\/v1\/lanes\/([^/]+)\/stop$/.exec(p);
    if (stop && method === 'POST') {
      s.startedLanes.delete(decodeURIComponent(stop[1]));
      return json(200, []);
    }
    if (/^\/v1\/lanes\/[^/]+\/reset$/.test(p) && method === 'POST') return json(200, []);
    const fw = /^\/v1\/lanes\/[^/]+(\/v1\/.*)$/.exec(p);
    if (fw) p = fw[1];
    const view = () => json(200, s.view);
    if (p === '/v1/stack' && method === 'GET') return view();
    if (p === '/v1/usage' && method === 'GET') return json(200, s.usageWithHistory());
    if (p === '/v1/up' && method === 'POST') return empty(202);
    if (p === '/v1/down' && method === 'POST') return empty(204);

    if (/^\/v1\/infra\/[^/]+\/[^/]+\/logs$/.test(p)) {
      const t = new Date().toISOString();
      return text(200, `${t} database system is ready to accept connections\n${t} LOG:  checkpoint starting: time\n${t} ERROR:  relation "orders" does not exist`);
    }
    if (/^\/v1\/services\/[^/]+\/builds$/.test(p)) {
      const t = Date.now();
      const svc = s.view.services.find((x) => p.includes(`/${x.name}/`));
      return json(200, [
        { build: 'f00dbabe12345678', started_ms: t - 40_000, updated_ms: t - 31_000, outcome: 'failed', reason: 'Program.cs(12,5): error CS1002: ; expected', current: false },
        { build: svc?.build_id ?? 'a1b2c3d4e5f6a7b8', started_ms: t - 600_000, updated_ms: t - 588_000, outcome: 'live', reason: null, current: true },
        { build: 'cafe0000cafe0000', started_ms: t - 640_000, updated_ms: t - 637_000, outcome: 'superseded', reason: null, current: false },
        { build: '99aa88bb77cc66dd', started_ms: t - 3_600_000, updated_ms: t - 3_590_000, outcome: 'live', reason: null, current: false },
      ]);
    }
    if (/^\/v1\/services\/[^/]+\/logs\/files$/.test(p)) {
      const t = Date.now();
      return json(200, [
        { file: `run-${t - 60_000}-5101-a1b2c3d4e5f6a7b8.log`, kind: 'run', started_ms: t - 60_000, modified_ms: t, build: 'a1b2c3d4e5f6a7b8', port: 5101, bytes: 18_400, current: true },
        { file: 'build-a1b2c3d4e5f6a7b8.log', kind: 'build', started_ms: t - 70_000, modified_ms: t - 70_000, build: 'a1b2c3d4e5f6a7b8', port: null, bytes: 6_100, current: false },
        { file: `run-${t - 3_600_000}-5100-99aa88bb77cc66dd.log`, kind: 'run', started_ms: t - 3_600_000, modified_ms: t - 90_000, build: '99aa88bb77cc66dd', port: 5100, bytes: 52_300, current: false },
      ]);
    }
    if (/^\/v1\/services\/[^/]+\/logs$/.test(p) && q.get('file')) {
      return text(200, `info: started\nwarn: something went wrong in ${q.get('file')}\nfail: Unhandled exception`);
    }

    const ga = /^\/v1\/groups\/([^/]+)\/(start|stop|restart)$/.exec(p);
    if (ga) {
      const g = s.view.groups.find((x) => x.name === decodeURIComponent(ga[1]));
      if (!g) return json(404, { error: `no group ${ga[1]}` });
      await sleep(ga[2] === 'restart' ? 2500 : 900);
      for (const m of g.members) {
        const svc = s.view.services.find((x) => x.name === m);
        if (!svc) continue;
        if (ga[2] === 'stop') s.setStatus(m, { state: 'stopped' });
        else s.startInstance(svc, svc.build_id ?? '0000000000000000');
      }
      return view();
    }
    let m = /^\/v1\/services\/([^/]+)\/(start|stop|restart|promote|rollback|build-log|logs|hot-reload-on|hot-reload-off)$/.exec(p);
    if (m) {
      const name = decodeURIComponent(m[1]);
      const action = m[2];
      const svc = s.view.services.find((x) => x.name === name);
      if (!svc) return json(404, { error: `no service ${name}` });
      if (action === 'build-log') return text(200, s.buildLog(name));
      if (action === 'logs') return text(200, '');
      if (action === 'hot-reload-on' || action === 'hot-reload-off') {
        await sleep(1500);
        svc.hot_reload = action === 'hot-reload-on';
        svc.candidate = null;
        return view();
      }
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

    m = /^\/v1\/infra\/([^/]+)\/([^/]+)\/(start|stop|restart)$/.exec(p);
    if (m && method === 'POST') {
      const group = decodeURIComponent(m[1]);
      const service = decodeURIComponent(m[2]);
      if (m[3] === 'stop') {
        s.infra(group, service, 'exited', '', `${service} stopped`);
        return empty(204);
      }
      s.infra(group, service, 'restarting', 'starting', `${service} ${m[3] === 'start' ? 'starting' : 'restarting'}`);
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
        route = hub.hubDown ? null : this.attach(viewedAs(path), listener);
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
      l.send({ type: 'docker', docker: hub.docker });
      l.send({ type: 'release', current: '0.4.0', newer: hub.newer });
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
    const lanePath = /^\/v1\/lanes\/[^/]+(\/v1\/.*)$/.exec(rest);
    const restInLane = lanePath ? lanePath[1] : rest;
    if (restInLane === '/v1/events') {
      const off = s.subscribeEvents(listener<StackEvent>());
      return { stop: off };
    }
    let mm = /^\/v1\/services\/([^/]+)\/logs\/stream$/.exec(restInLane);
    if (mm) {
      const tail = Number(url.searchParams.get('tail') ?? '200');
      const off = s.subscribeLogs(decodeURIComponent(mm[1]), tail, listener<LogStreamEvent>());
      return { stop: off };
    }
    // A container's log: the mock replays a service's lines under its name.
    mm = /^\/v1\/infra\/[^/]+\/([^/]+)\/logs\/stream$/.exec(restInLane);
    if (mm) {
      const off = s.subscribeLogs('api', Number(url.searchParams.get('tail') ?? '200'), listener<LogStreamEvent>());
      return { stop: off };
    }
    mm = /^\/v1\/runs\/(\d+)\/log\/stream$/.exec(restInLane);
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
