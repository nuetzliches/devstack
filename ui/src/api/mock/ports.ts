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
