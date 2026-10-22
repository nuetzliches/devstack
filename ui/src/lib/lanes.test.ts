import { describe, expect, it } from 'vitest';
import { runCounts, runsAnything } from './lanes';

describe('a started lane runs', () => {
  it('by its containers when the stack has no services', () => {
    const c = { running: 0, services: 0, containersRunning: 3, containers: 3 };
    expect(runsAnything(c)).toBe(true);
    expect(runCounts(c)).toBe('3 of 3 containers running');
  });

  it('nothing when it has services and containers and none of them runs', () => {
    const c = { running: 0, services: 2, containersRunning: 0, containers: 1 };
    expect(runsAnything(c)).toBe(false);
    expect(runCounts(c)).toBe('0 of 2 services, 0 of 1 container running');
  });

  it('as before without containers (an older daemon omits them)', () => {
    expect(runsAnything({ running: 1, services: 3, containersRunning: 0, containers: 0 })).toBe(true);
    expect(runCounts({ running: 1, services: 3, containersRunning: 0, containers: 0 })).toBe('1 of 3 services running');
    expect(runsAnything({ running: 0, services: 0, containersRunning: 0, containers: 0 })).toBe(true);
  });
});
