// Sorting a table by its column headers. Every table in the dashboard uses
// this with <SortTh>: a click sorts by the column, a second click turns the
// order round, a third goes back to the table's own order. The choice is
// remembered per table in this browser.
import { computed, ref, type Ref } from 'vue';

export type SortDir = 'asc' | 'desc';
export type SortValue = string | number | null | undefined;
export type SortState<K extends string> = { key: K; dir: SortDir } | null;

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** Compares two cell values; empty ones go last whatever the direction. */
export function compareValues(a: SortValue, b: SortValue, dir: SortDir): number {
  const ea = a === null || a === undefined || a === '';
  const eb = b === null || b === undefined || b === '';
  if (ea || eb) return ea === eb ? 0 : ea ? 1 : -1;
  const c = typeof a === 'number' && typeof b === 'number' ? a - b : collator.compare(String(a), String(b));
  return dir === 'asc' ? c : -c;
}

function load<K extends string>(id: string, keys: readonly string[]): SortState<K> | undefined {
  try {
    const raw = localStorage.getItem(`devstack.sort.${id}`);
    if (raw === null) return undefined;
    const v = JSON.parse(raw) as SortState<K>;
    if (v === null) return null;
    return keys.includes(v.key) && (v.dir === 'asc' || v.dir === 'desc') ? v : undefined;
  } catch {
    return undefined;
  }
}

function save(id: string, v: SortState<string>) {
  try {
    localStorage.setItem(`devstack.sort.${id}`, JSON.stringify(v));
  } catch {
    // storage unavailable: the order lasts for this page only
  }
}

/**
 * `columns` maps each sortable column to the value it sorts by. Numbers start
 * descending (the largest first), text ascending.
 */
export function useSort<T, K extends string>(rows: Ref<T[]>, columns: Record<K, (row: T) => SortValue>, opts: { id: string; initial?: SortState<K> }) {
  const keys = Object.keys(columns) as K[];
  const state = ref<SortState<K>>(load<K>(opts.id, keys) ?? opts.initial ?? null) as Ref<SortState<K>>;

  const sorted = computed(() => {
    const s = state.value;
    if (!s) return rows.value;
    const get = columns[s.key];
    // Ties keep the table's own order (Array.prototype.sort is stable).
    return [...rows.value].sort((a, b) => compareValues(get(a), get(b), s.dir));
  });

  function firstDir(key: K): SortDir {
    const sample = rows.value.map(columns[key]).find((v) => v !== null && v !== undefined && v !== '');
    return typeof sample === 'number' ? 'desc' : 'asc';
  }

  function toggle(key: K) {
    const s = state.value;
    const first = firstDir(key);
    if (!s || s.key !== key) state.value = { key, dir: first };
    else if (s.dir === first) state.value = { key, dir: first === 'asc' ? 'desc' : 'asc' };
    else state.value = opts.initial ?? null;
    save(opts.id, state.value);
  }

  const dirOf = (key: K): SortDir | null => (state.value?.key === key ? state.value.dir : null);

  return { sorted, state, toggle, dirOf };
}
