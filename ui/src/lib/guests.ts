// Lanes of other members on this machine ("guest lanes"), as the banner, the
// share tile and the pool area show them.
import type { Guest } from '@/api/types';
import { bytes } from '@/lib/format';

/** `anna: shop · feat-cart` */
export function guestLabel(g: Guest): string {
  return `${g.owner}: ${g.repo} · ${g.lane}`;
}

/** What the guest lanes asked for together: `3.5 CPU · 7.0 GB`. */
export function guestsReserved(gs: Guest[]): string {
  const cpus = gs.reduce((a, g) => a + g.cpus, 0);
  const memory = gs.reduce((a, g) => a + g.memory, 0);
  return `${Math.round(cpus * 10) / 10} CPU · ${bytes(memory)}`;
}

/** Changes whenever a guest lane comes or goes: a dismissed banner shows again. */
export function guestsSignature(gs: Guest[]): string {
  return gs
    .map((g) => `${g.pool}/${g.lease}`)
    .sort()
    .join(',');
}
