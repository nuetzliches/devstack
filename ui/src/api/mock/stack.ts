// One fake stack daemon: state, event stream, logs, usage, actions.
import type {
  Candidate,
  JournalEntry,
  LogStreamEvent,
  Point,
  Run,
  RunLogEvent,
  ServiceView,
  StackEntry,
  StackEvent,
  StackEventBody,
  StackView,
  Status,
  StoreEntry,
  UsageReport,
} from '../types';
import { buildId, buildLog, logLine, startupLines, type MockStackSeed } from './data';

export type Listener<T> = { send(data: T): void; kill(): void };

export class Abort extends Error {}

const HISTORY_POINTS = 300; // 10 minutes at 2 s

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

type Walk = { cpu: number; mem: number; baseCpu: number; baseMem: number };

export class MockStack {
  entry: StackEntry;
  view: StackView;
  previous: Record<string, string>;
  /** Mock lane `agent-login-fix` serves the main ports (the shop's from the start, set by an agent). */
  laneOwnsMain = false;
  /** Serve the main ports by lane `name`, or give them back (`main`): the view says who and how (ADR-0067). */
  serveMain(name: string, via: 'dashboard' | 'mcp' = 'dashboard', at = Date.now()): void {
    this.laneOwnsMain = name !== 'main';
    this.view.main_served = this.laneOwnsMain ? { lane: name, via, at } : undefined;
  }
  /** Mock lanes started from the dashboard (`agent-login-fix` runs from the start; the shop's `review-cart` too, on a node). */
  startedLanes = new Set<string>();
  /** Worktrees without a stack file of their own (ADR-0055) until one is copied in. */
  noStackFile = new Set<string>(['C:\\work\\_wt\\1333']);
  private seq = 1;
  private nextRun = 1;
  private eventSubs = new Set<Listener<StackEvent>>();
  private logs = new Map<string, string[]>();
  private logSubs = new Map<string, Set<Listener<LogStreamEvent>>>();
  runs: Run[] = [];
  /** The journal (ADR-0067), oldest first: some changes from before the page opened. */
  journal: JournalEntry[] = (() => {
    const t = Date.now();
    return [
      { at: t - 41 * 60_000, via: 'cli', method: 'POST', path: '/v1/up', status: 202 },
      { at: t - 23 * 60_000, via: 'mcp', lane: 'agent-login-fix', method: 'POST', path: '/v1/lanes/agent-login-fix/v1/services/api/restart', status: 200 },
      { at: t - 17 * 60_000, via: 'mcp', method: 'POST', path: '/v1/services/api/promote', status: 409, error: 'no candidate to promote' },
      { at: t - 9 * 60_000, via: 'dashboard', method: 'POST', path: '/v1/services/worker/stop', status: 200 },
      { at: t - 4 * 60_000, via: 'tray', method: 'POST', path: '/v1/commands/seed', status: 202 },
    ];
  })();
  private runLogs = new Map<number, { lines: string[]; end: RunLogEvent | null; subs: Set<Listener<RunLogEvent>> }>();
  private history = new Map<string, Point[]>();
  private walks = new Map<string, Walk>();
  private timers: ReturnType<typeof setInterval>[] = [];
  private generation = 0;
  buildsActive = 0;
  failedBuild = new Set<string>();
  private lastUsage: UsageReport | null = null;
  onEntryChange: () => void = () => {};

  constructor(seed: MockStackSeed) {
    this.entry = seed.entry;
    this.view = seed.view;
    this.previous = seed.previous;
    for (const s of this.view.services) {
      const lines: string[] = [];
      if (s.status.state !== 'stopped') {
        lines.push(...startupLines(s.name, s.instance_port ?? s.port));
        for (let i = 0; i < 120; i++) lines.push(logLine(s.name, s.kind));
      }
      this.logs.set(s.name, lines);
    }
    this.seedHistory();
  }

  get running(): boolean {
    return this.entry.running;
  }

  /** A token that changes whenever the stack goes down; scripts stop on a change. */
  get gen(): number {
    return this.generation;
  }

  // ------------------------------------------------------------ lifecycle

