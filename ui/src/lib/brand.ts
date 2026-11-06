// The mark, its back (the nut of the nuts family, as in their other apps)
// and the dot that says something needs attention (ADR-0097): for the top
// bar and, as SVG text, for the favicon.
import { computed, ref } from 'vue';
import type { AttentionItem } from '@/api/types';
import { useHub } from '@/stores/hub';
import { usePool } from '@/stores/pool';

/** Something failed (red), or something waits for the user (yellow). */
export type AttentionTone = 'error' | 'warn';

/** Whether the mark shows its back now (the pointer is on it): the favicon turns with it. */
export const markFlipped = ref(false);

/**
 * The dot's colour, from the worst there is: the hub out of reach or an
 * `error` item is red; a `warn` item, a request waiting for a pool admin or
 * a skill set waiting for an answer is yellow; else none.
 */
export function attentionTone(a: {
  unreachable: boolean;
  items: readonly Pick<AttentionItem, 'level'>[];
  poolWaiting: number;
  skillsWaiting: number;
}): AttentionTone | null {
  if (a.unreachable || a.items.some((i) => i.level !== 'warn')) return 'error';
  if (a.items.length || a.poolWaiting || a.skillsWaiting) return 'warn';
  return null;
}

/** The dot's colours: those of the dark theme, which the tile has. */
export const DOT: Record<AttentionTone, string> = { error: '#f2667a', warn: '#f0b043' };

/** The front's three lines and the point, on a 512 tile (as `favicon.svg`). */
const MARK_PATHS =
  '<g fill="none" stroke-linecap="round" stroke-linejoin="round" stroke-width="36"><path d="M136 176h240" stroke="#5b8cff"/><path d="M136 256h168" stroke="#3ecf8e"/><path d="M136 336h240" stroke="#5b8cff" opacity=".55"/></g><circle cx="364" cy="256" r="30" fill="#3ecf8e"/>';

/** The nut (Lucide's `nut`, ISC), on a 512 tile; `BrandMark` draws it too. */
const NUT_PATHS =
  '<svg x="84" y="84" width="344" height="344" viewBox="0 0 24 24" fill="none" stroke="#3ecf8e" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16.847 5.847L20 9a7.23 7.23 0 0 1 1.551 7.516C21.241 17.352 21 17.932 21 19v1a1 1 0 0 1-1 1h-1c-1.069 0-1.648.242-2.485.552A7.2 7.2 0 0 1 9.002 20l-3.155-3.153"/><path d="M18.21 5.43c-1.71.69-5.07 1.07-6.71 1.07c.46 1.38.91 2.74.61 4.88a.88.88 0 0 1-.73.74c-1.78.28-3.54-.17-4.88-.62c0 1.64-.38 5-1.07 6.71c-.21.52-.82.55-1.17.12A10 10 0 0 1 18.33 4.26c.43.35.4.97-.12 1.17"/><path d="M4.93 4.93L3 3a.7.7 0 0 1 0-1"/></svg>';

/** The favicon: the front or the nut, with the dot top right when there is one. */
export function faviconSvg(face: 'mark' | 'nut', tone: AttentionTone | null): string {
  const dot = tone ? `<circle cx="400" cy="112" r="104" fill="${DOT[tone]}" stroke="#fff" stroke-width="24"/>` : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" rx="112" fill="#0f1115"/>${face === 'nut' ? NUT_PATHS : MARK_PATHS}${dot}</svg>`;
}

/** `faviconSvg` as a URL for `<link rel="icon">` (the page's CSP allows `data:` images). */
export function faviconHref(face: 'mark' | 'nut', tone: AttentionTone | null): string {
  return `data:image/svg+xml,${encodeURIComponent(faviconSvg(face, tone))}`;
}

/** The dot's colour for this hub: `attentionTone` of what the stores know. */
export function useAttentionTone() {
  const hub = useHub();
  const pool = usePool();
  return computed(() =>
    attentionTone({
      unreachable: hub.status === 'unreachable' || (hub.status === 'ok' && hub.stream === 'error'),
      items: hub.attention,
      poolWaiting: pool.dueAll,
      skillsWaiting: hub.skillsWaiting,
    }),
  );
}

/** What the dot says, in words: its tooltip and the screen reader's. */
export function attentionLabel(tone: AttentionTone | null): string {
  if (tone === 'error') return 'Something failed or the hub is not reachable';
  if (tone === 'warn') return 'Something waits for you';
  return '';
}
