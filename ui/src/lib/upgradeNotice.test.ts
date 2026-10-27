import { describe, expect, it } from 'vitest';
import type { SkillsState } from '@/api/types';
import { minor, skillsState } from './upgradeNotice';

describe('the upgrade notice', () => {
  it('comes back with a minor release, not a patch', () => {
    expect(minor('0.25.0')).toBe('0.25');
    expect(minor('0.25.1')).toBe('0.25');
    expect(minor('0.25.0-rc.1')).toBe('0.25');
    expect(minor('0.26.0')).toBe('0.26');
  });

  it('tells why the skills are outdated', () => {
    const s = (over: Partial<SkillsState>) => ({ outdated: ['.claude/skills/devstack'], from: '0.24.1', current: '0.25.0', ...over }) as SkillsState;
    expect(skillsState(null)).toBeNull();
    expect(skillsState(s({ outdated: [] }))).toBeNull();
    expect(skillsState(s({ ahead: true }))).toBe('ahead');
    expect(skillsState(s({}))).toBe('older');
    expect(skillsState(s({ from: '0.25.0' }))).toBe('file');
  });
});
