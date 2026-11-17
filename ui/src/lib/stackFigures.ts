// What the start page shows of one stack, as plain values: the state, the
// main tree's services ready of all, the lanes started, since when the
// daemon runs, the last commit and what needs attention. One source for the
// tile (large and small), the list's row and the list's sorting, so the three
// cannot disagree.
import type { AttentionItem, StackEntry } from '@/api/types';
import type { Tone } from '@/lib/status';
import type { useHub } from '@/stores/hub';

type Hub = ReturnType<typeof useHub>;

export type StackLook = {
  label: string;
  tone: Tone;
  busy: boolean;
  /** For sorting: the more urgent, the larger (needs attention, starting, running, stopping, stopped, unknown). */
  rank: number;
};

export type StackFigures = {
  look: StackLook;
  /** Services ready of all in the main tree; `null` while the stack does not run or the hub has no figures. */
  services: { ready: number; total: number; share: number } | null;
  /** Lanes started; `null` while the stack does not run. */
  lanes: number | null;
  /** When the daemon started (milliseconds); `null` while the stack does not run. */
  upSince: number | null;
  /** The newest commit (milliseconds); `null` while unknown. */
  committedAt: number | null;
  /** The stack's items that need attention. */
  items: AttentionItem[];
};

function lookOf(s: StackEntry, hub: Hub, items: readonly AttentionItem[]): StackLook {
  if (!hub.reachable) return { label: 'Unknown', tone: 'neutral', busy: false, rank: 0 };
  // A start or stop this page asked for, before the hub's figures say it.
  if (hub.stopping[s.id]) return { label: 'Stopping', tone: 'neutral', busy: true, rank: 2 };
  if (hub.starting[s.id] && !s.running) return { label: 'Starting', tone: 'accent', busy: true, rank: 4 };
  const state = hub.glances[s.id]?.state ?? (s.start_error ? 'attention' : s.running ? 'ok' : 'stopped');
  switch (state) {
    case 'attention':
      return { label: 'Needs attention', tone: items.some((i) => i.level === 'error') || s.start_error ? 'danger' : 'warn', busy: false, rank: 5 };
    case 'starting':
      return { label: 'Starting', tone: 'accent', busy: true, rank: 4 };
    case 'stopping':
      return { label: 'Stopping', tone: 'neutral', busy: true, rank: 2 };
    case 'ok':
      return { label: 'Running', tone: 'ok', busy: false, rank: 3 };
    default:
      return { label: 'Stopped', tone: 'neutral', busy: false, rank: 1 };
  }
}

export function stackFigures(s: StackEntry, hub: Hub): StackFigures {
  const glance = hub.glances[s.id] ?? null;
  const items = hub.attention.filter((a) => a.stack === s.id);
  return {
    look: lookOf(s, hub, items),
    services: s.running && glance && glance.total ? { ready: glance.ready, total: glance.total, share: glance.ready / glance.total } : null,
    lanes: s.running && glance ? glance.lanes : null,
    upSince: s.running && s.daemon ? s.daemon.started : null,
    committedAt: s.last_commit ? s.last_commit * 1000 : null,
    items,
  };
}
