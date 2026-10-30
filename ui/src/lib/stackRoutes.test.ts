import { describe, expect, it } from 'vitest';
import { laneOfParam, laneParam, laneView, oldLaneLink } from './stackRoutes';

describe('where a stack and its lanes are (ADR-0081)', () => {
  it('names the main working tree `main` in the URL, `null` in the store', () => {
    expect(laneParam(null)).toBe('main');
    expect(laneParam('feat')).toBe('feat');
    expect(laneOfParam('main')).toBeNull();
    expect(laneOfParam('feat')).toBe('feat');
    expect(laneOfParam(undefined)).toBeNull();
    expect(laneView('ab12', null, 'usage')).toEqual({ name: 'usage', params: { stack: 'ab12', lane: 'main' } });
  });

  it('sends an older link to the lane it named, on the same tab', () => {
    expect(oldLaneLink('ab12', '', { lane: 'feat-x' }, null)).toEqual({ path: '/s/ab12/l/feat-x', query: {} });
    expect(oldLaneLink('ab12', 'services/api', { lane: 'feat-x' }, 'other')).toEqual({ path: '/s/ab12/l/feat-x/services/api', query: {} });
    // An empty `lane=` was the main working tree.
    expect(oldLaneLink('ab12', '', { lane: '' }, 'feat-x')).toEqual({ path: '/s/ab12/l/main', query: {} });
    // Other query parameters stay (a command's run).
    expect(oldLaneLink('ab12', 'commands', { lane: 'feat', cmd: 'seed', run: '3' }, null)).toEqual({ path: '/s/ab12/l/feat/commands', query: { cmd: 'seed', run: '3' } });
  });

  it('sends a tab without a lane to the lane shown last, else to main', () => {
    expect(oldLaneLink('ab12', 'usage', {}, 'feat')).toEqual({ path: '/s/ab12/l/feat/usage', query: {} });
    expect(oldLaneLink('ab12', 'infra/data/db', {}, null)).toEqual({ path: '/s/ab12/l/main/infra/data/db', query: {} });
    expect(oldLaneLink('ab12', 'events', {}, undefined)).toEqual({ path: '/s/ab12/l/main/events', query: {} });
  });

  it('leaves the stack page and anything else alone', () => {
    expect(oldLaneLink('ab12', '', {}, 'feat')).toBeNull();
    expect(oldLaneLink('ab12', 'nonsense', {}, 'feat')).toBeNull();
  });
});
