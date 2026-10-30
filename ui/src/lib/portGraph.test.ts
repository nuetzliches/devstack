import { describe, expect, it } from 'vitest';
import type { PortEntry, PortOverlap } from '@/api/types';
import { docsSeed, shopSeed } from '@/api/mock/data';
import { daemonPorts, hubPorts } from '@/api/mock/ports';
import { MockStack } from '@/api/mock/stack';
import { portGraph } from './portGraph';

const e = (p: Partial<PortEntry> & Pick<PortEntry, 'port' | 'lane' | 'kind'>): PortEntry => ({ proto: 'tcp', via: 'direct', listening: true, ...p });

describe('the port graph of a stack (ADR-0082 point 2)', () => {
  const entries: PortEntry[] = [
    // The main slot, served by lane `feat`.
    e({ port: 5080, lane: 'main', kind: 'fixed', holder: 'api', via: 'proxy', target: 5180, target_lane: 'feat' }),
    e({ port: 4200, lane: 'main', kind: 'fixed', holder: 'web', via: 'forward', target: 4300, target_lane: 'feat' }),
    // A container of the main tree.
    e({ port: 15432, lane: 'main', kind: 'published', holder: 'data.db', via: 'docker', target: 5432, listening: false }),
    // Lane `feat` here: its own ports.
    e({ port: 5180, lane: 'feat', kind: 'fixed', holder: 'api', via: 'proxy', target: 49120 }),
    e({ port: 49120, lane: 'feat', kind: 'instance', holder: 'api' }),
    e({ port: 15532, lane: 'feat', kind: 'published', holder: 'data.db', via: 'docker', target: 5432 }),
    // Lane `remote` on a node: its forwards.
    e({ port: 5280, lane: 'remote', kind: 'node', holder: 'api', via: 'forward', target: 5280, node: 'build-server' }),
  ];
  const overlaps: PortOverlap[] = [
    {
      port: 4200,
      proto: 'tcp',
      stacks: [
        { id: 'shop', name: 'shop', lane: 'main', holder: 'web', kind: 'fixed' },
        { id: 'docs', name: 'docs', lane: 'main', holder: 'site', kind: 'declared' },
      ],
    },
    // Not this stack's.
    { port: 8080, proto: 'tcp', stacks: [{ id: 'a', name: 'a', lane: 'main', kind: 'fixed' }, { id: 'b', name: 'b', lane: 'main', kind: 'fixed' }] },
  ];
  const lanes = [{ name: 'main' }, { name: 'feat' }, { name: 'remote', node: 'pool:build-server' }, { name: 'idle-one' }];
  const g = portGraph({ stack: 'shop', entries, overlaps, lanes, instance: false });
  const lane = (n: string) => g.lanes.find((l) => l.name === n)!;

  it('puts the fixed ports in the band of the lane they lead to, main first', () => {
    expect(g.lanes.map((l) => l.name)).toEqual(['main', 'feat', 'remote']);
    expect(lane('feat').servesMain).toBe(true);
    expect(lane('main').servesMain).toBe(false);
    expect(lane('feat').inbound.map((p) => p.label)).toEqual(['4200/tcp', '5080/tcp']);
    expect(lane('main').inbound.map((p) => p.label)).toEqual(['15432/tcp']);
  });

  it("gives each lane's holders their own ports, services before containers", () => {
    expect(lane('feat').holders.map((h) => h.name)).toEqual(['api', 'web', 'data.db']);
    expect(lane('feat').holders[0].ports.map((p) => p.label)).toEqual(['5180/tcp']);
    expect(lane('feat').holders[2]).toMatchObject({ container: true, ports: [expect.objectContaining({ label: '15532/tcp', kind: 'published' })] });
  });

  it('hides instance ports unless asked, and counts them', () => {
    expect(g.hiddenInstance).toBe(1);
    const all = portGraph({ stack: 'shop', entries, overlaps, lanes, instance: true });
    expect(all.hiddenInstance).toBe(0);
    expect(all.lanes.find((l) => l.name === 'feat')!.holders[0].ports.map((p) => p.label)).toEqual(['5180/tcp', '49120/tcp']);
  });

  it('collapses the lanes without ports into one line', () => {
    expect(g.idle).toEqual(['idle-one']);
  });

  it('draws fixed port -> lane -> holder, and a forward with its node', () => {
    expect(g.edges).toContainEqual(expect.objectContaining({ from: 'p:main:5080/tcp:fixed', to: 'l:feat' }));
    expect(g.edges).toContainEqual(expect.objectContaining({ from: 'l:feat', to: 'h:feat:api' }));
    expect(g.edges).toContainEqual(expect.objectContaining({ from: 'p:remote:5280/tcp:node', to: 'l:remote', label: 'build-server' }));
    expect(lane('remote').node).toBe('build-server');
    // Not listening: muted.
    expect(g.edges.find((x) => x.from === 'p:main:15432/tcp:published')?.muted).toBe(true);
    expect(g.related['p:main:5080/tcp:fixed']).toEqual(['p:main:5080/tcp:fixed', 'l:feat', 'h:feat:api']);
    expect(g.related['h:feat:api']).toEqual(expect.arrayContaining(['p:main:5080/tcp:fixed', 'p:feat:5180/tcp:fixed', 'l:feat']));
  });

  it('marks the overlaps of this stack with the other stack, its lane and holder', () => {
    expect(g.overlaps).toBe(1);
    const web = lane('feat').inbound.find((p) => p.port === 4200)!;
    expect(web.overlap).toEqual([{ stack: 'docs', lane: 'main', holder: 'site', kind: 'declared' }]);
    expect(web.title).toContain('also used by docs (lane main, site, declared)');
    expect(lane('feat').holders.find((h) => h.name === 'web')!.overlap).toBe(true);
    expect(g.edges.find((x) => x.from === web.id)?.overlap).toBe(true);
  });

  it('draws the mock shop: a lane serving the main ports, a container, a node, an overlap with docs', () => {
    const now = Date.now();
    const shop = new MockStack(shopSeed(now));
    shop.serveMain('agent-login-fix');
    const docs = new MockStack(docsSeed(now));
    const h = hubPorts([shop, docs], (s) =>
      s === shop
        ? [
            { name: 'agent-login-fix', port_offset: 100 },
            { name: 'review-cart', port_offset: 200, node: 'build-server' },
          ]
        : [],
    );
    const mine = h.stacks.find((s) => s.name === 'shop')!;
    const graph = portGraph({ stack: mine.id, entries: mine.ports, overlaps: h.overlaps, instance: false });
    expect(graph.lanes.map((l) => l.name)).toEqual(['main', 'agent-login-fix', 'review-cart']);
    expect(graph.lanes[1].servesMain).toBe(true);
    expect(graph.lanes[0].inbound.some((p) => p.kind === 'published' && p.holder === 'data.db')).toBe(true);
    expect(graph.lanes[2]).toMatchObject({ node: 'build-server' });
    expect(graph.overlaps).toBe(1);
    // Stable: two mock stacks (each with random instance ports in its view) give the same ports.
    const instances = () => daemonPorts(new MockStack(shopSeed(now)), [{ name: 'agent-login-fix', port_offset: 100 }]).filter((x) => x.kind === 'instance').map((x) => x.port);
    expect(instances().length).toBeGreaterThan(0);
    expect(instances()).toEqual(instances());
  });

  it("shows a stopped stack's declared ports in main", () => {
    const d = portGraph({ stack: 'docs', entries: [e({ port: 4200, lane: 'main', kind: 'declared', holder: 'site', listening: false })], overlaps, instance: false });
    expect(d.lanes.map((l) => l.name)).toEqual(['main']);
    expect(d.lanes[0].servesMain).toBe(false);
    expect(d.lanes[0].inbound[0]).toMatchObject({ kind: 'declared', listening: false });
  });
});
