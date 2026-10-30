// Where a stack and its lanes are in the dashboard (ADR-0081): the stack's
// page at `#/s/<stack>`, a lane's views at `#/s/<stack>/l/<lane>/...`, the
// main working tree as the lane `main` (ADR-0080). The lane shown is in the
// URL; the store follows it.
import type { RouteLocationRaw } from 'vue-router';
import { MAIN } from './lanes';

/** The lane of a route's `:lane`: `null` for the main working tree, as the stack store has it. */
export function laneOfParam(p: unknown): string | null {
  return typeof p !== 'string' || !p || p === MAIN ? null : p;
}

/** The `:lane` of a lane (the store's `null`: `main`). */
export function laneParam(lane: string | null | undefined): string {
  return lane || MAIN;
}

/** A stack's page. */
export function stackPage(stack: string): RouteLocationRaw {
  return { name: 'stack', params: { stack } };
}

/** A lane's view: `tab` (`overview` when not named), `main` for the main working tree. */
export function laneView(stack: string, lane: string | null | undefined, tab = 'overview', more: Record<string, string> = {}): RouteLocationRaw {
  return { name: tab, params: { stack, lane: laneParam(lane), ...more } };
}

/** The tabs of a lane's view whose paths an older link had right under `#/s/<stack>/`. */
const OLD = /^(services|infra|commands|usage|events)(\/|$)/;

/**
 * Where an older link goes (before ADR-0081 the lane was a `?lane=` query,
 * empty for the main working tree, and the tabs were under `#/s/<stack>/`):
 * the same tab of the lane it named, else of the lane shown last
 * (`remembered`), else of `main`. `null`: no older link (the stack's page).
 */
export function oldLaneLink(
  stack: string,
  rest: string,
  query: Record<string, unknown>,
  remembered: string | null | undefined,
): { path: string; query: Record<string, string> } | null {
  const named = 'lane' in query;
  if (!named && !OLD.test(rest)) return null;
  if (rest && !OLD.test(rest)) return null;
  const q = query.lane;
  const lane = named ? (typeof q === 'string' && q ? q : MAIN) : laneParam(remembered);
  const kept: Record<string, string> = {};
  for (const [k, v] of Object.entries(query)) if (k !== 'lane' && typeof v === 'string') kept[k] = v;
  return { path: `/s/${encodeURIComponent(stack)}/l/${encodeURIComponent(lane)}${rest ? `/${rest}` : ''}`, query: kept };
}