  start(): void {
    this.stopTimers();
    this.timers.push(setInterval(() => this.tickUsage(), 2000));
    this.timers.push(setInterval(() => this.tickLogs(), 650));
  }

  stopTimers(): void {
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
  }

  goDown(): void {
    this.generation++;
    this.stopTimers();
    this.entry = { ...this.entry, running: false, daemon: null, last_seen: Date.now() };
    for (const s of this.view.services) {
      if (s.status.state !== 'completed') s.status = { state: 'stopped' };
      s.pid = null;
      s.candidate = null;
      if (s.proxy) s.proxy = { ...s.proxy, upstream: null, active: 0, pending: null };
    }
    for (const l of [...this.eventSubs]) l.kill();
    for (const set of this.logSubs.values()) for (const l of [...set]) l.kill();
    this.eventSubs.clear();
    this.onEntryChange();
  }

  /** `main: false`: the daemon only, as a lane's start brings it up (ADR-0078). */
  async goUp(opts: { main?: boolean } = {}): Promise<void> {
    const gen = this.generation;
    // A new daemon reads the stack file afresh.
    this.view.config_changed = false;
    await sleep(1400);
    if (gen !== this.generation) return;
    this.entry = {
      ...this.entry,
      running: true,
      daemon: { pid: 20000 + Math.floor(Math.random() * 9000), port: 50000 + Math.floor(Math.random() * 9000), started: Date.now(), version: '0.2.0' },
      last_seen: Date.now(),
    };
    if (opts.main !== false) {
      for (const s of this.view.services) {
        if (s.autostart && s.kind !== 'job') s.status = { state: 'starting' };
      }
    }
    this.onEntryChange();
    this.start();
    await sleep(900);
    for (const s of this.view.services) {
      if (s.status.state === 'starting') {
        this.startInstance(s, s.build_id ?? buildId());
      }
    }
  }

  // ------------------------------------------------------------ streams

  subscribeEvents(l: Listener<StackEvent>): () => void {
    l.send({ seq: this.seq++, at: Date.now(), type: 'snapshot', stack: clone(this.view) });
    this.eventSubs.add(l);
    return () => this.eventSubs.delete(l);
  }

  subscribeLogs(service: string, tail: number, l: Listener<LogStreamEvent>): () => void {
    const lines = this.logs.get(service) ?? [];
    for (const text of lines.slice(-tail)) l.send({ type: 'line', text });
    let set = this.logSubs.get(service);
    if (!set) this.logSubs.set(service, (set = new Set()));
    set.add(l);
    return () => set!.delete(l);
  }

  subscribeRunLog(id: number, l: Listener<RunLogEvent>): (() => void) | null {
    const r = this.runLogs.get(id);
    if (!r) return null;
    for (const text of r.lines) l.send({ type: 'line', text });
    if (r.end) {
      l.send(r.end);
      return () => {};
    }
    r.subs.add(l);
    return () => r.subs.delete(l);
  }

  /** A change into the journal, and onto the stream (ADR-0067). */
  journalize(entry: JournalEntry): void {
    this.journal.push(entry);
    if (this.journal.length > 500) this.journal.shift();
    this.emit({ type: 'journal', entry });
  }

  emit(body: StackEventBody): void {
    const ev = { seq: this.seq++, at: Date.now(), ...body } as StackEvent;
    for (const l of this.eventSubs) l.send(ev);
  }

  log(service: string, text: string): void {
    let lines = this.logs.get(service);
    if (!lines) this.logs.set(service, (lines = []));
    lines.push(text);
    if (lines.length > 3000) lines.splice(0, lines.length - 3000);
    for (const l of this.logSubs.get(service) ?? []) l.send({ type: 'line', text });
  }

  instance(service: string, build: string | null): void {
    for (const l of this.logSubs.get(service) ?? []) l.send({ type: 'instance', build_id: build });
  }

  // ------------------------------------------------------------ state changes (emit events)

  svc(name: string): ServiceView {
    const s = this.view.services.find((x) => x.name === name);
    if (!s) throw new Error(`no service ${name}`);
    return s;
  }

  setStatus(name: string, status: Status): void {
    const s = this.svc(name);
    s.status = status;
    s.since = Date.now();
    this.emit({ type: 'service', service: name, status, build_id: s.build_id });
  }

