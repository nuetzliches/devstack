// The port world of one stack as a graph (ADR-0082 point 2): the machine's
// fixed ports on the left, the lanes in the middle, each lane's holders
// (services, `<group>.<service>` containers) with their own ports on the
// right. Each lane is a band: the ports that lead to it, it, its holders,
// so the connectors stay inside the band and never cross another lane's.
import type { PortEntry, PortOverlap } from '@/api/types';

/** Another stack's use of a port this stack uses too. */
export type OverlapUse = { stack: string; lane: string; holder?: string; kind: PortEntry['kind'] };

export type GraphPort = {
  /** Unique in the graph: `p:<lane>:<port>/<proto>:<kind>`. */
  id: string;
  port: number;
  proto: PortEntry['proto'];
  /** `5080/tcp`. */
  label: string;
  kind: PortEntry['kind'];
  via: PortEntry['via'];
  /** The tree the entry is of (`main` for the machine's fixed ports). */
  lane: string;
  holder?: string;
  target?: number;
  targetLane?: string;
  node?: string;
  listening: boolean;
  /** The other stacks that use it, or null. */
  overlap: OverlapUse[] | null;
  /** What it is, in a sentence: its title and accessible label. */
  title: string;
};

export type GraphHolder = {
  /** `h:<lane>:<holder>`. */
  id: string;
  lane: string;
  name: string;
  /** A Compose container (`<group>.<service>`), not a service. */
  container: boolean;
  /** Its own ports in its lane (instance, a lane's fixed and published ports): the right column's chips. */
  ports: GraphPort[];
  /** Something of it listens. */
  listening: boolean;
  /** One of its ports, or a fixed port leading to it, overlaps with another stack. */
  overlap: boolean;
};

export type GraphLane = {
  /** `l:<lane>`. */
  id: string;
  name: string;
  /** The node it runs on (from its forwards or the lane list); null: this machine. */
  node: string | null;
  /** It serves the main ports. */
  servesMain: boolean;
  /** The left column of its band: the machine's fixed ports that lead to it, a lane on a node's forwards. */
  inbound: GraphPort[];
  holders: GraphHolder[];
};

export type GraphEdge = { id: string; from: string; to: string; muted: boolean; overlap: boolean; label?: string };

export type PortGraph = {
  /** The lanes with ports, `main` first, in the order given. */
  lanes: GraphLane[];
  /** The lanes without a port (not started): one line. */
  idle: string[];
  edges: GraphEdge[];
  /** How many ports this stack shares with another stack. */
  overlaps: number;
  /** Instance ports left out (`instance: false`). */
  hiddenInstance: number;
  /** For each node id, the ids on its paths: what hovering it highlights. */
  related: Record<string, string[]>;
};

export type PortGraphInput = {
  /** The stack's id, to tell its own uses in an overlap from the others'. */
  stack: string;
  entries: PortEntry[];
  overlaps: PortOverlap[];
  /** The lanes known, `main` first; `node` names its node. Lanes only in `entries` follow. */
  lanes?: { name: string; node?: string | null }[];
  /** The lane serving the main ports, if known; else the one the main tree's fixed ports name. */
  servedBy?: string | null;
  /** Show instance ports. */
  instance: boolean;
};

const MAIN = 'main';
/** The kinds of the main tree that sit on the left: the machine's fixed ports of the stack. */
const LEFT: PortEntry['kind'][] = ['fixed', 'held', 'published', 'declared'];

const KIND_TEXT: Record<PortEntry['kind'], string> = {
  fixed: 'fixed port',
  instance: 'instance port',
  published: 'published port',
  held: 'held port',
  node: 'forwarded port',
  declared: 'declared port',
};
const VIA_TEXT: Record<PortEntry['via'], string> = {
  proxy: "devstack's proxy",
  forward: 'a devstack forward',
  direct: 'the process itself',
  docker: 'Docker',
};

function portTitle(e: PortEntry, overlap: OverlapUse[] | null): string {
  const lane = e.lane === MAIN ? 'main' : `lane ${e.lane}`;
  let s = `${e.port}/${e.proto}: ${KIND_TEXT[e.kind]} of ${e.holder ?? 'an unknown holder'} in ${lane}`;
  if (e.kind !== 'declared') s += `, via ${VIA_TEXT[e.via]}`;
  if (e.node) s += `, from node ${e.node}`;
  if (e.target !== undefined) s += `, leads to ${e.target_lane ? `lane ${e.target_lane} ` : ''}:${e.target}`;
  if (!e.listening) s += '; not listening';
  if (overlap) s += `; also used by ${overlap.map(useText).join('; ')}`;
  return s;
}

/** `docs (lane main, web, declared)`. */
export function useText(u: OverlapUse): string {
  return `${u.stack} (lane ${u.lane}${u.holder ? `, ${u.holder}` : ''}, ${u.kind})`;
}

