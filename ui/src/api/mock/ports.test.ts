import { describe, expect, it } from 'vitest';
import { docsSeed, shopSeed } from './data';
import { autoShift, daemonPorts, hubPorts, refusal } from './ports';
import { runningHolders } from '@/lib/shift';
import { MockStack } from './stack';

describe('mock ports (ADR-0082)', () => {
  const now = Date.now();
  const shop = new MockStack(shopSeed(now));
  const docs = new MockStack(docsSeed(now));
  const lanes = [{ name: 'agent-login-fix', port_offset: 100 }];

  it('lists the main tree and its lanes, moved by their offset', () => {
    const p = daemonPorts(shop, lanes);
    expect(p).toContainEqual(expect.objectContaining({ port: 5080, lane: 'main', kind: 'fixed', holder: 'api', via: 'proxy' }));
    expect(p).toContainEqual(expect.objectContaining({ port: 5180, lane: 'agent-login-fix', kind: 'fixed', holder: 'api' }));
    expect(p).toContainEqual(expect.objectContaining({ port: 15432, kind: 'published', holder: 'data.db', via: 'docker', target: 5432 }));
  });

  it('marks the port a stopped stack declares and a running one holds', () => {
    const h = hubPorts([shop, docs], (s) => (s === shop ? lanes : []));
    expect(h.stacks.find((s) => s.name === 'docs')?.ports.every((e) => e.kind === 'declared')).toBe(true);
    expect(h.overlaps).toHaveLength(1);
    expect(h.overlaps[0]).toMatchObject({ port: 4200, proto: 'tcp' });
    expect(h.overlaps[0].stacks.map((u) => u.name).sort()).toEqual(['docs', 'shop']);
  });

  it('starts a colliding stack moved by a shift (ADR-0082)', () => {
    const other = new MockStack(docsSeed(now));
    const h = hubPorts([shop, other], () => []);
    const holders = runningHolders(other.entry.id, h.overlaps);
    expect(holders).toEqual([{ name: 'shop', ports: [4200] }]);
    expect(refusal(holders)).toContain('devstack up --shift auto');
    const shift = autoShift(other, [shop, other]);
    expect(shift).toBe(1000);
    other.setShift(shift);
    expect(other.entry.shift).toBe(1000);
    expect(other.view.services.find((v) => v.name === 'site')?.port).toBe(5200);
    expect(hubPorts([shop, other], () => []).overlaps).toHaveLength(0);
    other.setShift(0);
    expect(other.entry.shift).toBeUndefined();
    expect(other.view.services.find((v) => v.name === 'site')?.port).toBe(4200);
  });
});
