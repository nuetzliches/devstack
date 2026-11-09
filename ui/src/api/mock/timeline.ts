// A session in time as the mock hub answers `GET /hub/activity/timeline`:
// built from the mock's activity, with turns, built-in calls and marks as an
// agent's transcript gives them. Seeded, so it looks the same on each load.
import type { ActivityRecord, SessionTimeline, TimelineCall, TimelineMark, TimelineTurn } from '../types';
import { mockActivity } from './activity';

function rng(seed: number) {
  let s = seed;
  return () => {
    s = (s * 1664525 + 1013904223) % 2 ** 32;
    return s / 2 ** 32;
  };
}

const BUILTIN = ['Read', 'Read', 'Grep', 'Bash', 'Edit', 'Edit', 'Glob', 'Bash', 'Write'];

/** Whether the mock reads transcripts: on, so the view shows everything; `PUT` switches it. */
export const mockTranscripts = { enabled: true };

export function mockTimeline(session: string): SessionTimeline | null {
  const view = mockActivity();
  const s = view.sessions.find((x) => x.session === session);
  if (!s) return null;
  const records = view.records.filter((r) => r.session === session || (r.kind === 'hook' && !r.session && r.tree === s.tree && r.at >= s.started && r.at <= s.last + 60_000));
  const hooks = records.filter((r): r is Extract<ActivityRecord, { kind: 'hook' }> => r.kind === 'hook');
  const ownCalls: TimelineCall[] = records
    .filter((r): r is Extract<ActivityRecord, { kind: 'tool' }> => r.kind === 'tool')
    .map((r) => ({ at: r.at - r.ms, ms: r.ms, tool: r.tool, kind: r.upstream ? 'upstream' : 'devstack', ok: r.ok, ...(r.upstream ? { upstream: r.upstream } : {}) }));
  const base = { ...s, hooks };
  if (!mockTranscripts.enabled) return { ...base, source: 'activity', why: 'off', turns: [], calls: ownCalls, marks: [] };

  // Prompts: at the session's start, and wherever a prompt's hook fired; a
  // pause before each but the first (the agent waited for the user).
  const rand = rng(session.charCodeAt(0) * 97 + session.length);
  const prompts = [...new Set([s.started + 5_000, ...hooks.filter((h) => h.event === 'prompt-submit').map((h) => h.at)])].sort((a, b) => a - b);
  const marks: TimelineMark[] = prompts.map((at) => ({ at, kind: 'prompt' }));
  const turns: TimelineTurn[] = [];
  const calls: TimelineCall[] = [...ownCalls];
  let ctx = 18_000;
  const end = s.ended ? s.last : Date.now() - 20_000;
  prompts.forEach((p, i) => {
    // The agent works for a while after each prompt, then waits.
    const until = Math.min(prompts[i + 1] ?? end, p + (i === prompts.length - 1 ? end - p : 2 * 60_000 + rand() * 6 * 60_000));
    let t = p + 2_000;
    while (t < until) {
      const output = Math.round(80 + rand() * (rand() < 0.15 ? 2400 : 600));
      const write = Math.round(200 + rand() * 2500);
      turns.push({ at: t, model: 'claude-opus', input: 2 + Math.round(rand() * 6), cache_write: write, cache_read: ctx, output });
      ctx += write;
      // Most turns call a built-in tool; now and then a sub-agent.
      if (rand() < 0.06) {
        const ms = 20_000 + rand() * 50_000;
        calls.push({ at: t + 400, ms: Math.round(ms), tool: 'Agent', kind: 'agent', ok: true });
        for (let k = 0; k < 4; k++) turns.push({ at: t + 3_000 + k * (ms / 5), model: 'claude-haiku', input: 5, cache_write: 900, cache_read: 4_000 + k * 900, output: 300, sidechain: true });
        t += ms + 2_000;
        continue;
      }
      if (rand() < 0.85) {
        const tool = BUILTIN[Math.floor(rand() * BUILTIN.length)];
        const ms = tool === 'Bash' ? 800 + rand() * 25_000 : 40 + rand() * 600;
        calls.push({ at: t + 300, ms: Math.round(ms), tool, kind: 'builtin', ok: rand() > 0.06 });
        t += ms + 1_500 + rand() * 9_000;
      } else t += 4_000 + rand() * 8_000;
    }
  });
  // The context grew past what fits once: compacted.
  if (turns.length > 30) {
    const at = turns[Math.floor(turns.length * 0.6)].at - 500;
    marks.push({ at, kind: 'compact' });
    let shrink = 0;
    for (const u of turns) {
      if (u.at < at || u.sidechain) continue;
      if (!shrink) shrink = Math.round(u.cache_read * 0.7);
      u.cache_read = Math.max(12_000, u.cache_read - shrink);
    }
  }
  if (prompts.length > 2) marks.push({ at: prompts[1] - 40_000, kind: 'interrupt' });
  calls.sort((a, b) => a.at - b.at);
  marks.sort((a, b) => a.at - b.at);
  return { ...base, source: 'transcript', turns, calls, marks };
}