  setCandidate(name: string, candidate: Candidate | null): void {
    const s = this.svc(name);
    s.candidate = candidate;
    if (s.proxy) s.proxy = { ...s.proxy, pending: candidate && candidate.build_id && ['starting', 'switching', 'held'].includes(candidate.state) ? candidate.build_id : null };
    this.emit({ type: 'candidate', service: name, candidate });
  }

  cand(name: string, state: Candidate['state'], build: string | null = null, reason: string | null = null): void {
    this.setCandidate(name, { state, build_id: build, since: Date.now(), reason });
  }

  /** The new build becomes the running instance. */
  startInstance(s: ServiceView, build: string): void {
    const old = s.build_id;
    if (old && old !== build) this.previous[s.name] = old;
    s.build_id = build;
    s.pid = 10000 + Math.floor(Math.random() * 40000);
    if (s.proxy) {
      const port = 49000 + Math.floor(Math.random() * 900);
      s.instance_port = port;
      s.proxy = { ...s.proxy, upstream: port, build, pending: null, active: 0 };
    }
    this.instance(s.name, build);
    for (const line of startupLines(s.name, s.instance_port ?? s.port)) this.log(s.name, line);
    this.setStatus(s.name, { state: 'healthy' });
  }

  infra(group: string, service: string, state: string, health: string, message: string): void {
    const g = this.view.infra.find((x) => x.group === group);
    const c = g?.containers.find((x) => x.service === service);
    if (c) {
      c.state = state;
      c.health = health;
    }
    this.emit({ type: 'infra', group, message });
  }

  // ------------------------------------------------------------ flows

  /**
   * A change travels through a candidate: building, then starting, switching
   * and promoted, or failed / held. Returns the final candidate (null = live).
   */
  async candidateFlow(
    name: string,
    opts: { changed?: boolean; fail?: string | null; hold?: string | null; buildMs?: number } = {},
  ): Promise<Candidate | null> {
    const gen = this.generation;
    const check = () => {
      if (gen !== this.generation) throw new Abort();
    };
    const s = this.svc(name);
    if (opts.changed !== false) {
      this.cand(name, 'changed');
      await sleep(1200);
      check();
    }
    this.cand(name, 'building');
    this.buildsActive++;
    try {
      await sleep(opts.buildMs ?? 4200);
    } finally {
      this.buildsActive--;
    }
    check();
    if (opts.fail) {
      this.failedBuild.add(name);
      this.cand(name, 'failed', null, opts.fail);
      return s.candidate;
    }
    this.failedBuild.delete(name);
    const build = buildId();
    this.cand(name, 'starting', build);
    await sleep(2400);
    check();
    if (opts.hold) {
      this.cand(name, 'held', build, opts.hold);
      return s.candidate;
    }
    await this.switchTo(name, build);
    return null;
  }

  async switchTo(name: string, build: string): Promise<void> {
    const gen = this.generation;
    this.cand(name, 'switching', build);
    await sleep(1300);
    if (gen !== this.generation) throw new Abort();
    this.startInstance(this.svc(name), build);
    this.setCandidate(name, null);
  }

  // ------------------------------------------------------------ commands

  startRun(command: string, plan: { lines: string[]; fail?: string; stepMs?: number }): Run {
    const run: Run = {
      id: this.nextRun++,
      command,
      started: Date.now(),
      finished: null,
      status: 'running',
      error: null,
      log: logPath(this.view.repo, command),
    };
    this.runs.push(run);
    const rl = { lines: [] as string[], end: null as RunLogEvent | null, subs: new Set<Listener<RunLogEvent>>() };
    this.runLogs.set(run.id, rl);
    this.emit({ type: 'command', command, run: run.id, status: 'running' });
    void (async () => {
      for (const text of plan.lines) {
        await sleep(plan.stepMs ?? 450);
        rl.lines.push(text);
        for (const l of rl.subs) l.send({ type: 'line', text });
      }
      await sleep(300);
      run.finished = Date.now();
      run.status = plan.fail ? 'failed' : 'succeeded';
      run.error = plan.fail ?? null;
      rl.end = { type: 'end', status: run.status, error: run.error };
      for (const l of rl.subs) l.send(rl.end);
      rl.subs.clear();
      this.emit({ type: 'command', command, run: run.id, status: run.status });
    })();
    return run;
  }

