// The last state the dashboard saw, kept in localStorage so the page can show
// it while the hub or a daemon is away (ADR-0021: "shows the last state it saw,
// marked as such"). Small JSON documents only.
import type { StackEntry, StackView } from '@/api/types';

const PREFIX = 'devstack.';

function read<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // quota or private mode: the cache is a nicety, not a requirement
  }
}

function remove(key: string): void {
  try {
    localStorage.removeItem(PREFIX + key);
  } catch {
    /* ignore */
  }
}

export type CachedHub = { stacks: StackEntry[]; version: string | null; at: number; linkUnavailable?: string | null };
export type CachedStack = { view: StackView; at: number };

export const persist = {
  hub: () => read<CachedHub>('hub'),
  saveHub: (v: CachedHub) => write('hub', v),

  stack: (id: string) => read<CachedStack>(`stack.${id}`),
  saveStack: (id: string, v: CachedStack) => write(`stack.${id}`, v),
  forgetStack: (id: string) => remove(`stack.${id}`),

  lastStack: () => read<string>('lastStack'),
  saveLastStack: (id: string) => write('lastStack', id),

  get: read,
  set: write,
};
