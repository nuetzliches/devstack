// The order of the stacks in the sidebar (#377): by name, or by when this
// browser last opened each. Both are kept in this browser, as the fold state
// is. "Last opened" is a snapshot taken when the page loads or the choice is
// made: opening a stack moves nothing while the page is open. Favourites
// (ADR-0086) list first, each part in that order; marking one is the user's
// own act on its row, so it moves at once.
import { ref } from 'vue';
import { persist } from './persist';

export type StackOrder = 'name' | 'opened';

const ORDER_KEY = 'sidebar.order';
const OPENED_KEY = 'sidebar.opened';
const FAVOURITES_KEY = 'sidebar.favourites';

export function savedOrder(): StackOrder {
  return persist.get<StackOrder>(ORDER_KEY) === 'opened' ? 'opened' : 'name';
}

export function saveOrder(order: StackOrder): void {
  persist.set(ORDER_KEY, order);
}

/** When this browser last opened each stack, by id (milliseconds). */
export function openedTimes(): Record<string, number> {
  return persist.get<Record<string, number>>(OPENED_KEY) ?? {};
}

export function noteOpened(id: string, at = Date.now()): void {
  persist.set(OPENED_KEY, { ...openedTimes(), [id]: at });
}

export function forgetOpened(id: string): void {
  const all = openedTimes();
  if (!(id in all)) return;
  delete all[id];
  persist.set(OPENED_KEY, all);
}

// One list for the sidebar and the switcher, so a star shows in both at once;
// read from the browser on first use.
const favourites = ref<string[] | null>(null);
const favs = (): string[] => (favourites.value ??= persist.get<string[]>(FAVOURITES_KEY) ?? []);
function saveFavs(next: string[]) {
  favourites.value = next;
  persist.set(FAVOURITES_KEY, next);
}

/** The stacks this browser marked as favourites, by id. */
export function useFavourites() {
  return {
    list: (): readonly string[] => favs(),
    has: (id: string) => favs().includes(id),
    toggle: (id: string) => saveFavs(favs().includes(id) ? favs().filter((x) => x !== id) : [...favs(), id]),
  };
}

export function forgetFavourite(id: string): void {
  if (favs().includes(id)) saveFavs(favs().filter((x) => x !== id));
}

type Named = { id: string; name: string };

const byName = (a: Named, b: Named) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id);

/**
 * The stacks in the chosen order, the favourites first. By last opened, the
 * most recent first; a stack never opened here (or added since the
 * snapshot) follows, by name.
 */
export function orderStacks<T extends Named>(
  stacks: readonly T[],
  order: StackOrder,
  opened: Record<string, number>,
  favourites: readonly string[] = [],
): T[] {
  const fav = (s: Named) => (favourites.includes(s.id) ? 0 : 1);
  const within = order === 'name' ? byName : (a: Named, b: Named) => (opened[b.id] ?? -1) - (opened[a.id] ?? -1) || byName(a, b);
  return [...stacks].sort((a, b) => fav(a) - fav(b) || within(a, b));
}
