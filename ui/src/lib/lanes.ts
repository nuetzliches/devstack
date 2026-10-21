// Lanes as the dashboard shows them (plan §3.5, ADR-0024): a lane is a git
// worktree, so it is named by its branch; its name (from the directory) is
// what the API and `{lane}` use.
import type { LaneView } from '@/api/types';

/** The branch, else the lane's name for a detached HEAD. */
export function laneLabel(l: LaneView): string {
  return l.branch ?? `${l.name} (detached)`;
}

/** Where a lane on a node runs: the node, with its pool unless the default one (ADR-0034). */
export function nodeLabel(l: LaneView): string {
  return l.pool && l.pool !== 'default' ? `${l.pool}:${l.node}` : (l.node ?? '');
}

/** Not started: a worktree that runs nothing yet. Older daemons omit `active` (all started). */
export function started(l: LaneView): boolean {
  return l.active !== false;
}

/**
 * Started lanes first (the one serving the main ports on top), then the
 * worktrees not started; each by label. Agents open many worktrees: the ones
 * that run are what matters.
 */
export function orderLanes(lanes: LaneView[]): { running: LaneView[]; idle: LaneView[] } {
  const byLabel = (a: LaneView, b: LaneView) => laneLabel(a).localeCompare(laneLabel(b));
  const running = lanes.filter(started).sort((a, b) => Number(b.owns_main) - Number(a.owns_main) || byLabel(a, b));
  const idle = lanes.filter((l) => !started(l)).sort(byLabel);
  return { running, idle };
}

/**
 * Two spellings of one folder: git lists `D:/src/app`, the hub answers
 * `D:\src\app`. Paths with a drive letter compare without case, as Windows
 * does.
 */
export function samePath(a: string, b: string): boolean {
  const norm = (p: string) => {
    const s = p.replace(/\\/g, '/').replace(/\/+$/, '');
    return /^[a-z]:/i.test(s) ? s.toLowerCase() : s;
  };
  return norm(a) === norm(b);
}