/** Turn a stack's port entries and the machine's overlaps into the graph the stack page draws. */
export function portGraph(input: PortGraphInput): PortGraph {
  const { stack, instance } = input;

  // The overlaps that concern this stack: port/proto -> the other stacks' uses.
  const shared = new Map<string, OverlapUse[]>();
  for (const o of input.overlaps) {
    if (!o.stacks.some((u) => u.id === stack)) continue;
    const others = o.stacks.filter((u) => u.id !== stack).map((u) => ({ stack: u.name, lane: u.lane, ...(u.holder ? { holder: u.holder } : {}), kind: u.kind }));
    if (others.length) shared.set(`${o.port}/${o.proto}`, others);
  }

  const servedBy =
    input.entries.find((e) => e.lane === MAIN && e.kind === 'fixed' && e.target_lane)?.target_lane ??
    input.servedBy ??
    (input.entries.some((e) => e.kind !== 'declared') ? MAIN : null);

  // The bands, in the order of the lanes given, then those only the entries name.
  const bands = new Map<string, GraphLane>();
  const band = (name: string): GraphLane => {
    let b = bands.get(name);
    if (!b) {
      b = { id: `l:${name}`, name, node: null, servesMain: name === servedBy, inbound: [], holders: [] };
      bands.set(name, b);
    }
    return b;
  };
  const order = [MAIN, ...(input.lanes ?? []).map((l) => l.name).filter((n) => n !== MAIN)];
  const nodes = new Map((input.lanes ?? []).filter((l) => l.node).map((l) => [l.name, l.node as string]));

  const holderOf = (b: GraphLane, name: string): GraphHolder => {
    let h = b.holders.find((x) => x.name === name);
    if (!h) {
      h = { id: `h:${b.name}:${name}`, lane: b.name, name, container: name.includes('.'), ports: [], listening: false, overlap: false };
      b.holders.push(h);
    }
    return h;
  };

  let hiddenInstance = 0;
  const seen = new Set<string>();
  const portOf = (e: PortEntry): GraphPort => {
    const key = `${e.port}/${e.proto}`;
    const overlap = e.kind === 'instance' ? null : (shared.get(key) ?? null);
    let id = `p:${e.lane}:${key}:${e.kind}`;
    for (let i = 2; seen.has(id); i++) id = `p:${e.lane}:${key}:${e.kind}:${i}`;
    seen.add(id);
    return {
      id,
      port: e.port,
      proto: e.proto,
      label: key,
      kind: e.kind,
      via: e.via,
      lane: e.lane,
      ...(e.holder ? { holder: e.holder } : {}),
      ...(e.target !== undefined ? { target: e.target } : {}),
      ...(e.target_lane ? { targetLane: e.target_lane } : {}),
      ...(e.node ? { node: e.node } : {}),
      listening: e.listening,
      overlap,
      title: portTitle(e, overlap),
    };
  };

  /** Each holder's left ports, for its edge and its marks. */
  const leftOf = new Map<string, GraphPort[]>();
  for (const e of input.entries) {
    if (e.kind === 'instance' && !instance) {
      // Left out, but its holder still shows in its lane.
      hiddenInstance++;
      if (e.holder) {
        const h = holderOf(band(e.lane), e.holder);
        h.listening ||= e.listening;
      }
      continue;
    }
    const p = portOf(e);
    const left = (e.lane === MAIN && LEFT.includes(e.kind)) || e.kind === 'node';
    const b = band(left && e.lane === MAIN ? (e.target_lane ?? MAIN) : e.lane);
    if (e.node && !b.node) b.node = e.node;
    const h = e.holder ? holderOf(b, e.holder) : null;
    if (left) {
      b.inbound.push(p);
      if (h) leftOf.set(h.id, [...(leftOf.get(h.id) ?? []), p]);
    } else if (h) {
      h.ports.push(p);
    } else {
      // An own port of a lane without a known holder: it leads to the lane.
      b.inbound.push(p);
    }
    if (h) {
      h.listening ||= e.listening;
      h.overlap ||= !!p.overlap;
    }
  }

  const byPort = (a: GraphPort, b: GraphPort) => a.port - b.port || a.proto.localeCompare(b.proto);
  const lanes: GraphLane[] = [];
  const idle: string[] = [];
  const names = [...order, ...[...bands.keys()].filter((n) => !order.includes(n))];
  for (const name of names) {
    const b = bands.get(name);
    if (!b || (!b.inbound.length && !b.holders.length)) {
      if (!idle.includes(name)) idle.push(name);
      continue;
    }
    b.node ??= nodes.get(name) ?? null;
    b.inbound.sort(byPort);
    for (const h of b.holders) h.ports.sort(byPort);
    // Services first, then containers; each by name.
    b.holders.sort((x, y) => Number(x.container) - Number(y.container) || x.name.localeCompare(y.name));
    lanes.push(b);
  }

  // The edges, and what each node's path is: a fixed port, its lane, its
  // holder; a holder, its lane, its own ports and the fixed ports leading to
  // it; a chip, its holder and lane; a lane, its band.
  const edges: GraphEdge[] = [];
  const related: Record<string, string[]> = {};
  for (const b of lanes) {
    related[b.id] = [b.id, ...b.inbound.map((p) => p.id), ...b.holders.map((h) => h.id)];
    for (const p of b.inbound) {
      const label = p.kind === 'node' ? p.node : undefined;
      edges.push({ id: `${p.id}>${b.id}`, from: p.id, to: b.id, muted: !p.listening, overlap: !!p.overlap, ...(label ? { label } : {}) });
      const h = p.holder ? b.holders.find((x) => x.name === p.holder) : undefined;
      related[p.id] = h ? [p.id, b.id, h.id] : [p.id, b.id];
    }
    for (const h of b.holders) {
      edges.push({ id: `${b.id}>${h.id}`, from: b.id, to: h.id, muted: !h.listening, overlap: h.overlap });
      related[h.id] = [h.id, b.id, ...h.ports.map((p) => p.id), ...(leftOf.get(h.id) ?? []).map((p) => p.id)];
      for (const p of h.ports) related[p.id] = [p.id, h.id, b.id];
    }
  }

  return { lanes, idle, edges, overlaps: shared.size, hiddenInstance, related };
}
