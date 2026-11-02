// The order of the stacks in the sidebar (#377): by name, or by when this
// browser last opened each. Both are kept in this browser, as the fold state
// is. "Last opened" is a snapshot taken when the page loads or the choice is
// made: opening a stack moves nothing while the page is open.
import { persist } from './persist';

export type StackOrder = 'name' | 'opened';

const ORDER_KEY = 'sidebar.order';
const OPENED_KEY = 'sidebar.opened';

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

type Named = { id: string; name: string };

const byName = (a: Named, b: Named) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id);

/**
 * The stacks in the chosen order. By last opened, the most recent first; a
 * stack never opened here (or added since the snapshot) follows, by name.
 */
export function orderStacks<T extends Named>(stacks: readonly T[], order: StackOrder, opened: Record<string, number>): T[] {
  if (order === 'name') return [...stacks].sort(byName);
  return [...stacks].sort((a, b) => (opened[b.id] ?? -1) - (opened[a.id] ?? -1) || byName(a, b));
}
