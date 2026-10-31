import { describe, expect, it } from 'vitest';
import type { PortOverlap } from '@/api/types';
import { refusedForPorts, runningHolders, shiftLabel } from './shift';

describe('port shift (ADR-0082)', () => {
  const overlaps: PortOverlap[] = [
    {
      port: 4200,
      proto: 'tcp',
      stacks: [
        { id: 'docs', name: 'docs', lane: 'main', holder: 'site', kind: 'declared' },
        { id: 'shop', name: 'shop', lane: 'main', holder: 'web', kind: 'fixed' },
      ],
    },
    {
      port: 8090,
      proto: 'tcp',
      stacks: [
        { id: 'docs', name: 'docs', lane: 'main', holder: 'search-api', kind: 'declared' },
        { id: 'blog', name: 'blog', lane: 'main', holder: 'api', kind: 'declared' },
      ],
    },
  ];

  it('names the running stacks that hold a port of a stack, not the stopped ones', () => {
    expect(runningHolders('docs', overlaps)).toEqual([{ name: 'shop', ports: [4200] }]);
    expect(runningHolders('blog', overlaps)).toEqual([]);
    expect(runningHolders('crm', overlaps)).toEqual([]);
  });

  it('tells a start refused for its ports and labels a shift', () => {
    expect(refusedForPorts({ start_error: 'Nothing started.\n  devstack up --shift auto' })).toBe(true);
    expect(refusedForPorts({ start_error: 'port 4000 is taken' })).toBe(false);
    expect(refusedForPorts(null)).toBe(false);
    expect(shiftLabel(1000)).toBe('shifted +1000');
    expect(shiftLabel(0)).toBeNull();
    expect(shiftLabel(undefined)).toBeNull();
  });
});
