import { describe, expect, it } from 'vitest';
import { minor } from './upgradeNotice';

describe('the upgrade notice', () => {
  it('comes back with a minor release, not a patch', () => {
    expect(minor('0.25.0')).toBe('0.25');
    expect(minor('0.25.1')).toBe('0.25');
    expect(minor('0.25.0-rc.1')).toBe('0.25');
    expect(minor('0.26.0')).toBe('0.26');
  });
});
