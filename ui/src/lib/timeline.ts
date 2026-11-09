// A session in time (the lane's Activity, view Timeline): an axis that
// pushes together the stretches where nothing happened (the agent waited
// for a prompt), the calls packed into rows, and the tokens summed up. Pure,
// so the view and its tests share it.
import type { SessionTimeline, TimelineCall, TimelineCallKind, TimelineTurn } from '@/api/types';

/** Nothing for this long: pushed together into a gap of `GAP_PX`. */
export const IDLE_MS = 120_000;
export const GAP_PX = 28;
/** Room around what happened, so a mark at the edge of a stretch shows whole. */
const PAD_MS = 8_000;

export type Span = { from: number; to: number };

/** A stretch where something happened, where it is on the axis. */
type Segment = Span & { x: number; w: number };

export type Axis = {
  x: (at: number) => number;
  /** The moment at a point of the axis (a gap: its start). */
  at: (x: number) => number;
  width: number;
  segments: Segment[];
  /** The stretches pushed together: where, and how long they were. */
  gaps: { x: number; ms: number }[];
  ticks: { x: number; at: number }[];
};

/** What happened when: each call from its start to its end, each turn, mark and hook as a moment, and `extra` moments. */
export function busySpans(t: SessionTimeline, extra: number[] = [], now = Date.now()): Span[] {
  const spans: Span[] = [];
  for (const c of t.calls) spans.push({ from: c.at, to: c.at + (c.ms ?? (t.ended ? 0 : Math.max(0, now - c.at))) });
  for (const x of [...t.turns.map((u) => u.at), ...t.marks.map((m) => m.at), ...t.hooks.map((h) => h.at), ...extra]) spans.push({ from: x, to: x });
  return spans;
}

const STEPS = [5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200].map((s) => s * 1000);

/** An axis `width` wide for `spans`, a pause longer than `idle` pushed together. */
export function axis(spans: Span[], width: number, idle = IDLE_MS, gapPx = GAP_PX): Axis {
  const sorted = spans.map((s) => ({ from: s.from - PAD_MS, to: Math.max(s.from, s.to) + PAD_MS })).sort((a, b) => a.from - b.from);
  const merged: Span[] = [];
  for (const s of sorted) {
    const last = merged[merged.length - 1];
    if (last && s.from - last.to <= idle) last.to = Math.max(last.to, s.to);
    else merged.push({ ...s });
  }
  if (!merged.length) merged.push({ from: 0, to: 1 });
  const gaps = merged.length - 1;
  const total = merged.reduce((n, s) => n + (s.to - s.from), 0);
  const k = Math.max(0, width - gaps * gapPx) / total;
  const segments: Segment[] = [];
  let x = 0;
  for (const s of merged) {
    const w = (s.to - s.from) * k;
    segments.push({ ...s, x, w });
    x += w + gapPx;
  }
  const toX = (at: number) => {
    let seg = segments[0];
    for (const s of segments) if (s.from <= at) seg = s;
    return seg.x + (Math.min(Math.max(at, seg.from), seg.to) - seg.from) * k;
  };
  const toAt = (px: number) => {
    let seg = segments[0];
    for (const s of segments) if (s.x <= px) seg = s;
    return seg.from + Math.min(Math.max(0, px - seg.x), seg.w) / (k || 1);
  };
  // Ticks: per stretch, the shortest round step at least 80 px apart.
  const step = STEPS.find((s) => s * k >= 80) ?? STEPS[STEPS.length - 1];
  const ticks: Axis['ticks'] = [];
  for (const s of segments) {
    for (let at = Math.ceil(s.from / step) * step; at <= s.to; at += step) ticks.push({ x: toX(at), at });
  }
  return {
    x: toX,
    at: toAt,
    width,
    segments,
    gaps: segments.slice(1).map((s, i) => ({ x: s.x - gapPx, ms: s.from - segments[i].to })),
    ticks,
  };
}

/** The rows of calls, in this order; a row without calls is left out. */
export const ROWS: { kind: TimelineCallKind; label: string }[] = [
  { kind: 'devstack', label: 'devstack' },
  { kind: 'upstream', label: 'Upstreams' },
  { kind: 'mcp', label: 'Other MCP' },
  { kind: 'builtin', label: 'Built-in' },
  { kind: 'agent', label: 'Sub-agents' },
];

/** Calls that overlap on the axis go one below the other: the line of each, and how many lines. */
export function pack(calls: TimelineCall[], x: (at: number) => number, end: (c: TimelineCall) => number, minPx = 3): { line: number[]; lines: number } {
  const ends: number[] = [];
  const line = calls.map((c) => {
    const from = x(c.at);
    let i = ends.findIndex((e) => e <= from);
    if (i < 0) i = ends.push(0) - 1;
    ends[i] = Math.max(x(end(c)), from + minPx) + 1;
    return i;
  });
  return { line, lines: Math.max(1, ends.length) };
}

export type Sum = { at: number; input: number; cache_write: number; cache_read: number; output: number };

/** The tokens summed up after each turn. */
export function cumulative(turns: TimelineTurn[]): Sum[] {
  const s = { input: 0, cache_write: 0, cache_read: 0, output: 0 };
  return turns.map((u) => {
    s.input += u.input;
    s.cache_write += u.cache_write;
    s.cache_read += u.cache_read;
    s.output += u.output;
    return { at: u.at, ...s };
  });
}

export const total = (s: Omit<Sum, 'at'>) => s.input + s.cache_write + s.cache_read + s.output;

/** The tokens summed up until `at` (the turns at or before it). */
export function sumAt(sums: Sum[], at: number): Sum {
  let last: Sum = { at, input: 0, cache_write: 0, cache_read: 0, output: 0 };
  for (const s of sums) {
    if (s.at > at) break;
    last = s;
  }
  return last;
}

/** How full the context was after each turn of the main agent (not a sub-agent's): what it read in. */
export function context(turns: TimelineTurn[]): { at: number; tokens: number }[] {
  return turns.filter((u) => !u.sidechain).map((u) => ({ at: u.at, tokens: u.input + u.cache_write + u.cache_read }));
}

/** The turn that asked for a call: the last one at or before it. */
export function turnOf(turns: TimelineTurn[], at: number): TimelineTurn | null {
  let found: TimelineTurn | null = null;
  for (const u of turns) {
    if (u.at > at) break;
    found = u;
  }
  return found;
}

/** Tokens, short: 950, 12.4k, 182k, 1.25M. */
export function tokens(n: number): string {
  if (n < 1000) return String(Math.round(n));
  if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

/** A tick's label: hours and minutes, seconds where the step is shorter than a minute. */
export function tickLabel(at: number, withSeconds: boolean): string {
  return new Date(at).toLocaleTimeString([], withSeconds ? { hour: '2-digit', minute: '2-digit', second: '2-digit' } : { hour: '2-digit', minute: '2-digit' });
}
