import { describe, expect, it } from 'vitest';
import type { Guest } from '@/api/types';
import { guestLabel, guestsReserved, guestsSignature } from './guests';

const g = (lease: string, pool = 'default'): Guest => ({
  pool,
  lease,
  member: 'm1',
  owner: 'anna',
  repo: 'shop',
  lane: 'feat-cart',
  state: 'running',
  since: 1,
  cpus: 1.25,
  memory: 2 * 2 ** 30,
});

describe('guest lanes', () => {
  it('are named by owner, repository and lane', () => {
    expect(guestLabel(g('l1'))).toBe('anna: shop · feat-cart');
  });

  it('add up what they asked for', () => {
    expect(guestsReserved([g('l1'), g('l2')])).toBe('2.5 CPU · 4.0 GB');
  });

  it('change their signature when a lane comes or goes, not when the order does', () => {
    expect(guestsSignature([g('l2'), g('l1')])).toBe(guestsSignature([g('l1'), g('l2')]));
    expect(guestsSignature([g('l1')])).not.toBe(guestsSignature([g('l1'), g('l2')]));
    expect(guestsSignature([g('l1', 'a')])).not.toBe(guestsSignature([g('l1', 'b')]));
  });
});
