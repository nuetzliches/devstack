import { describe, expect, it } from 'vitest';
import type { PoolSkillsState } from '@/api/types';
import { counts, filtered, groups, NO_FILTER, rows } from './skills';

const state: PoolSkillsState = {
  agents: [],
  conflicts: [{ dir: '/h/.claude/skills', name: 'push-tag', reason: 'mine' }],
  sets: [
    {
      pool: 'default',
      set: 'core',
      source: 'github:o/r@main/skills',
      state: 'accepted',
      auto: true,
      skills: [
        { name: 'work-issue', description: 'Work on an issue', sha256: 'a', installed: true, pending: false, enabled: true, group: 'Dev', tags: ['git'] },
        { name: 'push-tag', description: 'Tag a release', sha256: 'b', installed: false, pending: false, enabled: true, group: 'Dev' },
        { name: 'timetrack', description: 'Book time', sha256: 'c', installed: false, pending: true, enabled: true },
        { name: 'biz', description: 'Validate an idea', sha256: 'd', installed: false, pending: false, enabled: false, group: 'Business' },
      ],
    },
    { pool: 'default', set: 'shop', source: 'github:o/shop@main', state: 'offered', auto: false, skills: [{ name: 'shop-feature', description: 'x', sha256: 'e', installed: false, pending: false }] },
  ],
};

describe('the skills list', () => {
  it('gives each skill a status', () => {
    const r = rows(state);
    expect(r.map((x) => [x.skill.name, x.status])).toEqual([
      ['work-issue', 'installed'],
      ['push-tag', 'conflict'],
      ['timetrack', 'pending'],
      ['biz', 'off'],
      ['shop-feature', 'not-accepted'],
    ]);
    expect(counts(r)).toEqual({ installed: 1, conflict: 1, pending: 1, off: 1, 'not-accepted': 1 });
  });

  it('filters by text (tags too), set, group and status', () => {
    const r = rows(state);
    const names = (f: Partial<typeof NO_FILTER>) => filtered(r, { ...NO_FILTER, ...f }).map((x) => x.skill.name);
    expect(names({ text: 'GIT' })).toEqual(['work-issue']);
    expect(names({ set: 'default:shop' })).toEqual(['shop-feature']);
    expect(names({ group: 'Dev' })).toEqual(['work-issue', 'push-tag']);
    expect(names({ group: '' })).toEqual(['timetrack', 'shop-feature']);
    expect(names({ status: 'off' })).toEqual(['biz']);
  });

  it('lists the groups, the skills without one last', () => {
    expect(groups(rows(state))).toEqual(['Dev', 'Business', '']);
  });
});
