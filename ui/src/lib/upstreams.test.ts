import { describe, expect, it } from 'vitest';
import { ownServerAdvice, ownServerWhere } from './upstreams';
import type { AgentOwnServer } from '@/api/types';

const own: AgentOwnServer = {
  agent: 'claude-code',
  agent_name: 'Claude Code',
  scope: 'user',
  name: 'timetrack',
  url: 'https://timetrack.example/mcp',
  file: '~/.claude.json',
  remove: 'claude mcp remove --scope user timetrack',
};

describe('a pool upstream an agent has itself', () => {
  it('says where, in plain words', () => {
    expect(ownServerWhere(own)).toBe('Already set up directly in Claude Code (for you, in every folder, as “timetrack”).');
    expect(ownServerWhere({ ...own, scope: 'local', folder: 'D:/src/shop' })).toContain('for you, in D:/src/shop');
    expect(ownServerWhere({ ...own, scope: 'project', folder: 'D:/src/shop' })).toContain('in D:/src/shop/.mcp.json');
  });

  it('says what to do by what was made of the offer', () => {
    expect(ownServerAdvice('accepted')).toMatch(/twice/);
    expect(ownServerAdvice('offered')).toMatch(/use it here/);
    expect(ownServerAdvice('offered')).not.toMatch(/ADR/);
  });
});
