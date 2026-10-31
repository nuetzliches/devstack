// The ports of the mock stacks (ADR-0082): `GET /v1/ports` of a daemon and
// `GET /hub/ports`, derived from each stack's view. The docs stack is
// stopped and declares 4200, which shop's `web` holds: one overlap.
import type { HubPorts, PortEntry, PortOverlap, StackPorts } from '../types';
import type { MockStack } from './stack';

/** A started lane of a mock stack: its offset, and its node if it runs on one. */
export type MockLane = { name: string; port_offset: number; node?: string };

function hostPort(shown: string): { port: number; target: number } | null {
  // `5432->127.0.0.1:15432`
  const [target, addr] = shown.split('->');
  const port = Number(addr?.split(':').pop());
  return Number.isFinite(port) && port > 0 ? { port, target: Number(target) } : null;
}

/** One tree's entries: `main`, or a lane here moved by its offset. */
function tree(s: MockStack, lane: string, offset: number): PortEntry[] {
  const out: PortEntry[] = [];
  const servedBy = lane === 'main' ? s.view.main_served?.lane : undefined;
  for (const [i, v] of s.view.services.entries()) {
    if (v.port === null || (lane !== 'main' && v.singleton)) continue;
    const port = v.port + offset;
    const running = v.status.state === 'healthy' || v.status.state === 'running';
    const toLane = servedBy && !v.singleton ? { target_lane: servedBy } : {};
    if (v.proxy) {
      // Stable, not the view's random one, so the graph and its tests are deterministic.
      const instance = 49010 + 10 * i + offset;
      out.push({ port, proto: 'tcp', lane, kind: 'fixed', holder: v.name, via: 'proxy', target: instance, ...toLane, listening: true });
      if (!toLane.target_lane) out.push({ port: instance, proto: 'tcp', lane, kind: 'instance', holder: v.name, via: 'direct', listening: running });
    } else if (v.kind === 'devserver') {
      const instance = 51000 + port - 4000;
      const target = toLane.target_lane ? port + 100 : instance;
      out.push({ port, proto: 'tcp', lane, kind: 'fixed', holder: v.name, via: 'forward', target, ...toLane, listening: true });
      if (!toLane.target_lane) out.push({ port: instance, proto: 'tcp', lane, kind: 'instance', holder: v.name, via: 'direct', listening: running });
    } else {
      out.push({ port, proto: 'tcp', lane, kind: 'fixed', holder: v.name, via: 'direct', listening: running });
    }
  }
  for (const g of s.view.infra) {
    for (const c of g.containers) {
      for (const shown of c.ports) {
        const p = hostPort(shown);
        if (!p) continue;
        out.push({ port: p.port + offset, proto: 'tcp', lane, kind: 'published', holder: `${g.group}.${c.service}`, via: 'docker', target: p.target, listening: c.state === 'running' });
      }
    }
  }
  return out;
}

/** A lane on a node: its fixed and container ports, forwarded here. */
function onNode(s: MockStack, l: MockLane): PortEntry[] {
  return tree(s, l.name, l.port_offset)
    .filter((e) => e.kind === 'fixed' || e.kind === 'published')
    .map((e) => ({ port: e.port, proto: e.proto, lane: l.name, kind: 'node', holder: e.holder, via: 'forward', target: e.port, node: l.node, listening: true }));
}

/** What a stopped stack's stack file declares: its services' ports, a linked container. */
function declared(s: MockStack): PortEntry[] {
  const out: PortEntry[] = s.view.services
    .filter((v) => v.port !== null)
    .map((v) => ({ port: v.port!, proto: 'tcp', lane: 'main', kind: 'declared', holder: v.name, via: 'direct', listening: false }));
  for (const g of s.view.infra) {
    for (const c of g.containers) {
      const p = c.ports.map(hostPort).find((x) => x !== null);
      if (p) out.push({ port: p.port, proto: 'tcp', lane: 'main', kind: 'declared', holder: `${g.group}.${c.service}`, via: 'docker', listening: false });
    }
  }
  return out;
}

/** `GET /v1/ports` of the stack's daemon: the main tree's and every started lane's. */
export function daemonPorts(s: MockStack, lanes: MockLane[]): PortEntry[] {
  const out = tree(s, 'main', 0);
  for (const l of lanes) out.push(...(l.node ? onNode(s, l) : tree(s, l.name, l.port_offset)));
  return out;
}

export function overlapsOf(stacks: StackPorts[]): PortOverlap[] {
  const by = new Map<string, PortOverlap>();
  for (const s of stacks) {
    // An instance port the OS hands out is no overlap (as the daemon's).
    for (const e of s.ports.filter((x) => x.kind !== 'instance')) {
      const key = `${e.port}/${e.proto}`;
      const o = by.get(key) ?? { port: e.port, proto: e.proto, stacks: [] };
      o.stacks.push({ id: s.id, name: s.name, lane: e.lane, holder: e.holder, kind: e.kind });
      by.set(key, o);
    }
  }
  return [...by.values()].filter((o) => new Set(o.stacks.map((u) => u.id)).size > 1).sort((a, b) => a.port - b.port);
}

/** Every port the stack's main tree and its first three lanes would use, moved by `by`. */
function footprint(s: MockStack, by: number): number[] {
  const base = declared(s).map((e) => e.port);
  return [0, 100, 200, 300].flatMap((lane) => base.map((p) => p + by + lane));
}

/** `--shift auto` (ADR-0082): the lowest multiple of 1000 above its current shift's ports free of every other stack's. */
export function autoShift(s: MockStack, stacks: MockStack[]): number {
  const taken = new Set(
    stacks.filter((o) => o !== s).flatMap((o) => (o.running ? daemonPorts(o, []) : declared(o)).map((e) => e.port)),
  );
  const now = s.entry.shift ?? 0;
  for (let shift = 1000; shift <= 60000; shift += 1000) {
    if (footprint(s, shift - now).every((p) => p < 65536 && !taken.has(p))) return shift;
  }
  return 1000;
}

/** What `devstack up` says when a running stack holds the ports. */
export function refusal(holders: { name: string; ports: number[] }[]): string {
  const who = holders.map((h) => `the stack \`${h.name}\` runs on ${h.ports.join(', ')}`).join('; ');
  return [
    `this stack would start on its ports, and ${who}. Nothing started; whoever was first keeps its ports.`,
    'Start this stack moved by a port shift on this machine (its services, containers and links move; its data stays):',
    '  devstack up --shift auto',
    '  devstack up --shift <n>    (a multiple of 1000)',
    'The shift sticks to this stack on this machine until `devstack up --shift none`.',
  ].join('\n');
}

/** `GET /hub/ports`. */
export function hubPorts(stacks: MockStack[], lanesOf: (s: MockStack) => MockLane[]): HubPorts {
  const list: StackPorts[] = stacks.map((s) => ({
    id: s.entry.id,
    name: s.entry.name,
    running: s.running,
    ports: s.running ? daemonPorts(s, lanesOf(s)) : declared(s),
  }));
  return { stacks: list, overlaps: overlapsOf(list) };
}