  seedRun(command: string, agoMs: number, status: 'succeeded' | 'failed', lines: string[], error: string | null = null): void {
    const started = Date.now() - agoMs;
    const run: Run = { id: this.nextRun++, command, started, finished: started + 2100, status, error, log: null };
    this.runs.push(run);
    this.runLogs.set(run.id, { lines, end: { type: 'end', status, error }, subs: new Set() });
  }

  buildLog(name: string): string {
    return buildLog(name, this.failedBuild.has(name));
  }

  // ------------------------------------------------------------ usage

  private walk(key: string, baseCpu: number, baseMem: number): Walk {
    let w = this.walks.get(key);
    if (!w) this.walks.set(key, (w = { cpu: baseCpu, mem: baseMem, baseCpu, baseMem }));
    return w;
  }

  private step(w: Walk, active: boolean): { cpu: number; memory: number } {
    if (!active) return { cpu: 0, memory: 0 };
    w.cpu = Math.max(0, w.cpu + (Math.random() - 0.5) * w.baseCpu * 0.6 + (w.baseCpu - w.cpu) * 0.15);
    if (Math.random() < 0.05) w.cpu += w.baseCpu * 4;
    w.mem = Math.max(w.baseMem * 0.6, w.mem + (Math.random() - 0.48) * w.baseMem * 0.03 + (w.baseMem - w.mem) * 0.02);
    return { cpu: Math.round(w.cpu * 100) / 100, memory: Math.round(w.mem) };
  }

  private baseFor(kind: string, name: string): [number, number] {
    if (kind === 'devserver') return [4, 310e6];
    if (kind === 'job') return [12, 90e6];
    if (kind === 'tool') return [0.5, 30e6];
    if (name === 'worker' || name === 'mailer') return [2.5, 140e6];
    return [3.5, 190e6];
  }

  private containerBase(name: string): [number, number] {
    if (/db|postgres/.test(name)) return [1.8, 95e6];
    if (/cache/.test(name)) return [0.3, 9e6];
    if (/queue/.test(name)) return [1.2, 145e6];
    if (/search|meili/.test(name)) return [2.5, 520e6];
    return [0.6, 60e6];
  }

  report(at: number): UsageReport {
    const services = this.view.services.map((s) => {
      const [c, m] = this.baseFor(s.kind, s.name);
      const active = s.status.state !== 'stopped' && s.status.state !== 'completed' && s.status.state !== 'exited' && s.status.state !== 'failed';
      const l = this.step(this.walk(`svc:${s.name}`, c, m), active);
      return { name: s.name, ...l, cpu_time_ms: Math.round(at / 1000) % 100000, processes: active ? (s.kind === 'devserver' ? 3 : 2) : 0 };
    });
    const containers = this.view.infra.flatMap((g) =>
      g.containers.map((c) => {
        const [bc, bm] = this.containerBase(c.service);
        const l = this.step(this.walk(`ctr:${g.group}/${c.service}`, bc, bm), c.state === 'running');
        return { group: g.group, service: c.service, ...l };
      }),
    );
    const bw = this.walk('builds', 220, 950e6);
    const builds = { name: 'builds' as const, ...this.step(bw, this.buildsActive > 0), cpu_time_ms: 0, processes: this.buildsActive > 0 ? 7 : 0 };
    const groups = this.view.groups.map((g) => {
      let cpu = 0;
      let memory = 0;
      for (const m of g.members) {
        const s = services.find((x) => x.name === m);
        const c = m.startsWith('compose.') ? containers.find((x) => `compose.${x.group}.${x.service}` === m) : undefined;
        const v = s ?? c;
        if (v) {
          cpu += v.cpu;
          memory += v.memory;
        }
      }
      return { name: g.name, cpu: Math.round(cpu * 100) / 100, memory };
    });
    const all = [...services, ...containers, builds];
    const cpu = Math.round(all.reduce((a, b) => a + b.cpu, 0) * 100) / 100;
    const memory = all.reduce((a, b) => a + b.memory, 0);
    const logBytes = [...this.logs.values()].reduce((a, b) => a + b.length * 110, 0);
    const lanes = [{ name: 'main', cpu, memory, disk: { snapshot: 48_502_117, artifacts: 312_880_640, logs: 1_204_311 + logBytes, volumes: 734_003_200 } }];
    return { at, services, builds, containers, groups, lanes };
  }

