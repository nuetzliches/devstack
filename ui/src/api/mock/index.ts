// In-browser fake of the hub and three stack daemons. No network.
//
// URL switches for testing the edge screens:
//   ?mock=unpaired   GET /hub/session answers 401
//   ?mock=empty      no stack registered (a fresh install)
//   ?mock=configchanged  the shop stack's file changed since it started
//   ?mock=newer      a newer release (0.5.0) is out
//   ?mock=hubtrouble the hub's last release check failed, and devstack's name is not registered for notifications
//   ?mock=skills     shop's stack file and agent skills are from an older devstack, crm's skills miss a stack file change
//   ?mock=startfailed  the docs stack's last start from the dashboard failed
//   Add a stack with a path containing "broken": a stack file that does not load
//   ?mock=nodocker   the Docker engine is stopped; "Start Docker" brings it up after 4 s
//   ?mock=nopool     this machine is in no pool; ?mock=poolmember: a member, not an admin
//   ?mock=hubdown    the hub is unreachable (toggle at runtime: window.devstackMock.hubDown(false))
import type { DockerEngine, Guest, HubEvent, PoolFeed, PoolSkillsAdmin, PoolSkillsState, LaneView, McpUpstream, NotifySettings, PoolUpstream, LogStreamEvent, RunLogEvent, StackEvent, StackEntry } from '../types';
import type { Response, StreamHandlers, Subscription, Transport } from '../transport';
import { Unreachable } from '../transport';
import { persist } from '@/lib/persist';
import { crmSeed, docsSeed, failedReason, shopSeed, stamp } from './data';
import { Abort, MockStack, sleep, type Listener } from './stack';
import { MockPool } from './pool';

type Route = { stop(): void } | null;

/** Where a mock lane was moved to (`local` or a node). */
const laneNodes = new Map<string, string>();
/** Where lanes start without a place named (ADR-0025): `null` here. */
let mockDefaultOn: string | null = null;
/** Shared lanes this machine is connected to (ADR-0053). */
const connectedLeases = new Set<string>();
/** A shared lane's services, forwarded while connected: `api`'s owner port is taken here. */
function sharedServices(lease: string, services: { name: string; port: number; scheme: string; here: number }[]) {
  if (!connectedLeases.has(lease)) return services;
  return services.map((s) => ({ ...s, forwarded: s.name === 'api' ? 51234 : s.here }));
}
/** Lanes starting on a node, since when (ms): the mock plays the phases. */
const remoteStarts = new Map<string, number>();
/** `?mock=laneremoved`: the worktree of a running lane is removed (ADR-0024 point 7): removing from `at`, gone 8 s later. */
let laneRemoval: { lane: string; at: number } | null = null;
const START_PHASES: [number, string, string][] = [
  [1, 'lease', 'lease'],
  [2, 'sync: comparing 7177 files', 'sync'],
  [3, 'on {n}: lane image: pulled or built once, then kept', 'lane image: pulled or built once, then kept'],
  [3, 'on {n}: infrastructure: app: images (pull, build) and containers', 'infrastructure: app: images (pull, build) and containers'],
  [3, 'on {n}: lane container', 'lane container'],
  // The start ends with the lane's daemon; its containers come up under it (#164).
  [3, "on {n}: the lane's daemon", "the lane's daemon"],
];
/** A start on a node as the daemon reports it while it runs (40 s here). */
function startMove(to: string, t0: number) {
  const each = 40_000 / START_PHASES.length;
  const i = Math.min(START_PHASES.length - 1, Math.floor((Date.now() - t0) / each));
  const [step, what] = START_PHASES[i];
  return {
    kind: 'start' as const,
    to,
    step,
    steps: 3,
    what: what.replace('{n}', to),
    since: t0,
    phases: START_PHASES.slice(0, i).map(([, , phase]) => ({ phase, ms: Math.round(each) })),
    phase_since: t0 + i * each,
  };
}

/** The devstack the mock hub runs. */
const MOCK_VERSION = '0.2.0';

