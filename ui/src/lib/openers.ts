// The path menu's programs (ADR-0046) in the order and with the ones hidden
// the user chose, in this browser only: the hub finds them, the page arranges
// them. A program not arranged yet (installed since) keeps the hub's place,
// after the arranged ones. Favourites also get a button beside the path; a
// hidden program is none.
import { ref } from 'vue';
import type { Opener } from '@/api/types';
import { persist } from './persist';

export type OpenerPrefs = { order: string[]; hidden: string[]; favorites: string[] };

const KEY = 'openers';

/** A stored value as read: one an older page wrote has no favourites. */
export function normalize(p: Partial<OpenerPrefs> | null | undefined): OpenerPrefs {
  const list = (x: unknown) => (Array.isArray(x) ? x.filter((v): v is string => typeof v === 'string') : []);
  return { order: list(p?.order), hidden: list(p?.hidden), favorites: list(p?.favorites) };
}

const prefs = ref<OpenerPrefs>(normalize(persist.get<Partial<OpenerPrefs>>(KEY)));

/** `found` in the chosen order; the hub's order for the ones never arranged. */
export function arrange(found: Opener[], p: OpenerPrefs): Opener[] {
  const rank = (o: Opener) => {
    const i = p.order.indexOf(o.id);
    return i < 0 ? p.order.length : i;
  };
  return found.map((o, i) => ({ o, i })).sort((a, b) => rank(a.o) - rank(b.o) || a.i - b.i).map(({ o }) => o);
}

/** The favourites of `found` that are shown, in the menu's order. */
export function favorites(found: Opener[], p: OpenerPrefs): Opener[] {
  return arrange(found, p).filter((o) => p.favorites.includes(o.id) && !p.hidden.includes(o.id));
}

export function useOpenerPrefs() {
  return {
    prefs,
    /** What the menu lists: arranged, without the hidden ones. */
    shown: (found: Opener[]) => arrange(found, prefs.value).filter((o) => !prefs.value.hidden.includes(o.id)),
    /** What gets a button beside the path. */
    favorites: (found: Opener[]) => favorites(found, prefs.value),
    save(next: OpenerPrefs) {
      prefs.value = normalize(next);
      persist.set(KEY, prefs.value);
    },
  };
}
