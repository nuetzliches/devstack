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

/**
 * Another approved member that looks like the same machine as `m`: the same
 * address it asked from, or the same name up to a suffix such as `-wsl` (a
 * setup by hand in WSL before the share switch). Only a hint: the pool has no
 * id of a machine beside its members.
 */
export function sameMachine(m: PoolMember, o: PoolOverview | null | undefined): PoolMember | null {
  const others = (o?.members ?? []).filter((x) => x.id !== m.id && x.state === 'approved');
  return others.find((x) => (m.address && x.address === m.address) || stem(x.name) === stem(m.name)) ?? null;
}
