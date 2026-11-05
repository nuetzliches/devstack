// Several stacks on one machine (ADR-0082 points 3 to 7): a stopped stack
// whose ports a running stack holds does not start (`devstack up` refuses
// and names it); the stack page offers to start it with a port shift. A
// stopped stack's containers that run all the same hold ports too
// (ADR-0096): the page offers to remove them.
import type { PortOverlap, StackEntry, StackPorts } from '@/api/types';

/**
 * A stack that holds a port the stack `id` uses too; `containers`: its
 * daemon does not run, its containers hold them (ADR-0096).
 */
export type Holder = { id: string; name: string; ports: number[]; containers: boolean };

/**
 * The stacks in `overlaps` (`GET /hub/ports`) that hold a port of the stack
 * `id`: running ones, and stopped ones whose containers run (`stacks` says
 * which). A stopped stack's other ports are `declared`: its collision only
 * warns, so it is not listed.
 */
export function runningHolders(id: string, overlaps: PortOverlap[], stacks: StackPorts[] = []): Holder[] {
  const by = new Map<string, Holder>();
  for (const o of overlaps) {
    if (!o.stacks.some((u) => u.id === id)) continue;
    for (const u of o.stacks) {
      if (u.id === id || u.kind === 'declared' || u.kind === 'instance') continue;
      const containers = stacks.find((s) => s.id === u.id)?.running === false;
      const h = by.get(u.id) ?? { id: u.id, name: u.name, ports: [], containers };
      if (!h.ports.includes(o.port)) h.ports.push(o.port);
      by.set(u.id, h);
    }
  }
  return [...by.values()];
}

/** The last start failed because a running stack holds its ports (`devstack up` offers `--shift`). */
export function refusedForPorts(entry: Pick<StackEntry, 'start_error'> | null | undefined): boolean {
  return !!entry?.start_error?.includes('--shift auto');
}

/** "shifted +1000", or null for a stack on its own ports. */
export function shiftLabel(shift: number | undefined): string | null {
  return shift ? `shifted +${shift}` : null;
}
