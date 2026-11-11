// The order of the stacks in the sidebar (#377): by name, or by when this
// browser last opened each. Both are kept in this browser, as the fold state
// is. "Last opened" is a snapshot taken when the page loads or the choice is
// made: opening a stack moves nothing while the page is open. Favourites
// (ADR-0086) list first, each part in that order; marking one is the user's
// own act on its row, so it moves at once.
import { ref } from 'vue';
import { persist } from './persist';

export type StackOrder = 'name' | 'opened' | 'edited';

const ORDER_KEY = 'sidebar.order';
const OPENED_KEY = 'sidebar.opened';
const FAVOURITES_KEY = 'sidebar.favourites';
const HIDDEN_KEY = 'sidebar.hidden';

export function savedOrder(): StackOrder {
  const saved = persist.get<StackOrder>(ORDER_KEY);
  return saved === 'opened' || saved === 'edited' ? saved : 'name';
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

// Hidden stacks (this browser only, like the favourites): out of the list
// until shown again; the stack stays registered, unlike "Remove".
const hidden = ref<string[] | null>(null);
const hid = (): string[] => (hidden.value ??= persist.get<string[]>(HIDDEN_KEY) ?? []);
function saveHidden(next: string[]) {
  hidden.value = next;
  persist.set(HIDDEN_KEY, next);
}

/** The stacks this browser hid, by id. */
export function useHidden() {
  return {
    list: (): readonly string[] => hid(),
    has: (id: string) => hid().includes(id),
    hide: (id: string) => {
      if (!hid().includes(id)) saveHidden([...hid(), id]);
    },
    show: (id: string) => {
      if (hid().includes(id)) saveHidden(hid().filter((x) => x !== id));
    },
  };
}

export function forgetHidden(id: string): void {
  useHidden().show(id);
}

type Named = { id: string; name: string };

const byName = (a: Named, b: Named) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id);

/**
 * The stacks in the chosen order, the favourites first. By last opened, the
 * most recent first; a stack never opened here (or added since the
 * snapshot) follows, by name. By last edited, the newest commit first
 * (`edited`: its time by stack id, a snapshot as "opened" is); one without
 * a known commit follows, by name.
 */
export function orderStacks<T extends Named>(
  stacks: readonly T[],
  order: StackOrder,
  opened: Record<string, number>,
  favourites: readonly string[] = [],
  edited: Record<string, number> = {},
): T[] {
  const fav = (s: Named) => (favourites.includes(s.id) ? 0 : 1);
  const newest = (times: Record<string, number>) => (a: Named, b: Named) => (times[b.id] ?? -1) - (times[a.id] ?? -1) || byName(a, b);
  const within = order === 'name' ? byName : order === 'edited' ? newest(edited) : newest(opened);
  return [...stacks].sort((a, b) => fav(a) - fav(b) || within(a, b));
}

/**
 * The stacks that match what was typed: every word of it is part of the
 * stack's name or repository folder, or of the label of one of its worktrees
 * (`labels`), whatever the case. `byWorktree` names the stacks that match
 * only through a worktree, so the list can show those open.
 */
export function filterStacks<T extends Named & { repo: string }>(
  stacks: readonly T[],
  query: string,
  labels: (s: T) => readonly string[],
): { stacks: T[]; byWorktree: Set<string> } {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return { stacks: [...stacks], byWorktree: new Set() };
  const byWorktree = new Set<string>();
  const kept = stacks.filter((s) => {
    const own = `${s.name} ${s.repo}`.toLowerCase();
    const all = `${own} ${labels(s).join(' ')}`.toLowerCase();
    if (!words.every((w) => all.includes(w))) return false;
    if (!words.every((w) => own.includes(w))) byWorktree.add(s.id);
    return true;
  });
  return { stacks: kept, byWorktree };
}
