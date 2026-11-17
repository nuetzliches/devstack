// The start page's figures of a stack: the state and its rank, the services,
// the lanes, since when it runs, the last commit and what needs attention.
import { describe, expect, it } from 'vitest';
import type { AttentionItem, StackEntry, StackGlance } from '@/api/types';
import { commitAge } from './format';
import { stackFigures } from './stackFigures';
import type { useHub } from '@/stores/hub';

type Hub = ReturnType<typeof useHub>;

const stack = (more: Partial<StackEntry> = {}): StackEntry => ({
  id: 'ab12',
  name: 'shop',
  file: '/src/shop/devstack.toml',
  repo: '/src/shop',
  running: false,
  daemon: null,
  last_seen: null,
  ...more,
});

const hubOf = (more: { reachable?: boolean; glances?: Record<string, StackGlance>; attention?: AttentionItem[]; starting?: Record<string, boolean>; stopping?: Record<string, boolean> } = {}): Hub =>
  ({ reachable: true, glances: {}, attention: [], starting: {}, stopping: {}, ...more }) as unknown as Hub;

const item = (level: 'error' | 'warn', stackId = 'ab12'): AttentionItem => ({ stack: stackId, lane: null, service: 'api', text: 'api: failed', detail: '', level });

describe('stackFigures', () => {
  it('says stopped for a stack that does not run, and running for one that does', () => {
    expect(stackFigures(stack(), hubOf()).look).toMatchObject({ label: 'Stopped', rank: 1 });
    expect(stackFigures(stack({ running: true }), hubOf()).look).toMatchObject({ label: 'Running', tone: 'ok', rank: 3 });
  });

  it('ranks what needs attention the most urgent, and an unreachable hub the least', () => {
    const glances = { ab12: { id: 'ab12', state: 'attention' as const, ready: 1, total: 2, lanes: 0 } };
    const a = stackFigures(stack({ running: true }), hubOf({ glances, attention: [item('error')] })).look;
    expect(a).toMatchObject({ label: 'Needs attention', tone: 'danger', rank: 5 });
    expect(stackFigures(stack({ running: true }), hubOf({ glances, attention: [item('warn')] })).look.tone).toBe('warn');
    expect(stackFigures(stack(), hubOf({ reachable: false })).look).toMatchObject({ label: 'Unknown', rank: 0 });
  });

  it('shows a start or a stop the page asked for before the hub says it', () => {
    expect(stackFigures(stack(), hubOf({ starting: { ab12: true } })).look).toMatchObject({ label: 'Starting', busy: true });
    expect(stackFigures(stack({ running: true }), hubOf({ stopping: { ab12: true } })).look).toMatchObject({ label: 'Stopping', busy: true });
  });

  it('has services, lanes and the start of the daemon only while the stack runs', () => {
    const glances = { ab12: { id: 'ab12', state: 'ok' as const, ready: 2, total: 4, lanes: 1 } };
    const up = stackFigures(stack({ running: true, daemon: { pid: 1, port: 7000, started: 1000, version: '0.9.8' } }), hubOf({ glances }));
    expect(up).toMatchObject({ services: { ready: 2, total: 4, share: 0.5 }, lanes: 1, upSince: 1000 });
    expect(stackFigures(stack(), hubOf({ glances }))).toMatchObject({ services: null, lanes: null, upSince: null });
  });

  it('keeps the stack’s own items and its commit time in milliseconds', () => {
    const f = stackFigures(stack({ last_commit: 100 }), hubOf({ attention: [item('error'), item('warn', 'other')] }));
    expect(f.items).toHaveLength(1);
    expect(f.committedAt).toBe(100_000);
    expect(stackFigures(stack(), hubOf()).committedAt).toBeNull();
  });
});

describe('commitAge', () => {
  const now = Date.UTC(2026, 10, 17, 12, 0, 0);
  it('says it as ago does within a day, in days after, and nothing while unknown', () => {
    expect(commitAge(null, now)).toBeNull();
    expect(commitAge(now - 40 * 60_000, now)).toBe('40 min ago');
    expect(commitAge(now - 3 * 86_400_000, now)).toBe('3 d ago');
  });
});
