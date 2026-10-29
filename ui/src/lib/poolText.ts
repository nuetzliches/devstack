// What the pool area says about its parts, shared by its tabs and the
// machine page.
import type { NodePolicy, PoolNode, PoolRelease } from '@/api/types';

export const kindText: Record<string, string> = { server: 'server', node: 'node', machine: 'developer machine', unknown: 'not seen yet' };
export const kindTitle: Record<string, string> = {
  server: 'Runs lanes for the pool and never reclaims them (a dedicated node)',
  node: 'Runs lanes for the pool; its owner may reclaim them',
  machine: 'A developer machine: asks for lanes, may share',
  unknown: 'Has not reported to the coordinator yet',
};

export const actionText: Record<string, string> = {
  approve: 'approved',
  reject: 'rejected the request of',
  remove: 'removed',
  role: 'changed the role of',
  pause: 'paused',
  resume: 'resumed',
  end: 'ended a lane on',
  tags: 'changed the tags of',
  policy: 'changed the policy of',
  node: 'changed whether lanes run on',
  provision: 'changed whether lanes cause provisioning for',
  confirm: 'confirmed the provisioned node',
  enrol: 'saw the provisioned node enrol:',
  upstream: 'named an MCP upstream for the pool',
  'upstream-remove': "took an MCP upstream off the pool's list",
};

// Which devstack a node or the coordinator runs, beside this machine's (the
// hub's): another protocol does not work together, another version may
// behave differently (ADR-0031, amended 2026-10-19).
export function releaseClass(r: PoolRelease | undefined, reported: boolean, here: string | null | undefined) {
  if (!r) return reported ? 'warn' : 'outline';
  if (here && r.version !== here) return 'warn';
  return 'outline';
}
export function releaseTitle(r: PoolRelease | undefined, what: string, here: string | null | undefined) {
  const mine = here ? `this machine runs ${here}` : '';
  if (!r) return `${what} does not say which devstack it runs: an older one. ${mine}`.trim();
  const same = !here || r.version === here;
  return `${what} runs devstack ${r.version} (pool protocol ${r.protocol})${same ? '' : `; ${mine}`}`;
}

/** An update under way on a node (ADR-0039), or `null` when none is. */
export function updateText(n: PoolNode): string | null {
  const u = n.report?.update;
  if (!u || ['done', 'cancelled'].includes(u.state)) return null;
  if (u.state === 'draining') return 'updating: waits for its lanes';
  if (u.state === 'installing') return 'updating…';
  if (u.state === 'restarting') return 'restarting…';
  return `update ${u.state}`;
}

/** Who and what a node's policy lets run there (ADR-0033). */
export function policyText(p: NodePolicy | undefined) {
  if (!p) return 'everyone, any repository';
  const who = p.allow?.length ? p.allow.join(', ') : 'everyone';
  const what = p.repos?.length ? p.repos.join(', ') : 'any repository';
  const net = p.network === 'none' ? ' · no network outside the lane' : p.network === 'internet' ? ' · internet, not the local network' : '';
  const mem =
    (p.memory_headroom ? ` · memory need ×${p.memory_headroom}` : '') +
    (p.max_cpus ? ` · at most ${p.max_cpus} CPU per lane` : '') +
    (p.share_percent ? ` · offers ${p.share_percent}% of what is free` : '');
  return `${who} · ${what}${mem}${net}`;
}
