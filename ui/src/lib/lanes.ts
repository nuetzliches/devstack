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

/** What a restart of the stack does to its started lanes (ADR-0052). */
export type RestartPlan = {
  /** Lanes on nodes: they keep running there (carried), or stop (an older daemon). */
  onNodes: string[];
  /** Lanes on this machine: they restart with the daemon (carried), or stop. */
  here: string[];
  /** The daemon carries its lanes over; `false`: an older one, which stops them. */
  carried: boolean;
};

export function restartPlan(lanes: LaneView[], carriesLanes: boolean | undefined): RestartPlan {
  const up = lanes.filter(started);
  return { onNodes: up.filter((l) => l.node).map((l) => l.name), here: up.filter((l) => !l.node).map((l) => l.name), carried: !!carriesLanes };
}

/** `Lane a`, `lanes a, b`: capitalised at the start of a sentence. */
const names = (n: string[], first: boolean) => `${first ? 'L' : 'l'}ane${n.length === 1 ? '' : 's'} ${n.join(', ')}`;
const verb = (n: string[], v: string) => (n.length === 1 ? `${v}s` : v);

/** What happens to the lanes, for the restart's confirmation and the version banner; empty without lanes. */
export function restartLanesText(p: RestartPlan): string {
  const all = [...p.onNodes, ...p.here];
  if (!all.length) return '';
  if (!p.carried) return `This daemon is too old to pass its lanes on: ${names(all, false)} ${verb(all, 'stop')} and must be started again.`;
  const parts: string[] = [];
  if (p.onNodes.length) parts.push(`${names(p.onNodes, true)} ${verb(p.onNodes, 'keep')} running on ${p.onNodes.length === 1 ? 'its node' : 'their nodes'}`);
  if (p.here.length) parts.push(`${names(p.here, !parts.length)} ${verb(p.here, 'restart')} with it`);
  return `${parts.join('; ')}.`;
}

/** Of a started lane, its services and its Compose containers: running, and all. */
export type RunCounts = { running: number; services: number; containersRunning: number; containers: number };

/**
 * Whether a started lane runs what it has: its containers count as its
 * services do, since a stack of containers only runs by them. `false`: it
 * has something to run and runs none of it.
 */
export function runsAnything(c: RunCounts): boolean {
  return c.running > 0 || c.containersRunning > 0 || c.services + c.containers === 0;
}

/** `2 of 3 services, 1 of 1 container running`: of what it has. */
export function runCounts(c: RunCounts): string {
  const parts: string[] = [];
  if (c.services > 0 || c.containers === 0) parts.push(`${c.running} of ${c.services} services`);
  if (c.containers > 0) parts.push(`${c.containersRunning} of ${c.containers} container${c.containers === 1 ? '' : 's'}`);
  return `${parts.join(', ')} running`;
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

/**
 * Published ports this machine does not forward (`["5353/udp"]`, a lane on a
 * node, #192) as a sentence, with the daemon's `reason` (ADR-0056); empty for
 * none. Without one, UDP is not relayed by an older devstack.
 */
export function notForwardedText(ports: string[] | undefined, reason?: string): string {
  const list = (ports ?? []).map((p) => {
    const [port, proto = 'udp'] = p.split('/');
    return { port, proto: proto.toUpperCase() };
  });
  if (!list.length) return '';
  const one = list.every((p) => p.proto === list[0].proto);
  const named = one
    ? `${list[0].proto} port${list.length === 1 ? '' : 's'} ${list.map((p) => p.port).join(', ')}`
    : `ports ${list.map((p) => `${p.proto} ${p.port}`).join(', ')}`;
  const why = reason || (list.some((p) => p.proto === 'UDP') ? 'an older devstack forwards TCP only' : 'devstack forwards TCP and UDP only');
  return `${named} ${list.length === 1 ? 'is' : 'are'} not forwarded to this machine (${why})`;
}
