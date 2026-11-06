// The Skills page's flow (#482): the hooks of the skills switched on, by when
// they give their text, and what the agents' activity says about them. Pure,
// so the page and its tests share it.
import type { ActivityRecord, ActivitySession, HookDetail, HookEvent, HookRef, PoolSkillsState } from '@/api/types';

/** A hook in the flow. */
export type FlowNode = {
  key: string;
  pool: string;
  set: string;
  skill: string;
  hook: HookDetail;
  /** Its skill's colour, 0 to 5. */
  color: number;
};

export type PhaseId = 'start' | 'prompt' | 'lane-stop' | 'end';

/** A phase of a session and the hooks that give their text in it. */
export type Phase = {
  id: PhaseId;
  title: string;
  trigger: string;
  /** The prompt's hooks, by pattern; the others' in one group without one. */
  groups: { pattern: string | null; nodes: FlowNode[] }[];
};

export const COLORS = 6;

/** A hook's key: where it is. */
export const refKey = (r: HookRef) => `${r.pool}/${r.set}/${r.skill}#${r.index}`;

/** The hooks of every skill switched on of an accepted set, each skill in its own colour. */
export function flowNodes(state: PoolSkillsState | null): FlowNode[] {
  if (!state) return [];
  const colors = new Map<string, number>();
  const out: FlowNode[] = [];
  for (const s of state.sets) {
    if (s.state !== 'accepted') continue;
    for (const k of s.skills) {
      if (k.enabled === false || !k.hook_details?.length) continue;
      if (!colors.has(k.name)) colors.set(k.name, colors.size % COLORS);
      for (const hook of k.hook_details) {
        out.push({ key: refKey({ pool: s.pool, set: s.set, skill: k.name, index: hook.index }), pool: s.pool, set: s.set, skill: k.name, hook, color: colors.get(k.name)! });
      }
    }
  }
  return out;
}

const PHASES: { id: PhaseId; event: HookEvent; title: string; trigger: string }[] = [
  { id: 'start', event: 'session-start', title: 'A session starts', trigger: 'every new session, and again after its context was cleared' },
  { id: 'prompt', event: 'prompt-submit', title: 'A prompt matches', trigger: 'you send a prompt that matches the hook\'s pattern' },
  { id: 'lane-stop', event: 'lane-stop', title: 'A lane stops', trigger: 'devstack lane stop, or the agent\'s lane_stop: the text follows its output' },
  { id: 'end', event: 'session-end', title: 'A session ends', trigger: 'the agent ends the session: devstack notes it, the next session in the same working tree gets the text' },
];

/** The four phases with their hooks; a phase without any is left out. */
export function phases(nodes: FlowNode[]): Phase[] {
  const out: Phase[] = [];
  for (const p of PHASES) {
    const mine = nodes.filter((n) => n.hook.event === p.event);
    if (!mine.length) continue;
    const groups: Phase['groups'] = [];
    for (const n of mine) {
      const pattern = p.event === 'prompt-submit' ? (n.hook.pattern ?? null) : null;
      const g = groups.find((x) => x.pattern === pattern);
      if (g) g.nodes.push(n);
      else groups.push({ pattern, nodes: [n] });
    }
    out.push({ id: p.id, title: p.title, trigger: p.trigger, groups });
  }
  return out;
}

/** How often a hook gives its text, in words. */
export function onceText(h: HookDetail): string {
  if (h.event === 'lane-stop') return 'on every stop';
  if (h.event === 'session-end') return 'once, at the next start';
  return h.once === 'match' ? 'once a session for each match' : 'once a session';
}

/** What a hook waits for, in words. */
export function conditionText(h: HookDetail): string {
  const parts: string[] = [];
  if (h.upstream) parts.push(`the upstream ${h.upstream} is there (accepted, or set up in the agent)`);
  if (h.pattern) parts.push('the prompt matches its pattern');
  return parts.length ? parts.join('; ') : 'none';
}

/** The hook records that gave `key`, newest first. */
export function firings(records: ActivityRecord[], key: string): Extract<ActivityRecord, { kind: 'hook' }>[] {
  return records.filter((r): r is Extract<ActivityRecord, { kind: 'hook' }> => r.kind === 'hook' && r.hooks.some((h) => refKey(h) === key)).sort((a, b) => b.at - a.at);
}

/** The records that belong to a hook: its firings, and the tool calls of its skill's upstream (or a lane's stop, for `lane-stop`). All without one. Newest first. */
export function recordsFor(records: ActivityRecord[], node: FlowNode | null): ActivityRecord[] {
  const all = [...records].sort((a, b) => b.at - a.at);
  if (!node) return all;
  return all.filter((r) => {
    if (r.kind === 'hook') return r.hooks.some((h) => refKey(h) === node.key);
    if (node.hook.event === 'lane-stop' && r.tool === 'lane_stop') return true;
    return !!r.skills?.includes(node.skill);
  });
}

/** The tool calls, by the skill whose upstream they use (`null`: none), each tool with how often and how often it failed. */
export function toolsBySkill(records: ActivityRecord[]): { skill: string | null; tools: { tool: string; calls: number; failed: number }[] }[] {
  const out: { skill: string | null; tools: { tool: string; calls: number; failed: number }[] }[] = [];
  for (const r of records) {
    if (r.kind !== 'tool') continue;
    for (const skill of r.skills?.length ? r.skills : [null]) {
      let g = out.find((x) => x.skill === skill);
      if (!g) out.push((g = { skill, tools: [] }));
      let t = g.tools.find((x) => x.tool === r.tool);
      if (!t) g.tools.push((t = { tool: r.tool, calls: 0, failed: 0 }));
      t.calls++;
      if (!r.ok) t.failed++;
    }
  }
  // Tied to a skill first; the rest last.
  return out.sort((a, b) => (a.skill === null ? 1 : 0) - (b.skill === null ? 1 : 0));
}

/** A session in a line: its working tree's last folder, the agent, when it started. */
export function sessionLabel(s: ActivitySession): string {
  const tree = s.tree.split('/').filter(Boolean).pop() ?? s.tree;
  return `${tree} · ${s.agent} · ${new Date(s.started).toLocaleString()}`;
}

/** A record in words, for the activity list. */
export function recordText(r: ActivityRecord, skillOf: (key: string) => string | undefined): { title: string; detail: string } {
  if (r.kind === 'tool') {
    return { title: r.tool, detail: `${r.ms} ms · ${r.ok ? 'ok' : 'failed'}${r.upstream ? ` · upstream ${r.upstream}` : ''}` };
  }
  const event: Record<HookEvent, string> = {
    'session-start': 'session start',
    'prompt-submit': 'prompt',
    'lane-stop': 'lane stop',
    'session-end': 'session end',
  };
  const names = [...new Set(r.hooks.map((h) => skillOf(refKey(h)) ?? h.skill))];
  if (r.recorded) return { title: `End noted: ${names.join(', ')}`, detail: 'given at the next start in this working tree' };
  if (!names.length) return { title: `Hooks: nothing to give`, detail: event[r.event] };
  return { title: `Hook: ${names.join(', ')}`, detail: `${event[r.event]} · text given` };
}
