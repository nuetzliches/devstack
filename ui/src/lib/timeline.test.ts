// A session in time: pauses pushed together, overlapping calls one below
// the other, tokens summed up.
import { describe, expect, it } from 'vitest';
import type { TimelineCall, TimelineTurn } from '@/api/types';
import { axis, context, cumulative, GAP_PX, pack, sumAt, tokens, total, turnOf } from './timeline';

const s = 1000;
const min = 60 * s;

describe('the axis', () => {
  it('pushes a pause longer than two minutes together, and keeps the order of time', () => {
    const a = axis([{ from: 0, to: 30 * s }, { from: 40 * s, to: 50 * s }, { from: 20 * min, to: 21 * min }], 1000);
    expect(a.segments).toHaveLength(2);
    expect(a.gaps).toHaveLength(1);
    expect(a.gaps[0].ms).toBeGreaterThan(15 * min);
    // Both stretches fill the width but the gap.
    const last = a.segments[1];
    expect(last.x + last.w).toBeCloseTo(1000);
    expect(a.segments[1].x - (a.segments[0].x + a.segments[0].w)).toBeCloseTo(GAP_PX);
    // Later is further right, across the gap too; a moment in the gap sits at its start.
    expect(a.x(10 * s)).toBeLessThan(a.x(45 * s));
    expect(a.x(45 * s)).toBeLessThan(a.x(20 * min + 10 * s));
    expect(a.x(10 * min)).toBeCloseTo(a.segments[0].x + a.segments[0].w);
    // And back.
    expect(a.at(a.x(20 * min + 30 * s))).toBeCloseTo(20 * min + 30 * s, -2);
  });

  it('keeps a short pause as it is', () => {
    const a = axis([{ from: 0, to: 10 * s }, { from: 90 * s, to: 100 * s }], 500);
    expect(a.gaps).toHaveLength(0);
    expect(a.ticks.length).toBeGreaterThan(1);
  });
});

describe('the calls', () => {
  it('puts calls that overlap one below the other', () => {
    const c = (at: number, ms: number): TimelineCall => ({ at, ms, tool: 'Bash', kind: 'builtin', ok: true });
    const calls = [c(0, 10), c(5, 10), c(20, 5), c(21, 1)];
    const p = pack(calls, (at) => at, (x) => x.at + (x.ms ?? 0), 1);
    expect(p.line).toEqual([0, 1, 0, 1]);
    expect(p.lines).toBe(2);
  });
});

describe('the tokens', () => {
  const turns: TimelineTurn[] = [
    { at: 10, input: 5, cache_write: 1000, cache_read: 20_000, output: 300 },
    { at: 20, input: 3, cache_write: 400, cache_read: 21_000, output: 100, sidechain: true },
    { at: 30, input: 2, cache_write: 500, cache_read: 21_000, output: 200 },
  ];

  it('sums them up per turn, and says what was taken until a call', () => {
    const sums = cumulative(turns);
    expect(total(sums[2])).toBe(5 + 1000 + 20_000 + 300 + 3 + 400 + 21_000 + 100 + 2 + 500 + 21_000 + 200);
    expect(sumAt(sums, 25).output).toBe(400);
    expect(total(sumAt(sums, 5))).toBe(0);
    expect(turnOf(turns, 25)?.at).toBe(20);
  });

  it('reads the context of the main agent only', () => {
    expect(context(turns).map((c) => c.tokens)).toEqual([21_005, 21_502]);
  });

  it('writes them short', () => {
    expect([tokens(950), tokens(12_400), tokens(182_000), tokens(1_250_000)]).toEqual(['950', '12k', '182k', '1.25M']);
    expect(tokens(4_250)).toBe('4.3k');
  });
});
