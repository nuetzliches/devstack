import { describe, expect, it } from 'vitest';
import type { LaneView } from '@/api/types';
import { laneActions } from './laneActions';

const lane = (more: Partial<LaneView> = {}): LaneView => ({ name: 'feat', path: '/src/feat', number: 1, active: true, port_offset: 100, owns_main: false, running: 1, services: 1, ...more }) as LaneView;
const keys = (l: LaneView) => {
  const a = laneActions(l);
  return [a.main.key, ...a.items.map((i) => i.key)];
};

describe('a lane’s actions', () => {
  it('restart a started lane warm, or rebuild it, cold it, stop it', () => {
    expect(keys(lane())).toEqual(['restart', 'rebuild', 'cold', 'stop']);
    expect(laneActions(lane()).items.every((i) => !i.disabled)).toBe(true);
  });

  it('start a stopped one, or throw its data away, or remove its worktree', () => {
    expect(keys(lane({ active: false }))).toEqual(['start', 'reset', 'remove']);
  });

  it('keep a lane that serves the main ports running, and wait while it moves', () => {
    const served = laneActions(lane({ owns_main: true }));
    expect(served.items.filter((i) => i.disabled).map((i) => i.key)).toEqual(['cold', 'stop']);
    expect(served.main.disabled).toBeFalsy();
    const moving = laneActions(lane({ moving: { kind: 'move', to: 'local', step: 1, steps: 3, what: 'sync' } as LaneView['moving'] }));
    expect([moving.main, ...moving.items].every((i) => i.disabled)).toBe(true);
  });
});

describe('the lane main’s actions (ADR-0081)', () => {
  const main = (more: Partial<LaneView> = {}) => lane({ name: 'main', path: '/src/shop', port_offset: 0, owns_main: true, ...more });

  it('has the same items as any lane', () => {
    expect(keys(main())).toEqual(['restart', 'rebuild', 'cold', 'stop']);
    expect(keys(main({ active: false }))).toEqual(['start', 'reset', 'remove']);
  });

  it('restarts, rebuilds and stops while it serves the main ports', () => {
    const a = laneActions(main());
    expect(a.main.disabled).toBeFalsy();
    expect(a.items.filter((i) => !i.disabled).map((i) => i.key)).toEqual(['rebuild', 'stop']);
  });

  it('shows what has no meaning for it disabled, each with the reason', () => {
    const running = laneActions(main()).items.find((i) => i.key === 'cold')!;
    expect(running.disabled).toBe(true);
    expect(running.title).toMatch(/main working tree/);
    const stopped = laneActions(main({ active: false }));
    expect(stopped.main.disabled).toBeFalsy();
    for (const i of stopped.items) {
      expect(i.disabled).toBe(true);
      expect(i.title).toMatch(/main working tree/);
    }
  });
});
