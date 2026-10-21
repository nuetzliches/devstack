// The path menu's programs (ADR-0046) in the order and with the ones hidden
// the user chose, in this browser only: the hub finds them, the page arranges
// them. A program not arranged yet (installed since) keeps the hub's place,
// after the arranged ones.
import { ref } from 'vue';
import type { Opener } from '@/api/types';
import { persist } from './persist';

export type OpenerPrefs = { order: string[]; hidden: string[] };

const KEY = 'openers';
const prefs = ref<OpenerPrefs>(persist.get<OpenerPrefs>(KEY) ?? { order: [], hidden: [] });

/** `found` in the chosen order; the hub's order for the ones never arranged. */
export function arrange(found: Opener[], p: OpenerPrefs): Opener[] {
  const rank = (o: Opener) => {
    const i = p.order.indexOf(o.id);
    return i < 0 ? p.order.length : i;
  };
  return found.map((o, i) => ({ o, i })).sort((a, b) => rank(a.o) - rank(b.o) || a.i - b.i).map(({ o }) => o);
}

export function useOpenerPrefs() {
  return {
    prefs,
    /** What the menu lists: arranged, without the hidden ones. */
    shown: (found: Opener[]) => arrange(found, prefs.value).filter((o) => !prefs.value.hidden.includes(o.id)),
    save(next: OpenerPrefs) {
      prefs.value = next;
      persist.set(KEY, next);
    },
  };
}
