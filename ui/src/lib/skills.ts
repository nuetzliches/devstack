// The pools' skills on this machine (ADR-0074, ADR-0077) as the Skills page
// lists them: one row per skill, with the set it comes from, filtered by
// text, set, group and state. Pure, so the page and its tests share it.
import type { PoolSkillsState, SkillSetState, SkillState } from '@/api/types';

/** What a row shows of a skill's state, in the order the filter offers them. */
export type SkillStatus = 'installed' | 'pending' | 'off' | 'conflict' | 'not-installed' | 'not-accepted';

export const STATUS_LABEL: Record<SkillStatus, string> = {
  installed: 'installed',
  pending: 'update waits',
  off: 'off',
  conflict: 'conflict',
  'not-installed': 'not installed',
  'not-accepted': 'set not accepted',
};

export type SkillRow = { set: SkillSetState; skill: SkillState; status: SkillStatus; key: string };

/** A set's key, `pool:set`. */
export const setKey = (s: Pick<SkillSetState, 'pool' | 'set'>) => `${s.pool}:${s.set}`;

/** `''` for a skill without a group. */
export const groupOf = (k: SkillState) => k.group ?? '';

export function statusOf(set: SkillSetState, k: SkillState, conflicts: Set<string>): SkillStatus {
  if (set.state !== 'accepted') return 'not-accepted';
  if (k.enabled === false) return 'off';
  if (conflicts.has(k.name)) return 'conflict';
  if (k.pending) return 'pending';
  return k.installed ? 'installed' : 'not-installed';
}

/** Every skill of every set, as rows. */
export function rows(state: PoolSkillsState | null): SkillRow[] {
  if (!state) return [];
  const conflicts = new Set(state.conflicts.map((c) => c.name));
  return state.sets.flatMap((set) => set.skills.map((skill) => ({ set, skill, status: statusOf(set, skill, conflicts), key: `${setKey(set)}/${skill.name}` })));
}

export type SkillFilter = { text: string; set: string; group: string | null; status: SkillStatus | '' };

export const NO_FILTER: SkillFilter = { text: '', set: '', group: null, status: '' };

/** The rows that pass `f`: text in the name, description or tags; set by key; group (`''`: none); status. */
export function filtered(all: SkillRow[], f: SkillFilter): SkillRow[] {
  const t = f.text.trim().toLowerCase();
  return all.filter(
    (r) =>
      (!f.set || setKey(r.set) === f.set) &&
      (f.group === null || groupOf(r.skill) === f.group) &&
      (!f.status || r.status === f.status) &&
      (!t || r.skill.name.toLowerCase().includes(t) || r.skill.description.toLowerCase().includes(t) || (r.skill.tags ?? []).some((x) => x.toLowerCase().includes(t))),
  );
}

/** How many rows have each status, for the filter chips. */
export function counts(all: SkillRow[]): Partial<Record<SkillStatus, number>> {
  const out: Partial<Record<SkillStatus, number>> = {};
  for (const r of all) out[r.status] = (out[r.status] ?? 0) + 1;
  return out;
}

/** The groups of the rows, in order of first appearance; `''` for skills without one, last. */
export function groups(all: SkillRow[]): string[] {
  const seen = [...new Set(all.map((r) => groupOf(r.skill)))];
  return [...seen.filter((g) => g), ...(seen.includes('') ? [''] : [])];
}