  private seedHistory(): void {
    const now = Date.now();
    for (let i = HISTORY_POINTS; i > 0; i--) this.record(this.report(now - i * 2000));
  }

  private record(r: UsageReport): void {
    const add = (key: string, l: { cpu: number; memory: number }) => {
      let h = this.history.get(key);
      if (!h) this.history.set(key, (h = []));
      h.push({ at: r.at, cpu: l.cpu, memory: l.memory });
      if (h.length > HISTORY_POINTS) h.shift();
    };
    for (const s of r.services) add(`svc:${s.name}`, s);
    for (const c of r.containers) add(`ctr:${c.group}/${c.service}`, c);
    add('builds', r.builds);
    for (const g of r.groups) add(`grp:${g.name}`, g);
    for (const l of r.lanes) add(`lane:${l.name}`, l);
    this.lastUsage = r;
  }

  /** The store of what `prepare` makes (ADR-0062): the entries the services' last `prepare` used or made, and an older one. */
  private storeGone = new Set<string>();
  storeEntries(): StoreEntry[] {
    const now = Date.now();
    const used = this.view.services.flatMap((s) => (s.prepare?.key ? [{ s: s.name, k: s.prepare.key }] : []));
    const all: StoreEntry[] = [
      ...used.map(({ s, k }, i) => ({
        service: s,
        key: k,
        outputs: s === 'web' ? ['web/node_modules'] : [`${s}/obj`],
        bytes: (i + 1) * 410 * 2 ** 20,
        files: (i + 1) * 24_500,
        created: now - (3 + i) * 3_600_000,
        last_used: now - 26 * 60_000,
        prepare_ms: 162_000,
        copy_ms: 2400,
        used_by: ['', 'agent-login-fix'],
      })),
      { service: 'web', key: '0c77d2e1b9a4f3605d18', outputs: ['web/node_modules'], bytes: 395 * 2 ** 20, files: 24_100, created: now - 9 * 86_400_000, last_used: now - 6 * 86_400_000, prepare_ms: 158_000, used_by: [] },
    ];
    return all.filter((e) => !this.storeGone.has(`${e.service}/${e.key}`)).sort((a, b) => b.bytes - a.bytes);
  }

  removeStoreEntry(service: string, key: string): boolean {
    const id = `${service}/${key}`;
    if (!this.storeEntries().some((e) => `${e.service}/${e.key}` === id)) return false;
    this.storeGone.add(id);
    return true;
  }

  usageWithHistory(): UsageReport {
    const r = clone(this.lastUsage ?? this.report(Date.now()));
    for (const s of r.services) s.history = clone(this.history.get(`svc:${s.name}`) ?? []);
    for (const c of r.containers) c.history = clone(this.history.get(`ctr:${c.group}/${c.service}`) ?? []);
    r.builds.history = clone(this.history.get('builds') ?? []);
    for (const g of r.groups) g.history = clone(this.history.get(`grp:${g.name}`) ?? []);
    for (const l of r.lanes) l.history = clone(this.history.get(`lane:${l.name}`) ?? []);
    return r;
  }

  private tickUsage(): void {
    const r = this.report(Date.now());
    this.record(r);
    this.emit({ type: 'usage', ...r });
  }

  private tickLogs(): void {
    const live = this.view.services.filter((s) => ['healthy', 'running', 'unhealthy', 'starting'].includes(s.status.state));
    if (!live.length) return;
    const s = live[Math.floor(Math.random() * live.length)];
    this.log(s.name, logLine(s.name, s.kind));
  }
}

function logPath(repo: string, command: string): string {
  const sep = repo.includes('/') ? '/' : String.fromCharCode(92);
  return [repo, '.devstack', 'logs', 'commands', command, `${Date.now()}.log`].join(sep);
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
