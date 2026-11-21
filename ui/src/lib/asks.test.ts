import { describe, expect, it } from 'vitest';
import type { PoolMember, PoolNode, PoolOverview } from '@/api/types';
import { asking, related, relatedText } from './asks';

const member = (id: string, extra: Partial<PoolMember> = {}): PoolMember => ({ id, name: id, state: 'approved', role: 'member', requested_at: 1, ...extra });
const node = (id: string, online: boolean, sharing: boolean): PoolNode => ({ id, name: id, online, report: { sharing, cpus: 4, memory: 8 } }) as PoolNode;

describe('machines that ask to run lanes', () => {
  it('are approved members that share, online, without a grant', () => {
    const o: PoolOverview = {
      joined: true,
      members: [member('anna'), member('ben', { node: true }), member('carl'), member('dora'), member('eve', { state: 'pending' })],
      nodes: [node('anna', true, true), node('ben', true, true), node('carl', true, false), node('dora', false, true), node('eve', true, true)],
    };
    expect(asking(o).map((a) => a.member.id)).toEqual(['anna']);
    expect(asking(null)).toEqual([]);
  });

  it('point to another entry of the same machine', () => {
    const o: PoolOverview = {
      joined: true,
      members: [member('a1', { name: 'DESKTOP-42' }), member('a2', { name: 'DESKTOP-42-wsl', node: true }), member('b1', { name: 'build', address: '10.0.0.7' }), member('b2', { name: 'other', address: '10.0.0.7' }), member('c', { name: 'lonely' })],
    };
    const m = (id: string) => o.members!.find((x) => x.id === id)!;
    expect(related(m('a1'), o)).toMatchObject({ member: { id: 'a2' }, by: 'name' });
    expect(related(m('b1'), o)).toMatchObject({ member: { id: 'b2' }, by: 'address' });
    expect(related(m('c'), o)).toBeNull();
  });

  it('say a name match proves nothing, and name the other id', () => {
    // A member picks its own name: one granted already could call itself
    // after another machine (review before 0.52.0).
    const granted = member('5d1e8a77c0ffee00', { name: 'anna-wsl', node: true });
    expect(relatedText({ member: granted, by: 'name' })).toBe('a similar name to anna-wsl (5d1e8a77), not checked; that one may run lanes already');
    expect(relatedText({ member: member('b2', { name: 'other' }), by: 'address' })).toBe('asked from the same address as other (b2)');
  });
});
