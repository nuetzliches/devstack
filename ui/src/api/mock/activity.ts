// The agents' activity as the mock hub answers `GET /hub/activity` (#482):
// a session in shop's main tree half an hour ago, one in a lane now.
import type { ActivityRecord, ActivityView, HookDetail } from '../types';

const ref = (skill: string, index: number) => ({ pool: 'default', set: 'core', skill, index });

/** The hooks of the mock's skills, as `/hub/skills` describes them. */
export const mockHookDetails: Record<string, HookDetail[]> = {
  'work-session': [
    {
      index: 0,
      event: 'session-start',
      once: 'session',
      upstream: 'timetrack',
      preview: 'Work session: load the skill work-session now, before the first task. It starts the timer. If its tools are not listed yet, search for them…',
      agents: ['Claude Code'],
      applies: true,
    },
    {
      index: 1,
      event: 'prompt-submit',
      pattern: '(?i)\\b(done|wrap up|lane (stop|prune))\\b',
      once: 'match',
      upstream: 'timetrack',
      preview: 'End of the session in sight: if the work is done, stop the timer first, then review the session as the last step.',
      agents: ['Claude Code'],
      applies: true,
    },
  ],
  'work-issue': [
    {
      index: 0,
      event: 'prompt-submit',
      pattern: '(^|\\s)#[0-9]{2,}\\b|github\\.com/[^\\s/]+/[^\\s/]+/(issues|pull)/[0-9]+',
      once: 'match',
      preview: 'The prompt names a ticket or a pull request. Before the first change, load the skill work-issue and read its project file…',
      agents: ['Claude Code'],
      applies: true,
    },
  ],
  'session-end': [
    {
      index: 0,
      event: 'lane-stop',
      once: 'session',
      upstream: 'timetrack',
      preview: 'Lane stopped. If the work of this session ends here: stop the timer, then review the session.',
      agents: ['Claude Code'],
      applies: true,
    },
    {
      index: 1,
      event: 'session-end',
      once: 'session',
      upstream: 'tracker',
      preview: 'The previous session in this working tree ended without closing. Close its timer at its last activity…',
      agents: ['Claude Code'],
      applies: false,
      why: 'the upstream `tracker` is neither accepted nor set up in an agent',
    },
  ],
};

/** Two sessions of Claude Code, with what devstack gave and the tool calls through it. */
export function mockActivity(session?: string): ActivityView {
  const now = Date.now();
  const min = 60_000;
  const main = 'c:/work/shop';
  const lane = 'c:/work/shop-agent-login-fix';
  const at = (m: number) => now - m * min;
  const records: ActivityRecord[] = [
    { kind: 'hook', at: at(95), event: 'session-start', agent: 'claude-code', session: 'a1f3', tree: main, hooks: [ref('work-session', 0)] },
    { kind: 'tool', at: at(94), tool: 'timetrack__start_timer', client: 'claude-code', tree: main, pid: 4100, ms: 180, ok: true, session: 'a1f3', agent: 'claude-code', upstream: 'timetrack', skills: ['work-session', 'session-end'] },
    { kind: 'hook', at: at(93), event: 'prompt-submit', agent: 'claude-code', session: 'a1f3', tree: main, hooks: [ref('work-issue', 0)] },
    { kind: 'tool', at: at(90), tool: 'status', client: 'claude-code', tree: main, pid: 4100, ms: 35, ok: true, session: 'a1f3', agent: 'claude-code' },
    { kind: 'tool', at: at(70), tool: 'timetrack__append_to_timer', client: 'claude-code', tree: main, pid: 4100, ms: 95, ok: true, session: 'a1f3', agent: 'claude-code', upstream: 'timetrack', skills: ['work-session', 'session-end'] },
    { kind: 'hook', at: at(41), event: 'prompt-submit', agent: 'claude-code', session: 'a1f3', tree: main, hooks: [ref('work-session', 1)] },
    { kind: 'tool', at: at(40), tool: 'timetrack__stop_timer', client: 'claude-code', tree: main, pid: 4100, ms: 120, ok: true, session: 'a1f3', agent: 'claude-code', upstream: 'timetrack', skills: ['work-session', 'session-end'] },
    { kind: 'hook', at: at(25), event: 'session-start', agent: 'claude-code', session: 'b7c2', tree: lane, hooks: [ref('work-session', 0)] },
    { kind: 'tool', at: at(24), tool: 'timetrack__start_timer', client: 'claude-code', tree: lane, pid: 5200, ms: 210, ok: true, session: 'b7c2', agent: 'claude-code', upstream: 'timetrack', skills: ['work-session', 'session-end'] },
    { kind: 'hook', at: at(23), event: 'prompt-submit', agent: 'claude-code', session: 'b7c2', tree: lane, hooks: [ref('work-issue', 0)] },
    { kind: 'tool', at: at(18), tool: 'up', client: 'claude-code', tree: lane, pid: 5200, ms: 41_000, ok: true, session: 'b7c2', agent: 'claude-code' },
    { kind: 'hook', at: at(11), event: 'prompt-submit', agent: 'claude-code', session: 'b7c2', tree: lane, hooks: [ref('work-issue', 0)] },
    { kind: 'tool', at: at(9), tool: 'logs', client: 'claude-code', tree: lane, pid: 5200, ms: 60, ok: true, session: 'b7c2', agent: 'claude-code' },
    { kind: 'tool', at: at(6), tool: 'lane_stop', client: 'claude-code', tree: lane, pid: 5200, ms: 2_300, ok: false, session: 'b7c2', agent: 'claude-code' },
    { kind: 'tool', at: at(4), tool: 'lane_stop', client: 'claude-code', tree: lane, pid: 5200, ms: 2_100, ok: true, session: 'b7c2', agent: 'claude-code' },
    { kind: 'hook', at: at(4), event: 'lane-stop', agent: '-', tree: lane, hooks: [ref('session-end', 0)] },
    { kind: 'tool', at: at(2), tool: 'timetrack__append_to_timer', client: 'claude-code', tree: lane, pid: 5200, ms: 88, ok: true, session: 'b7c2', agent: 'claude-code', upstream: 'timetrack', skills: ['work-session', 'session-end'] },
  ];
  const sessions = [
    { agent: 'claude-code', session: 'b7c2', tree: lane, started: at(25), last: at(2), ended: false },
    { agent: 'claude-code', session: 'a1f3', tree: main, started: at(95), last: at(40), ended: true },
  ];
  return { sessions, records: session ? records.filter((r) => (r.kind === 'hook' ? r.session === session || (!r.session && r.tree === sessions.find((s) => s.session === session)?.tree) : r.session === session)) : records };
}
