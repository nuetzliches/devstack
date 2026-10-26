import { describe, expect, it } from 'vitest';
import { prepareLine, shortTime, storeLine, treeLabel } from './store';

describe('the store of what prepare makes', () => {
  it('says where the outputs came from', () => {
    expect(prepareLine({ source: 'store', at: 0, ms: 2400, key: 'abc', prepare_ms: 160_000 })).toBe('from the store, copied in 2.4s; prepare took 2m 40s when it made them');
    expect(prepareLine({ source: 'run', at: 0, ms: 160_000, key: 'abc', miss: 'no entry for these inputs on this machine', stored: true })).toBe(
      'prepare ran, 2m 40s (store miss: no entry for these inputs on this machine); kept in the store',
    );
    expect(prepareLine({ source: 'run', at: 0, ms: 3000, miss: 'its `version` (`node --version`) failed' })).toBe('prepare ran, 3.0s (store miss: its `version` (`node --version`) failed)');
    expect(prepareLine({ source: 'run', at: 0, ms: 3000 })).toBe('prepare ran, 3.0s (it names no outputs: no store)');
  });

  it('sums the store up', () => {
    expect(storeLine({ entries: [], bytes: 0 })).toBe('empty');
    const e = { service: 'web', key: 'k', outputs: [], bytes: 1536, files: 1, created: 0, last_used: 0, prepare_ms: 0, used_by: [] };
    expect(storeLine({ entries: [e], bytes: 1536 })).toBe('1 entry, 1.5 KB');
    expect(storeLine({ entries: [e, e], bytes: 3 * 1024 ** 3 })).toBe('2 entries, 3.0 GB');
  });

  it('names times and trees', () => {
    expect(shortTime(-5)).toBe('0.0s');
    expect(shortTime(65_000)).toBe('1m 5s');
    expect(treeLabel('')).toBe('main tree');
    expect(treeLabel('fix-login')).toBe('fix-login');
  });
});