/** The user's MCP upstreams in the mock (ADR-0069). */
const mockUpstreams: McpUpstream[] = [
  { name: 'tracker', url: 'https://mcp.tracker.example.com/mcp', prefix: 'tracker', allow: [], auth: { kind: 'oauth' }, logged_in: true },
  { name: 'wiki', url: 'https://wiki.example.com/mcp', prefix: 'wiki', allow: ['search', 'get_page'], auth: { kind: 'token' }, logged_in: false },
  // Named by the pool (ADR-0069 option B): one accepted, one offered.
  { name: 'default:docs', url: 'https://docs.example.com/mcp', prefix: 'docs', auth: { kind: 'oauth' }, logged_in: true, pool: { pool: 'default', name: 'docs', status: 'accepted', pool_prefix: 'docs' } },
  { name: 'default:crm', url: 'https://crm.example.com/mcp', prefix: 'crm', auth: { kind: 'oauth' }, logged_in: false, pool: { pool: 'default', name: 'crm', status: 'offered', pool_prefix: 'crm' } },
];

/** The MCP upstreams the mock pool names. */
const mockPoolUpstreams: PoolUpstream[] = [
  { name: 'docs', url: 'https://docs.example.com/mcp', auth: { kind: 'oauth' } },
  { name: 'crm', url: 'https://crm.example.com/mcp', allow: ['search'], auth: { kind: 'oauth' } },
];

