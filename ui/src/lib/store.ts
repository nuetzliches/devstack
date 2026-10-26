// The store of what `prepare` makes (ADR-0062), in words.
import type { PrepareView, StoreView } from '@/api/types';
import { bytes, duration } from './format';

/** A short time: tenths of a second below ten seconds (a copy out of the store), else as `duration`. */
export function shortTime(ms: number): string {
  if (ms < 0) ms = 0;
  return ms < 10_000 ? `${(ms / 1000).toFixed(1)}s` : duration(ms);
}

/** Where a service's last `prepare` outputs came from, in one line. */
export function prepareLine(p: PrepareView): string {
  if (p.source === 'store') {
    const made = p.prepare_ms !== undefined ? `; prepare took ${shortTime(p.prepare_ms)} when it made them` : '';
    return `from the store, copied in ${shortTime(p.ms)}${made}`;
  }
  const ran = `prepare ran, ${shortTime(p.ms)}`;
  if (!p.miss) return p.stored ? `${ran}; kept in the store` : `${ran} (it names no outputs: no store)`;
  return `${ran} (store miss: ${p.miss})${p.stored ? '; kept in the store' : ''}`;
}

/** The store in a few words: `3 entries, 1.2 GB`. */
export function storeLine(v: Pick<StoreView, 'entries' | 'bytes'>): string {
  const n = v.entries.length;
  if (!n) return 'empty';
  return `${n} ${n === 1 ? 'entry' : 'entries'}, ${bytes(v.bytes)}`;
}

/** A tree an entry is used by: a lane's name, or the main tree (`""`). */
export function treeLabel(tree: string): string {
  return tree === '' ? 'main tree' : tree;
}
