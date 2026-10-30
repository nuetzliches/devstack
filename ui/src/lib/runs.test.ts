import { describe, expect, it } from 'vitest';
import { runs } from './runs';

describe('runs', () => {
  it('splits where the gap is longer than allowed', () => {
    expect(runs([0, 30, 60, 600, 630], 120)).toEqual([
      [0, 1, 2],
      [3, 4],
    ]);
  });

  it('keeps a history without gaps whole, and has none for no points', () => {
    expect(runs([0, 30, 60], 120)).toEqual([[0, 1, 2]]);
    expect(runs([], 120)).toEqual([]);
  });
});
