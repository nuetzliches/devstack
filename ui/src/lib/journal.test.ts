import { describe, expect, it } from 'vitest';
import type { JournalEntry } from '@/api/types';
import { forTree, journalLabel, journalSubject, sameEntry, stackWide, viaText } from './journal';

const e = (path: string, more: Partial<JournalEntry> = {}): JournalEntry => ({ at: 1, via: 'dashboard', method: 'POST', path, status: 200, ...more });

describe('journal', () => {
  it('labels the mutating routes, a lane’s too', () => {
    expect(journalLabel(e('/v1/services/api/stop'))).toBe('stop api');
    expect(journalLabel(e('/v1/services/web/hot-reload-on'))).toBe('hot reload on web');
    expect(journalLabel(e('/v1/lanes/feat/v1/services/api/restart', { lane: 'feat' }))).toBe('restart api');
    expect(journalLabel(e('/v1/infra/db/postgres/restart'))).toBe('restart container db.postgres');
    expect(journalLabel(e('/v1/commands/seed'))).toBe('run seed');
    expect(journalLabel(e('/v1/lanes/feat/main', { lane: 'feat' }))).toBe('lane feat takes the main ports');
    expect(journalLabel(e('/v1/lanes/main/main', { method: 'AUTO', via: 'auto' }))).toBe('main ports back to the main tree');
    expect(journalLabel(e('/v1/services/a%20b/start'))).toBe('start a b');
    expect(journalLabel(e('/v1/lanes/feat/restart'))).toBe('restart lane feat');
    expect(journalLabel(e('/v1/groups/backend/rebuild'))).toBe('rebuild group backend');
  });

  it('shows a route it does not know as it came', () => {
    expect(journalLabel(e('/v1/something/new'))).toBe('POST /v1/something/new');
    expect(journalLabel(e('/v1/up', { method: 'DELETE' }))).toBe('DELETE /v1/up');
  });

  it('finds what it acted on and says the client', () => {
    expect(journalSubject(e('/v1/lanes/x/v1/services/api/promote'))).toBe('api');
    expect(journalSubject(e('/v1/store/web/1cb852a0a8ab', { method: 'DELETE' }))).toBe('web');
    expect(journalSubject(e('/v1/groups/backend/rebuild'))).toBe('backend');
    expect(journalSubject(e('/v1/infra/db/postgres/restart'))).toBe('db');
    expect(journalSubject(e('/v1/commands/seed'))).toBe('seed');
    expect(journalSubject(e('/v1/infra/check'))).toBeNull();
    expect(journalSubject(e('/v1/up'))).toBeNull();
    expect(viaText('auto')).toBe('by devstack');
    expect(viaText('cli')).toBe('via the CLI');
  });

  it('keeps for a lane its own, and the stack’s apart', () => {
    const main = e('/v1/up');
    const lane = e('/v1/lanes/feat/v1/up', { lane: 'feat' });
    const prune = e('/v1/lanes/prune');
    const started = e('/v1/lanes', { lane: 'feat' });
    const portsBack = e('/v1/lanes/main/main', { lane: 'main', method: 'AUTO', via: 'auto' });
    expect([main, lane, prune, started, portsBack].map((x) => forTree(x, null, false))).toEqual([true, false, false, false, true]);
    expect([main, lane, prune, started, portsBack].map((x) => forTree(x, 'feat', false))).toEqual([false, true, false, true, false]);
    expect(forTree(lane, 'other', false)).toBe(false);
    expect(forTree(main, 'feat', true)).toBe(true);
    expect([main, lane, prune, started, e('/v1/lanes'), e('/v1/shutdown')].map(stackWide)).toEqual([false, false, true, false, true, true]);
    expect(sameEntry(main, { ...main })).toBe(true);
    expect(sameEntry(main, { ...main, at: 2 })).toBe(false);
  });
});
