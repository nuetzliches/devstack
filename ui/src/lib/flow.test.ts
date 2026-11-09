// The Skills page's flow (#482): hooks by phase, grouped by pattern, and the
// activity tied to them.
import { describe, expect, it } from 'vitest';
import type { PoolSkillsState } from '@/api/types';
import { mockActivity, mockHookDetails } from '@/api/mock/activity';
import { firings, flowNodes, phases, recordText, refKey, toolsByUpstream } from './flow';

const skill = (name: string, extra: Record<string, unknown> = {}) => ({
  name,
  description: '',
  sha256: name,
  installed: true,
  pending: false,
  enabled: true,
  hooks: mockHookDetails[name]?.length,
  hook_details: mockHookDetails[name],
  ...extra,
});

const state: PoolSkillsState = {
  agents: [],
  sets: [
    { pool: 'default', set: 'core', source: 'x', state: 'accepted', auto: true, skills: [skill('work-session'), skill('work-issue'), skill('session-end'), skill('quiet', { enabled: false })] },
    { pool: 'default', set: 'other', source: 'y', state: 'offered', auto: false, skills: [skill('work-issue')] },
  ],
  conflicts: [],
};

describe('the flow', () => {
  it('puts each hook of a skill switched on of an accepted set into its phase, a colour per skill', () => {
    const nodes = flowNodes(state);
    expect(nodes.map((n) => n.key)).toEqual([
      'default/core/work-session#0',
      'default/core/work-session#1',
      'default/core/work-issue#0',
      'default/core/session-end#0',
      'default/core/session-end#1',
    ]);
    expect(new Set(nodes.filter((n) => n.skill === 'work-session').map((n) => n.color)).size).toBe(1);
    expect(nodes.find((n) => n.skill === 'work-issue')!.color).not.toBe(nodes[0].color);
    const p = phases(nodes);
    expect(p.map((x) => x.id)).toEqual(['start', 'prompt', 'lane-stop', 'end']);
    // The prompt's hooks by pattern: closing words and ticket references apart.
    const prompt = p.find((x) => x.id === 'prompt')!;
    expect(prompt.groups.map((g) => g.nodes.map((n) => n.skill))).toEqual([['work-session'], ['work-issue']]);
    expect(prompt.groups.every((g) => g.pattern)).toBe(true);
  });

  it('ties a hook to its firings', () => {
    const { records } = mockActivity();
    const nodes = flowNodes(state);
    const start = nodes.find((n) => n.key === 'default/core/work-session#0')!;
    expect(firings(records, start.key)).toHaveLength(2);
  });

  it('groups the tool calls by upstream, each call once however many skills name it, the rest last', () => {
    const records = mockActivity().records;
    const groups = toolsByUpstream(records);
    expect(groups.at(-1)!.upstream).toBeNull();
    expect(groups.at(-1)!.skills).toEqual([]);
    expect(groups.at(-1)!.tools.find((t) => t.tool === 'lane_stop')).toEqual({ tool: 'lane_stop', calls: 2, failed: 1 });
    const counted = groups.flatMap((g) => g.tools).reduce((n, t) => n + t.calls, 0);
    expect(counted).toBe(records.filter((r) => r.kind === 'tool').length);
    const tt = groups.find((g) => g.tools.some((t) => t.tool === 'timetrack__append_to_timer'))!;
    expect(tt.skills.length).toBeGreaterThan(1);
    expect(tt.tools.find((t) => t.tool === 'timetrack__append_to_timer')!.calls).toBe(2);
  });

  it('says what happened, never more', () => {
    const r = mockActivity().records.find((x) => x.kind === 'hook' && x.event === 'prompt-submit')!;
    const t = recordText(r, () => undefined);
    expect(t.title).toBe('Hook: work-issue');
    expect(refKey({ pool: 'p', set: 's', skill: 'k', index: 2 })).toBe('p/s/k#2');
  });
});
