// Machines that ask to run the pool's lanes (#599): a member switched sharing
// on, its node agent reports, and no admin granted it yet. Derived from the
// pool overview alone (members and nodes); the hub tells admins on the desktop.
import type { PoolMember, PoolNode, PoolOverview } from '@/api/types';

export type Asking = { member: PoolMember; node: PoolNode };

/** The approved members without a grant whose agent reports sharing, online. */
export function asking(o: PoolOverview | null | undefined): Asking[] {
  const nodes = o?.nodes ?? [];
  return (o?.members ?? []).flatMap((member) => {
    if (member.state !== 'approved' || member.node) return [];
    const node = nodes.find((n) => n.id === member.id);
    return node?.online && node.report?.sharing ? [{ member, node }] : [];
  });
}

/** The name with a suffix taken off (`laptop-wsl` → `laptop`), lower case. */
function stem(name: string): string {
  return name.toLowerCase().replace(/[-_.](wsl\d*|ubuntu[\w.-]*|debian|linux|node\d*)$/, '');
}

export type Related = { member: PoolMember; by: 'address' | 'name' };

/**
 * Another approved member that may be the same machine as `m`: one that asked
 * from the same address, else one with the same name up to a suffix such as
 * `-wsl` (a setup by hand in WSL before the share switch). Only a hint: the
 * pool has no id of a machine beside its members, and a member picks its own
 * name, so a name alone proves nothing (review before 0.52.0); the page says
 * which it is and names the other's id.
 */
export function related(m: PoolMember, o: PoolOverview | null | undefined): Related | null {
  const others = (o?.members ?? []).filter((x) => x.id !== m.id && x.state === 'approved');
  const byAddress = m.address ? others.find((x) => x.address === m.address) : undefined;
  if (byAddress) return { member: byAddress, by: 'address' };
  const byName = others.find((x) => stem(x.name) === stem(m.name));
  return byName ? { member: byName, by: 'name' } : null;
}

/** The hint as the Admin tab shows it. */
export function relatedText(r: Related): string {
  const who = `${r.member.name} (${r.member.id.slice(0, 8)})`;
  const runs = r.member.node ? '; that one may run lanes already' : '';
  return r.by === 'address' ? `asked from the same address as ${who}${runs}` : `a similar name to ${who}, not checked${runs}`;
}