class MockHub {
  stacks: MockStack[] = [];
  hubSubs = new Set<Listener<HubEvent>>();
  /** The pools as a hub follows them (ADR-0073): the default one live. */
  pools: Record<string, PoolFeed> = {
    default: { live: 'live', versions: { members: 'm1', nodes: 'n1', leases: 'l1', upstreams: 'u1', telemetry: 't1', skills: 's1' } },
  };
  /** The pool's skill sets on this machine (ADR-0074): one accepted, one offered. */
  poolSkillState: PoolSkillsState = {
    agents: [
      { id: 'claude-code', name: 'Claude Code', dir: '~/.claude/skills', enabled: true },
      { id: 'codex', name: 'Codex', dir: '~/.agents/skills', enabled: false },
    ],
    sets: [
      {
        pool: 'default',
        set: 'core',
        source: 'github:example-org/skills@main/skills',
        state: 'accepted',
        auto: true,
        fetched: Math.floor(Date.now() / 1000) - 600,
        commit: 'a1b2c3d',
        skills: [
          { name: 'work-issue', description: 'Work on an issue from the tracker to a pull request', sha256: 'aa', installed: true, pending: false },
          { name: 'push-tag', description: 'Tag a release and push it', sha256: 'bb', installed: true, pending: false },
        ],
      },
      {
        pool: 'default',
        set: 'shop',
        source: 'github:example-org/shop-agent@main/skills',
        state: 'offered',
        auto: false,
        skills: [{ name: 'shop-add-feature', description: 'Add a feature to the shop the way its team does', sha256: 'cc', installed: false, pending: false }],
      },
    ],
    conflicts: [],
  };
  poolSkills: PoolSkillsAdmin = {
    sets: [
      {
        name: 'core',
        source: { kind: 'github', repo: 'example-org/skills', ref: 'main', path: 'skills' },
        to: ['*'],
        manifest: { source: 'github:example-org/skills@main/skills', fetched: Math.floor(Date.now() / 1000) - 600, commit: 'a1b2c3d', skills: [{ name: 'work-issue', description: '' }, { name: 'push-tag', description: '' }] },
      },
    ],
    github_token: true,
    refresh_signed: true,
  };
  /** A lane of laptop-anna's runs here, a second one is admitted. */
  guests: Guest[] = [
    { pool: 'default', lease: 'l-7f3a', member: '1122334455667788', owner: 'laptop-anna', repo: 'shop', lane: 'feat-cart', state: 'running', since: Math.floor(Date.now() / 1000) - 2400, cpus: 2, memory: 4 * 2 ** 30 },
    { pool: 'default', lease: 'l-91c2', member: '1122334455667788', owner: 'laptop-anna', repo: 'crm', lane: 'agent-1242', state: 'offered', since: Math.floor(Date.now() / 1000) - 20, cpus: 1.5, memory: 3 * 2 ** 30 },
  ];
  hubDown = false;
  unpaired = false;
  empty = false;
  docker: DockerEngine = { state: 'running', version: '28.4.0' };
  newer: string | null = null;
  releaseError: string | null = null;
  /** `?mock=notices`: a lane whose worktree is missing (ADR-0050). */
  laneNotices = false;
  /** The agent skills in each stack's repository: the devstack that wrote them, and whether they differ from what it would write now. */
  skills: Record<string, { from: string; outdated: boolean }> = {};
  /** `[stack] reviewed` of each stack's file (`null`: not set; not listed: this devstack). */
  reviewed: Record<string, string | null> = {};
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
    if (q.includes('hubtrouble')) {
      this.releaseError = 'GitHub answered 403 (rate limit exceeded)';
      notify.trouble = "devstack's name could not be registered with Windows (error 5), so notifications may show without it, or not at all.";
    }
    if (q.includes('configchanged')) shop.view.config_changed = true;
    if (q.includes('startfailed')) docs.entry.start_error = 'Error: port 4000 (site) is taken by another process (node.exe, pid 4242)\n  devstack doctor names it; stop it or change the port in devstack.toml';
    // `?mock=skills`: shop was set up with an older devstack, crm's stack file changed since.
    if (q.includes('skills')) this.reviewed[shop.entry.id] = null;
    this.skills[shop.entry.id] = q.includes('skills') ? { from: '0.1.4', outdated: true } : { from: MOCK_VERSION, outdated: false };
    this.skills[crm.entry.id] = { from: MOCK_VERSION, outdated: q.includes('skills') };
    if (q.includes('notices')) {
      this.laneNotices = true;
      shop.view.notices = [
        {
          id: 'no_ports.C:/work/_wt/1400',
          level: 'warn',
          text: "The worktree C:/work/_wt/1400 gets no lane: no lane number up to 199 moves the stack's ports to ones free of the other lanes and below 65536 (`[lanes] port_step` is 100). Remove a worktree no longer needed, or lower `port_step`.",
          since: Date.now() - 4 * 60_000,
        },
      ];
    }
    if (q.includes('watcher')) shop.view.watcher_error = 'the file watcher lost events (overflow); every built service was rebuilt as a candidate';
    if (q.includes('laneremoved')) laneRemoval = { lane: 'agent-login-fix', at: Date.now() + 8000 };
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

/** A lane shared with this machine (ADR-0040) reads like the mock's crm stack. */
function viewedAs(path: string): string {
  const m = /^\/hub\/pool\/shared\/[^/]+\/api(\/[^?]*)(\?.*)?$/.exec(path);
  if (!m) return path;
  const q = (m[2] ?? '').replace(/[?&]pool=[^&]*/, '').replace(/^&/, '?');
  return `/hub/stacks/crm-81c0d2/api${m[1]}${q}`;
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


/** The hub's notification settings (ADR-0045), as a fresh install has them. */
const notify: NotifySettings = {
  available: true,
  unavailable: null,
  kinds: [
    { kind: 'build_failed', label: 'A build failed', on: true, default: true },
    { kind: 'held', label: 'A candidate is held and waits for promote', on: true, default: true },
    { kind: 'service_failed', label: 'A service exited by itself or failed to start', on: true, default: true },
    { kind: 'lane_failed', label: "A lane's start or move on a node failed", on: true, default: true },
    { kind: 'main_ports', label: 'A lane took or gave back the main ports', on: false, default: false },
    { kind: 'live', label: 'A candidate went live', on: false, default: false },
    { kind: 'guest_lane', label: 'A lane of another member started on this machine', on: true, default: true },
  ],
  muted: [],
};

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
    // The removal is told as the daemon tells it: `lanes` events.
    if (laneRemoval) {
      const shop = this.hub.stacks.find((x) => x.view.name === 'shop')!;
      setInterval(async () => {
        const r = await this.daemon(shop, 'GET', '/v1/lanes', new URLSearchParams());
        shop.emit({ type: 'lanes', lanes: r.body as LaneView[] });
      }, 1000);
    }
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
    if (p === '/hub/session') return hub.unpaired ? json(401, { error: 'not paired' }) : json(200, { version: MOCK_VERSION, link_unavailable: null });
    if (hub.unpaired) return json(401, { error: 'not paired' });
    if (p === '/hub/stacks' && method === 'GET') return json(200, hub.entries());
    if (p === '/hub/docker' && method === 'GET') return json(200, hub.docker);
    if (p === '/hub/pool/telemetry')
      return json(200, {
        consent: method === 'PUT' ? !!(body as { enabled?: boolean } | undefined)?.enabled : false,
        collects: true,
        what: ["devstack's diagnostic log", 'durations and outcomes of lane starts, syncs, moves and stops, service starts and builds', 'never: file contents, environment values'],
      });
    if (p === '/hub/pool/default-on') {
      if (method === 'PUT') mockDefaultOn = (body as { on?: string | null } | undefined)?.on ?? null;
      return json(200, { on: mockDefaultOn });
    }
    if (p === '/hub/pool/placement' && method === 'GET') {
      // The default pool has one node; a lane named like one that ran there is warm.
      const lane = url.searchParams.get('lane') ?? '';
      return json(200, [
        {
          node: '9f8e7d6c5b4a3921',
          name: 'build-server',
          eligible: true,
          why: lane === 'review-cart' ? "keeps this lane's data (mirror, builds, volumes)" : 'the repository ran there (its images and builds are there)',
          cpus: 12,
          memory: 48 * 2 ** 30,
          disk: lane === 'review-cart' ? 0 : 6 * 2 ** 30,
          disk_room: 180 * 2 ** 30,
          need: { cpus: 2.5, memory: 6 * 2 ** 30 },
          need_from: 'measured: the most of its last 3 runs in the pool',
          steady: { cpus: 0.6, memory: 2.25 * 2 ** 30 },
        },
      ]);
    }
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
                services: sharedServices('0d3a5b6c7e8f9012', [
                  { name: 'api', port: 5000, scheme: 'http', here: 5100 },
                  { name: 'web', port: 4200, scheme: 'https', here: 4300 },
                ]),
              },
            ],
      );
    const sharedAct = /^\/hub\/pool\/shared\/([^/]+)\/(open|connect|disconnect)$/.exec(p);
    if (sharedAct && method === 'POST') {
      const [, lease, what] = sharedAct;
      if (what === 'disconnect') {
        const closed = connectedLeases.delete(lease) ? 2 : 0;
        return json(200, { closed });
      }
      connectedLeases.add(lease);
      if (what === 'open') return json(200, { url: 'http://localhost:51234', port: 51234 });
      return json(200, {
        services: [
          { name: 'api', port: 51234, url: 'http://localhost:51234' },
          { name: 'web', port: 4300, url: 'https://localhost:4300' },
        ],
      });
    }
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
          { id: 'feedbeef00112233', name: 'gpu-box', online: true, seen: 5, kind: 'server', report: { sharing: true, cpus: 24, memory: 96 * 2 ** 30, api: 'https://172.20.1.9:4261', no_update: 'the agent runs in a container: update its image' } },
        ],
        leases: [],
        members: [
          { id: 'c0ffee0123456789', name: 'dev-pc', state: 'approved', role: 'member', requested_at: 1760000000 },
          { id: 'feedbeef00112233', name: 'gpu-box', state: 'approved', role: 'admin', requested_at: 1760000000 },
        ],
        join: { coordinator: 'https://172.20.1.5:4260', fingerprint: 'ab'.repeat(32), command: `devstack join https://172.20.1.5:4260 --fingerprint ${'ab'.repeat(32)} --pool customer` },
      });
    // MCP upstreams (ADR-0069): one logged in, one not.
    if (p === '/hub/upstreams' && method === 'GET') return json(200, { store: 'keychain', upstreams: mockUpstreams });
    if (p === '/hub/upstreams' && method === 'POST') {
      const u = body as unknown as McpUpstream;
      const at = mockUpstreams.findIndex((o) => o.name === u.name);
      const next = { ...u, prefix: u.prefix || u.name.replace(/-/g, '_'), logged_in: at >= 0 ? mockUpstreams[at].logged_in : u.auth.kind === 'none' };
      if (at >= 0) mockUpstreams[at] = next;
      else mockUpstreams.push(next);
      return empty(204);
    }
    const ua = /^\/hub\/upstreams\/pool\/([^/]+)\/([^/]+)\/(accept|decline)$/.exec(p);
    if (ua && method === 'POST') {
      const u = mockUpstreams.find((o) => o.pool?.pool === decodeURIComponent(ua[1]) && o.pool.name === decodeURIComponent(ua[2]));
      if (!u?.pool) return json(400, { error: 'the pool names no such upstream' });
      u.pool.status = ua[3] === 'accept' ? 'accepted' : 'declined';
      if (ua[3] === 'decline') u.logged_in = false;
      return empty(204);
    }
    // Skill sets (ADR-0074).
    if (p === '/hub/skills' && method === 'GET') return json(200, this.hub.poolSkillState);
    if (p === '/hub/skills/sync' && method === 'POST') return json(200, this.hub.poolSkillState);
    const sa = /^\/hub\/skills\/agents\/([^/]+)$/.exec(p);
    if (sa && method === 'PUT') {
      const a = this.hub.poolSkillState.agents.find((x) => x.id === decodeURIComponent(sa[1]));
      if (a) a.enabled = !!(body as { enabled?: boolean })?.enabled;
      return json(200, this.hub.poolSkillState);
    }
    const ss = /^\/hub\/skills\/sets\/([^/]+)\/([^/]+)(?:\/(accept|decline|update))?$/.exec(p);
    if (ss) {
      const s = this.hub.poolSkillState.sets.find((x) => x.pool === decodeURIComponent(ss[1]) && x.set === decodeURIComponent(ss[2]));
      if (!s) return json(409, { error: 'no such set' });
      const b = (body ?? {}) as { auto?: boolean };
      if (ss[3] === 'accept') Object.assign(s, { state: 'accepted', auto: !!b.auto, skills: s.skills.map((k) => ({ ...k, installed: true, pending: false })) });
      else if (ss[3] === 'decline') Object.assign(s, { state: 'declined', skills: s.skills.map((k) => ({ ...k, installed: false, pending: false })) });
      else if (ss[3] === 'update') s.skills = s.skills.map((k) => ({ ...k, installed: true, pending: false }));
      else s.auto = !!b.auto;
      for (const l of this.hub.hubSubs) l.send({ type: 'skills', skills: this.hub.poolSkillState });
      return json(200, this.hub.poolSkillState);
    }
    if (p === '/hub/pool/skills' && method === 'GET') return json(200, this.hub.poolSkills);
    if (p === '/hub/pool/skills/refresh' && method === 'POST') return json(202, {});
    if (p === '/hub/pool/upstreams' && method === 'GET') return json(200, mockPoolUpstreams);
    if (p === '/hub/pool/upstreams' && method === 'POST') {
      const u = body as unknown as PoolUpstream;
      if (!u.url.startsWith('https://')) return json(400, { error: `\`${u.url}\`: a pool's upstream is a URL reached over https` });
      const at = mockPoolUpstreams.findIndex((o) => o.name === u.name);
      if (at >= 0) mockPoolUpstreams[at] = u;
      else mockPoolUpstreams.push(u);
      return json(200, mockPoolUpstreams);
    }
    const pu = /^\/hub\/pool\/upstreams\/([^/]+)$/.exec(p);
    if (pu && method === 'DELETE') {
      const at = mockPoolUpstreams.findIndex((o) => o.name === decodeURIComponent(pu[1]));
      if (at < 0) return json(404, { error: 'the pool names no such upstream' });
      mockPoolUpstreams.splice(at, 1);
      return empty(204);
    }
    const up = /^\/hub\/upstreams\/([^/]+)(?:\/(login|token|logout))?$/.exec(p);
    if (up) {
      const u = mockUpstreams.find((o) => o.name === decodeURIComponent(up[1]));
      if (!u) return json(400, { error: `no upstream \`${up[1]}\`` });
      if (method === 'DELETE' && !up[2]) mockUpstreams.splice(mockUpstreams.indexOf(u), 1);
      else if (up[2] === 'login') return json(200, { url: 'https://auth.example.com/authorize?mock=1' });
      else if (up[2] === 'token') u.logged_in = true;
      else if (up[2] === 'logout') u.logged_in = false;
      return empty(204);
    }
    if (p === '/hub/pool' && method === 'GET') return json(200, hub.pool.overview());
    if (p === '/hub/pool/share' && method === 'GET') return json(200, hub.pool.share());
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
    const pv = /^\/hub\/pool\/members\/([^/]+)\/provision$/.exec(p);
    if (pv && method === 'POST') {
      const r = hub.pool.provision(decodeURIComponent(pv[1]), (body as { on?: boolean } | undefined)?.on === true);
      return json(r.status, r.body);
    }
    const pc = /^\/hub\/pool\/nodes\/([^/]+)\/(confirm|give-back)$/.exec(p);
    if (pc && method === 'POST') {
      const r = hub.pool.provisioned(decodeURIComponent(pc[1]), pc[2]);
      return json(r.status, r.body);
    }
    if (p === '/hub/pool/provisioning' && method === 'GET') return json(200, hub.pool.provisioning);
    if (p === '/hub/pool/provisioned' && method === 'PUT') {
      const r = hub.pool.setProvisioned((body as { on?: boolean } | undefined)?.on === true);
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
      // A path with "broken" in it has a stack file that does not load.
      const valid = /broken/i.test(path) ? { Err: `${path}\\devstack.toml: 1 problem(s)\n  - stack.nmae: unknown key` } : { Ok: null };
      return json(200, { repo: path, is_dir: !/missing/i.test(path), has_stack_file: has, valid: has ? valid : null, registered: null, id: 'b1a2c3d4e5f6' });
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
    if (p === '/hub/notify' && method === 'GET') return json(200, notify);
    if (p === '/hub/notify' && method === 'PUT') {
      const b = (body ?? {}) as { kinds?: Record<string, boolean>; muted?: string[] };
      for (const [k, on] of Object.entries(b.kinds ?? {})) {
        const kind = notify.kinds.find((x) => x.kind === k);
        if (!kind) return json(400, { error: `no kind of notification \`${k}\`` });
        kind.on = on;
      }
      if (b.muted) notify.muted = [...b.muted].sort();
      return json(200, notify);
    }
    if (p === '/hub/notify/test' && method === 'POST') return empty(204);
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
    if (p === '/hub/openers' && method === 'GET')
      return json(200, [
        { id: 'files', label: 'Explorer', kind: 'files' },
        { id: 'wt', label: 'Windows Terminal', kind: 'terminal' },
        { id: 'powershell', label: 'PowerShell', kind: 'terminal' },
        { id: 'vscode', label: 'VS Code', kind: 'editor' },
        { id: 'github-desktop', label: 'GitHub Desktop', kind: 'git' },
        { id: 'claude', label: 'Claude Desktop', kind: 'agent', prompt: 'prefill' },
        { id: 'claude-cli', label: 'Claude Code', kind: 'agent', prompt: 'prefill' },
        { id: 'orca', label: 'Orca', kind: 'agent', prompt: 'send' },
      ]);
    if (p === '/hub/open' && method === 'POST') return empty(204);
    // The hub writes the prompts (ADR-0046, amendment 2026-10-22); the mock's are short.
    if (p === '/hub/prompt' && method === 'GET') {
      const kind = url.searchParams.get('kind');
      const target = url.searchParams.get('stack') ?? url.searchParams.get('path');
      const what: Record<string, string> = {
        setup: `Set up devstack for the repository at ${target}.`,
        'fix-file': `The devstack stack file in ${target} does not load.`,
        upgrade: `Bring the devstack setup of ${target} up to devstack ${MOCK_VERSION}.`,
        'start-failed': `The devstack stack ${target} does not start.`,
        'lane-file': `The git worktree ${url.searchParams.get('path')} has no devstack stack file of its own.`,
        mcp: `Register devstack's MCP server for the coding agents of ${target}.`,
        skills: `Refresh the devstack agent skills of the repository at ${target}.`,
      };
      if (!kind || !what[kind]) return json(400, { error: `no prompt \`${kind}\`` });
      return json(200, { text: `${what[kind]}

1. Read the stack file reference.
2. Run \`devstack check\` and \`devstack doctor\`.
3. Ask me before committing, naming the branch: the branch checked out may hold other work.`, path: target });
    }
    if (p === '/hub/agent' && method === 'POST') {
      await sleep(400);
      return empty(204);
    }
    if (p === '/hub/stacks/add' && method === 'POST') return json(200, { id: hub.stacks[0].entry.id, name: hub.stacks[0].entry.name });
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

    // The hub writes no file of a repository (ADR-0071): the skills are read only.
    if (rest === '/skills' && method === 'GET') {
      const k = hub.skills[stack.entry.id];
      const names = ['devstack', 'devstack-config'];
      return json(200, { found: k ? names : [], outdated: k?.outdated ? names : [], from: k?.from ?? null, current: MOCK_VERSION, ahead: false });
    }
    if (rest === '/upgrade' && method === 'GET') {
      const r = hub.reviewed[stack.entry.id];
      const reviewed = r === undefined ? MOCK_VERSION : r;
      const k = hub.skills[stack.entry.id];
      const from = reviewed ?? (k && k.from !== MOCK_VERSION ? k.from : null);
      return json(200, { reviewed, from, current: MOCK_VERSION, behind: reviewed !== MOCK_VERSION });
    }
    // The first stack registers devstack's MCP server, the others do not.
    if (rest === '/mcp' && method === 'GET') {
      const registered = stack === hub.stacks[0];
      return json(200, { registered, files: registered ? [`${stack.entry.repo}\\.mcp.json`] : [] });
    }
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
    // Branches (ADR-0047): the worktrees below, and some without one. The
    // mock makes and removes no worktree; it answers as the hub would.
    if (rest === '/branches' && method === 'GET') {
      const day = Math.floor(Date.now() / 1000) - 86_400;
      const tree = (b: string, path: string) => ({ name: b, main: false, worktree: path, upstream: `origin/${b}`, stack_file: !stack.noStackFile.has(path) });
      return json(200, {
        fetched: day + 80_000,
        branches: [
          { name: 'main', main: true, worktree: stack.view.repo, upstream: 'origin/main', date: day + 70_000 },
          { name: 'feat/coupons', main: false, upstream: 'origin/feat/coupons', date: day + 65_000 },
          { ...tree('agent/login-fix', 'C:\\work\\shop-agent-login-fix'), date: day + 60_000 },
          { ...tree('review-cart', 'C:\\work\\shop-review-cart'), date: day + 50_000 },
          { ...tree('agent/checkout-totals', 'C:\\work\\_wt\\1242'), date: day + 40_000 },
          { ...tree('agent/i18n-de', 'C:\\work\\_wt\\1333'), date: day + 30_000 },
          { name: 'spike/search', main: false, date: day, stack_file: false },
          { name: 'origin/release/2.4', remote: 'origin', main: false, date: day - 90_000, stack_file: false },
          { name: 'origin/fix/vat-rounding', remote: 'origin', main: false, date: day - 200_000 },
        ],
      });
    }
    if (rest === '/fetch' && method === 'POST') {
      await sleep(900);
      return json(200, {});
    }
    if (rest === '/worktrees' && method === 'POST') {
      await sleep(600);
      const name = String(body?.new ?? body?.branch ?? '').replace(/^origin\//, '').replace(/[^a-z0-9]+/gi, '-').toLowerCase();
      return json(200, { path: `C:\\work\\shop.worktrees\\${name}` });
    }
    if (rest === '/worktrees/remove' && method === 'POST') return json(409, { error: 'error: worktree contains modified or untracked files, use --force to delete it' });
    if (!rest.startsWith('/api/')) return json(404, { error: 'no route' });
    if (!stack.running) return json(503, { error: 'stack not running' });
    return this.daemon(stack, method, rest.slice(4), url.searchParams, body);
  }

  /** The daemon's API; what changes something goes into its journal, as the daemon's `audit` layer does (ADR-0067). */
  private async daemon(s: MockStack, method: string, p: string, q: URLSearchParams, body?: Record<string, unknown>): Promise<Response> {
    const inLane = /^\/v1\/lanes\/([^/]+)(\/v1\/.*)$/.exec(p);
    if ((inLane ? inLane[2] : p) === '/v1/journal' && method === 'GET') {
      const since = Number(q.get('since') ?? '0');
      return json(200, s.journal.filter((e) => e.at > since));
    }
    const res = await this.daemonApi(s, method, p, q, body);
    if (method !== 'GET' && method !== 'HEAD') {
      const lane = /^\/v1\/lanes\/([^/]+)\//.exec(p)?.[1];
      const error = res.status >= 400 && res.body && typeof res.body === 'object' && 'error' in res.body ? String((res.body as { error: unknown }).error) : undefined;
      s.journalize({ at: Date.now(), via: 'dashboard', ...(lane ? { lane: decodeURIComponent(lane) } : {}), method, path: p, status: res.status, ...(error ? { error } : {}) });
    }
    return res;
  }

  private async daemonApi(s: MockStack, method: string, p: string, q: URLSearchParams, body?: Record<string, unknown>): Promise<Response> {
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
            const t0 = remoteStarts.get(l.name);
            if (t0 !== undefined && Date.now() - t0 < 40_000) {
              return { ...l, active: false, running: 0, services: 5, moving: startMove(laneNodes.get(l.name) ?? 'build-server', t0) };
            }
            if (laneRemoval?.lane === l.name && Date.now() >= laneRemoval.at) {
              return Date.now() - laneRemoval.at < 8000 ? { ...l, active: false, owns_main: false, running: 0, services: 5, removing: true } : null;
            }
            const active = l.name === 'agent-login-fix' || s.startedLanes.has(l.name);
            const owns = l.name === 'agent-login-fix' && s.laneOwnsMain;
            // review-cart runs on a node of the pool once started (ADR-0025);
            // a lane moved goes where it was moved.
            const placed = laneNodes.get(l.name) ?? (l.name === 'review-cart' ? 'build-server' : 'local');
            const node = active && placed !== 'local' ? { node: placed } : {};
            const notices =
              this.hub.laneNotices && l.name === '1242'
                ? {
                    notices: [
                      {
                        id: 'worktree_missing',
                        level: 'error' as const,
                        text: `Its worktree ${l.path} is not there, though git still lists it. Unless it is back within 10 minutes, the lane goes, with its containers and volumes.`,
                        since: Date.now() - 2 * 60_000,
                      },
                    ],
                  }
                : {};
            const file = s.noStackFile.has(l.path) ? { main_stack_file: true } : {};
            return { ...l, active, owns_main: owns, running: active ? (l.name === 'agent-login-fix' ? 3 : 5) : 0, services: 5, ...node, ...notices, ...file };
          })
          .filter((l) => l !== null)
        : [];
    if (p === '/v1/lanes' && method === 'GET') return json(200, lanes());
    if (p === '/v1/lanes' && method === 'POST') {
      await sleep(600);
      const l = lanes().find((x) => x.path === body?.path);
      if (!l) return json(409, { error: 'no worktree there' });
      s.startedLanes.add(l.name);
      // `on`: a node, or `pool` (the coordinator picks build-server here).
      if (body?.on) {
        laneNodes.set(l.name, body.on === 'pool' ? 'build-server' : String(body.on));
        remoteStarts.set(l.name, Date.now());
      } else laneNodes.set(l.name, 'local');
      // On a node the daemon answers 202 and goes on in the background; here
      // it has already started by the time the dashboard looks.
      return json(body?.on ? 202 : 201, body?.on ? { ...l, active: false } : { ...l, active: true, running: 5 });
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
    // A lane restarts where it runs (warm, rebuild), or cold: stopped, its
    // data gone, started again there.
    const rs = /^\/v1\/lanes\/([^/]+)\/restart$/.exec(p);
    if (rs && method === 'POST') {
      const name = decodeURIComponent(rs[1]);
      if (!s.startedLanes.has(name)) return json(409, { error: `lane \`${name}\` is not started` });
      await sleep(body?.mode === 'cold' ? 2500 : 1500);
      return json(200, lanes());
    }
    const fw = /^\/v1\/lanes\/[^/]+(\/v1\/.*)$/.exec(p);
    if (fw) p = fw[1];
    const view = () => json(200, s.view);
    if (p === '/v1/stack' && method === 'GET') return view();
    if (p === '/v1/usage' && method === 'GET') return json(200, s.usageWithHistory());
    // The store of what `prepare` makes (ADR-0062).
    if (p === '/v1/store' && method === 'GET') {
      const entries = s.storeEntries();
      return json(200, { owned: true, bytes: entries.reduce((n, e) => n + e.bytes, 0), entries });
    }
    const se = /^\/v1\/store\/([^/]+)\/([^/]+)$/.exec(p);
    if (se && method === 'DELETE') return s.removeStoreEntry(decodeURIComponent(se[1]), decodeURIComponent(se[2])) ? empty(204) : json(404, { error: 'no such entry in the store' });
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
        { build: 'dead0000beef1111', started_ms: t - 7_200_000, updated_ms: t - 7_190_000, outcome: 'interrupted', reason: null, current: false },
        // Older builds of a long day: the list shows the newest first, the rest on request.
        ...Array.from({ length: 14 }, (_, i) => ({
          build: `0ld${String(i).padStart(2, '0')}0000aaaa0000`,
          started_ms: t - 86_400_000 - i * 600_000,
          updated_ms: t - 86_400_000 - i * 600_000 + 20_000,
          outcome: i % 4 === 3 ? 'superseded' : 'live',
          reason: null,
          current: false,
        })),
      ].map((b, i) => ({ ...b, artifact: i < 5, log: i < 10 })));
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

    const ga = /^\/v1\/groups\/([^/]+)\/(start|stop|restart|rebuild)$/.exec(p);
    if (ga) {
      const g = s.view.groups.find((x) => x.name === decodeURIComponent(ga[1]));
      if (!g) return json(404, { error: `no group ${ga[1]}` });
      await sleep(ga[2] === 'restart' || ga[2] === 'rebuild' ? 2500 : 900);
      for (const m of g.members) {
        const svc = s.view.services.find((x) => x.name === m);
        if (!svc) continue;
        if (ga[2] === 'stop') s.setStatus(m, { state: 'stopped' });
        else s.startInstance(svc, svc.build_id ?? '0000000000000000');
      }
      return view();
    }
    let m = /^\/v1\/services\/([^/]+)\/(start|stop|restart|rebuild|promote|rollback|build-log|logs|hot-reload-on|hot-reload-off)$/.exec(p);
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
          case 'rebuild':
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
      l.send({ type: 'release', current: '0.4.0', newer: hub.newer, error: hub.releaseError });
      l.send({ type: 'guests', guests: hub.guests });
      l.send({ type: 'pools', pools: hub.pools });
      l.send({ type: 'skills', skills: hub.poolSkillState });
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
