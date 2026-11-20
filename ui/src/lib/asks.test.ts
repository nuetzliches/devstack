import { describe, expect, it } from 'vitest';
import type { PoolMember, PoolNode, PoolOverview } from '@/api/types';
import { asking, sameMachine } from './asks';

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
    expect(sameMachine(m('a1'), o)?.id).toBe('a2');
    expect(sameMachine(m('b1'), o)?.id).toBe('b2');
    expect(sameMachine(m('c'), o)).toBeNull();
  });
});
